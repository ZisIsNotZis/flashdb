#!/usr/bin/env bash
# 89 — M3 clause (b): the paired 200 rps block, declared vs indexed, behind the quiet-window gate.
#
#   bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 30   # the pair, if the window opens
#   bash .scratch/89-m3b-paired-200/paired-200.sh --smoke         # plumbing only, never evidence
#
# The gate (root's grant, `docs/roadmap.md` decision 7): /proc/loadavg under 2.0 on twelve
# consecutive 5-second readings (one quiet minute). Every poll is written to
# `logs/paired-200-gate.log`, so a run that never fired still carries the series that shows why.
# Nothing is run above the gate; `--smoke` skips it and writes `smoke-*` files the scorer does not
# read, so a smoke block cannot be mistaken for the pair.
#
# The pair: the same frozen trace, the same snapshot, one process at a time in a 128 MiB scope, five
# blocks with the arm order rotated (odd blocks declared first, even blocks indexed first). The one
# thing that changes between arms is `--policy-file`, which is the whole claim.
set -uo pipefail
cd "$(dirname "$0")/../.."
L=.scratch/89-m3b-paired-200/logs
R=.scratch/89-m3b-paired-200/runs
SNAP=.scratch/82-controlled-m1/runs/snapshot/flashdb-declared
TRACE=.scratch/82-controlled-m1/runs/b2/rung-200/trace.jsonl
POL=.scratch/89-m3b-paired-200/policy/indexed.policy.json
BIN=target/release/flashdb-load
WAIT_MIN=0
PAIRS=5
RATE=200
SMOKE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --wait-min) WAIT_MIN=$2; shift 2 ;;
        --pairs) PAIRS=$2; shift 2 ;;
        --smoke) SMOKE=1; shift ;;
        -h|--help) sed -n '2,16p' "$0"; exit 0 ;;
        *) echo "unknown argument $1" >&2; exit 2 ;;
    esac
done
mkdir -p "$L" "$R"
say() { echo "$*" | tee -a "$L/paired-200.log"; }

say "== paired 200 rps, started $(date -Is) =="
say "# loadavg at start: $(cat /proc/loadavg)"
say "# paths modified in the tree (for the record; another agent mid-write is a judgement, not a file count): $(git status --porcelain | wc -l)"

# --- preflight: everything the run needs, checked before the gate, so a missing piece cannot eat the
# --- quiet window.
missing=0
for p in "$SNAP" "$TRACE" "$POL" "$BIN"; do
    [ -e "$p" ] || { say "MISSING: $p"; missing=1; }
done
[ "$missing" = 0 ] || { say "REFUSED: preflight failed; nothing was run"; exit 3; }
systemd-run --user --scope -q -p MemoryMax=128M -p MemorySwapMax=0 true 2>/dev/null \
    || { say "REFUSED: no user systemd scope, so the 128 MiB cap cannot be enforced; nothing was run"; exit 3; }
say "# preflight ok: snapshot $(du -sh "$SNAP" | cut -f1), trace sha $(sha256sum "$TRACE" | cut -c1-16)"

if [ "$SMOKE" = 1 ]; then
    # Plumbing only: a 100-request trace at 50 rps. The outputs are named `smoke-*`, which the
    # scorer does not match, so a smoke block cannot be read as evidence.
    head -100 "$TRACE" > "$R/smoke-trace.jsonl"
    TRACE="$R/smoke-trace.jsonl"; RATE=50; PAIRS=1
    say "== SMOKE: gate skipped, 1 pair at ${RATE} rps on $(wc -l < "$TRACE") requests — NOT evidence =="
else
    # --- the gate
    say "== gate: loadavg < 2.0 on twelve consecutive 5-second readings (--wait-min $WAIT_MIN) =="
    deadline=$(( $(date +%s) + WAIT_MIN * 60 ))
    quiet=0
    while :; do
        read -r l1 _ < /proc/loadavg
        echo "$(date -Is) loadavg1=$l1 quiet=${quiet}/12" >> "$L/paired-200-gate.log"
        if awk -v l="$l1" 'BEGIN { exit !(l < 2.0) }'; then
            quiet=$(( quiet + 1 ))
        else
            quiet=0
        fi
        [ "$quiet" -ge 12 ] && break
        if [ "$(date +%s)" -ge "$deadline" ]; then
            say "GATE UNMET after ${WAIT_MIN} min: loadavg never held under 2.0 for one minute (series in logs/paired-200-gate.log)"
            exit 4
        fi
        sleep 5
    done
    say "GATE MET at $(date -Is): loadavg $(cat /proc/loadavg)"
fi

# --- the blocks
for p in $(seq 1 "$PAIRS"); do
    if [ $(( p % 2 )) = 1 ]; then order="declared indexed"; else order="indexed declared"; fi
    for variant in $order; do
        inst=$R/live-$variant
        rm -rf "$inst"
        cp -a --reflink=auto "$SNAP" "$inst"
        args=("$BIN" "$inst" "$TRACE" --rate-rps "$RATE" --window-ms 1000 --metrics-json
              --group-requests 64 --lag-ceiling-us 10000 --peak-report)
        [ "$variant" = indexed ] && args+=(--policy-file "$POL")
        prefix=p; [ "$SMOKE" = 1 ] && prefix=smoke-p
        out=$R/$prefix$p-$variant
        {
            echo "# paired-200 arm=$variant pair=$p rev=$(git rev-parse --short HEAD)"
            echo "# loadavg at start: $(cat /proc/loadavg)  ($(date -Is))"
        } > "$out.out"
        systemd-run --user --scope -q -p MemoryMax=128M -p MemorySwapMax=0 \
            bash -c "$(printf '%q ' "${args[@]}")" >> "$out.out" 2> "$out.err"
        echo "# loadavg at end: $(cat /proc/loadavg)  ($(date -Is))" >> "$out.out"
        say "block p$p $variant done: loadavg $(cut -d' ' -f1-3 /proc/loadavg)"
    done
done
rm -rf "$R"/live-*
say "== blocks done $(date -Is); scoring =="
if [ "$SMOKE" = 1 ]; then
    say "smoke: not scored — a smoke block is plumbing, and its file names are outside the scorer's pattern"
    exit 0
fi
uv run --project harness python .scratch/89-m3b-paired-200/score-pair.py "$R" 2>&1 | tee -a "$L/paired-200.log"
rc=${PIPESTATUS[0]}
say "== loadavg at end: $(cat /proc/loadavg)"
exit "$rc"
