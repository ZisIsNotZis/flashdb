"""Build the slice-44 cold base instance in the slice-30 regime (one new harness file).

Why this exists
---------------
The 0b capacity runs reuse a cold base instance (``/tmp/cold33/per_region-mixed`` in slices
29-39). That directory no longer exists, so the slice-44 capacity curve needs an instance rebuilt
in the SAME regime: the inventory dimensions of ``cold_axis_compare`` default arguments, published
as per-region tiles with ``FLASHDB_REGION_LAYOUT=P|StockMovement=65536:10`` and an 8 MiB
checkpoint/rotate cadence so the retained WAL suffix a later open replays stays bounded.

This module adds no measurement logic. It calls the existing ``cold_axis_compare`` trace emitters
and ``build_inventory`` unchanged, so the rebuilt instance is produced by the same code that built
the published one rather than by a re-implementation that could drift from it.

Usage (from the repo root)::

    source $HOME/.cargo/env
    cd harness && uv run python build_cold_44.py --release --workdir /tmp/cold44

It prints the instance directory, the seed phase's document counts, the data bytes, the cap and
the data:cap ratio, and the post-open ``openinfo`` (active tiles, live RSS, VmHWM).
"""

# Built instance 2026-10-09 at /tmp/cold44/per_region-mixed: data_bytes=1,643,933,528 cap_bytes=201,326,592 ratio=8.17x; seed-inventory docs=10,400,000 customers=2,000,000 stocks=8,400,000 publishes=146 rotations=145 tiles=294; openinfo active_tiles=336 tile_bytes_total=1,638,589,633 rss_kb=65,868 vm_hwm_kb=80,832; region_mode=per_region.

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from flashdb_harness import cold_axis_compare as c  # noqa: E402


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="Build the slice-44 cold base instance.")
    ap.add_argument("--release", action="store_true")
    ap.add_argument("--workdir", type=Path, default=Path("/tmp/cold44"))
    ap.add_argument("--cap-mib", type=int, default=192,
                    help="cgroup MemoryMax for the build phases (the scored run sets its own)")
    ap.add_argument("--checkpoint-bytes", type=int, default=8 << 20)
    ap.add_argument("--rotate-bytes", type=int, default=8 << 20)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=2_000_000)
    # The published instance (slices 29-39) held ~10.4 M rows / ~1.65 GB at a 192 MiB cap
    # (8.18x). The cold_axis_compare default of 750k SKUs only reaches ~5 M rows / ~786 MB
    # (3.9x, a different, much warmer regime), so the slice-44 rebuild raises the SKU dimension
    # to reproduce the published data:cap regime. See the ticket for the exact command.
    ap.add_argument("--skus", type=int, default=2_100_000)
    ap.add_argument("--locs", type=int, default=4)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--block-docs", type=int, default=2_000)
    ap.add_argument("--write-requests", type=int, default=30_000)
    ap.add_argument("--write-rate", type=int, default=100_000)
    ap.add_argument("--group-requests", type=int, default=64)
    ap.add_argument("--key-encoding", choices=("family_first", "entity_first"),
                    default="family_first",
                    help="key spelling for the built instance (entity_first co-locates P/U/R)")
    a = ap.parse_args(argv)

    cap_bytes = a.cap_mib << 20
    workroot = a.workdir.resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    bins = c.build_binaries(a.release)
    env = os.environ.copy()

    seed_trace = workroot / "seed.jsonl"
    write_trace = workroot / "write.jsonl"
    n_seed = c.emit_seed_trace(seed_trace, a)
    n_write = c.emit_write_trace(write_trace, a)
    traces = {"seed": seed_trace, "write": write_trace}
    print(f"traces seed_blocks={n_seed} write_requests={n_write}", flush=True)

    name = "per_region-mixed"
    res = c.build_inventory(name, "per_region", c.MIXED_REGION_LAYOUT, bins, traces, a,
                            workroot, env, cap_bytes, key_encoding=a.key_encoding)
    docs = None
    for line in res.stdout.splitlines():
        if line.startswith("phase=seed-inventory ok=true"):
            docs = line
            print("  " + line, flush=True)
    if res.rc != 0:
        raise SystemExit(f"build failed rc={res.rc}: {res.stderr[-2000:]}")

    data = c.dir_bytes(workroot / name)
    print(f"instance={workroot / name}")
    print(f"data_bytes={data} cap_bytes={cap_bytes} ratio={data / cap_bytes:.2f}x")
    print(f"seed_phase={docs}")

    # Post-open diagnostic: what opening the checkpointed instance costs (slice 31 openinfo).
    openinfo = subprocess.run([bins["scale"], "openinfo", str(workroot / name)],
                              cwd=c.REPO_ROOT, env=env, capture_output=True, text=True)
    for line in openinfo.stdout.splitlines():
        if line.startswith("phase=openinfo"):
            print(line)
    if openinfo.returncode != 0:
        print(f"openinfo rc={openinfo.returncode}: {openinfo.stderr[-1000:]}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
