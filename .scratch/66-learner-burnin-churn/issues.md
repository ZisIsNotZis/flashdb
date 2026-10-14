# 66 - WP-O: remove the learner-side rung-50 lag churn (learned matches the frozen layout's lag)

## Contract

Work package O of the program. WP-M (ticket 64) added the learned secondary access path
(`index_order_no` on `P|StockMovement`) and WP-N (ticket 65) then measured that the LAG-ONLY
sustained score is still 0 at rung 50 for two learner-side reasons:

1. the learner's 36-window rung-50 burn-in keeps changing the movement region's layout while the
   trace is live, so learned has 25-29/40 lag-rejected windows vs `static-derived`'s 3-4/40 — the
   CHURN, not the layout, is the learner's remaining lag cost; and
2. the learner toggles `index_order_no` ON for `P|StockMovement` even on the adversarial workload
   where the region's scan count is 0, burning 3-6 index builds (6.4-13.0 MB) for zero read
   benefit (the frozen `static-derived` correctly keeps it off).

**Objective:** make the learner's own transients stop costing lag, WITHOUT removing genuine
adaptation. Concretely: (1) do not toggle/build the `order_no` index for a region whose observed
scan traffic does not justify it (gate on measured demand, not on exploration); (2) make the
rung-50 burn-in not degrade the lag ceiling.

**Preserve:** the axis must still be discoverable and committed on the win workload (the
+40-51% win must remain), and the adversarial rung-50 lag must stay at declared's level.

**Acceptance (verbatim):**
- On win, learned's rung-50 lag-rej is within a small band of static-derived's (the churn cost
  removed) while the +40-51% throughput win over declared remains.
- On adversarial, learned builds no `order_no` index on the 0-scan region and its rung-50 lag-rej
  is within declared's band; the adversarial reorg ratio moves toward <=1.5x.
- OR the exact remaining cause with evidence. Never overstate; label pilot-grade.

## Quantified diagnosis (committed artifacts, ticket 65)

Baseline artifacts are the WP-N committed JSONs on branch `wave13/eval` @
`f1452c027b623f88f0a91d753339683bf3f32679`, directory
`/home/z/vibe/worktrees/flashdb/wave13-eval/.scratch/65-eval-post-axis/` (seed42, seed43, seed44,
seed42-repeat). Re-counted per window at rung 50 (windows 0-39, `rate_rps == 50`) here.

### win (T1) rung-50 lag-rejections

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 40/40 | 40/40 | 40/40 | 40/40 |
| learned@1.0 | 29/40 | 26/40 | 25/40 | 26/40 |
| static-derived | 4/40 | 3/40 | 3/40 | 3/40 |

### adversarial (T2) rung-50 lag-rejections

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 1/40 | 0/40 | 0/40 | 6/40 |
| learned@1.0 | 4/40 | 1/40 | 0/40 | 1/40 |
| static-derived | 1/40 | 0/40 | 0/40 | 5/40 |

### Learner decision / index-build attribution at rung 50 (learned@1.0)

Per seed, counted from `learner_decisions` (the movement region's active candidate per window) and
the `P|StockMovement` region's `index_output_bytes` per window:

| seed | lag-rej | idx-ON lag-rej | idx-OFF lag-rej | index toggles | index builds | build bytes |
|---|---:|---:|---:|---:|---:|---:|
| seed42 (win) | 29 | 14 | 15 | 5 | 3 | 6,430,317 B (3 x 2,143,439) |
| seed43 (win) | 26 | 11 | 15 | 5 | 3 | 6,430,317 B |
| seed44 (win) | 25 | 11 | 14 | 5 | 3 | 6,430,317 B |
| seed42-repeat (win) | 26 | 11 | 15 | 5 | 3 | 6,430,317 B |
| seed42 (adv) | 4 | 4 | 0 | 5 | 3 | 6,430,317 B |
| seed42-repeat (adv) | 1 | 1 | 0 | 5 | 3 | 6,430,317 B |

The index builds land at windows 8, 23, 29 (the build is off-path, so its bytes appear 1-2
windows after the policy toggle). The learner's movement-region candidate sequence is a seeded
permutation of **12 candidates** (6 base page/merge layouts x {index off, on}) held 3 windows each
(`min_samples=3`, `cooldown_windows=3`), so the movement region churns through the WHOLE 40-window
rung (last change at window 33-36), toggling the index off->on->off->on 5 times.

**Conclusion:** on win the index-off windows run the full 38-49 ms movement scan on the pacer
(which WP-M proved is the lag blocker), and the index-on/off toggles each trigger an off-path
2.14 MB build/retire; on adversarial the SAME 12-candidate exploration burns 3 builds for a region
whose scan count is 0 (count=0 every window, so the index has zero read benefit and only the build
cost). `static-derived` freezes index ON from window 0 on win (3-4/40) and index OFF on adversarial
(0 builds) — pinning both defects to the learner's unconditional 12-candidate movement burn-in.

## Design (chosen) + rejected alternatives

**Chosen (smallest sound fix, harness-side only):** make `index_order_no` a **demand-gated,
one-shot, sticky axis** layered over the unchanged 6-candidate base bandit, instead of a
12-candidate cross product.

- The movement region (`P|StockMovement`) starts on the base 6-grid (index OFF), exactly like
  every other region. Its burn-in is back to the pre-WP-M 18 windows, and its candidate count no
  longer doubles.
- The first window in which the region actually serves a scan request (`count > 0`) flips its
  candidate set to the same 6 layouts with `index_order_no=True`, and the bandit re-explores
  page_size x merge among those. The flip happens AFTER the window's evidence is recorded against
  the index-off candidate it actually ran under, so the policy written is index-on for the NEXT
  window (no cross-attribution of slow-scan telemetry onto the index-on arm).
- The index is **sticky**: once flipped, the index-off candidates are gone, so the learner toggles
  the axis at most once per region per run (one build on win, zero on adversarial) instead of once
  per candidate change.
- The reward still prices the build (`index_output_bytes`) and the bandit still genuinely adapts
  the page_size/merge axis among the index-on layouts.

Why this satisfies both clauses: (1) a 0-scan region never observes `count > 0`, so the index is
never offered and never built — the axis is gated on measured demand, not exploration; (2) on win
the index is ON from the first window of demand (window 1 in practice), so the slow full-scan
period collapses from ~18-30 windows to ~1 window, and the movement region's remaining burn-in
churns only page_size/merge on a fast point-probe region (the same churn the other regions already
produce at 0-1/40 on adversarial).

### Rejected alternatives (recorded)

- **(a) keep the 12-candidate cross product but gate index variants on `count > 0` only.** This
  fixes the adversarial builds but on win still samples all 6 index-off layouts (18 windows of the
  slow scan) before/around committing the index, so the rung-50 lag churn remains. Rejected as not
  addressing clause (2).
- **(b) hard-code index ON for `P|StockMovement`.** Removes the learner's ability to keep the axis
  off on a 0-scan region and removes the (demand) adaptation; the task explicitly requires gating
  on measured demand rather than prescribing. Rejected.
- **(c) separate the index into a second UCB bandit arm evaluated in parallel with the base
  bandit.** More machinery (two-level bandit) for no measured benefit over the one-shot flip; the
  axis is a monotone capability once demand exists, so a persistent on/off bandit only re-opens the
  toggling this package removes. Rejected as over-engineering.
- **(d) defer/quiet-window gating of the whole burn-in.** Gating every layout change on a lag-clean
  window circularly starves the burn-in on a workload whose windows only become clean once the
  learner has settled; it also defers the beneficial index adoption. Rejected.
- **(e) bound live-region changes per window (a global change budget).** The movement region's own
  12-candidate burn-in is the dominant churn; a global per-window change cap staggers the other
  regions' already-tolerable burn-in but does not shrink the 36-window movement sweep. Rejected as
  not the binding term.

## Change

`harness/flashdb_harness/layout_learner.py` (harness-side learner; the engine's
build/adopt/retire path and the policy schema are unchanged):

- `LayoutCandidate` docstring: the index axis is now demand-gated, not a cross product.
- Removed the module-level `INDEXED_CANDIDATES` 12-candidate constant; `_candidates_for` now
  returns the base 6-grid for every region (the movement region no longer doubles).
- Added `LayoutLearner._index_on_candidates()` (the 6-grid with `index_order_no=True`) and
  `LayoutLearner._flip_index_axis(name, stats)`: the one-shot demand gate + sticky flip.
- `_RegionState` gains `index_flipped: bool` (a region flips at most once per run).
- `observe` calls `_flip_index_axis` after `record` and before `step`, so the flip's policy is
  index-on for the next window and the index-off evidence is not mixed into the index-on arms.

`harness/tests/test_layout_learner.py`:

- `test_order_no_index_axis_is_offered_only_for_the_movement_region` updated for the 6+6
  demand-gated structure.
- New `test_movement_region_never_offers_the_index_without_scan_demand` (fails pre-change: the
  pre-change learner offered 12 movement candidates unconditionally).
- New `test_movement_region_flips_to_index_on_once_scan_demand_appears_and_stays` (fails
  pre-change: the pre-change learner toggled the index back off during its 12-candidate burn-in).

## Tests

- `cd harness && uv run --with pytest pytest -q`: see gates below.
- `cargo test --workspace --offline`: see gates below.

## Verbatim gates (final HEAD, run after the change)

1. `cargo build --workspace --offline` — see gates section.
2. `cargo test --workspace --offline` — see gates section.
3. `cd harness && uv run --with pytest pytest -q` — see gates section.
4. `git diff --check` — see gates section.

## Measurement protocol (WP-O re-run)

Same protocol as ticket 65 (verbatim): `cp -a` copy of `/tmp/cold51/per_region-mixed` (12.25x),
release binaries built once, one process at a time under
`systemd-run --user --scope -p MemoryMax=128MiB -p MemorySwapMax=0`, harness
`cold_0b.py`, `--workloads win adversarial --arms declared learned static-derived`, full ladder
(50/100/200/400 rps). At least 2 seeds + a repeat.

## Before/after tables + both judging views + verdict

(Filled after the measurement; see the gates/verdict section at the end.)

## Residual items

- The adversarial reorg ratio may still exceed 1.5x after this fix: ticket 65 measured a
  PRE-EXISTING `U|Customer` merge oscillation (90.2-115.3 MB on the two seed-42 runs) that this
  package does not touch; this fix removes only the NEW index-toggle churn (6.4-13.0 MB
  `index_output_bytes` + 2.6-12.0 MB associated compaction). Reported exactly if so.
- Rung-50 lag on this shared box remains noise-limited: declared/adversarial itself swung 1/40 ->
  6/40 on the same trace seed across the two ticket-65 runs. All timing numbers name their load.
