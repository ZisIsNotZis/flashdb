#!/usr/bin/env bash
# TEST FIXTURE ONLY — exercises WP-17's harvest rule without a real box or a real pair.
#
#   bash .scratch/89-m3b-paired-200/fixtures/test-pair-harvest.sh
#
# What it proves, all against fake `pN-*.out` files in the real driver format:
#   a. a trigger loss after two completed pairs keeps p1/p2 and moves ONLY the incomplete p3 out;
#   b. a second invocation resumes at p3, never overwriting p1/p2, and reaches five valid pairs;
#   c. the scorer refuses a verdict below five valid pairs with `partial: N/5` (exit 3);
#   d. at five valid pairs it returns 0 (met) or 1 (falsified);
#   e. a start loadavg >= 2.0 or errors > 0 excludes the pair and names the arm.
# No engine code, no snapshot, no throughput claim.
set -uo pipefail
cd "$(dirname "$0")/../../.."
TICKET=.scratch/89-m3b-paired-200
FIX=$TICKET/fixtures
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
fails=0

record() { # desc expected-rc actual-rc
    if [ "$3" -eq "$2" ]; then
        echo "PASS: $1 (exit $3)"
    else
        echo "FAIL: $1 (exit $3, wanted $2)"
        fails=$(( fails + 1 ))
    fi
}
contains() { # desc needle haystack
    if grep -qF -- "$2" <<< "$3"; then
        echo "PASS: $1"
    else
        echo "FAIL: $1 — output did not contain: $2"
        fails=$(( fails + 1 ))
    fi
}
check() { # desc command...
    local desc=$1; shift
    if "$@"; then echo "PASS: $desc"; else echo "FAIL: $desc"; fails=$(( fails + 1 )); fi
}

echo "== syntax =="
check "bash -n paired-200.sh" bash -n "$TICKET/paired-200.sh"
check "python3 -m py_compile score-pair.py" python3 -m py_compile "$TICKET/score-pair.py"

echo
echo "== (a) trigger loss after two completed pairs =="
box=$TMP/box
mkdir -p "$box/snapshot" "$box/policy"
echo "fixture" > "$box/snapshot/one"
printf '{}\n' > "$box/trace.jsonl"
printf '{}\n' > "$box/policy/indexed.policy.json"
printf '#!/bin/sh\n' > "$box/bin"; chmod +x "$box/bin"
echo "1.00 1.00 1.00 1/100 1234" > "$box/loadavg"
run_sim() { # runs-dir logs-dir lose-after deadline-seconds errors extra-args...
    local runs=$1 logs=$2 lose=$3 deadline=$4 errors=$5; shift 5
    RUNS_DIR=$runs LOGS_DIR=$logs LOADAVG_FILE=$box/loadavg FAKE_ARM=$FIX/fake-arm.sh \
    FAKE_LOSE_AFTER=$lose FAKE_ERRORS=$errors SNAP=$box/snapshot TRACE=$box/trace.jsonl \
    POL=$box/policy/indexed.policy.json BIN=$box/bin \
    GATE_POLL_SECONDS=0.01 GATE_DEADLINE_SECONDS=$deadline GATE_QUIET_READINGS=3 \
    bash "$TICKET/paired-200.sh" --simulate --pairs 5 "$@"
}
out=$(run_sim "$TMP/runs1" "$TMP/logs1" 3-declared 8 0 2>&1); rc=$?
record "simulated run exits 4 when the trigger is lost before p3 indexed" 4 "$rc"
contains "gate loss is named" "GATE UNMET (before p3 indexed)" "$out"
check "p1 complete pair stays in runs/" bash -c "ls '$TMP/runs1'/p1-declared.out '$TMP/runs1'/p1-indexed.out >/dev/null 2>&1"
check "p2 complete pair stays in runs/" bash -c "ls '$TMP/runs1'/p2-declared.out '$TMP/runs1'/p2-indexed.out >/dev/null 2>&1"
shopt -s nullglob
invalid_out=("$TMP"/runs1/invalid-*/p*-*.out)
shopt -u nullglob
if [ "${#invalid_out[@]}" -eq 1 ]; then
    echo "PASS: exactly one arm file moved to invalid-<stamp>/ (${invalid_out[0]##*/})"
    if [ "${invalid_out[0]##*/}" = "p3-declared.out" ]; then
        echo "PASS: only the incomplete pair's arm was moved"
    else
        echo "FAIL: moved file is ${invalid_out[0]##*/}, wanted p3-declared.out"; fails=$(( fails + 1 ))
    fi
    if head -1 "${invalid_out[0]}" | grep -qF "# INVALID - do not cite:"; then
        echo "PASS: the moved file carries the INVALID marker"
    else
        echo "FAIL: moved file has no INVALID marker"; fails=$(( fails + 1 ))
    fi
else
    echo "FAIL: ${#invalid_out[@]} .out files moved to invalid-<stamp>/ (wanted 1)"; fails=$(( fails + 1 ))
fi
check "no p3 indexed arm was created" test ! -e "$TMP/runs1/p3-indexed.out"
valid1=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(sum(1 for e in m["pairs"] if e["valid"] and e["source"]=="runs1"))' "$TMP/runs1/pairs.json")
if [ "$valid1" = 2 ]; then echo "PASS: manifest records 2 valid pairs"; else echo "FAIL: manifest has $valid1 valid pairs, wanted 2"; fails=$(( fails + 1 )); fi
manifest_has_p3_invalid=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(any((not e["valid"]) and e["pair"]==3 and e["source"].startswith("invalid-") for e in m["pairs"]))' "$TMP/runs1/pairs.json")
if [ "$manifest_has_p3_invalid" = True ]; then echo "PASS: manifest names p3 as invalid"; else echo "FAIL: manifest does not name p3 invalid"; fails=$(( fails + 1 )); fi

echo
echo "== (b) --resume continues without overwriting =="
before=$(sha256sum "$TMP"/runs1/p1-*.out "$TMP"/runs1/p2-*.out)
echo "1.00 1.00 1.00 1/100 1234" > "$box/loadavg"
out=$(run_sim "$TMP/runs1" "$TMP/logs1" "" 15 0 --resume 2>&1); rc=$?
record "resumed run reaches five valid pairs and exits 0 (met)" 0 "$rc"
contains "resume starts at p3" "resuming after 2 valid pair(s); next pair is p3" "$out"
contains "scorer saw five valid pairs" "valid paired blocks: 5" "$out"
after=$(sha256sum "$TMP"/runs1/p1-*.out "$TMP"/runs1/p2-*.out)
if [ "$before" = "$after" ]; then echo "PASS: p1 and p2 are byte-identical after resume"; else echo "FAIL: resume changed an existing valid pair"; fails=$(( fails + 1 )); fi
check "p3 was taken by the resumed run" test -e "$TMP/runs1/p3-declared.out"
check "p4 was taken by the resumed run" test -e "$TMP/runs1/p4-indexed.out"
check "p5 was taken by the resumed run" test -e "$TMP/runs1/p5-declared.out"
valid2=$(python3 -c 'import json,sys; m=json.load(open(sys.argv[1])); print(sum(1 for e in m["pairs"] if e["valid"] and e["source"]=="runs1"))' "$TMP/runs1/pairs.json")
if [ "$valid2" = 5 ]; then echo "PASS: manifest records 5 valid pairs"; else echo "FAIL: manifest has $valid2 valid pairs, wanted 5"; fails=$(( fails + 1 )); fi
if grep -qF "p3-declared.out" <<< "$(ls "$TMP"/runs1/invalid-*/*.out)"; then
    echo "PASS: the stale p3 arm from the lost run stays marked in invalid-<stamp>/"
else
    echo "FAIL: the stale p3 arm is missing from invalid-<stamp>/"; fails=$(( fails + 1 ))
fi

echo
echo "== a re-run with five valid pairs scores without needing a quiet window =="
out=$(run_sim "$TMP/runs1" "$TMP/logs1" "" 0 0 --resume 2>&1); rc=$?
record "a fifth-pair-complete re-run skips the gate and scores" 0 "$rc"
contains "no new pair was run" "resuming after 5 valid pair(s); next pair is p6" "$out"

echo
echo "== --fresh cannot overwrite an existing valid pair =="
out=$(run_sim "$TMP/runs1" "$TMP/logs1" "" 2 0 --fresh 2>&1); rc=$?
record "--fresh refuses when valid pairs exist" 3 "$rc"
contains "--fresh names the overwrite risk" "use --resume (the default) so no valid pair is overwritten" "$out"

echo
echo "== an arm with errors > 0 is refused and the pair is quarantined =="
echo "1.00 1.00 1.00 1/100 1234" > "$box/loadavg"
out=$(run_sim "$TMP/runs3" "$TMP/logs3" "" 5 1 2>&1); rc=$?
record "errors>0 arm exits 6" 6 "$rc"
contains "the errored arm is named" "recorded errors=1" "$out"
if [ -e "$TMP"/runs3/invalid-*/p1-declared.out ]; then
    echo "PASS: the errored pair's arm was moved out of the scorer's glob"
else
    echo "FAIL: the errored pair's arm is still in runs/"; fails=$(( fails + 1 ))
fi

echo
echo "== (c)-(e) scorer on fixture pairs =="
scenario() { # name expected-rc
    local dir=$TMP/score-$1
    rm -rf "$dir"
    python3 "$FIX/make-fixtures.py" "$1" "$dir" >/dev/null
    SCORE_OUT=$(python3 "$TICKET/score-pair.py" "$dir" 2>&1)
    SCORE_RC=$?
    record "scenario $1" "$2" "$SCORE_RC"
}
scenario two 3
contains "two valid pairs print partial: 2/5" "partial: 2/5" "$SCORE_OUT"
contains "two valid pairs refuse a verdict" "cannot decide: 2 complete valid pair(s)" "$SCORE_OUT"
scenario four 3
contains "four valid pairs print partial: 4/5" "partial: 4/5" "$SCORE_OUT"
scenario five-met 0
contains "five valid pairs can meet the clause" "VERDICT: clause (b) MET" "$SCORE_OUT"
scenario five-falsified 1
contains "five valid pairs can falsify the clause" "VERDICT: clause (b) FALSIFIED" "$SCORE_OUT"
scenario five-vacuous 3
contains "five valid pairs with no sustained block stay undecidable" "no block sustained by either arm" "$SCORE_OUT"
scenario excluded 3
contains "excluded scenario still prints partial: 3/5" "partial: 3/5" "$SCORE_OUT"
contains "a start loadavg >= 2.0 arm is named" "p3-declared: start loadavg 2.50 >= 2.0" "$SCORE_OUT"
contains "an errors > 0 arm is named" "p4-indexed: errors=1 != 0" "$SCORE_OUT"

echo
if [ "$fails" -eq 0 ]; then
    echo "ALL CHECKS PASSED"
    exit 0
fi
echo "$fails CHECK(S) FAILED"
exit 1
