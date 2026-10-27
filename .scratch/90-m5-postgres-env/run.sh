#!/usr/bin/env bash
# WP-15: the postgres arm's server is the M1 user-space 18.6 cluster, not the vanished container.
#
#   bash .scratch/90-m5-postgres-env/run.sh
#
# This reproduces, in order:
#   * `baseline-env.sh check` on the running 18.6 cluster (version and the M1 settings block);
#   * the same check refusing a required version and a setting that differ (the negative cases);
#   * one committed `m1-baseline-run.sh` command that starts the server through baseline-env with no
#     Docker and runs the arm end to end at 18.6;
#   * the controlled protocol's two steps, `--mode instance` (build + snapshot) then `--mode arm`
#     (digest-verified restore with the server stopped, start, check, serve, oracle).
#
# It is a CHAIN CHECK, not a rate: the box is under foreign load (loadavg is in every log and in
# `loadavg.jsonl`), so no throughput number is claimed. Logs land in logs/, run records in runs/;
# the large snapshot is regenerable and gitignored.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
HERE=$REPO/.scratch/90-m5-postgres-env
LOG=$HERE/logs
WORK=$HERE/runs/chain-18.6
FULL=$HERE/runs/full-18.6
export M1_PG_ROOT=${M1_PG_ROOT:-/var/tmp/m1-pg}
export M1_PG_PORT=${M1_PG_PORT:-55432}
mkdir -p "$LOG"

if [ ! -x "$REPO/target/release/flashdb-load" ]; then
    echo "+ cargo build --release (aliyun crates mirror; crates.io is unreachable)" >&2
    ( cd "$REPO" && cargo --config 'source.crates-io.replace-with="aliyun"' \
        --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
        build --release --bin flashdb-scale --bin flashdb-load --bin flashdb-replay )
fi

# Run one command into its log with the loadavg it started at; rc is recorded, not ignored.
run_log() {
    local name=$1; shift
    local rc=0
    {
        echo "# start $(date -Is) loadavg $(cut -d' ' -f1-3 /proc/loadavg), root $M1_PG_ROOT:$M1_PG_PORT"
        "$@" && rc=0 || rc=$?
        echo "rc=$rc"
    } > "$LOG/$name.out" 2>&1
    echo "$name: rc=$rc ($LOG/$name.out)"
}

# 1. The check on the running server; the version and every setting are read back.
M1_PG_ROOT=$M1_PG_ROOT M1_PG_PORT=$M1_PG_PORT bash "$REPO/scripts/baseline-env.sh" start
run_log check-18.6 env M1_PG_ROOT="$M1_PG_ROOT" M1_PG_PORT="$M1_PG_PORT" \
    bash "$REPO/scripts/baseline-env.sh" check
grep -q '^rc=0$' "$LOG/check-18.6.out" || { echo "check-18.6 did not pass" >&2; exit 1; }

# 2. The negatives: a required version and a setting that differ both exit non-zero and name the
#    difference. PGOPTIONS is used for the setting because it changes what the client sees without
#    changing the M1 record's configured value on disk.
run_log check-version-mismatch env M1_PG_ROOT="$M1_PG_ROOT" M1_PG_PORT="$M1_PG_PORT" \
    M1_PG_REQUIRE=16.15 bash "$REPO/scripts/baseline-env.sh" check
grep -q '^rc=1$' "$LOG/check-version-mismatch.out" || { echo "version mismatch did not refuse" >&2; exit 1; }
run_log check-setting-mismatch env PGOPTIONS='-c work_mem=8MB' M1_PG_ROOT="$M1_PG_ROOT" \
    M1_PG_PORT="$M1_PG_PORT" bash "$REPO/scripts/baseline-env.sh" check
grep -q '^rc=1$' "$LOG/check-setting-mismatch.out" || { echo "setting mismatch did not refuse" >&2; exit 1; }

# 3. One committed command: start through baseline-env, read the version/settings back, seed, serve,
#    oracle. No Docker anywhere in this path.
rm -rf "$FULL" "$WORK"
run_log arm-18.6-full bash "$REPO/scripts/m1-baseline-run.sh" --mode full --arms postgres \
    --requests 200 --rate 50 --skip-build --workdir "$FULL"
grep -q '^rc=0$' "$LOG/arm-18.6-full.out" || { echo "the full arm did not complete" >&2; exit 1; }

# 4. The controlled chain: prepare + one full run (which prepares the work directory), then build the
#    pre-load instance and snapshot it, then restore (digest verified, server stopped) and serve.
run_log chain-18.6-full bash "$REPO/scripts/m1-baseline-run.sh" --mode full --arms postgres \
    --requests 200 --rate 50 --skip-build --workdir "$WORK"
grep -q '^rc=0$' "$LOG/chain-18.6-full.out" || { echo "the chain's prepare/full step did not complete" >&2; exit 1; }
cp "$WORK/run-postgres.json" "$WORK/run-postgres-full.json"
run_log chain-18.6-instance bash "$REPO/scripts/m1-baseline-run.sh" --mode instance --arms postgres \
    --requests 200 --rate 50 --skip-build --workdir "$WORK" \
    --snapshot-dir "$WORK/snapshot" --instance-root "$WORK/live"
grep -q '^rc=0$' "$LOG/chain-18.6-instance.out" || { echo "the instance/snapshot step did not complete" >&2; exit 1; }
# 5. The runner refuses a required version or a setting it did not measure (the same check inside
#    the arm). These run before the successful arm so the final live records belong to it.
run_log chain-18.6-require-mismatch env M1_PG_REQUIRE=16.15 \
    bash "$REPO/scripts/m1-baseline-run.sh" --mode arm --arms postgres --requests 200 --rate 50 \
    --skip-build --workdir "$WORK" --snapshot-dir "$WORK/snapshot" --instance-root "$WORK/live"
grep -q '^rc=1$' "$LOG/chain-18.6-require-mismatch.out" || { echo "the arm accepted another version" >&2; exit 1; }
run_log chain-18.6-setting-mismatch env PGOPTIONS='-c work_mem=8MB' \
    bash "$REPO/scripts/m1-baseline-run.sh" --mode arm --arms postgres --requests 200 --rate 50 \
    --skip-build --workdir "$WORK" --snapshot-dir "$WORK/snapshot" --instance-root "$WORK/live"
grep -q '^rc=1$' "$LOG/chain-18.6-setting-mismatch.out" || { echo "the arm accepted another setting" >&2; exit 1; }

# 6. The matching restored arm: stop, restore (digest verified), start, check, serve, oracle.
run_log chain-18.6-arm-restored bash "$REPO/scripts/m1-baseline-run.sh" --mode arm --arms postgres \
    --requests 200 --rate 50 --skip-build --workdir "$WORK" \
    --snapshot-dir "$WORK/snapshot" --instance-root "$WORK/live"
grep -q '^rc=0$' "$LOG/chain-18.6-arm-restored.out" || { echo "the restored arm did not complete" >&2; exit 1; }

M1_PG_ROOT=$M1_PG_ROOT M1_PG_PORT=$M1_PG_PORT bash "$REPO/scripts/baseline-env.sh" status
echo "run.sh: chain check complete; logs in $LOG, records in $WORK"
