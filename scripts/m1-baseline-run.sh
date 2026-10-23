#!/usr/bin/env bash
# The M1 external-baseline run: one frozen trace, three arms, each in its own 128 MiB scope.
#
#   scripts/m1-baseline-run.sh [--mode full|instance|arm] [--workdir DIR] [--requests N] [--rate R]
#                              [--arms a,b,c] [--on-hand N] [--cap-mib M] [--seed-cap-mib M]
#                              [--snapshot-dir DIR] [--instance-root DIR] [--database NAME]
#                              [--skip-build]
#
# Modes (ticket 82's controlled protocol; `full` is the ticket-80 one-shot run and is unchanged):
#   full      prepare the trace, build each arm's instance, serve it, report.
#   instance  build each arm's PRE-LOAD instance once (seed only, no request served) and snapshot it.
#   arm       restore each arm's snapshot and serve ONE run from it, seeding nothing.
# The controlled protocol never seeds inside the measurement loop: an arm-run costs its own
# duration, and the state it starts from is a verified copy of the seeded one (a digest mismatch
# stops the run).
#
# What it guarantees:
#   * one process at a time: the whole run holds `flock /tmp/m1-measure.lock`;
#   * one trace for every arm: `prepare` emits it once and each `arm` step re-hashes it against the
#     manifest before measuring, so an arm cannot measure a different stream;
#   * the same memory regime: every arm runs inside `systemd-run --user --scope
#     -p MemoryMax=<cap>M -p MemorySwapMax=0`, and the arm record carries the scope's own
#     `memory.peak`;
#   * postgres is a container this tree starts and checks
#     (`.scratch/90-m5-postgres-env/postgres-server.sh`): the SERVER (the engine) is a container capped with `--memory` and the
#     client stays OUTSIDE it, so the cap covers the engine and not the instrument. The image's
#     settings block is the M1 record's, read back from the running server; a setting that differs
#     stops the arm, and `M5_PG_REQUIRE` stops an arm measured against a version nobody named. The
#     user-space cluster `scripts/baseline-env.sh` builds stays as the M1 record's route.
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
# sqlite's page cache, stated to the arm exactly as `baseline_0b.SQLITE_CACHE_KIB` defaults it.
SQLITE_CACHE_KIB=8192
# sqlite's WAL auto-checkpoint, stated rather than left to sqlite's compiled default (1000 pages).
# The measured default defers it to the post-run `wal_checkpoint(TRUNCATE)`; the checkpoint probe
# passes 1000 to reproduce the stall the f70d91c arm-runs show.
SQLITE_WAL_AUTOCHECKPOINT=0
MODE=full
SNAPSHOT_DIR=
INSTANCE_ROOT=
DATABASE=m1-controlled
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
        --mode) MODE=$2; shift 2 ;;
        --snapshot-dir) SNAPSHOT_DIR=$2; shift 2 ;;
        --instance-root) INSTANCE_ROOT=$2; shift 2 ;;
        --database) DATABASE=$2; shift 2 ;;
        --sqlite-cache-kib) SQLITE_CACHE_KIB=$2; shift 2 ;;
        --sqlite-wal-autocheckpoint) SQLITE_WAL_AUTOCHECKPOINT=$2; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --skip-verify) SKIP_VERIFY=1; shift ;;
        *) die "unknown argument $1" ;;
    esac
done

# One process at a time (`.scratch/STATUS.md` §6). The lock is held for the whole run, including
# the report, so two invocations can never interleave their arms. `M1_LOCK_HELD=1` is how the
# controlled orchestrator hands down the lock it took for its WHOLE measured phase: an arm step
# inside that phase must not block on a lock its own parent holds.
LOCK_HELD_BY="${M1_LOCK_HELD:-}"
if [ "$LOCK_HELD_BY" = 1 ]; then
    echo "m1-baseline-run: using the caller's $LOCK (held for the whole measured phase)"
else
    exec 9>"$LOCK"
    flock -w 7200 9 || die "another M1 run holds $LOCK after 2h; nothing was measured"
fi

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
        "${M1_PG_ROOT:-}" "$RUNNER_CMD" "$RUNNER_CWD" "$MODE" "$SNAPSHOT_DIR" \
        "$INSTANCE_ROOT" "$DATABASE" "${LOCK_HELD_BY:-}" <<'PY'
import json, sys
path, cap, seed_cap, lock, pg_root, argv, cwd, mode, snap, live, db, held = sys.argv[1:13]
spec = json.load(open(path))
spec["runner"] = {"argv": argv, "cwd": cwd, "engine_cap_mib": int(cap),
                  "seed_cap_mib": int(seed_cap), "lock": lock, "postgres_root": pg_root,
                  "mode": mode, "snapshot_dir": snap or None, "instance_root": live or None,
                  "postgres_database": db, "lock_held_by_caller": held == "1"}
json.dump(spec, open(path, "w"), sort_keys=True, indent=2)
PY
}

# The machine state every schedule-lag reading depends on, taken at each arm boundary by the
# harness (`machine-state`), which also refuses to continue when a build, a seed job or a second
# harness is running: ticket 82 allows the desktop applications to stay and nothing else.
MACHINE_STATE=$WORKDIR/machine-state.jsonl
record_machine_state() {
    local boundary=$1 arm=$2
    ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b machine-state \
        --out "$MACHINE_STATE" --phase "$boundary" --arm "$arm" --rung "$RATE" ) \
        || die "machine state at $boundary for $arm found concurrent work (or failed); stopping"
}

# The controlled protocol's two per-arm state steps, deliberately OUTSIDE every engine scope: a
# 1.7 GB copy is the instrument's I/O, not the engine's memory, and the instance it leaves behind is
# what the arm step measures.
restore_arm() {
    # The postgres instance is the server's data directory, so the server must not be running while
    # the snapshot is written under it: a container left up would serve the pre-restore bytes under
    # the restored record's name, and the record would then describe one state and measure another.
    if [ "$1" = postgres ]; then
        M5_PG_IMAGE=$M5_PG_IMAGE M5_PG_ROOT=$M1_PG_ROOT "$POSTGRES_SERVER" down >/dev/null
    fi
    ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b restore \
        --workdir "$WORKDIR" --arms "$1" --snapshot-dir "$SNAPSHOT_DIR" \
        --instance-root "$INSTANCE_ROOT" )
}

snapshot_arm() {
    ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b snapshot \
        --workdir "$WORKDIR" --arms "$1" --snapshot-dir "$SNAPSHOT_DIR" \
        --instance-root "$INSTANCE_ROOT" )
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
# One cluster root for the whole chain: the harness that restores the data directory and the
# container that serves it must resolve the SAME path (the container bind-mounts `$M1_PG_ROOT/data`
# at that same path and PGDATA points at it, so `show data_directory` reports what the harness
# restored — the harness refuses a restored run whose served directory is another one).
#
# The postgres leg is a container, not a host cluster: `M5_PG_IMAGE` names the image (default 18,
# the major version M1 measured), `M5_PG_REQUIRE` names the version a result may claim, and the
# server's settings block is checked against the M1 record by `postgres-server.sh` on every start.
M5_PG_IMAGE=${M5_PG_IMAGE:-postgres:18}
M5_PG_REQUIRE=${M5_PG_REQUIRE:-}
POSTGRES_SERVER=$REPO/.scratch/90-m5-postgres-env/postgres-server.sh
export M5_PG_IMAGE M5_PG_REQUIRE
export M1_PG_ROOT=${M1_PG_ROOT:-/var/tmp/m5-pg}
export M1_CAP_MIB=$CAP_MIB M1_SEED_CAP_MIB=$SEED_CAP_MIB M1_WORKDIR=$WORKDIR
export M1_HARNESS=$HARNESS M1_REPO=$REPO M1_SKIP_VERIFY=$SKIP_VERIFY
export M1_INSTANCE_ROOT=$INSTANCE_ROOT M1_DATABASE=$DATABASE
export M1_SQLITE_CACHE_KIB=$SQLITE_CACHE_KIB M1_SKIP_ORACLE=${M1_SKIP_ORACLE:-0}
export M1_SQLITE_WAL_AUTOCHECKPOINT=$SQLITE_WAL_AUTOCHECKPOINT

run_step() {
    local name=$1
    local payload=$PAYLOADS/$name.sh
    case "$name" in
        flashdb-declared|flashdb-learned)
            cat > "$payload" <<'EOF'
set -euo pipefail
cd "$M1_HARNESS"
if [ "$M1_STEP" = instance ]; then
    uv run python -m flashdb_harness.baseline_0b build-instance --workdir "$M1_WORKDIR" \
        --arms ARG_ARM --engine-cap-mib "$M1_CAP_MIB" --seed-cap-mib "$M1_SEED_CAP_MIB" \
        --instance-root "$M1_INSTANCE_ROOT" --database "$M1_DATABASE"
    exit 0
fi
if [ "$M1_RESTORED" = 1 ]; then
    restored_args=(--restored --instance-root "$M1_INSTANCE_ROOT")
else
    restored_args=()
fi
uv run python -m flashdb_harness.baseline_0b arm --workdir "$M1_WORKDIR" --arm ARG_ARM \
    --engine-cap-mib "$M1_CAP_MIB" --seed-cap-mib "$M1_SEED_CAP_MIB" --skip-verify \
    --database "$M1_DATABASE" "${restored_args[@]}"
if [ "$M1_SKIP_VERIFY" = 1 ]; then
    echo "flashdb read-back DEFERRED (--skip-verify): see the run record" >&2
else
    uv run python -m flashdb_harness.baseline_0b verify --workdir "$M1_WORKDIR" --arm ARG_ARM
fi
EOF
            sed -i "s/ARG_ARM/$name/" "$payload"
            chmod +x "$payload"
            bash "$payload" || die "$M1_STEP $name failed (see $WORKDIR)"
            ;;
        sqlite)
            cat > "$payload" <<'EOF'
set -euo pipefail
cd "$M1_HARNESS"
if [ "$M1_STEP" = instance ]; then
    exec uv run python -m flashdb_harness.baseline_0b build-instance --workdir "$M1_WORKDIR" \
        --arms sqlite --sqlite-cache-kib "$M1_SQLITE_CACHE_KIB" \
        --sqlite-wal-autocheckpoint "$M1_SQLITE_WAL_AUTOCHECKPOINT" \
        --instance-root "$M1_INSTANCE_ROOT"
fi
oracle_args=()
if [ "$M1_SKIP_ORACLE" = 1 ]; then oracle_args=(--skip-oracle); fi
if [ "$M1_RESTORED" = 1 ]; then
    exec uv run python -m flashdb_harness.baseline_0b arm --workdir "$M1_WORKDIR" --arm sqlite \
        --engine-cap-mib "$M1_CAP_MIB" --sqlite-cache-kib "$M1_SQLITE_CACHE_KIB" \
        --sqlite-wal-autocheckpoint "$M1_SQLITE_WAL_AUTOCHECKPOINT" \
        --restored --instance-root "$M1_INSTANCE_ROOT" "${oracle_args[@]}"
fi
exec uv run python -m flashdb_harness.baseline_0b arm --workdir "$M1_WORKDIR" --arm sqlite \
    --engine-cap-mib "$M1_CAP_MIB" --sqlite-cache-kib "$M1_SQLITE_CACHE_KIB" \
    --sqlite-wal-autocheckpoint "$M1_SQLITE_WAL_AUTOCHECKPOINT" "${oracle_args[@]}"
EOF
            chmod +x "$payload"
            systemd-run --user --scope -q \
                -p "MemoryMax=${CAP_MIB}M" -p MemorySwapMax=0 bash "$payload" \
                || die "$M1_STEP $name failed inside the ${CAP_MIB} MiB scope (see $WORKDIR)"
            ;;
        postgres)
            cat > "$payload" <<'EOF'
set -euo pipefail
export M1_PG_ROOT=${M1_PG_ROOT:-/var/tmp/m5-pg}
# The server is the container `postgres-server.sh` starts and checks: no host `pg_ctl`, no system
# package. `down` first, so the step serves the data directory on disk and not a container left
# behind by an earlier step; `up` pulls if the image is absent, starts it under `--memory`, waits
# for `pg_isready`, then checks every setting against the M1 record and refuses a mismatch (and
# refuses a version `M5_PG_REQUIRE` does not name).
server="$M1_REPO/.scratch/90-m5-postgres-env/postgres-server.sh"
export M5_PG_IMAGE M5_PG_REQUIRE M5_PG_ROOT="$M1_PG_ROOT"
"$server" down >/dev/null
"$server" up
# The container's cap IS the engine's budget, and it is refused when it is not the one the run
# record states: a cap that drifted makes every resident-byte number a different measurement.
container="${M5_PG_NAME:-flashdb-m5-pg}"
cap_bytes=$(docker inspect -f '{{.HostConfig.Memory}}' "$container")
[ "$cap_bytes" = "$((M1_CAP_MIB * 1024 * 1024))" ] || {
    echo "postgres container cap is $cap_bytes bytes, the arm's recorded cap is ${M1_CAP_MIB} MiB" >&2
    exit 1
}
eval "$("$server" env)"
# The version is read back here, not inherited from the image tag, and it is written beside the
# step that measured it so a report can name it without guessing.
printf '{"step": "%s", "at": "%s", "image": "%s", "require": "%s", "server_version": "%s", "data_directory": "%s"}\n' \
    "$M1_STEP" "$(date -Is)" "$M5_PG_IMAGE" "${M5_PG_REQUIRE:-none}" "$M1_PG_SERVER_VERSION" \
    "$(docker exec "$container" psql -U "$M1_PGUSER" -d postgres -tAc 'show data_directory')" \
    >> "$M1_WORKDIR/postgres-server.jsonl"
rc=0
# The container's cgroup is the ENGINE's; hand its path to the client so the client can sample the
# SERVER's `memory.current` across the measured phase (the client itself stays outside the cap).
pid=$(docker inspect -f '{{.State.Pid}}' "$container")
cg=$(awk -F: '$1==0{print $3}' "/proc/$pid/cgroup")
[ -n "$cg" ] || { echo "the postgres container has no cgroup" >&2; exit 1; }
export M1_ENGINE_CGROUP="/sys/fs/cgroup$cg"
cd "$M1_HARNESS"
if [ "$M1_STEP" = instance ]; then
    uv run --with 'psycopg[binary]==3.3.6' python -m flashdb_harness.baseline_0b \
        build-instance --workdir "$M1_WORKDIR" --arms postgres \
        --engine-cap-mib "$M1_CAP_MIB" --database "$M1_DATABASE" || rc=$?
else
    if [ "$M1_RESTORED" = 1 ]; then
        restored_args=(--restored --database "$M1_DATABASE" --instance-root "$M1_INSTANCE_ROOT")
    else
        restored_args=()
    fi
    if [ "$M1_SKIP_ORACLE" = 1 ]; then restored_args+=(--skip-oracle); fi
    uv run --with 'psycopg[binary]==3.3.6' python -m flashdb_harness.baseline_0b \
        arm --workdir "$M1_WORKDIR" --arm postgres --engine-cap-mib "$M1_CAP_MIB" \
        "${restored_args[@]}" || rc=$?
fi
if [ -f "$M1_WORKDIR/run-postgres.json" ] && [ "$M1_STEP" = arm ]; then
    peak=$(cat "/sys/fs/cgroup$cg/memory.peak" 2>/dev/null || echo 0)
    printf '{"arm": "postgres", "server_lifetime_peak_bytes": %s, "cap_mib": %s}\n' \
        "$peak" "${M1_CAP_MIB}" > "$M1_WORKDIR/pg-server-peak.json"
    python3 - "$M1_WORKDIR/run-postgres.json" "$peak" <<'PYINNER'
import json, sys
path, peak = sys.argv[1], int(sys.argv[2])
record = json.load(open(path))
# The server scope's LIFETIME peak holds the bulk load as well, so it is recorded as the arm-scope
# number; the serving peak is the measured-phase sample the harness took.
record["arm_scope_peak_bytes"] = peak
json.dump(record, open(path, "w"), sort_keys=True, indent=2)
PYINNER
fi
# The peak, while the container still exists, is reported as the arm-scope number: it holds the
# bulk load too, so it is not the serving peak — the harness samples that itself. The container is
# then removed (its data directory stays) so the next restore writes a directory nobody serves.
"$server" down
exit $rc
EOF
            chmod +x "$payload"
            bash "$payload" || die "$M1_STEP $name failed (see $WORKDIR)"
            ;;
        *) die "unknown arm $name" ;;
    esac
}

build_binaries
case "$MODE" in
    full)
        record_load prepare-before
        prepare
        record_load prepare-after
        ;;
    instance|arm)
        # The controlled protocol prepares its work directories once, up front, and the instance
        # lives OUTSIDE them (`--instance-root`): a restored instance is one copy per arm, not one
        # copy per block and rung.
        [ -f "$WORKDIR/manifest.json" ] || die "mode $MODE needs a prepared workdir ($WORKDIR)"
        [ -n "$SNAPSHOT_DIR" ] || die "mode $MODE needs --snapshot-dir"
        [ -n "$INSTANCE_ROOT" ] || die "mode $MODE needs --instance-root"
        mkdir -p "$SNAPSHOT_DIR" "$INSTANCE_ROOT"
        # The harness runs from its own directory, so a relative path handed to it would silently
        # land under harness/. Resolve both once, the way $WORKDIR is resolved above.
        SNAPSHOT_DIR=$(cd "$SNAPSHOT_DIR" && pwd)
        INSTANCE_ROOT=$(cd "$INSTANCE_ROOT" && pwd)
        # The payloads `cd` into the harness, so they get the absolute path too: a relative
        # `M1_INSTANCE_ROOT` resolved under harness/ and the restore record was looked for there.
        export M1_INSTANCE_ROOT=$INSTANCE_ROOT
        ;;
    *) die "unknown --mode $MODE (expected full, instance or arm)" ;;
esac
record_runner
IFS=, read -r -a arm_list <<< "$ARMS"
for arm in "${arm_list[@]}"; do
    record_load "$MODE-$arm-before"
    if [ "$MODE" != full ]; then
        record_machine_state before "$arm"
    fi
    case "$MODE" in
        full)
            export M1_STEP=arm M1_RESTORED=0
            run_step "$arm"
            ;;
        instance)
            export M1_STEP=instance M1_RESTORED=0
            run_step "$arm"
            # The snapshot is taken with the engine STOPPED (the payload stopped it): postgres's
            # instance is its data directory, and a copy taken while the server ran would not be a
            # state any restore could reproduce.
            snapshot_arm "$arm"
            ;;
        arm)
            # Restore BEFORE the arm's scope opens: copying 1.7 GB back is the instrument's I/O,
            # and the engine must find the seeded state already in place.
            restore_arm "$arm"
            export M1_STEP=arm M1_RESTORED=1
            run_step "$arm"
            ;;
    esac
    if [ "$MODE" != full ]; then
        record_machine_state after "$arm"
    fi
    record_load "$MODE-$arm-after"
done
if [ "$MODE" = full ]; then
    ( cd "$HARNESS" && uv run python -m flashdb_harness.baseline_0b report \
        --workdir "$WORKDIR" --arms "$ARMS" )
fi
record_load done
echo "m1-baseline-run: mode $MODE, artefacts in $WORKDIR"
