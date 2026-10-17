#!/usr/bin/env bash
# The M1 external-baseline run: one frozen trace, three arms, each in its own 128 MiB scope.
#
#   scripts/m1-baseline-run.sh [--workdir DIR] [--requests N] [--rate R] [--arms a,b,c]
#                              [--on-hand N] [--cap-mib M] [--skip-build]
#
# What it guarantees:
#   * one process at a time: the whole run holds `flock /tmp/m1-measure.lock`;
#   * one trace for every arm: `prepare` emits it once and each `arm` step re-hashes it against the
#     manifest before measuring, so an arm cannot measure a different stream;
#   * the same memory regime: every arm runs inside `systemd-run --user --scope
#     -p MemoryMax=<cap>M -p MemorySwapMax=0`, and the arm record carries the scope's own
#     `memory.peak`;
#   * postgres is the user-space cluster `scripts/baseline-env.sh` builds, started and stopped
#     inside the arm's scope so the cap covers the server as well as the client.
#
# Artefacts: $WORKDIR/{manifest,spec}.json, windows-<arm>.jsonl, run-<arm>.json, scoreboard.{md,json}.
# Regenerable state (the trace, the engines' instances, the sqlite file, driver logs) stays out of
# git: see $WORKDIR/.gitignore.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
HARNESS=$REPO/harness
WORKDIR=$REPO/.scratch/80-external-baselines/runs/m1-build
CAP_MIB=128
LOCK=/tmp/m1-measure.lock
SEED=42
REQUESTS=2000
RATE=50
CUSTOMERS=200
SKUS=500
LOCS=4
BATCH=64
# The seeded `Stock.on_hand` per cell. The scored rung keeps it high enough that the guard passes
# (the pre-registered workload's shape); a low value forces the guard's `else` branch, which is how
# the backorder arm of the oracle is exercised end to end.
ON_HAND=1000000
ARMS=flashdb-declared,sqlite,postgres
SKIP_BUILD=0
PAYLOADS=$REPO/.tmp/m1-run

die() { printf 'm1-baseline-run: %s\n' "$*" >&2; exit 1; }

while [ $# -gt 0 ]; do
    case "$1" in
        --workdir) WORKDIR=$2; shift 2 ;;
        --requests) REQUESTS=$2; shift 2 ;;
        --rate) RATE=$2; shift 2 ;;
        --customers) CUSTOMERS=$2; shift 2 ;;
        --skus) SKUS=$2; shift 2 ;;
        --locs) LOCS=$2; shift 2 ;;
        --batch) BATCH=$2; shift 2 ;;
        --on-hand) ON_HAND=$2; shift 2 ;;
        --seed) SEED=$2; shift 2 ;;
        --cap-mib) CAP_MIB=$2; shift 2 ;;
        --arms) ARMS=$2; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        *) die "unknown argument $1" ;;
    esac
done

# One process at a time (`.scratch/STATUS.md` §6). The lock is held for the whole run, including
# the report, so two invocations can never interleave their arms.
exec 9>"$LOCK"
flock -w 7200 9 || die "another M1 run holds $LOCK after 2h; nothing was measured"

# Resolve the work directory against the caller's CWD once: the arm payloads `cd` into the harness,
# so a relative path handed to them would silently land inside harness/ instead.
mkdir -p "$WORKDIR"
WORKDIR=$(cd "$WORKDIR" && pwd)
mkdir -p "$PAYLOADS"

# crates.io is unreachable from this box; the distro's aliyun mirror carries the sparse index and
# the crate files, so the build is pinned to it by flags and touches no user or host config.
CARGO_MIRROR=(
    --config 'source.crates-io.replace-with="aliyun"'
    --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"'
)

build_binaries() {
    if [ "$SKIP_BUILD" = 1 ]; then
        [ -x "$REPO/target/release/flashdb-load" ] || die "--skip-build but target/release is empty"
        return
    fi
    echo "+ cargo build --release (aliyun crates mirror; crates.io is unreachable)"
    ( cd "$REPO" && cargo "${CARGO_MIRROR[@]}" build --release \
        --bin flashdb-scale --bin flashdb-load --bin flashdb-replay ) \
        || die "cargo build failed; the aliyun crates mirror must be reachable"
}

prepare() {
    ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b prepare \
        --workdir "$WORKDIR" --seed "$SEED" --requests "$REQUESTS" --rate "$RATE" \
        --customers "$CUSTOMERS" --skus "$SKUS" --locs "$LOCS" --batch "$BATCH" \
        --on-hand "$ON_HAND" )
}

# Every arm runs inside its own capped scope. The payload is a file, not a quoted `bash -c`
# string, so the scope's command line stays inspectable in the artefacts.
run_in_scope() {
    local name=$1
    local payload=$PAYLOADS/$name.sh
    systemd-run --user --scope -q \
        -p "MemoryMax=${CAP_MIB}M" -p MemorySwapMax=0 bash "$payload" \
        || die "arm $name failed inside the ${CAP_MIB} MiB scope (see $WORKDIR)"
}

run_arm() {
    local name=$1
    local payload=$PAYLOADS/$name.sh
    case "$name" in
        sqlite|flashdb-declared|flashdb-learned)
            cat > "$payload" <<EOF
set -euo pipefail
cd "$HARNESS"
exec uv run python -m flashdb_harness.baseline_0b arm --workdir "$WORKDIR" --arm "$name"
EOF
            ;;
        postgres)
            cat > "$payload" <<EOF
set -euo pipefail
export M1_PG_ROOT=$(printf %q "${M1_PG_ROOT:-/tmp/m1-pg}")
eval "\$("$REPO/scripts/baseline-env.sh" env)"
"$REPO/scripts/baseline-env.sh" start
rc=0
cd "$HARNESS"
uv run --with 'psycopg[binary]==3.3.6' python -m flashdb_harness.baseline_0b \
    arm --workdir "$WORKDIR" --arm postgres || rc=\$?
"$REPO/scripts/baseline-env.sh" stop
exit \$rc
EOF
            ;;
        *) die "unknown arm $name" ;;
    esac
    chmod +x "$payload"
    run_in_scope "$name"
}

build_binaries
prepare
IFS=, read -r -a arm_list <<< "$ARMS"
for arm in "${arm_list[@]}"; do
    run_arm "$arm"
done
( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b report \
    --workdir "$WORKDIR" --arms "$ARMS" )
echo "m1-baseline-run: artefacts in $WORKDIR"
