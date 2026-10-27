#!/usr/bin/env bash
# 89 — the four defects the verdict names, reproduced at a named revision, before and after.
#
#   bash .scratch/89-m3b-paired-200/defect-repros.sh e5fb0e1 present   # the tree the contract started from
#   bash .scratch/89-m3b-paired-200/defect-repros.sh <after> absent    # the repairs
#
# Each check prints the observation it makes and a verdict against the stated criterion; the exit code
# is 0 only when all four match the expected state, so the run is the test.
set -uo pipefail
cd "$(dirname "$0")/../.."
REV=${1:?rev}
EXPECT=${2:?present|absent}
declare -i fails=0

echo "# defect repros at $REV, expecting defects $EXPECT"
echo "# loadavg at start: $(cat /proc/loadavg)  ($(date -Is))"

file_at() { # path -> content, empty when the path does not exist at that revision
    git cat-file -e "$REV:$1" 2>/dev/null && git show "$REV:$1" || true
}

verdict() { # name criterion-holds(0/1) detail
    local holds=$1 detail=${2:-}
    if [ "$EXPECT" = present ]; then
        if [ "$holds" = 1 ]; then echo "  VERDICT: defect present, as expected"; else
            echo "  VERDICT: FAIL — expected the defect present"; fails+=1; fi
    else
        if [ "$holds" = 0 ]; then echo "  VERDICT: cured, as expected"; else
            echo "  VERDICT: FAIL — defect still present"; fails+=1; fi
    fi
    echo "  ($detail)"
}

echo
echo "## d1 mail/README.md (now inbox/README.md): the header count claimed vs the lines listed"
readme=$(file_at mail/README.md; file_at inbox/README.md)
claim=$(printf '%s\n' "$readme" | grep -c "Four header lines" || true)
listed=$(printf '%s\n' "$readme" | grep -cE "^(kind|from|to|spend|id):" || true)
echo "  claims 'Four header lines': $claim   lists header lines: $listed"
echo "  listed: $(printf '%s\n' "$readme" | grep -oE '^(kind|from|to|spend|id):' | tr '\n' ' ')"
if [ "$claim" = 1 ] && [ "$listed" != 4 ]; then verdict 1 "claim=$claim listed=$listed"; else verdict 0 "claim=$claim listed=$listed"; fi

echo
echo "## d2 a document claiming ONE loadavg for EVERY log, against the logs themselves"
echo "  head -1 .scratch/88-m3-notes/logs/*.out .scratch/85-m3-clause-c/workspace-pinned.out:"
head -1 .scratch/88-m3-notes/logs/*.out .scratch/85-m3-clause-c/workspace-pinned.out 2>/dev/null | sed 's/^/    /'
# `inbox/` and `.scratch/` are excluded: a message or a measurement record quoting the defective
# sentence while reporting it is not a document asserting it. The criterion is about the project's
# own documents (`docs/`, README, AGENTS.md).
claims=$(git grep -l "first line of every log" "$REV" -- '*.md' ':!inbox/' ':!.scratch/' 2>/dev/null || true)
echo "  documents claiming it: ${claims:-none}"
if [ -n "$claims" ]; then verdict 1 "$claims"; else verdict 0 "no document claims one reading for every log"; fi

echo
echo "## d3 docs/0_ROADMAP.md M3 (a): max-over-rows cited to the test that binds it"
weak=$(file_at docs/0_ROADMAP.md | grep -c 'coverage_csn` half is bound by `a_stale_adopted_index_is_rebuilt_and_coverage_advances`' || true)
strong=$(file_at docs/0_ROADMAP.md | grep -c 'order_no_index.rs:171' || true)
echo "  cites order_no_index.rs:171 (max over the rows): $strong   attributes max-csn to flashdb-load.rs:3065: $weak"
if [ "$weak" -ge 1 ] || [ "$strong" = 0 ]; then verdict 1 "strong=$strong weak=$weak"; else verdict 0 "strong=$strong weak=$weak"; fi

echo
echo "## d4 docs/6_ARCHITECTURE.md: the M3 record's per-phase ledger the M4 deliverable names"
ledger=$(file_at docs/6_ARCHITECTURE.md | grep -c '2.1 The M3 record' || true)
echo "  §2.1 present: $ledger"
if [ "$ledger" = 0 ]; then verdict 1 "the ledger is M1-only"; else verdict 0 "§2.1 present"; fi
echo "  re-derived from the committed log (115,888,464/188 and 793,037/188):"
python3 -c "print('    before %.0f B/scan, after-tiled %.0f B/scan' % (115888464/188, 793037/188))"

echo
echo "# loadavg at end: $(cat /proc/loadavg)"
if [ "$fails" -gt 0 ]; then echo "# RESULT: $fails check(s) did not match the expected state"; exit 1; fi
echo "# RESULT: all four match the expected state ($EXPECT)"
