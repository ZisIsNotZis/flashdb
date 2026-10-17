#!/usr/bin/env bash
# The sqlite checkpoint probe ticket 82's report (§9.2) named as an inference and never ran: the
# f70d91c arm-runs show a schedule-lag p99 of ~254 ms at EVERY crossing with a 3 ms IQR, which fits
# a WAL auto-checkpoint writing the 542 MB database back on the committing thread, but a probe is a
# measurement and an inference is not.
#
# This runs the SAME restored pre-load instance twice at one rung, one process at a time, in the
# arm's own 128 MiB scope: once with sqlite's compiled default `wal_autocheckpoint` (1000 pages) and
# once with the value the controlled run states (0: the checkpoint is deferred out of the measured
# phase and run afterwards). Nothing else differs -- same snapshot, same trace, same machine state,
# same pacer -- so the difference in the stall is attributable to the setting.
#
#   scripts/m1-checkpoint-probe.sh [--root DIR] [--rate R] [--requests N] [--seconds S]
#
# Artefacts: $ROOT/probe-checkpoint/autocheckpoint-<pages>/{run-sqlite.json,windows-sqlite.jsonl,
# machine-state.jsonl} plus `comparison.json` beside them.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HARNESS=$REPO/harness
ROOT=$REPO/.scratch/82-controlled-m1/runs
SOURCE=$REPO/.scratch/80-external-baselines/runs/m1-10x/rung-50
RATE=50
REQUESTS=1000
CAP_MIB=128
SQLITE_CACHE_KIB=8192
# The two policies compared: sqlite's compiled default, and the one the controlled run states.
PAGES_LIST="1000 0"
LOCK=/tmp/m1-measure.lock

die() { printf 'm1-checkpoint-probe: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --root) ROOT=$2; shift 2 ;;
        --rate) RATE=$2; shift 2 ;;
        --requests) REQUESTS=$2; shift 2 ;;
        --pages) PAGES_LIST=$2; shift 2 ;;
        *) die "unknown argument $1" ;;
    esac
done

mkdir -p "$ROOT"
ROOT=$(cd "$ROOT" && pwd)
SOURCE=$(cd "$SOURCE" && pwd)
SNAPSHOT_DIR=$ROOT/snapshot
INSTANCE_ROOT=$ROOT/live
PROBE_ROOT=$ROOT/probe-checkpoint
[ -f "$SNAPSHOT_DIR/sqlite.json" ] || die "no committed sqlite snapshot under $SNAPSHOT_DIR"

# One process at a time, the same lock the measured phase takes.
exec 9>"$LOCK"
flock -w 3600 9 || die "another M1 run holds $LOCK; nothing was probed"

harness() { ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b "$@" ); }

eval "$(python3 - "$SOURCE" <<'PYSPEC'
import json, pathlib, sys
src = pathlib.Path(sys.argv[1])
manifest = json.load(open(src / "manifest.json"))
spec = json.load(open(src / "spec.json"))
print(f"SEED={manifest['seed']} CUSTOMERS={manifest['n_customers']} SKUS={manifest['n_sku']}"
      f" LOCS={manifest['n_loc']} THETA={manifest['theta']} ON_HAND={spec['on_hand']}"
      f" BATCH={spec['batch']}")
PYSPEC
)"

for pages in $PAGES_LIST; do
    workdir=$PROBE_ROOT/autocheckpoint-$pages
    mkdir -p "$workdir"
    if [ ! -f "$workdir/manifest.json" ]; then
        harness prepare --workdir "$workdir" --seed "$SEED" --requests "$REQUESTS" --rate "$RATE" \
            --customers "$CUSTOMERS" --skus "$SKUS" --locs "$LOCS" --theta "$THETA" \
            --on-hand "$ON_HAND" --batch "$BATCH" --seed-trace "$SOURCE/seed.jsonl" >/dev/null
    fi
    # The machine state at both boundaries, and a stop if a build/seed/second harness is running.
    harness machine-state --out "$workdir/machine-state.jsonl" --phase probe-before \
        --arm sqlite --rung "$RATE" \
        || die "machine state found concurrent work; the probe was not run"
    harness restore --workdir "$workdir" --arms sqlite --snapshot-dir "$SNAPSHOT_DIR" \
        --instance-root "$INSTANCE_ROOT"
    systemd-run --user --scope -q -p "MemoryMax=${CAP_MIB}M" -p MemorySwapMax=0 \
        bash -c "cd '$HARNESS' && exec uv run python -m flashdb_harness.baseline_0b arm \
            --workdir '$workdir' --arm sqlite --engine-cap-mib $CAP_MIB \
            --sqlite-cache-kib $SQLITE_CACHE_KIB --sqlite-wal-autocheckpoint $pages \
            --restored --instance-root '$INSTANCE_ROOT' --skip-oracle" \
        || die "probe arm sqlite (wal_autocheckpoint=$pages) failed"
    harness machine-state --out "$workdir/machine-state.jsonl" --phase probe-after \
        --arm sqlite --rung "$RATE" || true
done

python3 - "$PROBE_ROOT" $PAGES_LIST <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
rows = {}
for pages in sys.argv[2:]:
    record = json.load(open(root / f"autocheckpoint-{pages}" / "run-sqlite.json"))
    curve = record["curve"][0]
    rows[pages] = {
        "wal_autocheckpoint_pages": record["durability_observed"]["wal_autocheckpoint_pages"],
        "pacing": record["pacing"],
        "achieved_rps": round(curve["achieved_rps"], 3),
        "lag_p50_us": curve["lag_p50_us"],
        "lag_p99_us": curve["lag_p99_us"],
        "service_p50_us": curve["p50_us"],
        "service_p99_us": curve["p99_us"],
        "group_elapsed_us": curve["group_elapsed_us"],
        "mean_group_size": round(curve["mean_group_size"], 3),
        "max_group_size": curve["max_group_size"],
        "rejected_windows": curve["invalid_windows"],
        "reject_reasons": curve["reject_reasons"],
        "sustained": curve["sustained"],
        "errors": record["counters"]["errors"],
        "wal": record["sqlite_wal"],
        "work_dir": str(pathlib.Path(root / f"autocheckpoint-{pages}").name),
    }
comparison = {
    "schema": "flashdb.m1.sqlite-checkpoint-probe.v1",
    "question": "does sqlite's WAL auto-checkpoint, not the rung, produce the ~254 ms stall the"
                " f70d91c controlled arm-runs show?",
    "design": "the same restored snapshot, trace, pacer, cap and machine state at one rung; only"
              " PRAGMA wal_autocheckpoint differs (1000 = sqlite's compiled default, 0 = deferred"
              " to the post-run wal_checkpoint(TRUNCATE))",
    "rows": rows,
}
(pathlib.Path(root) / "comparison.json").write_text(json.dumps(comparison, sort_keys=True,
                                                              indent=2) + "\n", encoding="utf-8")
print(f"{'wal_autocheckpoint':>20} | {'lag p99':>9} | {'service p99':>11} | {'group_elapsed':>13}"
      f" | {'rejected':>8} | {'sustained':>9} | {'wal bytes':>9}")
for pages, row in rows.items():
    print(f"{pages:>20} | {row['lag_p99_us']:>9} | {row['service_p99_us']:>11}"
          f" | {row['group_elapsed_us']:>13} | {row['rejected_windows']:>8}"
          f" | {str(row['sustained']):>9} | {row['wal']['wal_bytes_after_run']:>9}")
PY
