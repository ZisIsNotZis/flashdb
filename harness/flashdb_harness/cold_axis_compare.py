"""Re-run the per-region axis comparisons under a CACHE-EVICTING memory cap (Slice 30).

Why
---
Every earlier per-region comparison (page size, slice 25; compaction trigger, slice 27) ran
with a multi-hundred-MB dataset on a box with ~94 GB of page cache: the working set fit in
RAM, so a dependent read cost memory latency instead of the measured device constant
(4 KiB O_DIRECT random read p50 measured at ~100 us on this box). Those runs measured CPU,
not I/O, and cannot say whether a layout reduces dependent random reads. The pre-registration
(``.scratch/15-objective-v0/pre-registration.md``) requires the instance to be >= 10x the
memory budget precisely to force reads to the device.

This harness builds that instance and re-runs the two axes cold:

* instance: the inventory dimensions (``seed.py``) published as tiles through
  ``FLASHDB_SEED_CHECKPOINT_BYTES`` plus ``FLASHDB_SEED_ROTATE_BYTES`` (slice 30 added the
  rotation knob; without it the un-rotated WAL is replayed in full at open, which made a
  few-hundred-MB instance open with ~1.4 GB RSS and ~70 s of CPU), then a flow-driven
  ``order_flow`` write phase applied through the adapter (``--group-requests``) and
  checkpointed as tiles. Slice 32 gave the write phase the same checkpoint+rotate cadence
  (``--checkpoint-bytes``/``--rotate-bytes`` on ``flashdb-load``): without it the write load
  grows ONE WAL with no rotation, so a later open replays the whole multi-tens-of-MB suffix
  and peaks at ~5x it (measured 47 MB suffix -> 242 MB open peak on this box), which is what
  OOM-killed the load under a 192 MiB cap. With rotation the suffix is bounded by the cadence
  and the open peak is ~16-45 MB;
* every phase runs under ``systemd-run --user --scope -p MemoryMax=<cap> -p MemorySwapMax=0``,
  so the cgroup - which counts page cache - caps and evicts the working set. Both views are
  reported: the process ``rss_peak`` (``/usr/bin/time -v``) and the scope's own
  ``memory.peak``;
* axis (a): ``global-4k`` vs ``per_region-mixed`` (64 KiB for the scan-heavy region);
* axis (b): per-region compaction trigger ``eager`` vs ``defer`` on copies of the per-region
  base (slice 27's configurations);
* the read load is a probe-heavy trace (uniform Stock probes over the large region, uniform
  Customer probes, movements scans, order_flow writes whose qty forces the stock guard's
  else branch). It is *not* the 70/15/10/5 MIX: the cold property needs the read working set
  to exceed the cgroup's page-cache budget, and the Zipfian head of the standard mix fits in
  a small cache. The same trace is used for every arm, so the offered load is equal by
  construction; the mix is printed and recorded.

The data:cap ratio, per-region ``bytes_read``/latency, tile counts, write-side numbers, the
device fsync p50 and the commit rate are all printed so a reader can judge whether the run
actually reached the cold regime and whether an axis moved.

Usage (from the harness directory)::

    uv run python -m flashdb_harness.cold_axis_compare --release --cap-mib 192

With the bounded suffix the open peak is ~16-45 MB, so the smaller >= 10x pair is a 192 MiB
cap over the default ~2.6 GB instance (13.6x); raise ``--cap-mib`` and the dimension counts
together for the 256 MiB / >= 2.7 GB or 384 MiB / >= 4 GB pairs. The ``ratio`` line prints the
achieved value and warns when it is below the pre-registration 10x floor.

The long run is meant to be started with ``nohup`` and polled; it prints one machine-readable
line per phase. See ``harness/cold_axis_report.md`` for the captured run.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from statistics import median

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import Generator, Rng, loc_name, render, sku_name  # noqa: E402
from flashdb_harness.seed import SeedGenerator  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]

MIXED_REGION_LAYOUT = "P|StockMovement=65536:10"
# The regions that accumulate multiple tiles on the dimension-heavy instance; deferring them
# shows the compaction-trigger gradient (slice 27). Probe-hot regions are patched repeatedly.
DEFER_REGIONS = ("P|Stock", "U|Stock", "P|Customer", "U|Customer")
# Measured on this box (4 KiB O_DIRECT random read, 60 samples): p50 = 99.9 us. A dependent
# read served from the device therefore costs ~100 us; anything well below that is cache/CPU.
DEVICE_4K_READ_P50_US = 99.9


# ---------------------------------------------------------------------------
# Pure output parsing (tested without running an engine)
# ---------------------------------------------------------------------------
def _kv(line: str, key: str) -> str | None:
    m = re.search(rf"(?:^|\s){re.escape(key)}=(\S+)", line)
    return m.group(1) if m else None


def parse_rss_peak_bytes(time_stderr: str) -> int | None:
    """Max ``Maximum resident set size (kbytes)`` across ``/usr/bin/time -v`` blocks."""
    vals = [int(v) for v in re.findall(r"Maximum resident set size \(kbytes\): (\d+)", time_stderr)]
    return max(vals) * 1024 if vals else None


def parse_load_output(text: str) -> dict:
    """Parse the pacer's stdout into a summary dict (schema mirrors the driver)."""
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
        "regions": {},
        "commit_rate": None,
        "invalid_windows": None,
        "late_total": None,
        "open_peak_kb": None,
        "load_peak_kb": None,
    }
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("device_fsync_p50_us="):
            out["device_fsync_p50_us"] = int(_kv(line, "device_fsync_p50_us"))
            out["device_fsync_p99_us"] = int(_kv(line, "device_fsync_p99_us"))
        elif line.startswith("load_peak "):
            stage = _kv(line, "stage")
            if stage in ("open", "end"):
                out["open_peak_kb" if stage == "open" else "load_peak_kb"] = int(_kv(line, "hwm_kb"))
        elif line.startswith("engine_open "):
            out["discover"] = _kv(line, "discover")
            out["active_tiles"] = int(_kv(line, "active_tiles"))
            out["serving_memtable_entries"] = int(_kv(line, "serving_memtable_entries"))
        elif line.startswith("window="):
            out["windows"].append({
                "rate_rps": float(_kv(line, "rate_rps")),
                "p50_us": int(_kv(line, "p50_us")),
                "p99_us": int(_kv(line, "p99_us")),
                "errors": int(_kv(line, "errors_window")),
            })
        elif line.startswith("{"):
            _merge_metrics_json(out, line)
        elif line.startswith("load_summary "):
            for key in ("requests", "completed", "errors", "else_used",
                        "elapsed_ms", "invalid_windows", "late_total"):
                out[key] = int(_kv(line, key))
            out["achieved_rps"] = float(_kv(line, "achieved_rps"))
    if out["requests"]:
        out["commit_rate"] = out["completed"] / out["requests"]
    return out


def _merge_metrics_json(out: dict, line: str) -> None:
    """Fold one ``--metrics-json`` window line into the summary (last window wins)."""
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return
    out["windows"].append({
        "rate_rps": obj.get("rate_rps"),
        "p50_us": obj.get("p50_us"),
        "p99_us": obj.get("p99_us"),
        "errors": obj.get("errors"),
    })
    for r in obj.get("current", {}).get("regions", []):
        out["regions"][r["region"]] = {
            "count": r["count"],
            "probes": r["probes"],
            "p50_us": r["p50_ns"] / 1000.0,
            "p99_us": r["p99_ns"] / 1000.0,
            "bytes_read": r["bytes_read"],
        }


def steady_state(parsed: dict) -> dict:
    """Median window p50 and max window p99 over all but the first (warmup) window."""
    windows = [w for w in parsed["windows"] if w.get("p50_us") is not None]
    steady = windows[1:] if len(windows) > 1 else windows
    if not steady:
        return {"p50_us": None, "p99_us": None, "windows": 0}
    return {
        "p50_us": int(median(w["p50_us"] for w in steady)),
        "p99_us": max(w["p99_us"] for w in steady),
        "windows": len(steady),
    }


def parse_tileinfo(text: str) -> dict:
    """Active tile count, memtable entries, page sizes and per-region tile counts."""
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


def parse_compact(text: str) -> dict:
    """The ``phase=compact ok=true`` accounting line, or an empty dict."""
    for line in text.splitlines():
        if line.startswith("phase=compact ok=true"):
            return {
                "merges": int(_kv(line, "merges")),
                "compaction_bytes": int(_kv(line, "compaction_bytes")),
                "rewrite_factor": float(_kv(line, "rewrite_factor")),
                "active_tiles_before": int(_kv(line, "active_tiles_before")),
                "active_tiles_after": int(_kv(line, "active_tiles_after")),
            }
    return {}


def parse_scope_peak(text: str) -> int | None:
    """The scope's own ``memory.peak`` (bytes), captured inside the scope."""
    m = re.search(r"^cgroup_peak_bytes=(\d+)", text, re.MULTILINE)
    return int(m.group(1)) if m else None


def ratio(data_bytes: int, cap_bytes: int) -> float:
    """Achieved data:cap ratio. The pre-registration target is >= 10."""
    return data_bytes / cap_bytes if cap_bytes else 0.0


def dir_bytes(path: Path) -> int:
    """Total bytes of the directory's files (data on disk; superseded tile files included)."""
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


# ---------------------------------------------------------------------------
# Traces
# ---------------------------------------------------------------------------
def write_trace(path: Path, lines) -> int:
    with open(path, "w", encoding="utf-8", newline="\n") as out:
        count = 0
        for line in lines:
            out.write(line + "\n")
            count += 1
    return count


def emit_seed_trace(path: Path, args: argparse.Namespace) -> int:
    seed = SeedGenerator(seed=args.seed, n_customers=args.customers, n_sku=args.skus,
                         n_loc=args.locs, on_hand=args.on_hand, block_docs=args.block_docs)
    return write_trace(path, render(seed.blocks()))


def emit_write_trace(path: Path, args: argparse.Namespace) -> int:
    """The flow's own order_flow writes (all requests are order_flow)."""
    gen = Generator(args.seed, args.write_requests, args.customers, args.skus, args.locs)
    return write_trace(path, (next(render([gen.order_flow()])) for _ in range(args.write_requests)))


def emit_read_trace(path: Path, args: argparse.Namespace) -> dict:
    """Probe-heavy read trace: uniform Stock probes (large cold working set), uniform
    Customer probes, movements scans, and order_flow writes whose qty forces the guard's
    else branch (so the else count is a real signal, not a silent zero)."""
    rng = Rng(args.seed)
    total = args.read_requests
    n_stock = int(total * args.stock_share)
    n_customer = int(total * args.customer_share)
    n_scan = int(total * args.scan_share)
    n_write = total - n_stock - n_customer - n_scan
    gen = Generator(args.seed, total, args.customers, args.skus, args.locs)
    lines = []
    for i in range(n_stock):
        sku = sku_name(1 + rng.below(args.skus))
        loc = loc_name(rng.below(args.locs))
        lines.append(json.dumps({
            "id": f"cold-s-{i:07d}",
            "class": {"durability": "batched", "max_staleness": 2},
            "blocks": {"q": {"get": {"Stock": {"where": {"sku": sku, "loc": loc}}}}},
        }, sort_keys=True, separators=(",", ":")))
    for i in range(n_customer):
        email = gen.emails[rng.below(args.customers)]
        lines.append(json.dumps({
            "id": f"cold-c-{i:07d}",
            "class": {"durability": "batched"},
            "blocks": {"q": {"find": {"Customer": {"email": email}}}},
        }, sort_keys=True, separators=(",", ":")))
    for i in range(n_scan):
        order_no = f"O{1 + rng.below(max(1, args.write_requests)):08d}"
        lines.append(json.dumps({
            "id": f"cold-m-{i:07d}",
            "class": {"durability": "batched", "max_staleness": 5},
            "blocks": {"q": {"find": {"StockMovement": {"where": {"order_no": order_no}},
                                        "order": ["movement_no"], "limit": 100}}},
        }, sort_keys=True, separators=(",", ":")))
    counts = {"stock_lookup": n_stock, "customer_lookup": n_customer,
              "movements_scan": n_scan, "order_flow": 0}
    # The write phase already committed order_no O1..O<write_requests>; start the read trace's
    # order_flows above that range so their backorders are new work (a fresh order), not an
    # "order number already used" conflict that never reaches the guard's else branch. The
    # movement scans above deliberately address the build's commitment range O1..O<write_requests>.
    gen.order_no = args.write_requests
    for i in range(n_write):
        req = gen.order_flow()
        # Force the stock guard's `on_hand >= qty` predicate false without touching stock:
        # a qty above the seeded on_hand makes the else/backorder branch fire deterministically.
        req["params"]["qty"] = args.on_hand + 1
        lines.append(next(render([req])))
        counts["order_flow"] += 1
    write_trace(path, lines)
    return {"total": len(lines), "counts": counts}


# ---------------------------------------------------------------------------
# Subprocess plumbing: every phase runs inside a MemoryMax scope
# ---------------------------------------------------------------------------
def scope(cap_bytes: int, shell: str, env: dict, name: str) -> "ScopeResult":
    """Run ``shell`` under ``systemd-run --user --scope`` with a hard memory cap.

    ``MemoryMax`` counts page cache; the scope's own ``memory.peak`` is read from inside the
    scope after the payload exits, so the process view and the cgroup view stay separate.
    """
    inner = (
        "set -u\n"
        f"{shell}\n"
        "rc=$?\n"
        'cg=$(sed -n "s/^0:://p" /proc/self/cgroup 2>/dev/null)\n'
        'if [ -n "$cg" ] && [ -r "/sys/fs/cgroup$cg/memory.peak" ]; then\n'
        '  echo "cgroup_peak_bytes=$(cat "/sys/fs/cgroup$cg/memory.peak")"\n'
        "fi\n"
        "exit $rc\n"
    )
    cmd = ["systemd-run", "--user", "--scope", "-q",
           "-p", f"MemoryMax={cap_bytes}", "-p", "MemorySwapMax=0", "bash", "-c", inner]
    print(f"+ scope mem={cap_bytes} {name}", flush=True)
    proc = subprocess.run(cmd, cwd=REPO_ROOT, env=env, capture_output=True, text=True)
    return ScopeResult(name, proc.stdout, proc.stderr, proc.returncode)


class ScopeResult:
    def __init__(self, name: str, stdout: str, stderr: str, rc: int) -> None:
        self.name = name
        self.stdout = stdout
        self.stderr = stderr
        self.rc = rc
        self.timed_lines: list[str] = []

    @property
    def peak_bytes(self) -> int | None:
        return parse_scope_peak(self.stdout + self.stderr)

    @property
    def rss_peak_bytes(self) -> int | None:
        return parse_rss_peak_bytes(self.stderr)


def binary_dir(release: bool) -> Path:
    return REPO_ROOT / "target" / ("release" if release else "debug")


def build_binaries(release: bool) -> dict:
    profile = ["--release"] if release else []
    proc = subprocess.run(["cargo", "build", "--offline", *profile,
                           "--bin", "flashdb-scale", "--bin", "flashdb-load",
                           "--bin", "flashdb-replay"],
                          cwd=REPO_ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(proc.stderr[-4000:])
    d = binary_dir(release)
    return {"scale": str(d / "flashdb-scale"), "load": str(d / "flashdb-load"),
            "replay": str(d / "flashdb-replay")}


# ---------------------------------------------------------------------------
# Phases
# ---------------------------------------------------------------------------
def build_inventory(name: str, mode: str, region_layout: str | None, bins: dict,
                    traces: dict, args: argparse.Namespace, workroot: Path,
                    env: dict, cap: int) -> "ScopeResult":
    """Seed dimensions, apply the flow writes (grouped), checkpoint: inside one capped scope."""
    d = workroot / name
    seed_env = {
        "FLASHDB_REGION_MODE": mode,
        "FLASHDB_SEED_CHECKPOINT_BYTES": str(args.checkpoint_bytes),
        "FLASHDB_SEED_ROTATE_BYTES": str(args.rotate_bytes),
    }
    if region_layout:
        seed_env["FLASHDB_REGION_LAYOUT"] = region_layout
    env2 = {**env, **seed_env}
    script = " && ".join([
        f"/usr/bin/time -v {bins['scale']} seed-inventory {d} {traces['seed']}",
        (f"/usr/bin/time -v {bins['load']} {d} {traces['write']} --rate-rps {args.write_rate} "
         f"--window-ms 1000 --group-requests {args.group_requests} "
         f"--checkpoint-bytes {args.checkpoint_bytes} --rotate-bytes {args.rotate_bytes} "
         f"--region-mode {mode}"),
        f"/usr/bin/time -v {bins['scale']} checkpoint {d}",
    ])
    return scope(cap, script, env2, f"build-{name}")


def copy_and_compact(src: Path, dst: Path, policy: dict, bins: dict, env: dict,
                     cap: int, name: str) -> tuple["ScopeResult", dict]:
    policy_path = dst.parent / f"{name}.policy.json"
    policy_path.write_text(json.dumps(policy), encoding="utf-8")
    script = " && ".join([
        f"cp -a {src} {dst}",
        f"/usr/bin/time -v {bins['scale']} compact {dst}",
    ])
    res = scope(cap, script, {**env, "FLASHDB_POLICY_FILE": str(policy_path),
                              "FLASHDB_REGION_MODE": "per_region"}, f"compact-{name}")
    return res, parse_compact(res.stdout)


def load_dir(d: Path, trace: Path, bins: dict, env: dict, args: argparse.Namespace,
             cap: int, name: str) -> "ScopeResult":
    cmd = (f"/usr/bin/time -v {bins['load']} {d} {trace} --rate-rps {args.rate} "
           f"--window-ms {args.window_ms} --metrics-json --peak-report")
    return scope(cap, cmd, env, f"load-{name}")


def tileinfo(bins: dict, d: Path, env: dict) -> dict:
    proc = subprocess.run([bins["scale"], "tileinfo", str(d)], cwd=REPO_ROOT,
                          env=env, capture_output=True, text=True)
    return parse_tileinfo(proc.stdout)


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------
def format_report(results: dict, traces: dict, args: argparse.Namespace, cap_bytes: int,
                  data_bytes: int) -> str:
    warm = {"p_stock_p50_us": 32.7, "p_customer_p50_us": 34.4,
            "device_fsync_p50_us": 6353, "source": "slices 25/27 (warm box)"}
    lines: list[str] = []
    w = lines.append
    w("=== instance ===")
    for name in ("global-4k", "per_region-mixed", "eager", "defer"):
        ti = results[name]["tileinfo"]
        w(f"{name:<16} tile_bytes_total={ti['tile_bytes_total']:>12} "
          f"active_tiles={ti['active_tiles']:>4} page_sizes={ti['page_sizes']} "
          f"regions={sorted(ti['regions'])}")
    w(f"data_bytes={data_bytes} cap_bytes={cap_bytes} ratio={ratio(data_bytes, cap_bytes):.2f}x "
      f"(pre-registration target 10x)")
    if ratio(data_bytes, cap_bytes) < 10.0:
        w("WARNING: data:cap is below the pre-registration 10x floor; raise --customers/--skus or lower --cap-mib")
    # `traces` holds PATHS for the trace files; the counts live in the separate `trace_stats`
    # dict. An earlier version indexed the path dict and crashed at report time, after the whole
    # run had succeeded - which is the worst moment to lose the numbers.
    stats = traces.get("stats", {}) if isinstance(traces, dict) else {}
    read_stats = stats.get("read", {}) if isinstance(stats, dict) else {}
    w(f"read_trace requests={read_stats.get('total', 'unknown')} "
      f"mix={read_stats.get('counts', 'unknown')} offered_rate_rps={args.rate}")
    w(f"device 4 KiB O_DIRECT random read p50 = {DEVICE_4K_READ_P50_US} us (cold read cost)")
    w(f"warm reference ({warm['source']}): P|Stock p50~{warm['p_stock_p50_us']}us "
      f"P|Customer p50~{warm['p_customer_p50_us']}us device_fsync_p50~"
      f"{warm['device_fsync_p50_us']}us")

    w("")
    w("=== (a) global 4 KiB vs per-region mixed pages, COLD at equal offered load ===")
    w(f"{'config':<16} {'achieved':>9} {'win_p50':>8} {'win_p99':>8} {'dev_fs_p50':>10} "
      f"{'else':>6} {'commit':>7} {'act_tiles':>9} {'tile_b':>10}")
    for name in ("global-4k", "per_region-mixed"):
        r = results[name]
        p = r["load"]
        s = steady_state(p)
        w(f"{name:<16} {p['achieved_rps']:>9.1f} {s['p50_us']!s:>8} {s['p99_us']!s:>8} "
          f"{p['device_fsync_p50_us']!s:>10} {p['else_used']:>6} {p['commit_rate']:>7.3f} "
          f"{r['tileinfo']['active_tiles']:>9} {r['tileinfo']['tile_bytes_total']:>10}")

    w("")
    w("=== (b) per-region compaction trigger, COLD at equal offered load ===")
    w(f"{'config':<16} {'merges':>6} {'compact_bytes':>13} {'rewrite':>8} {'achieved':>9} "
      f"{'win_p50':>8} {'win_p99':>8} {'dev_fs_p50':>10} {'act_tiles':>9}")
    for name in ("eager", "defer"):
        r = results[name]
        c = r["compact"]
        p = r["load"]
        s = steady_state(p)
        w(f"{name:<16} {c.get('merges', 0):>6} {c.get('compaction_bytes', 0):>13} "
          f"{c.get('rewrite_factor', 0):>8.3f} {p['achieved_rps']:>9.1f} {s['p50_us']!s:>8} "
          f"{s['p99_us']!s:>8} {p['device_fsync_p50_us']!s:>10} "
          f"{r['tileinfo']['active_tiles']:>9}")

    w("")
    w("=== per-region telemetry (last window; p50/p99 us; bytes_read = deterministic) ===")
    for name in ("global-4k", "per_region-mixed", "eager", "defer"):
        w(f"-- {name} --")
        for region, m in sorted(results[name]["load"]["regions"].items()):
            w(f"  {region:<20} count={m['count']:>6} probes={m['probes']:>6} "
              f"p50_us={m['p50_us']:>9.1f} p99_us={m['p99_us']:>9.1f} "
              f"bytes_read={m['bytes_read']:>12}")

    w("")
    w("=== scope peaks (rss_peak from the process, memory.peak from the cgroup) ===")
    w(f"{'config':<16} {'open_hwm_kb':>11} {'load_hwm_kb':>11} {'rss_peak_b':>12} "
      f"{'cgroup_peak_b':>13} load_rc")
    for name in ("global-4k", "per_region-mixed", "eager", "defer"):
        r = results[name]
        p = r.get("load", {})
        w(f"{name:<16} {p.get('open_peak_kb')!s:>11} {p.get('load_peak_kb')!s:>11} "
          f"{r.get('rss_peak_bytes')!s:>12} {r.get('cgroup_peak_bytes')!s:>13} {r['load_rc']}")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release", action="store_true")
    ap.add_argument("--cap-mib", type=int, default=192,
                    help="cgroup MemoryMax; page cache is counted, so it must cover process RSS")
    ap.add_argument("--checkpoint-bytes", type=int, default=8 << 20,
                    help="publish a tile once the retained WAL exceeds this during seed AND the write load")
    ap.add_argument("--rotate-bytes", type=int, default=8 << 20,
                    help="retire the published WAL once it exceeds this, bounding the suffix a later open replays")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=2_000_000)
    ap.add_argument("--skus", type=int, default=750_000)
    ap.add_argument("--locs", type=int, default=4)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--block-docs", type=int, default=2_000)
    ap.add_argument("--write-requests", type=int, default=30_000)
    ap.add_argument("--write-rate", type=int, default=100_000)
    ap.add_argument("--group-requests", type=int, default=64)
    ap.add_argument("--read-requests", type=int, default=30_000)
    ap.add_argument("--stock-share", type=float, default=0.5)
    ap.add_argument("--customer-share", type=float, default=0.2)
    ap.add_argument("--scan-share", type=float, default=0.2)
    ap.add_argument("--rate", type=int, default=300)
    ap.add_argument("--window-ms", type=int, default=1_000)
    ap.add_argument("--workdir", type=Path, default=None)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args(argv)

    cap_bytes = a.cap_mib << 20
    workroot = (a.workdir or Path(tempfile.mkdtemp(prefix="cold-axis-"))).resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    traces = {
        "seed": workroot / "seed.jsonl",
        "write": workroot / "write.jsonl",
        "read": workroot / "read.jsonl",
        # the counts are kept alongside the paths: the report needs them, and an earlier version
        # indexed the path at report time and crashed AFTER the run had succeeded
        "stats": {},
    }
    print(f"workdir={workroot} cap_mib={a.cap_mib}", flush=True)
    n_seed = emit_seed_trace(traces["seed"], a)
    n_write = emit_write_trace(traces["write"], a)
    read_meta = emit_read_trace(traces["read"], a)
    traces["stats"]["read"] = read_meta
    print(f"traces seed_blocks={n_seed} write_requests={n_write} "
          f"read={read_meta['total']} mix={read_meta['counts']}", flush=True)

    bins = build_binaries(a.release)
    env = os.environ.copy()

    results: dict = {}
    # Two builds: global-4k for axis (a); per_region-mixed doubles as axis (b)'s base.
    for name, mode, layout in (("global-4k", "global", None),
                               ("per_region-mixed", "per_region", MIXED_REGION_LAYOUT)):
        res = build_inventory(name, mode, layout, bins, traces, a, workroot, env, cap_bytes)
        results[name] = {"build": res}
        print(f"build {name} rc={res.rc} peak={res.peak_bytes} rss_peak={res.rss_peak_bytes}",
              flush=True)
        for line in res.stdout.splitlines():
            if line.startswith("phase=") and "ok=true" in line:
                print(f"  {line}", flush=True)

    eager_policy = {"default": {"merge_min_tiles": 2}}
    defer_policy = {"default": {"merge_min_tiles": 2},
                    "regions": {r: {"merge_min_tiles": 200} for r in DEFER_REGIONS}}
    for name, policy in (("eager", eager_policy), ("defer", defer_policy)):
        res, compact = copy_and_compact(workroot / "per_region-mixed", workroot / name,
                                        policy, bins, env, cap_bytes, name)
        results[name] = {"compact": compact, "compact_scope": res}
        print(f"compact {name} merges={compact.get('merges')} "
              f"bytes={compact.get('compaction_bytes')} rc={res.rc}", flush=True)

    data_bytes = dir_bytes(workroot / "per_region-mixed")
    print(f"data_bytes={data_bytes} ratio={ratio(data_bytes, cap_bytes):.2f}x", flush=True)

    for name in ("global-4k", "per_region-mixed", "eager", "defer"):
        res = load_dir(workroot / name, traces["read"], bins, env, a, cap_bytes, name)
        p = parse_load_output(res.stdout)
        ti = tileinfo(bins, workroot / name, env)
        results[name].update({"load": p, "tileinfo": ti, "load_rc": res.rc,
                              "cgroup_peak_bytes": res.peak_bytes,
                              "rss_peak_bytes": res.rss_peak_bytes})
        print(f"load {name} rc={res.rc} achieved={p['achieved_rps']} "
              f"commit_rate={p['commit_rate']} errors={p['errors']} else={p['else_used']} "
              f"cgroup_peak={res.peak_bytes} rss_peak={res.rss_peak_bytes}", flush=True)

    text = format_report(results, traces, a, cap_bytes, data_bytes)
    print()
    print(text)
    (workroot / "results.json").write_text(json.dumps(
        {"cap_bytes": cap_bytes, "data_bytes": data_bytes,
         "ratio": ratio(data_bytes, cap_bytes), "read_mix": read_meta,
         "results": {k: {kk: vv for kk, vv in v.items()
                         if kk not in ("build", "compact_scope")}
                     for k, v in results.items()}},
        indent=2, default=str), encoding="utf-8")

    if not a.keep and a.workdir is None:
        shutil.rmtree(workroot)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
