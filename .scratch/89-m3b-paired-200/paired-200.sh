#!/usr/bin/env bash
# 89 — M3 clause (b): the paired 200 rps block, declared vs indexed, behind the quiet-window gate.
#
#   bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 30   # the pair, if the window opens
#   bash .scratch/89-m3b-paired-200/paired-200.sh --smoke         # plumbing only, never evidence
#
# The gate (root's grant, `docs/roadmap.md` decision 7): /proc/loadavg under 2.0 on twelve
# consecutive 5-second readings (one quiet minute). Every poll is written to
# `logs/paired-200-gate.out`, so a run that never fired still carries the series that shows why.
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
say() { echo "$*" | tee -a "$L/paired-200.out"; }

# The gate, as `AGENTS.md` §The quiet window states it: an arm above the trigger is **refused**, not
# merely logged, and the check runs again before every arm because the box can be quiet for one minute
# and pick up foreign load again before the next one. The 2026-10-27 run is the failing example that
# put this here: the gate opened once (0.87), the first pair ran inside it (0.88/1.63), and arms 2-5
# started at 2.15-6.75 while the script never looked again — so nine blocks were spent on a reading
# nobody may cite (`runs/invalid-2026-10-27/`).
DEADLINE=$(( $(date +%s) + WAIT_MIN * 60 ))
await_quiet() {
    local phase=$1 quiet=0 l1
    say "== gate ($phase): loadavg < 2.0 on twelve consecutive 5-second readings, deadline ${WAIT_MIN} min from the run's start =="
    while :; do
        read -r l1 _ < /proc/loadavg
        echo "$(date -Is) phase=$phase loadavg1=$l1 quiet=${quiet}/12" >> "$L/paired-200-gate.out"
        if awk -v l="$l1" 'BEGIN { exit !(l < 2.0) }'; then
            quiet=$(( quiet + 1 ))
        else
            quiet=0
        fi
        [ "$quiet" -ge 12 ] && { say "GATE MET ($phase) at $(date -Is): loadavg $(cat /proc/loadavg)"; return 0; }
        if [ "$(date +%s)" -ge "$DEADLINE" ]; then
            say "GATE UNMET ($phase): loadavg never held under 2.0 for one minute before the deadline (series in logs/paired-200-gate.out)"
            return 1
        fi
        sleep 5
    done
}

# A run whose trigger was lost mid-way is not partial evidence, it is no evidence: the partial blocks
# are moved out of the scorer's glob and marked, so the next reader cannot put them together with a
# later run's blocks and call the mixture a pair.
refuse_run() {
    local phase=$1 stamp inv f
    stamp=$(date +%Y-%m-%dT%H%M%S)
    inv=$R/invalid-$stamp
    mkdir -p "$inv"
    for f in "$R"/p*-*.out "$R"/p*-*.err; do
        [ -e "$f" ] || continue
        { echo "# INVALID - do not cite: run abandoned $phase at $(date -Is), arm start loadavg above the 2.0 trigger"; cat "$f"; } > "$inv/$(basename "$f")"
        rm -f "$f"
    done
    rm -rf "$R"/live-*
    gzip -9 "$inv"/*.err 2>/dev/null || true
    say "REFUSED: the trigger was lost $phase; the blocks taken so far were moved to ${inv#"$R"/} and marked INVALID, and nothing in this run is a verdict"
}

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
# the snapshot's own record of what it is; overridable because the regenerated instance's record is
# kept beside this ticket (the committed one is the M1 run's, and stays the M1 run's)
SNAP_RECORD=${SNAP_RECORD:-"$(dirname "$SNAP")/$(basename "$SNAP").json"}
SNAP_BYTES=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['bytes'])" "$SNAP_RECORD")
SNAP_SHA=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['sha256'])" "$SNAP_RECORD")
say "# preflight ok: snapshot $(du -sh "$SNAP" | cut -f1) ($SNAP_BYTES B per its record), trace sha $(sha256sum "$TRACE" | cut -c1-16)"

if [ "$SMOKE" = 1 ]; then
    # Plumbing only: a 100-request trace at 50 rps. The outputs are named `smoke-*`, which the
    # scorer does not match, so a smoke block cannot be read as evidence.
    head -100 "$TRACE" > "$R/smoke-trace.jsonl"
    TRACE="$R/smoke-trace.jsonl"; RATE=50; PAIRS=1
    say "== SMOKE: gate skipped, 1 pair at ${RATE} rps on $(wc -l < "$TRACE") requests — NOT evidence =="
else
    await_quiet "before the run" || exit 4
fi

# --- the blocks
for p in $(seq 1 "$PAIRS"); do
    if [ $(( p % 2 )) = 1 ]; then order="declared indexed"; else order="indexed declared"; fi
    for variant in $order; do
        if [ "$SMOKE" = 0 ]; then
            await_quiet "before p$p $variant" || { refuse_run "before p$p $variant"; exit 4; }
        fi
        inst=$R/live-$variant
        rm -rf "$inst"
        cp -a --reflink=auto "$SNAP" "$inst"
        # The restore is verified, not assumed: the copy's tree digest and byte total against the
        # snapshot record (tree_sha256 is the harness's own function, so the units agree).
        read -r got_sha got_bytes _ < <(uv run --project harness python -c \
            "import sys; from pathlib import Path; sys.path.insert(0, 'harness'); from flashdb_harness.baseline_0b import tree_sha256; d,t,f=tree_sha256(Path(sys.argv[1])); print(d,t,f)" \
            "$inst" 2>/dev/null)
        if [ "$got_sha" != "$SNAP_SHA" ] || [ "$got_bytes" != "$SNAP_BYTES" ]; then
            say "REFUSED: restore of $inst digests ${got_sha:0:16}/${got_bytes} B, the snapshot record says ${SNAP_SHA:0:16}/$SNAP_BYTES B"
            exit 5
        fi
        # The record's own argv (`baseline_0b.flashdb_load_command`), plus nothing: the one
        # difference between the arms is `--policy-file`.
        args=("$BIN" "$inst" "$TRACE" --rate-rps "$RATE" --window-ms 1000 --metrics-json
              --group-requests 64 --lag-ceiling-us 10000)
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
uv run --project harness python .scratch/89-m3b-paired-200/score-pair.py "$R" 2>&1 | tee -a "$L/paired-200.out"
rc=${PIPESTATUS[0]}
say "== loadavg at end: $(cat /proc/loadavg)"
exit "$rc"
