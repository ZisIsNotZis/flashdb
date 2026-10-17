#!/usr/bin/env bash
# The M1 external-baseline run: one frozen trace, three arms, each in its own 128 MiB scope.
#
#   scripts/m1-baseline-run.sh [--workdir DIR] [--requests N] [--rate R] [--arms a,b,c]
#                              [--on-hand N] [--cap-mib M] [--seed-cap-mib M] [--skip-build]
#
# What it guarantees:
#   * one process at a time: the whole run holds `flock /tmp/m1-measure.lock`;
#   * one trace for every arm: `prepare` emits it once and each `arm` step re-hashes it against the
#     manifest before measuring, so an arm cannot measure a different stream;
#   * the same memory regime: every arm runs inside `systemd-run --user --scope
#     -p MemoryMax=<cap>M -p MemorySwapMax=0`, and the arm record carries the scope's own
#     `memory.peak`;
#   * postgres is the user-space cluster `scripts/baseline-env.sh` builds: the SERVER (the engine)
#     runs in its own capped scope and the client stays OUTSIDE it, so the cap covers the engine and
#     not the instrument.
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
# The construction (seeding) budget, separate from the engine's serving cap: `flashdb-scale
# seed-inventory` peaks with the dimension set, so a 10^7-doc instance is BUILT under a larger
# transient cap. Recorded in `spec.json` and in every flashdb run record.
SEED_CAP_MIB=192
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
SKIP_VERIFY=0
PAYLOADS=$REPO/.tmp/m1-run

die() { printf 'm1-baseline-run: %s\n' "$*" >&2; exit 1; }

# The exact invocation, shell-quoted, taken BEFORE the options are shifted away: it lands in
# `spec.json` so a committed run names the command that produced it.
RUNNER_CMD=$(printf '%q ' "$0" "$@")
RUNNER_CWD=$PWD

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
        --seed-cap-mib) SEED_CAP_MIB=$2; shift 2 ;;
        --arms) ARMS=$2; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --skip-verify) SKIP_VERIFY=1; shift ;;
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

# The machine's load average at every boundary of this run: the schedule-lag rule is sensitive to
# whatever else the shared box runs, so a boundary reading is what lets a reader judge one rung
# against another. Written per work directory (`loadavg.jsonl`), arm by arm.
LOADAVG=$WORKDIR/loadavg.jsonl
record_load() {
    printf '{"phase": "%s", "at": "%s", "loadavg": "%s", "nproc": %s}\n' \
        "$1" "$(date -Is)" "$(cut -d' ' -f1-3 /proc/loadavg)" "$(nproc)" >> "$LOADAVG"
}

# Record in `spec.json` what the arm records cannot say for themselves: the exact command line, the
# working directory, the two caps and the lock that serialised the run.
record_runner() {
    python3 - "$WORKDIR/spec.json" "$CAP_MIB" "$SEED_CAP_MIB" "$LOCK" \
        "${M1_PG_ROOT:-}" "$RUNNER_CMD" "$RUNNER_CWD" <<'PY'
import json, sys
path, cap, seed_cap, lock, pg_root, argv, cwd = sys.argv[1:8]
spec = json.load(open(path))
spec["runner"] = {"argv": argv, "cwd": cwd, "engine_cap_mib": int(cap),
                  "seed_cap_mib": int(seed_cap), "lock": lock, "postgres_root": pg_root}
json.dump(spec, open(path, "w"), sort_keys=True, indent=2)
PY
}

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

# The measured memory budget is the STORAGE ENGINE's, so each arm bounds its engine and nothing
# else (the ticket's "memory boundary" note):
#   * flashdb: the harness wraps `flashdb-load` in its own `MemoryMax` scope, and the post-hoc
#     inventory read-back runs after it as a separate `verify` step, outside that budget;
#   * sqlite: the engine is EMBEDDED in the arm's process, so this one arm's process (driver +
#     pacer + engine) is what the scope caps, with `PRAGMA cache_size` stating the engine's cache;
#   * postgres: the server runs in its own named scope and only the client stays outside it.
# The payloads are written with a QUOTED heredoc: they expand their own variables at run time, and
# the parameters reach them through the environment (`M1_*`), never through shell interpolation.
export M1_CAP_MIB=$CAP_MIB M1_SEED_CAP_MIB=$SEED_CAP_MIB M1_WORKDIR=$WORKDIR
export M1_HARNESS=$HARNESS M1_REPO=$REPO M1_SKIP_VERIFY=$SKIP_VERIFY

run_arm() {
    local name=$1
    local payload=$PAYLOADS/$name.sh
    case "$name" in
        flashdb-declared|flashdb-learned)
            cat > "$payload" <<'EOF'
set -euo pipefail
cd "$M1_HARNESS"
uv run python -m flashdb_harness.baseline_0b arm --workdir "$M1_WORKDIR" --arm ARG_ARM \
    --engine-cap-mib "$M1_CAP_MIB" --seed-cap-mib "$M1_SEED_CAP_MIB" --skip-verify
if [ "$M1_SKIP_VERIFY" = 1 ]; then
    echo "flashdb read-back DEFERRED (--skip-verify): see the run record" >&2
else
    uv run python -m flashdb_harness.baseline_0b verify --workdir "$M1_WORKDIR" --arm ARG_ARM
fi
EOF
            sed -i "s/ARG_ARM/$name/" "$payload"
            chmod +x "$payload"
            bash "$payload" || die "arm $name failed (see $WORKDIR)"
            ;;
        sqlite)
            cat > "$payload" <<'EOF'
set -euo pipefail
cd "$M1_HARNESS"
exec uv run python -m flashdb_harness.baseline_0b arm --workdir "$M1_WORKDIR" --arm sqlite \
    --engine-cap-mib "$M1_CAP_MIB"
EOF
            chmod +x "$payload"
            systemd-run --user --scope -q \
                -p "MemoryMax=${CAP_MIB}M" -p MemorySwapMax=0 bash "$payload" \
                || die "arm $name failed inside the ${CAP_MIB} MiB scope (see $WORKDIR)"
            ;;
        postgres)
            cat > "$payload" <<'EOF'
set -euo pipefail
export M1_PG_ROOT=${M1_PG_ROOT:-/var/tmp/m1-pg}
eval "$("$M1_REPO/scripts/baseline-env.sh" env)"
unit="m1-pg-postgres-$$"
# The server IS the engine here: its own capped cgroup, client outside it.
systemd-run --user --scope -q --unit="$unit" -p "MemoryMax=${M1_CAP_MIB}M" -p MemorySwapMax=0 \
    bash -c "pg_ctl start -w -D '$M1_PG_ROOT/data' -l '$M1_PG_ROOT/pg.log' && exec sleep 7200" &
scope_pid=$!
ready=0
for _ in $(seq 1 600); do
    if [ -S "$M1_PGHOST/.s.PGSQL.$M1_PGPORT" ]; then ready=1; break; fi
    sleep 0.1
done
[ "$ready" = 1 ] || { echo "postgres did not start" >&2; exit 1; }
rc=0
# The server's scope is the ENGINE's cgroup; hand its path to the client so the client can sample
# the SERVER's `memory.current` across the measured phase (the client itself stays outside the cap).
cg=$(systemctl --user show --property=ControlGroup --value "$unit.scope")
export M1_ENGINE_CGROUP="/sys/fs/cgroup$cg"
cd "$M1_HARNESS"
uv run --with 'psycopg[binary]==3.3.6' python -m flashdb_harness.baseline_0b \
    arm --workdir "$M1_WORKDIR" --arm postgres --engine-cap-mib "$M1_CAP_MIB" || rc=$?
peak=$(cat "/sys/fs/cgroup$cg/memory.peak" 2>/dev/null || echo 0)
printf '{"arm": "postgres", "server_lifetime_peak_bytes": %s, "cap_mib": %s}\n' \
    "$peak" "${M1_CAP_MIB}" > "$M1_WORKDIR/pg-server-peak.json"
python3 - "$M1_WORKDIR/run-postgres.json" "$peak" <<'PY'
import json, sys
path, peak = sys.argv[1], int(sys.argv[2])
record = json.load(open(path))
# The server scope's LIFETIME peak holds the bulk load as well, so it is recorded as the arm-scope
# number; the serving peak is the measured-phase sample the harness took.
record["arm_scope_peak_bytes"] = peak
json.dump(record, open(path, "w"), sort_keys=True, indent=2)
PY
pg_ctl stop -w -D "$M1_PG_ROOT/data" >/dev/null 2>&1 || true
systemctl --user stop "$unit.scope" >/dev/null 2>&1 || true
wait "$scope_pid" 2>/dev/null || true
exit $rc
EOF
            chmod +x "$payload"
            bash "$payload" || die "arm $name failed (see $WORKDIR)"
            ;;
        *) die "unknown arm $name" ;;
    esac
}

build_binaries
record_load prepare-before
prepare
record_load prepare-after
record_runner
IFS=, read -r -a arm_list <<< "$ARMS"
for arm in "${arm_list[@]}"; do
    record_load "arm-$arm-before"
    run_arm "$arm"
    record_load "arm-$arm-after"
done
( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b report \
    --workdir "$WORKDIR" --arms "$ARMS" )
record_load done
echo "m1-baseline-run: artefacts in $WORKDIR"
