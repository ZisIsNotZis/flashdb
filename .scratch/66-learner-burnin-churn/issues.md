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

## Verbatim gates (final HEAD `08661b4`, run after the change)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.98s
    Finished `release` profile [optimized] target(s) in 13.36s
```
2. `cargo test --workspace --offline` — totals: **299 passed; 0 failed; 0 ignored**
(126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 11 group_commit + 6 manifest_engine + 7 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 nvme_bench + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
140 passed in 3.95s
```
4. `git diff --check` — silent (exit 0).

## Measurement protocol (WP-O re-run)

Same protocol as ticket 65 (verbatim): `cp -a` copy of `/tmp/cold51/per_region-mixed` (12.25x),
release binaries built once, one process at a time under
`systemd-run --user --scope -p MemoryMax=128MiB -p MemorySwapMax=0`, harness
`cold_0b.py`, `--workloads win adversarial --arms declared learned static-derived`, full ladder
(50/100/200/400 rps). At least 2 seeds + a repeat.

## Box load during the runs (shared, noisy)

| invocation | start loadavg | end loadavg |
|---|---:|---:|
| seed42 | 8.03 | 5.51 |
| seed43 | 4.68 | 4.08 |
| seed42-repeat | 3.93 | 6.91 |

Present at the seed42 start: a sibling worktree's `multi_tile` test at ~215% CPU (ended mid-run),
`python3` ~99% CPU / ~2.1 GB RSS (the box's other work), `mpv` ~89% CPU, `wdavdaemon`/mdatp ~38%
CPU. The rung-50 lag metric is noise-limited on this box (see ticket 65: declared/adversarial
alone swung 1/40 -> 6/40 on the same trace seed); every timing number below names its load.

## Results (3 runs = 2 independent seeds 42/43 + seed 42 repeated; 18 arm-runs)

Release binaries: `/tmp/wave14-o-bins/` (`flashdb-load`
`bbe19236…`, `flashdb-scale` `927f4ee5…`, `flashdb-replay` `fd944df2…`), built once from HEAD
`3604161`. Raw artifacts committed here: `seed42.json`, `seed43.json`, `seed42-repeat.json` (plus
rendered `.md`).

### win (T1) rung-50 lag-rejections — the churn cost removed

| arm | seed42 | seed43 | seed42-repeat |
|---|---:|---:|---:|
| declared | 39/40 | 40/40 | 40/40 |
| learned@1.0 | **5/40** | **7/40** | **3/40** |
| static-derived | 3/40 | 3/40 | 5/40 |

Pre-change (ticket 65): learned 29/26/25/26, static-derived 4/3/3/3. learned now sits within a
small band of static-derived (differences +2 / +4 / -2 windows) instead of +25.

### win (T1) aggregate achieved rps and per-rung achieved (the +40-51% win must remain)

| arm | seed42 | seed43 | seed42-repeat |
|---|---:|---:|---:|
| declared | 68.0 | 74.0 | 72.0 |
| learned@1.0 | **107.0** | **107.0** | **107.0** |
| static-derived | 107.0 | 107.0 | 107.0 |

learned gain over declared: **+57.4% / +44.6% / +48.6%** (pre-change +41-51%). Per-rung achieved
rps (50/100/200/400) — learned and static-derived hold ~100% of offered, declared collapses:

| seed | declared 50/100/200/400 | learned 50/100/200/400 |
|---|---|---|
| seed42 | 50 / 72.1 / 78.2 / 81.6 | 50 / 100.0 / 200.0 / 399.6 |
| seed43 | 50 / 87.5 / 90.9 / 86.1 | 50 / 100.0 / 199.9 / 400.0 |
| seed42-repeat | 50 / 87.4 / 85.0 / 79.9 | 50 / 100.0 / 200.0 / 399.4 |

The learner commits `P|StockMovement = 4096 / 200 / index_order_no=True` on all three win runs
and the movement region never toggles the index back off.

### adversarial (T2) rung-50 lag-rejections

| arm | seed42 | seed43 | seed42-repeat |
|---|---:|---:|---:|
| declared | 0/40 | 0/40 | 0/40 |
| learned@1.0 | **0/40** | **0/40** | **0/40** |
| static-derived | 0/40 | 2/40 | 0/40 |

learned matches declared (0/40) on all three runs.

### Learner decision / index-build counts (whole run, learned@1.0)

| workload+seed | movement index toggles | index builds | index_output_bytes |
|---|---:|---:|---:|
| win seed42 | **0** | **1** | 2,143,439 B |
| win seed43 | **0** | **1** | 2,143,439 B |
| win seed42-repeat | **0** | **1** | 2,143,439 B |
| adversarial seed42 | **0** | **0** | 0 |
| adversarial seed43 | **0** | **0** | 0 |
| adversarial seed42-repeat | **0** | **0** | 0 |

Pre-change: 5 toggles and 3 builds on EVERY workload (win and adversarial alike). The demand gate
removes the 3 adversarial builds entirely, and the one-shot flip removes the win toggles (one
build per run, the single off->on adoption).

### Reorganisation (endurance term)

win learned/declared compaction ratio (T3):

| seed42 | seed43 | seed42-repeat |
|---:|---:|---:|
| 7.08x | 4.91x | 1.88x |

(Reported as pre-registered; the win reorg ratio is not part of this package's acceptance. The
seed42/43 blowups are the pre-existing `U|Stock`/`P|Stock` merge oscillation recorded in ticket
65, not the index axis: `index_output_bytes` is a separate 2.14 MB on win and the movement region
never toggles.)

adversarial learned/declared compaction ratio (T2 reorg band, the acceptance axis):

| seed42 | seed43 | seed42-repeat | median |
|---:|---:|---:|---:|
| **1.36x** | 3.39x | **0.33x** | **1.36x** |

Pre-change (ticket 65): 4.63x / 0.39x / 0.30x / 6.01x, median 2.51x. The median moves from
2.51x to **1.36x** (<=1.5x). The single seed43 outlier (3.39x) is the pre-existing `U|Stock`
oscillation (45.2 MB / 5 merges, `index_output_bytes=0`), not the index churn this package removes;
see Residual items.

### svc-rej at rung 50 (device-bound, unchanged)

win learned 37/28/32; adversarial learned 38/31/40. Every arm rejects ~28-40/40 rung-50 windows on
the service ceiling — the per-group `fdatasync` durability floor (STATUS §3), unchanged by this
package and reported for completeness.

## Both judging views

- **§7.1 lag-only (primary):** win rung-50 lag-rej learned 5/7/3 vs static-derived 3/3/5
  (churn removed); adversarial 0/0/0 = declared. Achieved throughput on win: declared 68-74 ->
  learned/static 107 rps (~100% at 100/200/400 vs declared 72-87/78-91/80-86).
- **Both ceilings (pre-registration):** sustained = 0 rps everywhere, as pre-registered — the
  service ceiling rejects ~28-40/40 rung-50 windows on every arm (device-bound). Same open §7.1
  reconciliation as ticket 65; not resolved here.

## Acceptance verdict (honest, pilot-grade)

**Pilot-grade, 2 independent seeds + 1 full repeat on the 12.25x c51 instance, shared box
(loadavg 3.9-8.0).**

1. **Win churn cost removed — PASS direction.** learned rung-50 lag-rej 5/7/3 vs static-derived
   3/3/5 (pre-change 25-29 vs 3-4); the learner's movement-region index toggles drop from 5 to 0
   and builds from 3 to 1 per run, and it commits the index on win (4096/200/index-on) on all three
   runs.
2. **Win +40-51% throughput preserved — PASS.** aggregate achieved 107 vs declared 68-74 rps
   (+44.6% to +57.4%), ~100% at 100/200/400 rps where declared collapses.
3. **Adversarial 0-scan index builds removed — PASS.** 0 builds, 0 toggles on all three adversarial
   runs (pre-change 3 builds each); rung-50 lag-rej 0/40 = declared 0/40.
4. **Adversarial reorg ratio — PASS direction (median <=1.5x).** 1.36x / 3.39x / 0.33x, median
   1.36x (pre-change median 2.51x). The 3.39x outlier is the pre-existing `U|Stock` oscillation,
   not the index axis.

Remaining cause for the still-zero lag-only sustained score (NOT this package): the per-group
`fdatasync` durability floor breaches the lag ceiling on static-derived too (3-5/40 on win) — the
same "necessary but not sufficient" WP-M recorded.

## Residual items

- The win T3 reorg ratio is still above 3x on seed42 (7.08x) and seed43 (4.91x) from the
  pre-existing `U|Stock`/`P|Stock` merge oscillation (ticket 65's seed43 blowup); this package
  removes only the index-toggle churn. Out of scope.
- The adversarial seed43 3.39x reorg ratio is the same pre-existing `U|Stock` oscillation
  (45.2 MB / 5 merges, 0 index bytes). Out of scope; named with evidence.
- Rung-50 lag remains noise-limited: static-derived itself moved 3 -> 5 -> (ticket-65) 4/3/3/3
  across runs, and declared/adversarial swung 1/40 -> 6/40 in ticket 65 on the same trace seed.
  All numbers are pilot-grade, not a 10-seed verdict.
