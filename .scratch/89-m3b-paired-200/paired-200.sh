#!/usr/bin/env bash
# 89 — M3 clause (b): the paired 200 rps block, declared vs indexed, behind the quiet-window gate.
#
#   bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 30   # the pair, if the window opens
#   bash .scratch/89-m3b-paired-200/paired-200.sh --resume        # continue after the pairs already kept
#   bash .scratch/89-m3b-paired-200/paired-200.sh --smoke         # plumbing only, never evidence
#   --simulate is the fixture hook used by fixtures/test-pair-harvest.sh; never evidence.
#
# The gate (root's grant, `docs/0_ROADMAP.md` decision 7): loadavg under 2.0 on twelve consecutive
# 5-second readings (one quiet minute). Every poll is written to `logs/paired-200-gate.out`, so a
# run that never fired still carries the series that shows why. Nothing is run above the gate;
# `--smoke` skips it and writes `smoke-*` files the scorer does not read, so a smoke block cannot be
# mistaken for the pair.
#
# The pair: the same frozen trace, the same snapshot, one process at a time in a 128 MiB scope, five
# blocks with the arm order rotated (odd blocks declared first, even blocks indexed first). The one
# thing that changes between arms is `--policy-file`, which is the whole claim.
#
# WP-17 — the harvest rule. A rare quiet window is not wasted when its trigger is lost: a pair whose
# two arms both started under 2.0, recorded `errors=0` and verified their restore is a complete block
# of evidence, and it stays in `runs/` when a later arm is refused. Only the incomplete pair's files
# are moved to `runs/invalid-<stamp>/` with the `# INVALID - do not cite:` marker. A normal run
# resumes by default at the next pair number after the valid ones; `--resume` states that explicitly
# and `--fresh` refuses when valid pairs already exist, so no pair is ever overwritten. The
# pre-registered unit is still five valid pairs — `score-pair.py` returns 3 with `partial: N/5` for
# anything less.
#
# Exit: 0 met, 1 falsified, 3 cannot decide/preflight, 4 the trigger was lost / gate never opened,
# 5 a restore did not verify, 6 an arm completed but is not admissible (errors>0 or its start
# loadavg was already above the trigger).
set -uo pipefail
cd "$(dirname "$0")/../.."
L=${LOGS_DIR:-.scratch/89-m3b-paired-200/logs}
R=${RUNS_DIR:-.scratch/89-m3b-paired-200/runs}
SNAP=${SNAP:-.scratch/82-controlled-m1/runs/snapshot/flashdb-declared}
TRACE=${TRACE:-.scratch/82-controlled-m1/runs/b2/rung-200/trace.jsonl}
POL=${POL:-.scratch/89-m3b-paired-200/policy/indexed.policy.json}
BIN=${BIN:-target/release/flashdb-load}
SELF=.scratch/89-m3b-paired-200/score-pair.py
WAIT_MIN=0
PAIRS=5
RATE=200
SMOKE=0
SIMULATE=0
FRESH=0
LOADAVG_FILE=${LOADAVG_FILE:-/proc/loadavg}
GATE_POLL_SECONDS=${GATE_POLL_SECONDS:-5}
GATE_DEADLINE_SECONDS=${GATE_DEADLINE_SECONDS:-}
GATE_QUIET_READINGS=${GATE_QUIET_READINGS:-}
FAKE_ARM=${FAKE_ARM:-}
while [ $# -gt 0 ]; do
    case "$1" in
        --wait-min) WAIT_MIN=$2; shift 2 ;;
        --pairs) PAIRS=$2; shift 2 ;;
        --resume) shift ;;                  # the default; accepted so an invocation can say so
        --fresh) FRESH=1; shift ;;
        --simulate) SIMULATE=1; shift ;;    # fixture test only: fake box, never evidence
        --smoke) SMOKE=1; shift ;;
        -h|--help) sed -n '2,29p' "$0"; exit 0 ;;
        *) echo "unknown argument $1" >&2; exit 2 ;;
    esac
done
# The gate's time base and its reading source are fixed on the real path. The overrides exist only
# under `--simulate` so a fixture test cannot double as a way to shorten or redirect the real gate.
if [ "$SIMULATE" = 0 ]; then
    LOADAVG_FILE=/proc/loadavg
    GATE_POLL_SECONDS=5
    GATE_DEADLINE_SECONDS=""
    GATE_QUIET_READINGS=""
fi
mkdir -p "$L" "$R"
say() { echo "$*" | tee -a "$L/paired-200.out"; }
py() { python3 "$SELF" "$@"; }

# Per-arm admissibility, one parser for the shell, the manifest and the scorer.
pair_valid() { py --pair-valid "$R/p$1-declared.out" "$R/p$1-indexed.out" 2>/dev/null; }
pair_present() { compgen -G "$R/p$1-*.out" >/dev/null; }
arm_facts() { py --arm-facts "$1"; }

# The gate, as `AGENTS.md` §The quiet window states it: an arm above the trigger is **refused**, not
# merely logged, and the check runs again before every arm because the box can be quiet for one minute
# and pick up foreign load again before the next one. The 2026-10-27 run is the failing example that
# put this here: the gate opened once (0.87), the first pair ran inside it (0.88/1.63), and arms 2-5
# started at 2.15-6.75 while the script never looked again — so nine blocks were spent on a reading
# nobody may cite (`runs/invalid-2026-10-27/`).
if [ -n "$GATE_DEADLINE_SECONDS" ]; then GATE_SECONDS=$GATE_DEADLINE_SECONDS; else GATE_SECONDS=$(( WAIT_MIN * 60 )); fi
QUIET_READINGS=${GATE_QUIET_READINGS:-12}
DEADLINE_NS=$(( $(date +%s%N) + GATE_SECONDS * 1000000000 ))
await_quiet() {
    local phase=$1 quiet=0 l1
    say "== gate ($phase): loadavg < 2.0 on ${QUIET_READINGS} consecutive ${GATE_POLL_SECONDS}s readings, deadline ${GATE_SECONDS}s from the run's start =="
    while :; do
        read -r l1 _ < "$LOADAVG_FILE"
        echo "$(date -Is) phase=$phase loadavg1=$l1 quiet=${quiet}/${QUIET_READINGS} source=$LOADAVG_FILE" >> "$L/paired-200-gate.out"
        if awk -v l="$l1" 'BEGIN { exit !(l < 2.0) }'; then
            quiet=$(( quiet + 1 ))
        else
            quiet=0
        fi
        [ "$quiet" -ge "$QUIET_READINGS" ] && { say "GATE MET ($phase) at $(date -Is): loadavg $(head -c 80 "$LOADAVG_FILE")"; return 0; }
        if [ "$(date +%s%N)" -ge "$DEADLINE_NS" ]; then
            say "GATE UNMET ($phase): loadavg never held under 2.0 for the required quiet window before the deadline (series in logs/paired-200-gate.out)"
            return 1
        fi
        sleep "$GATE_POLL_SECONDS"
    done
}

# A pair that is not complete valid evidence is moved out of the scorer's glob and marked. WP-17:
# only the pairs that fail the rule move — the completed pairs taken earlier in the same run stay in
# `runs/`, because losing the trigger later does not un-take them.
refuse_run() {
    local phase=$1 stamp inv f base p moved=0
    stamp=$(date +%Y-%m-%dT%H%M%S)
    inv=$R/invalid-$stamp
    mkdir -p "$inv"
    for f in "$R"/p*-*.out "$R"/p*-*.err; do
        [ -e "$f" ] || continue
        base=$(basename "$f")
        p=${base#p}; p=${p%%-*}
        pair_valid "$p" && continue
        { echo "# INVALID - do not cite: run abandoned $phase at $(date -Is); this pair is incomplete or failed the trigger/errors/restore rule"; cat "$f"; } > "$inv/$base"
        rm -f "$f"
        moved=$(( moved + 1 ))
    done
    rm -rf "$R"/live-*
    gzip -9 "$inv"/*.err 2>/dev/null || true
    py --manifest "$R" >/dev/null 2>&1 || true
    say "REFUSED: $phase; $moved file(s) from the incomplete pair moved to ${inv#"$R"/} and marked INVALID; completed valid pairs stay in runs/"
}

say "== paired 200 rps, started $(date -Is) =="
say "# loadavg at start: $(head -c 80 "$LOADAVG_FILE")"
say "# paths modified in the tree (for the record; another agent mid-write is a judgement, not a file count): $(git status --porcelain | wc -l)"

# --- resume: a pair that already passes the rule is never run again or overwritten. START_PAIR is
# --- 1 + the last contiguous valid pair; stale files at or above it (a crash mid-pair, or a refusal
# --- from an earlier invocation) are quarantined, not scored.
START_PAIR=1
if [ "$SMOKE" = 0 ]; then
    max_p=0
    for f in "$R"/p*-*.out; do
        [ -e "$f" ] || continue
        base=$(basename "$f"); p=${base#p}; p=${p%%-*}
        [ "$p" -gt "$max_p" ] 2>/dev/null && max_p=$p
    done
    valid_n=0
    for p in $(seq 1 "$max_p"); do
        pair_present "$p" || continue
        if pair_valid "$p"; then
            if [ "$p" -ne $(( valid_n + 1 )) ]; then
                say "REFUSED: pair $p is valid but pair $(( valid_n + 1 )) is missing or invalid; a non-contiguous series cannot decide the clause"
                exit 3
            fi
            valid_n=$(( valid_n + 1 ))
        fi
    done
    START_PAIR=$(( valid_n + 1 ))
    if [ "$FRESH" = 1 ] && { [ "$max_p" -gt 0 ] || pair_present 1; }; then
        say "REFUSED: --fresh with existing pair files in $R; use --resume (the default) so no valid pair is overwritten"
        exit 3
    fi
    if [ "$max_p" -gt 0 ]; then
        for p in $(seq 1 "$max_p"); do
            if pair_present "$p" && ! pair_valid "$p"; then
                refuse_run "on resume (stale partial pair p$p from an earlier invocation)"
            fi
        done
        say "resuming after $valid_n valid pair(s); next pair is p$START_PAIR"
    fi
    py --manifest "$R" >/dev/null 2>&1 || true
fi

# --- preflight: everything the run needs, checked before the gate, so a missing piece cannot eat the
# --- quiet window.
if [ "$SIMULATE" = 0 ]; then
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
else
    [ -n "$FAKE_ARM" ] && [ -e "$FAKE_ARM" ] || { say "REFUSED: --simulate needs FAKE_ARM=<fixture script>; nothing was run"; exit 3; }
    [ -e "$SNAP" ] && [ -e "$TRACE" ] && [ -e "$POL" ] && [ -e "$BIN" ] \
        || { say "REFUSED: --simulate preflight missing a fixture path; nothing was run"; exit 3; }
    say "== SIMULATE: fixture arms, fake loadavg from $LOADAVG_FILE — NOT evidence =="
    SNAP_BYTES=fixture; SNAP_SHA=fixture
fi

if [ "$SMOKE" = 1 ]; then
    # Plumbing only: a 100-request trace at 50 rps. The outputs are named `smoke-*`, which the
    # scorer does not match, so a smoke block cannot be read as evidence.
    head -100 "$TRACE" > "$R/smoke-trace.jsonl"
    TRACE="$R/smoke-trace.jsonl"; RATE=50; PAIRS=1
    say "== SMOKE: gate skipped, 1 pair at ${RATE} rps on $(wc -l < "$TRACE") requests — NOT evidence =="
elif [ "$START_PAIR" -le "$PAIRS" ]; then
    await_quiet "before the run" || exit 4
fi

# --- the blocks
for p in $(seq "$START_PAIR" "$PAIRS"); do
    if [ $(( p % 2 )) = 1 ]; then order="declared indexed"; else order="indexed declared"; fi
    for variant in $order; do
        if [ "$SMOKE" = 0 ]; then
            await_quiet "before p$p $variant" || { refuse_run "before p$p $variant (trigger lost)"; exit 4; }
        fi
        inst=$R/live-$variant
        if [ "$SIMULATE" = 0 ]; then
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
            restore_line="# restore: verified sha256=$got_sha bytes=$got_bytes"
        else
            restore_line="# restore: verified (simulate: fixture restore, no digest)"
        fi
        prefix=p; [ "$SMOKE" = 1 ] && prefix=smoke-p
        out=$R/$prefix$p-$variant
        start_l1=$(cut -d' ' -f1 "$LOADAVG_FILE")
        # The header is written before the arm runs, so the arm's file states exactly the reading it
        # started at. If that reading is already above the trigger the arm is refused, not run.
        {
            echo "# paired-200 arm=$variant pair=$p rev=$(git rev-parse --short HEAD 2>/dev/null || echo fixture)"
            echo "$restore_line"
            echo "# loadavg at start: $(head -c 80 "$LOADAVG_FILE")  ($(date -Is))"
        } > "$out.out"
        if [ "$SMOKE" = 0 ] && ! awk -v l="$start_l1" 'BEGIN { exit !(l < 2.0) }'; then
            say "REFUSED: p$p $variant start loadavg $start_l1 is not under the 2.0 trigger"
            refuse_run "before p$p $variant (start loadavg $start_l1)"
            exit 4
        fi
        # The record's own argv (`baseline_0b.flashdb_load_command`), plus nothing: the one
        # difference between the arms is `--policy-file`.
        args=("$BIN" "$inst" "$TRACE" --rate-rps "$RATE" --window-ms 1000 --metrics-json
              --group-requests 64 --lag-ceiling-us 10000)
        [ "$variant" = indexed ] && args+=(--policy-file "$POL")
        if [ "$SIMULATE" = 0 ]; then
            systemd-run --user --scope -q -p MemoryMax=128M -p MemorySwapMax=0 \
                bash -c "$(printf '%q ' "${args[@]}")" >> "$out.out" 2> "$out.err"
        else
            bash "$FAKE_ARM" "$variant" "$p" "$out.out" "$LOADAVG_FILE" >> "$out.out" 2> "$out.err"
        fi
        echo "# loadavg at end: $(head -c 80 "$LOADAVG_FILE")  ($(date -Is))" >> "$out.out"
        # The arm's own header is the authority, not the gate's last poll: a spike between the poll,
        # the restore and the first request would otherwise be recorded only in the gate series.
        if [ "$SMOKE" = 0 ]; then
            facts=$(arm_facts "$out.out")
            a_start=$(printf '%s' "$facts" | python3 -c 'import json,sys; print(json.load(sys.stdin)["start_loadavg"])')
            a_errors=$(printf '%s' "$facts" | python3 -c 'import json,sys; print(json.load(sys.stdin)["errors"])')
            if [ "$a_start" = None ] || ! awk -v l="$a_start" 'BEGIN { exit !(l < 2.0) }'; then
                say "REFUSED: p$p $variant recorded start loadavg $a_start, at or above the 2.0 trigger"
                refuse_run "p$p $variant (recorded start loadavg $a_start)"
                exit 4
            fi
            if [ "$a_errors" != 0 ]; then
                say "REFUSED: p$p $variant recorded errors=$a_errors; a pair with errors is not evidence"
                refuse_run "p$p $variant (errors=$a_errors)"
                exit 6
            fi
        fi
        say "block p$p $variant done: loadavg $(cut -d' ' -f1-3 "$LOADAVG_FILE")"
    done
    if [ "$SMOKE" = 0 ]; then
        if pair_valid "$p"; then
            say "pair p$p complete and valid (both arms under 2.0, errors=0, restore verified)"
        else
            say "REFUSED: pair p$p is not admissible after both arms"
            refuse_run "after p$p (pair failed the admissibility rule)"
            exit 6
        fi
        py --manifest "$R" >/dev/null 2>&1 || true
    fi
done
rm -rf "$R"/live-*
say "== blocks done $(date -Is); scoring =="
if [ "$SMOKE" = 1 ]; then
    say "smoke: not scored — a smoke block is plumbing, and its file names are outside the scorer's pattern"
    exit 0
fi
py --manifest "$R" | tee -a "$L/paired-200.out"
if [ "$SIMULATE" = 1 ]; then
    python3 "$SELF" "$R" 2>&1 | tee -a "$L/paired-200.out"
else
    uv run --project harness python "$SELF" "$R" 2>&1 | tee -a "$L/paired-200.out"
fi
rc=${PIPESTATUS[0]}
say "== loadavg at end: $(head -c 80 "$LOADAVG_FILE")"
exit "$rc"
