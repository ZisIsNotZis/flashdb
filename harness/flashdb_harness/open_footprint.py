"""Locate the engine's open-time memory footprint with a controlled experiment (Slice 31).

Why
---
Opening a checkpointed, many-tile instance was reported to cost 151-255 MB of anonymous
memory (ticket 31), which set a floor under the >= 10x-cache cold comparison: under a
192 MiB ``MemoryMax`` every arm was OOM-killed even with an empty serving memtable, and
glibc tuning did not make it fit. The ticket could not say *what* drove the footprint -
it is not proportional to data bytes (5 GB opened at 151 MB while 760 MB opened at
255 MB) nor obviously to tile count. The ticket's leading hypothesis was glibc arena
retention of the open-time verification working set (slice 10a showed ``malloc_trim``
returned such memory), but the 5 GB number contradicted it, so it had to be tested.

What this measures
------------------
``flashdb-scale openinfo <dir>`` opens through ``Engine::open_discover`` and reports,
immediately after open:

* ``rss_kb``       live resident set (``VmRSS``) after open returned;
* ``vm_hwm_kb``    peak resident set (``VmHWM``) reached during open - the open-time
                   working set plus any allocator retention;
* ``vsize_kb``     virtual size (allocator arenas are reserved here, so the pair
                   ``VmSize``/``VmRSS`` separates reserved-but-free from resident);
* ``active_tiles``, ``tile_bytes_total``, ``serving_memtable_entries``.

Each variant below is built by ``flashdb-scale seed-inventory`` on one freshly generated
trace and differs from ``base`` in exactly one knob, so the axis that moves ``rss_kb`` /
``vm_hwm_kb`` is the driver:

* tile count at constant data - checkpoint cadence 16 MeB / 4 MiB / 1 MiB;
* retained WAL suffix at constant data - rotate cadence, plus rotation disabled entirely
  (which replays the whole WAL at open);
* bloom bits (10 vs 0);
* page size (4 KiB vs 64 KiB).

Finding
-------
The driver is glibc arena retention of the open-time verification working set, whose size
is set by the **retained WAL suffix** (the rotate threshold), not by tile count and not by
total data. A rotated instance opens at a ~5 MB live RSS floor regardless of tile count;
the peak scales with the suffix, and the pre-fix build kept that peak resident afterwards.
``open_impl`` now drops the suffix temporaries and calls ``malloc_trim(0)`` before
returning (allocator hygiene only), which is what makes the >= 10x cold run possible.

Usage (from the harness directory)::

    uv run --offline python -m flashdb_harness.open_footprint --release
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

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import render  # noqa: E402
from flashdb_harness.seed import SeedGenerator  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]

# One knob off ``base`` at a time. ``checkpoint``/``rotate``/``page``/``bloom`` are the
# env knobs ``flashdb-scale``/``LayoutPolicy`` already read; ``rotate`` of ``None`` means
# "unset", i.e. the WAL is retained in full and open replays all of it.
BASE = {"checkpoint": 4 << 20, "rotate": 4 << 20, "page": 4096, "bloom": 10}
VARIANTS = [
    # name,             axis,                              overrides
    ("base",            "base (4 MiB checkpoint+rotate)",  {}),
    ("manytiles",       "tile count: 1 MiB cadence",       {"checkpoint": 1 << 20, "rotate": 1 << 20}),
    ("fewtiles",        "tile count: 16 MiB cadence",      {"checkpoint": 16 << 20, "rotate": 16 << 20}),
    ("bigsuffix",       "suffix: 4 MiB ckpt / 16 MiB rot", {"rotate": 16 << 20}),
    ("nosuffix",        "suffix: no rotation (full WAL)",  {"rotate": None}),
    ("nobloom",         "bloom off",                       {"bloom": 0}),
    ("page64k",         "page size 64 KiB",                {"page": 65536}),
]


# ---------------------------------------------------------------------------
# Pure output parsing (tested without running an engine)
# ---------------------------------------------------------------------------
def _kv(line: str, key: str) -> str | None:
    m = re.search(rf"(?:^|\s){re.escape(key)}=(\S+)", line)
    return m.group(1) if m else None


def parse_openinfo(text: str) -> dict:
    """The ``phase=openinfo ok=true`` line into a dict of ints (``None`` when absent)."""
    out: dict = {k: None for k in
                 ("active_tiles", "serving_memtable_entries", "tile_bytes_total",
                  "rss_kb", "vm_hwm_kb", "vsize_kb")}
    for line in text.splitlines():
        if line.startswith("phase=openinfo ok=true"):
            for key in out:
                out[key] = int(_kv(line, key))
    return out


def parse_seed_peak_bytes(time_stderr: str) -> int | None:
    """Max ``Maximum resident set size (kbytes)`` across ``/usr/bin/time -v`` blocks."""
    vals = [int(v) for v in re.findall(r"Maximum resident set size \(kbytes\): (\d+)", time_stderr)]
    return max(vals) * 1024 if vals else None


def seed_docs(text: str) -> int:
    """``docs=`` from the ``seed-inventory`` accounting line (0 when absent)."""
    for line in text.splitlines():
        if line.startswith("phase=seed-inventory ok=true"):
            return int(_kv(line, "docs") or 0)
    return 0


def dir_bytes(path: Path) -> int:
    """Total bytes of the directory's files on disk (tiles plus retained WAL)."""
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def mb(value: int | None) -> str:
    return "-" if value is None else f"{value / (1 << 20):.1f}"


def format_table(rows: list[dict]) -> str:
    """The controlled-experiment table (one row per variant)."""
    lines = [
        f"{'variant':<12} {'axis':<30} {'tiles':>5} {'rows':>8} {'data_MB':>8} "
        f"{'seed_pk_MB':>10} {'open_HWM_MB':>11} {'open_RSS_MB':>11} {'vsize_MB':>9}",
    ]
    for r in rows:
        lines.append(
            f"{r['name']:<12} {r['axis']:<30} {r['active_tiles']!s:>5} {r['rows']:>8} "
            f"{mb(r['data_bytes']):>8} {mb(r['seed_peak_bytes']):>10} "
            f"{mb(r['vm_hwm_kb'] and r['vm_hwm_kb'] * 1024):>11} "
            f"{mb(r['rss_kb'] and r['rss_kb'] * 1024):>11} "
            f"{mb(r['vsize_kb'] and r['vsize_kb'] * 1024):>9}"
        )
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Running
# ---------------------------------------------------------------------------
def build_binaries(release: bool) -> dict:
    profile = ["--release"] if release else []
    proc = subprocess.run(["cargo", "build", "--offline", *profile,
                           "--bin", "flashdb-scale"],
                          cwd=REPO_ROOT, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(proc.stderr[-4000:])
    return str(REPO_ROOT / "target" / ("release" if release else "debug") / "flashdb-scale")


def emit_trace(path: Path, args: argparse.Namespace) -> int:
    seed = SeedGenerator(seed=args.seed, n_customers=args.customers, n_sku=args.skus,
                         n_loc=args.locs, on_hand=args.on_hand, block_docs=args.block_docs)
    with open(path, "w", encoding="utf-8", newline="\n") as out:
        count = 0
        for line in render(seed.blocks()):
            out.write(line + "\n")
            count += 1
    return count


def run_variant(name: str, axis: str, spec: dict, bin_path: str, trace: Path,
                workroot: Path) -> dict:
    env = os.environ.copy()
    env["FLASHDB_SEED_CHECKPOINT_BYTES"] = str(spec["checkpoint"])
    if spec["rotate"] is not None:
        env["FLASHDB_SEED_ROTATE_BYTES"] = str(spec["rotate"])
    env["FLASHDB_PAGE_SIZE"] = str(spec["page"])
    env["FLASHDB_BLOOM_BITS"] = str(spec["bloom"])
    d = workroot / name
    shutil.rmtree(d, ignore_errors=True)
    d.mkdir(parents=True)
    seed = subprocess.run(["/usr/bin/time", "-v", bin_path, "seed-inventory", str(d), str(trace)],
                          cwd=REPO_ROOT, env=env, capture_output=True, text=True)
    if seed.returncode != 0:
        raise SystemExit(f"seed-inventory {name} failed: {seed.stdout[-2000:]}{seed.stderr[-2000:]}")
    opened = subprocess.run([bin_path, "openinfo", str(d)],
                             cwd=REPO_ROOT, env=env, capture_output=True, text=True)
    if opened.returncode != 0:
        raise SystemExit(f"openinfo {name} failed: {opened.stdout[-2000:]}{opened.stderr[-2000:]}")
    row = {"name": name, "axis": axis, **parse_openinfo(opened.stdout)}
    row["rows"] = seed_docs(seed.stdout)
    row["data_bytes"] = dir_bytes(d)
    row["seed_peak_bytes"] = parse_seed_peak_bytes(seed.stderr)
    return row


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release", action="store_true")
    ap.add_argument("--customers", type=int, default=400_000)
    ap.add_argument("--skus", type=int, default=100_000)
    ap.add_argument("--locs", type=int, default=4)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--block-docs", type=int, default=2_000)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--workdir", type=Path, default=None)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args(argv)

    workroot = (a.workdir or Path(tempfile.mkdtemp(prefix="open-footprint-"))).resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    trace = workroot / "seed.jsonl"
    n = emit_trace(trace, a)
    print(f"workdir={workroot} seed_blocks={n}", flush=True)
    bin_path = build_binaries(a.release)

    rows = []
    for name, axis, overrides in VARIANTS:
        spec = {**BASE, **overrides}
        row = run_variant(name, axis, spec, bin_path, trace, workroot)
        rows.append(row)
        print(f"{name}: tiles={row['active_tiles']} data_MB={mb(row['data_bytes'])} "
              f"open_HWM_MB={mb(row['vm_hwm_kb'] and row['vm_hwm_kb'] * 1024)} "
              f"open_RSS_MB={mb(row['rss_kb'] and row['rss_kb'] * 1024)}", flush=True)

    text = format_table(rows)
    print()
    print(text)
    (workroot / "open_footprint.json").write_text(
        json.dumps({"base": BASE, "variants": rows}, indent=2, default=str), encoding="utf-8")

    if not a.keep and a.workdir is None:
        shutil.rmtree(workroot)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
