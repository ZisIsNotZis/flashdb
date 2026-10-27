#!/usr/bin/env bash
# 92 — barrier queue-wait instrument and the WAL preallocation candidate on the
# M1 rung-200 trace. Work-per-request counter only: no throughput number is
# claimed from this replay (docs/0_ROADMAP.md WP-14).
#
# Each block is a fresh copy of the frozen M1 flashdb-declared snapshot, replayed
# at 200 rps for 20 s (4,000 requests, 20 x 1 s windows), under the same
# `MemoryMax=128M` scope the M1 arm used. The arms differ only in
# `--wal-prealloc-bytes` (16 MiB, the postgres segment size behind the WP-6
# mechanism candidate). Blocks are taken under whatever foreign load the box
# carries: the ticket states the stall is a work-per-request count, so load does
# not invalidate the attribution; the first line of every log states loadavg.
set -euo pipefail

REPO=$(cd "$(dirname "$0")/../.." && pwd)
TICKET=$REPO/.scratch/92-barrier-stall
SNAPSHOT=${SNAPSHOT:-/home/z/vibe/flashdb/.scratch/82-controlled-m1/runs/snapshot/flashdb-declared}
TRACE=${TRACE:-/home/z/vibe/flashdb/.scratch/82-controlled-m1/runs/b2/rung-200/trace.jsonl}
BIN=${BIN:-$REPO/target/release/flashdb-load}
BLOCKS=${BLOCKS:-5}
RATE=${RATE:-200}
WINDOW_MS=${WINDOW_MS:-1000}
GROUP_REQUESTS=${GROUP_REQUESTS:-64}
CAP_MIB=${CAP_MIB:-128}
PREALLOC=${PREALLOC:-16777216}
ARMS=${ARMS:-"off on"}
SUMMARY=${SUMMARY:-summary.out}
# LOG_TAG suffixes per-block logs, so a second arm set (e.g. no memory cap)
# does not overwrite the first. Empty keeps the ticket's canonical names.
LOG_TAG=${LOG_TAG:-}

mkdir -p "$TICKET/logs" "$TICKET/runs"
[ -x "$BIN" ] || { echo "missing $BIN; build with: cargo build --release --bin flashdb-load" >&2; exit 1; }
[ -e "$SNAPSHOT" ] || { echo "missing snapshot $SNAPSHOT" >&2; exit 1; }
[ -e "$TRACE" ] || { echo "missing trace $TRACE" >&2; exit 1; }

run_out=$TICKET/logs/run.out
: > "$run_out"
: > "$TICKET/logs/$SUMMARY"
echo "loadavg $(cat /proc/loadavg)" | tee -a "$run_out"
echo "snapshot=$SNAPSHOT" | tee -a "$run_out"
echo "trace=$TRACE" | tee -a "$run_out"
echo "rate=$RATE blocks=$BLOCKS window_ms=$WINDOW_MS group_requests=$GROUP_REQUESTS cap_mib=$CAP_MIB prealloc=$PREALLOC" | tee -a "$run_out"

for arm in $ARMS; do
    case "$arm" in
        off) prealloc_flag=() ;;
        on)  prealloc_flag=(--wal-prealloc-bytes "$PREALLOC") ;;
        *) echo "unknown arm $arm (off|on)" >&2; exit 1 ;;
    esac
    for b in $(seq 1 "$BLOCKS"); do
        inst=$TICKET/runs/instance-$arm-b$b$LOG_TAG
        log=$TICKET/logs/replay-$arm-b$b$LOG_TAG.out
        rm -rf "$inst"
        echo "loadavg $(cat /proc/loadavg)" > "$log"
        echo "+ block arm=$arm b=$b copy+run $(date -Is)" | tee -a "$run_out"
        cp -a --reflink=auto "$SNAPSHOT" "$inst"
        driver="'$BIN' '$inst' '$TRACE' --rate-rps $RATE --window-ms $WINDOW_MS \
                --metrics-json --group-requests $GROUP_REQUESTS --lag-ceiling-us 10000 ${prealloc_flag[*]:-}"
        set +e
        if [ "$CAP_MIB" = 0 ]; then
            # Control: the same replay with no cgroup memory cap (the M1 arm ran
            # under MemoryMax=128M; this isolates whether the cap is the cause).
            bash -c "$driver" >> "$log" 2>&1
        else
            systemd-run --user --scope -q -p "MemoryMax=${CAP_MIB}M" -p MemorySwapMax=0 \
                bash -c "exec $driver" >> "$log" 2>&1
        fi
        rc=$?
        set -e
        echo "rc=$rc" >> "$log"
        wal=$(find "$inst" -maxdepth 1 -name 'data-*.wal' -print -quit)
        if [ -n "$wal" ]; then
            stat -c 'wal_after len=%s blocks=%b' "$wal" >> "$log"
        fi
        tail -1 "$log" | tee -a "$run_out"
        python3 "$TICKET/summarize.py" --arm "$arm" --block "$b" --log "$log" \
            >> "$TICKET/logs/$SUMMARY"
        rm -rf "$inst"
    done
done

echo "+ done $(date -Is); summary $TICKET/logs/$SUMMARY" | tee -a "$run_out"
cat "$TICKET/logs/$SUMMARY"
