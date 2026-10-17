#!/usr/bin/env bash
# The M1 offered-load ladder: the pre-registered steps `reference 50 rps x [1,2,4,8]`, four arms.
#
#   scripts/m1-ladder-run.sh [--root DIR] [--rungs 50,100,200,400] [--seconds 20] [--skip-build]
#                            [--customers N] [--skus N] [--locs N] [--batch B] [--cap-mib M]
#
# Shape of a run:
#   * one work directory per rung (`$ROOT/rung-<rps>`), each with its OWN frozen trace: a rung's
#     four arms share one stream and one schedule, and one rung can never be mistaken for another;
#   * each rung is one `scripts/m1-baseline-run.sh` invocation, so the `flock /tmp/m1-measure.lock`
#     discipline, the 128 MiB `MemoryMax` scope per arm and the per-arm `memory.peak` are the
#     existing ones rather than a second implementation;
#   * `--seconds` fixes the offered load per rung (`requests = seconds x rps`), so every rung is
#     scored over the same number of 1 s windows;
#   * the machine's load average is recorded before and after every rung (`loadavg.jsonl`), because
#     the schedule-lag rule is sensitive to whatever else the shared box is running.
#
# Postgres runs its data directory under `$M1_PG_ROOT` (default /var/tmp/m1-pg, an ext4 mount): on
# tmpfs `fsync` is a no-op, so a tmpfs data directory would quietly weaken the one baseline whose
# durability claim is its fsync.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
ROOT=$REPO/.scratch/80-external-baselines/runs/m1-measure
RUNGS=50,100,200,400
SECONDS_PER_RUNG=20
CUSTOMERS=2500
SKUS=5000
LOCS=4
BATCH=64
CAP_MIB=128
ARMS=flashdb-declared,flashdb-learned,sqlite,postgres
EXTRA=()

die() { printf 'm1-ladder-run: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --root) ROOT=$2; shift 2 ;;
        --rungs) RUNGS=$2; shift 2 ;;
        --seconds) SECONDS_PER_RUNG=$2; shift 2 ;;
        --customers) CUSTOMERS=$2; shift 2 ;;
        --skus) SKUS=$2; shift 2 ;;
        --locs) LOCS=$2; shift 2 ;;
        --batch) BATCH=$2; shift 2 ;;
        --cap-mib) CAP_MIB=$2; shift 2 ;;
        --arms) ARMS=$2; shift 2 ;;
        --skip-build) EXTRA+=(--skip-build); shift ;;
        *) die "unknown argument $1" ;;
    esac
done

mkdir -p "$ROOT"
ROOT=$(cd "$ROOT" && pwd)
LOADAVG=$ROOT/loadavg.jsonl
: > "$LOADAVG"

record_load() {
    printf '{"rung": %s, "phase": "%s", "at": "%s", "loadavg": "%s", "nproc": %s}\n' \
        "$1" "$2" "$(date -Is)" "$(cut -d' ' -f1-3 /proc/loadavg)" "$(nproc)" >> "$LOADAVG"
}

# Postgres is the user-space cluster from scripts/baseline-env.sh; keep its data off tmpfs.
export M1_PG_ROOT=${M1_PG_ROOT:-/var/tmp/m1-pg}

IFS=, read -r -a rung_list <<< "$RUNGS"
for rung in "${rung_list[@]}"; do
    requests=$((SECONDS_PER_RUNG * rung))
    workdir=$ROOT/rung-$rung
    record_load "$rung" before
    echo "=== rung ${rung} rps: ${requests} requests, arms ${ARMS}"
    "$REPO/scripts/m1-baseline-run.sh" \
        --workdir "$workdir" \
        --requests "$requests" --rate "$rung" \
        --customers "$CUSTOMERS" --skus "$SKUS" --locs "$LOCS" \
        --batch "$BATCH" --cap-mib "$CAP_MIB" --arms "$ARMS" "${EXTRA[@]}"
    record_load "$rung" after
done
echo "m1-ladder-run: artefacts under $ROOT"
