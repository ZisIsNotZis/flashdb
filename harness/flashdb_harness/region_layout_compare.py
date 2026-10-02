"""Region layout axis at equal offered load, on the real inventory flow (Slice 25).

Why
---
Slice 24 built region-partitioned tiles and a per-region ``(page_size, bloom_bits)``
policy, but every "real workload" measurement so far was memtable-only: the pacer
(``flashdb-load``) and ``flashdb-replay`` opened with ``Engine::open`` (no tile
discovery) and ``seed-inventory`` never published tiles. So the per-region page
size axis had no way to act on a flow read.

This harness closes that loop end to end:

1. ``flashdb-scale seed-inventory`` applies the Customer/Stock seed and, when
   ``FLASHDB_SEED_CHECKPOINT_BYTES`` is set, publishes it as tiles through the
   same ``maybe_checkpoint``/``publish_tile`` protocol ``seed`` uses;
2. ``flashdb-replay <engine-dir>`` (tile-aware open) runs the write phase of the
   flow, reading the seeded dimensions from tiles and writing StockMovement /
   Order rows;
3. ``flashdb-scale checkpoint <dir>`` publishes those writes as tiles too, so the
   scan-heavy region (``P|StockMovement``) is tile-resident;
4. ``flashdb-scale tileinfo <dir>`` reports the active tile count, each tile's
   page size / bloom / fence / region, and the serving-memtable entry count;
5. ``flashdb-load <engine-dir>`` (``Engine::open_discover``) offers a read-only
   trace at one fixed rate; the same command with ``--no-tiles`` opens the WAL
   only, so the identical data is replayed into the memtable -- the controlled
   tile-vs-memtable A/B.

The two configurations under test differ *only* in the region layout policy:

* ``per_region-4k``    -- per-region tiles, 4 KiB pages everywhere (the "global
                          4 KiB" default page size);
* ``per_region-mixed``  -- per-region tiles, 64 KiB for ``P|StockMovement`` and
                          4 KiB for every probe region;
* ``global-4k``         -- one whole-keyspace tile, 4 KiB (reference).

Usage (from the harness directory)::

    uv run python -m flashdb_harness.region_layout_compare --release
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from statistics import median

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import Generator, render  # noqa: E402
from flashdb_harness.load_profile import LoadProfile, Segment, generate  # noqa: E402
from flashdb_harness.seed import SeedGenerator  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]

# ``P|StockMovement`` is the scan-heavy region: movements_scan walks the whole
# StockMovement primary prefix and filters by order_no. Probes (Stock/Customer)
# stay at the 4 KiB default, so the two configs differ only on the scanned region.
MIXED_REGION_LAYOUT = "P|StockMovement=65536:10"

CONFIGS = (
    ("per_region-4k", "per_region", None,
     "per-region tiles, 4 KiB pages everywhere"),
    ("per_region-mixed", "per_region", MIXED_REGION_LAYOUT,
     "per-region tiles, 64 KiB P|StockMovement + 4 KiB rest"),
    ("global-4k", "global", None,
     "one whole-keyspace tile, 4 KiB"),
)


# ---------------------------------------------------------------------------
# Pure output parsing (tested without running an engine)
# ---------------------------------------------------------------------------
def _kv(line: str, key: str) -> str | None:
    """First ``key=value`` token on ``line`` (value is the non-space run)."""
    m = re.search(rf"(?:^|\s){re.escape(key)}=(\S+)", line)
    return m.group(1) if m else None


def parse_load_output(text: str) -> dict:
    """Parse the pacer's machine-readable stdout into a summary dict.

    ``first_error`` may contain spaces, so only the fixed-schema fields are
    extracted by token, never the line split as a whole.
    """
    out: dict = {
        "device_fsync_p50_us": None,
        "device_fsync_p99_us": None,
        "active_tiles": None,
        "serving_memtable_entries": None,
        "discover": None,
        "windows": [],
        "requests": None,
        "completed": None,
        "errors": None,
        "else_used": None,
        "achieved_rps": None,
        "elapsed_ms": None,
    }
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("device_fsync_p50_us="):
            out["device_fsync_p50_us"] = int(_kv(line, "device_fsync_p50_us"))
            out["device_fsync_p99_us"] = int(_kv(line, "device_fsync_p99_us"))
        elif line.startswith("engine_open "):
            out["discover"] = _kv(line, "discover")
            out["active_tiles"] = int(_kv(line, "active_tiles"))
            out["serving_memtable_entries"] = int(_kv(line, "serving_memtable_entries"))
        elif line.startswith("window="):
            out["windows"].append({
                "rate_rps": float(_kv(line, "rate_rps")),
                "p50_us": int(_kv(line, "p50_us")),
                "p99_us": int(_kv(line, "p99_us")),
                "errors": int(_kv(line, "errors")),
            })
        elif line.startswith("load_summary "):
            for key in ("requests", "completed", "errors", "else_used", "elapsed_ms"):
                out[key] = int(_kv(line, key))
            out["achieved_rps"] = float(_kv(line, "achieved_rps"))
    return out


def steady_state(parsed: dict) -> dict:
    """Median window p50/p99 over all but the first (warmup) window."""
    windows = parsed["windows"]
    steady = windows[1:] if len(windows) > 1 else windows
    if not steady:
        return {"p50_us": None, "p99_us": None, "windows": 0}
    return {
        "p50_us": int(median(w["p50_us"] for w in steady)),
        "p99_us": max(w["p99_us"] for w in steady),
        "windows": len(steady),
    }


def parse_tileinfo(text: str) -> dict:
    """Active tile count, memtable entries, per-region page sizes from tileinfo."""
    active = None
    memtable = None
    page_sizes: set[int] = set()
    regions: dict[str, int] = {}
    tile_bytes = 0
    tiles = 0
    for line in text.splitlines():
        line = line.strip()
        if "tile_bytes_total=" in line:
            tiles = int(_kv(line, "tiles"))
            tile_bytes = int(_kv(line, "tile_bytes_total"))
        elif line.startswith("phase=tileinfo tile="):
            page_sizes.add(int(_kv(line, "page_size")))
            region = _kv(line, "region")
            regions[region] = regions.get(region, 0) + 1
        elif "active_tiles=" in line and "serving_memtable_entries=" in line:
            active = int(_kv(line, "active_tiles"))
            memtable = int(_kv(line, "serving_memtable_entries"))
    return {
        "active_tiles": active,
        "serving_memtable_entries": memtable,
        "page_sizes": sorted(page_sizes),
        "regions": regions,
        "tiles": tiles,
        "tile_bytes_total": tile_bytes,
    }


# ---------------------------------------------------------------------------
# Subprocess plumbing
# ---------------------------------------------------------------------------
def write_trace(path: Path, lines) -> int:
    with open(path, "w", encoding="utf-8", newline="\n") as out:
        count = 0
        for line in lines:
            out.write(line + "\n")
            count += 1
    return count


def run(cmd: list[str], env: dict, capture: bool = False, check: bool = True):
    print("+ " + " ".join(str(c) for c in cmd), flush=True)
    kwargs = {"cwd": REPO_ROOT, "env": env, "text": True}
    if capture:
        kwargs["capture_output"] = True
    else:
        kwargs["stdout"] = subprocess.DEVNULL
        kwargs["stderr"] = subprocess.DEVNULL
    proc = subprocess.run(cmd, **kwargs)
    if check and proc.returncode != 0:
        raise SystemExit(f"command failed ({proc.returncode}): {' '.join(map(str, cmd))}")
    return proc


def build_binaries(release: bool) -> dict:
    profile = ["--release"] if release else []
    run(["cargo", "build", "--offline", *profile, "--bin", "flashdb-scale",
         "--bin", "flashdb-load", "--bin", "flashdb-replay"], os.environ.copy())
    bin_dir = REPO_ROOT / "target" / ("release" if release else "debug")
    return {
        "scale": str(bin_dir / "flashdb-scale"),
        "load": str(bin_dir / "flashdb-load"),
        "replay": str(bin_dir / "flashdb-replay"),
    }


def emit_traces(workdir: Path, args: argparse.Namespace) -> dict:
    seed_trace = workdir / "seed.jsonl"
    write_trace_path = workdir / "write.jsonl"
    read_trace_path = workdir / "read.jsonl"
    seed = SeedGenerator(seed=args.seed, n_customers=args.customers,
                         n_sku=args.skus, n_loc=args.locs, on_hand=args.on_hand,
                         block_docs=args.block_docs)
    seed_blocks = write_trace(seed_trace, render(seed.blocks()))

    if args.variant == "flow":
        # The real inventory flow is measured as offered: the MIX trace is replayed
        # once, writes included (order_flow commits during the run). No pre-write
        # phase, so its StockMovements stay memtable-resident by construction.
        write_requests = []
        read_requests = list(render(Generator(args.seed, args.reads, args.customers,
                                              args.skus, args.locs).requests()))
    else:
        # One generator drives both segments so order_no advances across the
        # write phase and the read phase's movements_scan picks real orders.
        profile = LoadProfile(
            seed=args.seed, n_customers=args.customers, n_sku=args.skus, n_loc=args.locs,
            segments=[
                Segment(offered_rate=1, count=args.writes, mix=(("order_flow", 1),)),
                Segment(offered_rate=args.rate, count=args.reads,
                        mix=(("movements_scan", 1),)),
            ],
        )
        data, manifest = generate(profile)
        lines = data.decode("utf-8").splitlines()
        split = manifest["segments"][1]["start_request"]
        write_requests, read_requests = lines[:split], lines[split:]

    out = {"seed": seed_trace, "seed_blocks": seed_blocks,
           "write": None, "read": read_trace_path,
           "write_count": len(write_requests), "read_count": len(read_requests)}
    if write_requests:
        write_trace(write_trace_path, write_requests)
        out["write"] = write_trace_path
    write_trace(read_trace_path, read_requests)
    return out


def measure_config(name: str, mode: str, region_layout: str | None, bins: dict,
                   traces: dict, args: argparse.Namespace, workroot: Path) -> dict:
    engine_dir = workroot / name / "engine"
    engine_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["FLASHDB_REGION_MODE"] = mode
    env["FLASHDB_SEED_CHECKPOINT_BYTES"] = str(args.checkpoint_bytes)
    if region_layout:
        env["FLASHDB_REGION_LAYOUT"] = region_layout

    seeded = run([bins["scale"], "seed-inventory", str(engine_dir), str(traces["seed"])],
                 env, capture=True)
    if traces["write"] is not None:
        # Read the seeded dimensions from tiles, write the movements, then publish
        # the movement regions so the scan-heavy tile exists before the measurement.
        run([bins["replay"], str(engine_dir), str(traces["write"])], env)
        run([bins["scale"], "checkpoint", str(engine_dir)], env, capture=True)
    tileinfo = run([bins["scale"], "tileinfo", str(engine_dir)], env, capture=True).stdout

    load_cmd = [bins["load"], str(engine_dir), str(traces["read"]),
                "--rate-rps", str(args.rate), "--window-ms", str(args.window_ms), "--patterns"]
    tiles = run(load_cmd, env, capture=True).stdout
    mem = run(load_cmd + ["--no-tiles"], env, capture=True).stdout

    return {
        "name": name,
        "mode": mode,
        "region_layout": region_layout,
        "seed_line": seeded.stdout.strip().splitlines()[-1],
        "tileinfo": parse_tileinfo(tileinfo),
        "tile_load": parse_load_output(tiles),
        "mem_load": parse_load_output(mem),
    }


def report(results: list[dict], traces: dict, args: argparse.Namespace) -> None:
    print()
    print("=== tile-touch evidence (reads cannot be served from RAM) ===")
    print(f"{'config':<18} {'active':>6} {'mt_entries':>10} {'tiles':>6} {'page_sizes':>14} "
          f"{'tile_b':>9} {'scan p50 tile/mem (us)':>26} {'scan p99 tile/mem (us)':>26}")
    for r in results:
        ti = r["tileinfo"]
        tile_s = steady_state(r["tile_load"])
        mem_s = steady_state(r["mem_load"])
        page = ",".join(str(p // 1024) + "K" for p in ti["page_sizes"])
        print(f"{r['name']:<18} {ti['active_tiles']:>6} {ti['serving_memtable_entries']:>10} "
              f"{ti['tiles']:>6} {page:>14} {ti['tile_bytes_total']:>9} "
              f"{tile_s['p50_us']!s:>13}/{mem_s['p50_us']!s:<12} "
              f"{tile_s['p99_us']!s:>13}/{mem_s['p99_us']!s:<12}")

    print()
    print("=== equal offered load: per-region layout axis ===")
    print(f"offered_rate_rps={args.rate} window_ms={args.window_ms} "
          f"write_requests={traces['write_count']} read_requests={traces['read_count']}")
    print(f"{'config':<18} {'achieved_rps':>12} {'win_p50_us':>10} {'win_p99_us':>10} "
          f"{'dev_fsync_p50':>13} {'else_used':>9} {'errors':>7}")
    for r in results:
        p = r["tile_load"]
        s = steady_state(p)
        print(f"{r['name']:<18} {p['achieved_rps']:>12.1f} {s['p50_us']!s:>10} {s['p99_us']!s:>10} "
              f"{p['device_fsync_p50_us']!s:>13} {p['else_used']:>9} {p['errors']:>7}")


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--release", action="store_true", help="build/run the release profile")
    ap.add_argument("--variant", choices=("scan", "flow"), default="scan",
                    help="scan: read-only movements_scan trace; flow: the full MIX trace")
    ap.add_argument("--writes", type=int, default=2000, help="write-phase order_flow requests")
    ap.add_argument("--reads", type=int, default=400, help="read-phase requests")
    ap.add_argument("--rate", type=int, default=25, help="fixed offered rate (requests/s)")
    ap.add_argument("--window-ms", type=int, default=1000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=5000)
    ap.add_argument("--skus", type=int, default=2000)
    ap.add_argument("--locs", type=int, default=8)
    ap.add_argument("--on-hand", type=int, default=1_000_000,
                    help="seeded Stock.on_hand; a low value makes the take guard's else branch fire")
    ap.add_argument("--block-docs", type=int, default=1000)
    ap.add_argument("--checkpoint-bytes", type=int, default=262144,
                    help="seed-inventory WAL-publish cadence (env FLASHDB_SEED_CHECKPOINT_BYTES)")
    ap.add_argument("--workdir", type=Path, default=None)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args(argv)

    workroot = (a.workdir or Path(tempfile.mkdtemp(prefix="region-layout-"))).resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    traces = emit_traces(workroot, a)
    print(f"workdir={workroot} seed_blocks={traces['seed_blocks']} "
          f"write_requests={traces['write_count']} read_requests={traces['read_count']}")

    bins = build_binaries(a.release)
    results = []
    for name, mode, region_layout, _desc in CONFIGS:
        results.append(measure_config(name, mode, region_layout, bins, traces, a, workroot))
    report(results, traces, a)

    if not a.keep and a.workdir is None:
        shutil.rmtree(workroot)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
