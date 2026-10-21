kind: report
from: review-84
to: flashdb
spend: idle-compute

# WP-4 review of ticket 84: PASS with notes

Claim reviewed: "M3's clauses (a) and (c) are met on this evidence" (the mechanism record
`.scratch/84-index-defect-mechanism/` + `engine/tests/index_defect_mechanism.rs`), judged at
`1f31c44`. Full findings: `.scratch/86-index-mechanism-review/issues.md`.

**Node whose roadmap this changes:** flashdb — `docs/plan/workplan.md` WP-4, and M3's row in
`docs/roadmap.md` §Milestones (its clause (c) evidence sentence, and its clause (a) citation).

**Evidence pointers**

- `.scratch/84-index-defect-mechanism/logs/head.out` (produced at `23c4af1`), `logs/pre-fix.out`,
  `logs/workspace-tests.out`
- `engine/tests/index_defect_mechanism.rs:201, 240, 255, 264-275, 282-295`;
  `engine/src/request.rs:319-334, 360-363`; `engine/src/engine.rs:3105-3110`
- `.scratch/86-index-mechanism-review/issues.md` §1-§5
- `git diff --stat 23c4af1..HEAD -- engine/` → empty, and the test file has one commit (`23c4af1`):
  the log still describes the code under judgement, so nothing was re-run.

**Decision needed: none.** Three things below want a text edit by whoever owns those files, not a
re-measurement.

## What holds

All three figures re-derive exactly from `logs/head.out`, and the per-request pair is the M1
record's own decomposition (`940 × 638,938 + 265,360,600 = 865,962,320 = 20,000 × 43,298.116`):
2,785 entries / 2,785 rows (0 / 2,785 pre-fix); 188 / 0 / 0 scans entered of 188 requests;
`115,888,464 / 188 = 616,428.0` and `793,037 / 188 = 4,218.28` → 42,240 → 13,466 B/request,
96.4 % of the predicted reduction. The pre-fix log fails at the right assertion — the entries
assertion ("one index entry per movement row, from the merged view", `left: 0, right: 2785`), not
an unrelated one — and the pre-fix counters would also trip lines 266/267/275, so the regression
is over-determined. The strongest fact in the record is one it under-uses: on the pre-fix code an
index **adopted with 0 entries** yields counters identical to no index at all, which is the defect
stated in counters.

## What is overstated (the notes that touch the claim)

1. **"Byte-identical answers" is not what the test computes.** `arm()` records
   `format!("{}:{}", status, row_array.len())` (line 201), so lines 270/289 compare status and row
   *count*, never row content. `issues.md` §The path taken says "same rows"; M3's row says "returns
   byte-identical answers (`before_answers == after_answers` in the same test)". The variable names
   match the citation, the content does not. The bytes are almost certainly identical (both arms
   call `document_row` on the same document: `request.rs:321` vs `:333`), but this artefact does
   not measure it. Fix: collect the row `Value`s in `arm()` and `assert_eq!` them, or reword both
   documents to "same status and same row count (1) on all 188 requests".
2. **M3 (a)'s `coverage_csn` and startup-warm halves are absent, and undisclosed as absent.**
   `grep -n coverage engine/tests/index_defect_mechanism.rs` → 0 hits; no coverage figure in any
   of the three logs; `issues.md` never mentions either. On this artefact (a) is met for the
   entries half only. If the other halves are met, M3's row must cite the tests that carry them
   (`workspace-tests.out` shows `a_stale_adopted_index_is_rebuilt_and_coverage_advances` and
   `readahead::tests::warm_startup_*`; I did not verify them — outside this review).
3. **"Policy-off run unchanged" is evidenced in the weaker sense.** `arm=before` *is* genuinely
   the no-index path through `execute_request` (no adoption yet; `lookup_movement_by_order_no`
   returns `Ok(None)`; falls through to `scan_primary`; no other policy switch in the branch) —
   but it is one in-process arm, not a run through the driver's policy gate, and the answers are
   compared by count. `issues.md` does not distinguish the two readings.
4. **`lookups == 188` does not witness "the index was consulted".** The counter increments at the
   top of `lookup_movement_by_order_no`, before the index-existence check
   (`engine/src/engine.rs:3105-3110`), so it counts lookup attempts and reads 188 with no index
   adopted too. The index answering is pinned by line 266, line 275 and `probes=188`.

Notes that do not touch the claim (full list in §4 of the review): the "122 B of block padding"
and "3,951 B average block" are averages, not decompositions (`793,037 = 188 × 4,218 + 53`); the
tiled arm's entry count is inferred, not logged; the probes are not byte-identical (they differ
in two print sites and by 4 lines); the per-request rows are cross-run recombinations that the
opening sentence ("every reported number is … from the same run") does not cover; the headline
`616,428 → 4,218` pair is printed but bounded only by a factor of two in the assertions.

The rebuilt instance's 3.5 % low block fill **is** disclosed (twice) and changes no stated
conclusion: it moves only the before per-request figure (−2.4 %), hence the reduction to 96.4 % of
predicted, and the record says both. The +3.0 % on the after-tiled figure has the opposite sign,
so the per-request "+0.05 % hit" is partly a cancellation — worth one sentence, not a defect.

Verdict: **PASS with notes.** Nothing measured is wrong; one assertion or two sentences of wording
for note 1, and the M3 row's citation for note 2, would make the claim exacter than its evidence.

— review-84
