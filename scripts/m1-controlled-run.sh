#!/usr/bin/env bash
# Ticket 82's controlled M1 measurement: paired blocks over a snapshot-restored pre-load instance,
# machine state at every boundary, and a device floor measured with no engine in the path.
#
#   scripts/m1-controlled-run.sh [--root DIR] [--source DIR] [--blocks N] [--rungs 50,100,200]
#                                [--seconds 20] [--arms a,b,c] [--cap-mib M] [--seed-cap-mib M]
#                                [--sqlite-wal-autocheckpoint N] [--since ISO8601]
#                                [--phase all|setup|floor|measure|blocks|score] [--build]
#
# What it fixes, and how
# ----------------------
# * Seeding is SETUP: each arm's pre-load instance is built ONCE from the frozen seed trace, copied
#   aside, and restored before every arm-run (`--mode instance` then `--mode arm`). No seed job runs
#   inside the measurement loop, so an arm-run costs its own 20 s instead of 20 s plus 7-12 minutes.
# * Contention is CONTROLLED rather than hoped away: every (block, rung, arm) crossing runs in one
#   block, so each arm is compared against the others measured in the same window, and the arm order
#   is rotated per (block, rung) so no arm always runs first (or last) in a window's history.
# * The machine state is RECORDED at every arm boundary (AC, power profile, EPP, effective frequency,
#   load and the busiest processes) and the harness stops the run if a build, a seed job or a second
#   harness is running (ticket 82: no subagents, builds or seed jobs during the measured phase).
# * The device FLOOR is measured at the same state with no engine: an idle pacer and an fsync-per-
#   request pacer, so the absolute 10 ms ceiling has a floor-adjusted reading beside it.
#
# One process at a time: this script holds `flock /tmp/m1-measure.lock` for its WHOLE run and hands
# it down to every arm step (`M1_LOCK_HELD=1`), so nothing can interleave between two arms of a
# block.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HARNESS=$REPO/harness
ROOT=$REPO/.scratch/82-controlled-m1/runs
SOURCE=$REPO/.scratch/80-external-baselines/runs/m1-10x/rung-50
BLOCKS=5
RUNGS=50,100,200
SECONDS_PER_RUNG=20
ARMS=flashdb-declared,sqlite,postgres
CAP_MIB=128
SEED_CAP_MIB=512
# sqlite's WAL auto-checkpoint in pages, handed to the runner instead of left to sqlite's default.
SQLITE_WAL_AUTOCHECKPOINT=0
# The ISO instant the score step keeps for the machine-state summary: a phase that re-runs the
# blocks into the same work directories appends its boundaries to the same files, so the summary is
# restricted to one attempt rather than mixing two machine states.
SINCE=
DATABASE=m1-controlled
PHASE=all
BUILD=0
LOCK=/tmp/m1-measure.lock

die() { printf 'm1-controlled-run: %s\n' "$*" >&2; exit 1; }

RUNNER_CMD=$(printf '%q ' "$0" "$@")

while [ $# -gt 0 ]; do
    case "$1" in
        --root) ROOT=$2; shift 2 ;;
        --source) SOURCE=$2; shift 2 ;;
        --blocks) BLOCKS=$2; shift 2 ;;
        --rungs) RUNGS=$2; shift 2 ;;
        --seconds) SECONDS_PER_RUNG=$2; shift 2 ;;
        --arms) ARMS=$2; shift 2 ;;
        --cap-mib) CAP_MIB=$2; shift 2 ;;
        --seed-cap-mib) SEED_CAP_MIB=$2; shift 2 ;;
        --database) DATABASE=$2; shift 2 ;;
        --sqlite-wal-autocheckpoint) SQLITE_WAL_AUTOCHECKPOINT=$2; shift 2 ;;
        --since) SINCE=$2; shift 2 ;;
        --phase) PHASE=$2; shift 2 ;;
        --build) BUILD=1; shift ;;
        *) die "unknown argument $1" ;;
    esac
done

case "$PHASE" in
    all|setup|floor|measure|blocks|score) ;;
    *) die "unknown --phase $PHASE (expected all, setup, floor, measure, blocks or score)" ;;
esac

mkdir -p "$ROOT"
ROOT=$(cd "$ROOT" && pwd)
SOURCE=$(cd "$SOURCE" && pwd)
SNAPSHOT_DIR=$ROOT/snapshot
INSTANCE_ROOT=$ROOT/live
export M1_PG_ROOT=${M1_PG_ROOT:-$ROOT/pg}

# One process at a time, for the WHOLE run: the arm steps inherit this lock instead of taking it.
exec 9>"$LOCK"
flock -w 7200 9 || die "another M1 run holds $LOCK after 2h; nothing was measured"
export M1_LOCK_HELD=1

# The frozen pre-load dataset: the same 200k customers x 2.65M skus x 4 locs the ticket-80 regime
# run used, whose seed trace is the one every arm here is built from.
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
SEED_TRACE=$SOURCE/seed.jsonl
[ -s "$SEED_TRACE" ] || die "frozen seed trace $SEED_TRACE is missing"

harness() { ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b "$@" ); }
record() { printf '%s\n' "$*" >> "$ROOT/orchestrator.log"; }

IFS=, read -r -a RUNG_LIST <<< "$RUNGS"

run_spec() {
    python3 - "$ROOT/run-spec.json" "$RUNNER_CMD" "$REPO" "$SOURCE" "$BLOCKS" "$RUNGS" \
        "$SECONDS_PER_RUNG" "$ARMS" "$CAP_MIB" "$SEED_CAP_MIB" "$DATABASE" \
        "$SEED" "$CUSTOMERS" "$SKUS" "$LOCS" "$ON_HAND" "$BATCH" "$M1_PG_ROOT" "$LOCK" \
        "$SQLITE_WAL_AUTOCHECKPOINT" <<'PY'
import json, sys
(path, argv, repo, source, blocks, rungs, seconds, arms, cap, seed_cap, database, seed,
 customers, skus, locs, on_hand, batch, pg_root, lock, sqlite_wal) = sys.argv[1:21]
json.dump({
    "schema": "flashdb.m1.controlled-run.v1",
    "argv": argv, "repo": repo, "frozen_source": source,
    "blocks": int(blocks), "rungs": [int(r) for r in rungs.split(",")],
    "seconds_per_arm_run": int(seconds), "arms": arms.split(","),
    "engine_cap_mib": int(cap), "seed_cap_mib": int(seed_cap), "postgres_database": database,
    "sqlite": {"cache_kib": 8192, "wal_autocheckpoint_pages": int(sqlite_wal)},
    "dataset": {"seed": int(seed), "customers": int(customers), "skus": int(skus),
                "locs": int(locs), "on_hand": int(on_hand), "batch": int(batch)},
    "postgres_root": pg_root, "lock": lock,
}, open(path, "w"), sort_keys=True, indent=2)
PY
}

# ---------------------------------------------------------------------------
# setup: work directories, a clean cluster, the pre-load instances and their snapshots
# ---------------------------------------------------------------------------
prepare_workdirs() {
    local block rung requests workdir
    for block in $(seq 1 "$BLOCKS"); do
        for rung in "${RUNG_LIST[@]}"; do
            requests=$((SECONDS_PER_RUNG * rung))
            workdir=$ROOT/b$block/rung-$rung
            mkdir -p "$workdir"
            harness prepare --workdir "$workdir" --seed "$SEED" --requests "$requests" \
                --rate "$rung" --customers "$CUSTOMERS" --skus "$SKUS" --locs "$LOCS" \
                --theta "$THETA" --on-hand "$ON_HAND" --batch "$BATCH" \
                --seed-trace "$SEED_TRACE" >/dev/null
            record "prepared b$block rung-$rung: requests=$requests"
        done
    done
}

prepare_cluster() {
    # A CLUSTER OF THIS RUN'S OWN, from the already-extracted server: the seeded database IS the
    # postgres instance, so its data directory has to hold the pre-load state and nothing else.
    mkdir -p "$M1_PG_ROOT"
    for shared in root debs; do
        if [ ! -e "$M1_PG_ROOT/$shared" ] && [ -e "/var/tmp/m1-pg/$shared" ]; then
            ln -sfn "/var/tmp/m1-pg/$shared" "$M1_PG_ROOT/$shared"
        fi
    done
    if [ -d "$M1_PG_ROOT/data" ]; then
        M1_PG_ROOT=$M1_PG_ROOT "$REPO/scripts/baseline-env.sh" stop >/dev/null 2>&1 || true
        rm -rf "$M1_PG_ROOT/data"
    fi
    M1_PG_ROOT=$M1_PG_ROOT "$REPO/scripts/baseline-env.sh" setup >/dev/null
    record "postgres cluster initialised at $M1_PG_ROOT/data"
}

build_instances() {
    # The instances are built from ONE prepared work directory: the dataset depends on the manifest's
    # dimensions and `on_hand`, not on the rung or the block, and the seed trace is the same file.
    local workdir=$ROOT/b1/rung-${RUNG_LIST[0]}
    local build_flag=()
    [ "$BUILD" = 1 ] || build_flag=(--skip-build)
    "$REPO/scripts/m1-baseline-run.sh" --mode instance --workdir "$workdir" --arms "$ARMS" \
        --rate "${RUNG_LIST[0]}" --snapshot-dir "$SNAPSHOT_DIR" --instance-root "$INSTANCE_ROOT" \
        --cap-mib "$CAP_MIB" --seed-cap-mib "$SEED_CAP_MIB" --database "$DATABASE" \
        --sqlite-wal-autocheckpoint "$SQLITE_WAL_AUTOCHECKPOINT" "${build_flag[@]}"
}

run_floor() {
    local label=$1 out=$ROOT/floor/$1
    mkdir -p "$out"
    harness machine-state --out "$out/machine-state.jsonl" --phase "floor-$label-before" \
        --arm floor --rung "${RUNG_LIST[-1]}"
    harness floor --out "$out/floor.json" --rate "${RUNG_LIST[-1]}" \
        --requests $((SECONDS_PER_RUNG * RUNG_LIST[-1]))
    harness machine-state --out "$out/machine-state.jsonl" --phase "floor-$label-after" \
        --arm floor --rung "${RUNG_LIST[-1]}"
    record "device floor measured ($label) at the same machine state"
}

# ---------------------------------------------------------------------------
# measure: N blocks; within a block each rung is crossed by every arm, restore -> run -> restore
# ---------------------------------------------------------------------------
measure_blocks() {
    local block rung_index rung arm rot workdir order i
    for block in $(seq 1 "$BLOCKS"); do
        rung_index=0
        for rung in "${RUNG_LIST[@]}"; do
            workdir=$ROOT/b$block/rung-$rung
            IFS=, read -r -a order <<< "$ARMS"
            # Rotate the arm order per (block, rung): the block is the pairing unit, so no arm may
            # always run first and no arm may always run last.
            rot=$(( (block - 1 + rung_index) % ${#order[@]} ))
            order=("${order[@]:rot}" "${order[@]:0:rot}")
            for arm in "${order[@]}"; do
                record "block $block rung $rung arm $arm"
                # The SQL arms' inventory oracle runs in block 1 only: the restore digest proves the
                # instance is byte-identical in every block, so the committed state it checks is the
                # same state, and fifteen more scans of 10.6M cells would buy nothing.
                if [ "$block" -gt 1 ]; then export M1_SKIP_ORACLE=1; else export M1_SKIP_ORACLE=0; fi
                "$REPO/scripts/m1-baseline-run.sh" --mode arm --workdir "$workdir" \
                    --arms "$arm" --rate "$rung" \
                    --requests $((SECONDS_PER_RUNG * rung)) \
                    --snapshot-dir "$SNAPSHOT_DIR" --instance-root "$INSTANCE_ROOT" \
                    --cap-mib "$CAP_MIB" --seed-cap-mib "$SEED_CAP_MIB" --database "$DATABASE" \
                    --sqlite-wal-autocheckpoint "$SQLITE_WAL_AUTOCHECKPOINT" \
                    --skip-verify --skip-build
            done
            rung_index=$((rung_index + 1))
        done
    done
}

score() {
    local since_args=()
    [ -n "$SINCE" ] && since_args=(--since "$SINCE")
    python3 "$REPO/scripts/m1-controlled-score.py" --root "$ROOT" \
        --out "$ROOT/report.json" --markdown "$ROOT/table.md" "${since_args[@]}"
}

run_spec
if [ "$PHASE" = all ] || [ "$PHASE" = setup ]; then
    prepare_workdirs
    prepare_cluster
    build_instances
    run_floor before
fi
if [ "$PHASE" = all ] || [ "$PHASE" = floor ]; then
    run_floor after
fi
if [ "$PHASE" = all ] || [ "$PHASE" = measure ]; then
    measure_blocks
    run_floor after
fi
# A re-measurement of the blocks alone, reusing the committed floor and snapshots: the two fairness
# fixes change the arms, not the device, so the floor is not re-taken.
if [ "$PHASE" = blocks ]; then
    measure_blocks
fi
if [ "$PHASE" = all ] || [ "$PHASE" = score ]; then
    score
fi
printf 'm1-controlled-run: artefacts in %s\n' "$ROOT"
