"""Slice 49 - attribute the per-request latency tail of the slice-47 scored run.

MEASURE-ONLY. This script changes no engine or harness module: it imports
:mod:`flashdb_harness.cold_0b`, overrides only its binary resolver so every arm runs the ONE
frozen ``flashdb-load`` copied for this slice, and drives the SAME scored configuration (one arm,
one workload, one trace, the two 10 ms ceilings) under five flag variations plus one separation
experiment. It then prints the per-rung per-request service/lag percentiles and the deltas against
the baseline.

Run from the harness directory with the frozen binary already placed:

    cd harness && uv run python cold49_tail_attribution.py

Raw artifacts (per config: curve.json, report.md, arm stderr, the kept engine copy) land under
``--root`` (default ``/tmp/cold49``); nothing here writes into the repo except stdout.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path

import flashdb_harness.cold_0b as cold_0b

# The frozen copy of the driver built for this slice. Every arm must run this one file so a later
# rebuild cannot change what was measured; the script refuses to run otherwise.
FROZEN_LOAD = Path("/tmp/cold49/flashdb-load")


def _frozen_binaries(REPO_ROOT: Path):  # noqa: N803 - mirrors cold_0b.build_binaries signature
    """cold_0b.build_binaries replacement: never rebuild, always resolve to the frozen driver.

    ``scale``/``replay`` are unused by this single-arm adversarial path; they are returned only so
    the dict shape matches what ``cold_0b`` expects.
    """
    def resolve(release: bool) -> dict:
        profile = "release" if release else "debug"
        d = REPO_ROOT / "target" / profile
        return {"scale": str(d / "flashdb-scale"), "load": str(FROZEN_LOAD),
                "replay": str(d / "flashdb-replay")}
    return resolve


def require_frozen_load() -> None:
    """Fail closed unless the frozen driver exists: a rebuild must never change what is measured."""
    if not FROZEN_LOAD.is_file():
        raise SystemExit(
            f"frozen driver {FROZEN_LOAD} missing; build it once and copy it there first "
            f"(cargo build --release --offline && cp target/release/flashdb-load {FROZEN_LOAD})")


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def wal_state(engine_dir: Path) -> dict:
    """The arm copy's WAL name/size and active tile count, to show whether a checkpoint/rotation
    fired during the run (a rotation renames the segment, a checkpoint adds a tile-<csn>.tile)."""
    wal = sorted(engine_dir.glob("data-*.wal"))
    return {
        "wal_name": wal[0].name if wal else None,
        "wal_bytes": wal[0].stat().st_size if wal else None,
        "tiles": len(list(engine_dir.glob("tile-*.tile"))),
    }


# name -> extra cold_0b flags. The baseline is run-47's exact scored configuration.
CONFIGS: list[tuple[str, list[str]]] = [
    ("baseline", []),
    ("group1", ["--group-requests", "1"]),
    ("nocompact", ["--compact-bytes", "0"]),
    ("bigcadence", ["--checkpoint-bytes", "1073741824", "--rotate-bytes", "1073741824"]),
    ("bigcap", ["--cap-mib", "384"]),
    # Separation experiment (not one of the five requested configs): remove BOTH compaction
    # actuation and WAL maintenance/rotation, so each of the two suspected stall sources is
    # absent and only the read path / page-cache pressure can produce a tail.
    ("neither", ["--compact-bytes", "0",
                  "--checkpoint-bytes", "1073741824", "--rotate-bytes", "1073741824"]),
]

# Base args are run-47's exact scored configuration; a config's extra flags are appended and win
# (argparse keeps the last occurrence of a repeated flag).
BASE_ARGS = [
    "--release",
    "--reference-rate", "50",
    "--workloads", "adversarial",
    "--arms", "declared",
    "--segment-requests", "2000",
    "--window-ms", "1000",
    "--group-requests", "64",
    "--checkpoint-bytes", "8388608",
    "--rotate-bytes", "8388608",
    "--compact-bytes", "67108864",
    "--ceiling-us", "10000",
    "--lag-ceiling-us", "10000",
    "--ladder-ceiling-rps", "400",
    "--keep-engines",
]


def build_argv(instance: Path, workdir: Path, extra: list[str]) -> list[str]:
    return BASE_ARGS + extra + [
        "--instance", str(instance),
        "--workdir", str(workdir),
        "--report", str(workdir / "report.md"),
        "--json", str(workdir / "curve.json"),
    ]


def row_delta(base: dict, cur: dict) -> dict:
    out = {}
    for k in ("p95_us", "p99_us", "lag_p95_us", "lag_p99_us", "p50_us", "lag_p50_us",
              "achieved_rps"):
        a, b = base.get(k), cur.get(k)
        out[k] = None if a is None or b is None else b - a
    return out


def rung_table(name: str, rec: dict) -> str:
    lines = [f"#### {name}", "",
             "| offered rps | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | "
             "lag p50 | lag p95 | lag p99 | svc-rej win | lag-rej win | compact B |",
             "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for r in rec["curve"]:
        lines.append(
            f"| {r['offered_rps']:.0f} | {r['achieved_rps']:.2f} | "
            f"{r['achieved_fraction']*100:.2f}% | {r['p50_us']:.0f} | {r['p95_us']:,} | "
            f"{r['p99_us']:,} | {r['lag_p50_us']:.0f} | {r['lag_p95_us']:,} | "
            f"{r['lag_p99_us']:,} | {r['service_ceiling_rejected_windows']} | "
            f"{r['lag_ceiling_rejected_windows']} | {r['compact_bytes']:,} |")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", type=Path, default=Path("/tmp/cold44/per_region-mixed"))
    ap.add_argument("--root", type=Path, default=Path("/tmp/cold49"))
    ap.add_argument("--dry-run", action="store_true",
                    help="print the argv per config and exit (no copy, no run)")
    ap.add_argument("--only", type=str, default=None,
                    help="comma-separated config names to run (default: all)")
    a = ap.parse_args()

    configs = CONFIGS if a.only is None else [c for c in CONFIGS if c[0] in a.only.split(",")]
    if not configs:
        print(f"--only selected no known config from {a.only!r}", file=sys.stderr)
        return 2

    root: Path = a.root
    root.mkdir(parents=True, exist_ok=True)
    require_frozen_load()
    frozen_sha = sha256(FROZEN_LOAD)
    print(f"frozen_load={FROZEN_LOAD} sha256={frozen_sha}", flush=True)

    cold_0b.build_binaries = _frozen_binaries(cold_0b.REPO_ROOT)

    if a.dry_run:
        for name, extra in configs:
            print(" ".join(build_argv(root / f"{name}-instance", root / name, extra)))
        return 0

    records: dict[str, dict] = {}
    wal_before: dict[str, dict] = {}
    for name, extra in configs:
        instance = root / f"{name}-instance"
        workdir = root / name
        if workdir.exists():
            shutil.rmtree(workdir)
        subprocess.run(["cp", "-a", str(a.source), str(instance)], check=True)
        wal_before[name] = wal_state(instance)
        print(f"\n===== config={name} instance={instance} =====", flush=True)
        argv = build_argv(instance, workdir, extra)
        print("+ " + " ".join(argv), flush=True)
        rc = cold_0b.main(argv)
        if rc != 0:
            print(f"ERROR: config {name} exited {rc}", file=sys.stderr, flush=True)
            return rc
        records[name] = json.loads((workdir / "curve.json").read_text(encoding="utf-8"))
        records[name]["_wal_after"] = wal_state(workdir / "adversarial-declared")
        records[name]["_wal_before"] = wal_before[name]

    # ---- comparison report (stdout; captured to the ticket) ----
    out = []
    if "baseline" not in records:
        records["baseline"] = json.loads(
            (root / "baseline" / "curve.json").read_text(encoding="utf-8"))
    base = records["baseline"]["arms"]["adversarial/declared"]
    out.append("# cold49 tail attribution - per-rung tables\n")
    for name, _ in configs:
        rec = records[name]["arms"]["adversarial/declared"]
        out.append(f"\n## {name} (sha {frozen_sha[:12]})\n")
        out.append(rung_table(name, rec))
        s = rec["summary"]
        c = rec["coldness"]
        wb, wa = records[name]["_wal_before"], records[name]["_wal_after"]
        out.append(
            f"\nwhole-run: achieved_rps={s['achieved_rps']}, completed={s['completed']}, "
            f"errors={s['errors']}, commit_rate={s['commit_rate']}, "
            f"invalid_windows={s['invalid_windows']} "
            f"(svc {s['service_ceiling_rejected_windows']}/lag "
            f"{s['lag_ceiling_rejected_windows']}), late_total={s['late_total']}, "
            f"compact_bytes_total={s['compact_bytes_total']}, merges={s['compact_merges']}, "
            f"device_fsync p50/p99={s['device_fsync_p50_us']}/{s['device_fsync_p99_us']} us, "
            f"cgroup_peak={c['cgroup_peak_bytes']} cap={c['cap_bytes']} "
            f"cache_evicting={c['cache_evicting']}, data:cap={c['data_cap_ratio']:.2f}x")
        out.append(
            f"WAL before: {wb['wal_name']} {wb['wal_bytes']:,} B, tiles {wb['tiles']} | "
            f"WAL after: {wa['wal_name']} {wa['wal_bytes']:,} B, tiles {wa['tiles']}")

    out.append("\n## Deltas vs baseline (us; negative = tail moved in)\n")
    out.append("| config | rung | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | "
               "achieved rps |")
    out.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for name, _ in configs:
        if name == "baseline":
            continue
        rec = records[name]["arms"]["adversarial/declared"]
        for rb, rc_ in zip(base["curve"], rec["curve"]):
            d = row_delta(rb, rc_)
            out.append(
                f"| {name} | {rc_['offered_rps']:.0f} | {d['p50_us']:+,.0f} | "
                f"{d['p95_us']:+,.0f} | {d['p99_us']:+,.0f} | {d['lag_p50_us']:+,.0f} | "
                f"{d['lag_p95_us']:+,.0f} | {d['lag_p99_us']:+,.0f} | "
                f"{d['achieved_rps']:+.2f} |")

    report = "\n".join(out) + "\n"
    (root / "comparison.md").write_text(report, encoding="utf-8")
    print("\n" + report, flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
