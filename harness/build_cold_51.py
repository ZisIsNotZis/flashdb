"""Build the slice-51 cold instance at >= 10x data:regime-cap (one new harness file, measure-only).

Why this exists
---------------
Every published capacity curve (slices 44/47) ran on a cold instance at ``8.17x`` data:cap
(192 MiB cap over ~1.64 GB), so each throughput verdict had to carry the caveat that the
pre-registration's own floor - ``Data >= 10x the memory budget``
(``.scratch/15-objective-v0/pre-registration.md``) - was not met. This script rebuilds the SAME
per-region inventory regime (slice 29/31 build path; exactly the dimensions ``build_cold_44.py``
used) and reports the ratio against a **128 MiB regime cap**: ``1,643,933,528 / 134,217,728 =
12.25x``. The dataset is unchanged; only the memory budget the floor is judged at is lowered, per
the lane's rule to prefer a smaller cap over a smaller dataset.

Two caps, deliberately
----------------------
- ``--build-cap-mib`` (default 192) is the transient cgroup MemoryMax for the construction phases
  (seed + grouped write load + final checkpoint). Construction is heavier than serving: the seed
  scope peaks at ~117 MiB and opening the just-seeded instance (a ~8 MiB retained WAL suffix
  replayed into the memtable) peaks at ~140 MiB, and the write phase at ~163 MiB - all measured
  with ``/usr/bin/time -v`` on this box. So a 128 MiB build scope OOM-kills the write phase
  (observed: ``rc=-15`` right after ``phase=seed-inventory ok=true``). 192 MiB is the cap the
  slice-44 instance was built at.
- ``--regime-cap-mib`` (default 128) is the memory budget the ``>= 10x`` floor is judged at and the
  cap the scored run should use (``cold_0b --cap-mib 128``). The scored load path is much lighter
  than construction: the slice-44/47 runs recorded ``load_hwm_bytes`` of 74.1/73.7 MB, i.e. ~74 MiB
  live, so a 128 MiB cap holds with headroom and still evicts (page cache fills the cap, so
  ``cache_evicting`` stays true).

No measurement logic is added. It calls the existing ``cold_axis_compare`` trace emitters and
``build_inventory`` unchanged (as ``build_cold_44.py`` does), so the instance is produced by the
same code that built the published one rather than by a re-implementation that could drift.

Cadence (the slice-46 constraint): a ``RegionMode::PerRegion`` publish emits one tile per region,
against a fixed 1024-tile manifest cap, so the checkpoint/rotate cadence must be large enough that
the cap is not exhausted mid-run. This script keeps the **8 MiB** cadence that works (the runner's
shipped 16 KiB default aborts, see ``.scratch/46-partitioned-publish-tile-cap``).

Usage (from the repo root)::

    source $HOME/.cargo/env
    cd harness && uv run python build_cold_51.py --release --workdir /tmp/cold51

It prints the instance directory, the seed phase's document counts, the data bytes, both caps, the
data:regime-cap ratio against the 10x floor, the build scope's own peak RSS, and the post-open
``openinfo`` (active tiles, tile bytes, live RSS, VmHWM).
"""

# The instance this script produces is recorded in .scratch/51-instance-10x/issues.md. Seeding is
# deterministic, so a rerun with the same dimensions reproduces byte-identical traces and data.

from __future__ import annotations

import argparse
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from flashdb_harness import cold_axis_compare as c  # noqa: E402

PREREG_RATIO_FLOOR = 10.0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="Build the slice-51 cold instance (>= 10x data:cap).")
    ap.add_argument("--release", action="store_true")
    ap.add_argument("--workdir", type=Path, default=Path("/tmp/cold51"))
    ap.add_argument("--build-cap-mib", type=int, default=192,
                    help="cgroup MemoryMax for the construction phases only (measured ~163 MiB "
                         "peak; a 128 MiB build scope OOM-kills the write phase)")
    ap.add_argument("--regime-cap-mib", type=int, default=128,
                    help="memory budget the >= 10x floor is judged at and the cap the scored run "
                         "should use; the scored load path peaks at ~74 MiB")
    ap.add_argument("--checkpoint-bytes", type=int, default=8 << 20,
                    help="8 MiB cadence keeps the per-region publish under the 1024-tile cap "
                         "(slice 46: the 16 KiB shipped default exhausts it and aborts)")
    ap.add_argument("--rotate-bytes", type=int, default=8 << 20)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=2_000_000)
    # Same dimensions as build_cold_44.py: ~10.4 M rows / ~1.64 GB of tiles. At the 128 MiB regime
    # cap this is 12.25x, i.e. the slice-44 dataset with a smaller memory budget rather than a
    # bigger dataset (the lane's stated preference).
    ap.add_argument("--skus", type=int, default=2_100_000)
    ap.add_argument("--locs", type=int, default=4)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--block-docs", type=int, default=2_000)
    ap.add_argument("--write-requests", type=int, default=30_000)
    ap.add_argument("--write-rate", type=int, default=100_000)
    ap.add_argument("--group-requests", type=int, default=64)
    a = ap.parse_args(argv)

    build_cap = a.build_cap_mib << 20
    regime_cap = a.regime_cap_mib << 20
    workroot = a.workdir.resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    bins = c.build_binaries(a.release)
    env = os.environ.copy()

    seed_trace = workroot / "seed.jsonl"
    write_trace = workroot / "write.jsonl"
    n_seed = c.emit_seed_trace(seed_trace, a)
    n_write = c.emit_write_trace(write_trace, a)
    print(f"traces seed_blocks={n_seed} write_requests={n_write}", flush=True)

    traces = {"seed": seed_trace, "write": write_trace}
    name = "per_region-mixed"
    res = c.build_inventory(name, "per_region", c.MIXED_REGION_LAYOUT, bins, traces,
                            a, workroot, env, build_cap)
    docs = None
    for line in res.stdout.splitlines():
        if line.startswith("phase=seed-inventory ok=true"):
            docs = line
            print("  " + line, flush=True)
    if res.rc != 0:
        raise SystemExit(f"build failed rc={res.rc}: {res.stderr[-2000:]}")

    data = c.dir_bytes(workroot / name)
    r = c.ratio(data, regime_cap)
    print(f"instance={workroot / name}")
    print(f"data_bytes={data} build_cap_bytes={build_cap} regime_cap_bytes={regime_cap} "
          f"ratio_vs_regime={r:.2f}x floor={PREREG_RATIO_FLOOR:.0f}x "
          f"meets_floor={r >= PREREG_RATIO_FLOOR}")
    print(f"seed_phase={docs}")
    print(f"build_scope peak_bytes={res.peak_bytes} rss_peak_bytes={res.rss_peak_bytes} rc={res.rc}")
    if r < PREREG_RATIO_FLOOR:
        print(f"WARNING: data:regime-cap {r:.2f}x is below the pre-registration floor",
              file=sys.stderr)

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
