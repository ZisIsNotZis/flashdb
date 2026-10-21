kind:    report
from:    orient-mechanism (package launched by root)
to:      root
spend:   idle-compute
id:      none — answered in the run final message

Filed here by root. Body is the run report verbatim.

**REPORT** — node whose roadmap this changes: **root, `/home/vibe`** (it decides whether to write the deferral wording into `README.md:68-75`). Evidence pointer: `/home/z/vibe/flashdb` at `ba248dd` (on `8f334d0` + the two mail commits), tree clean, nothing staged; the run is `.scratch/84-index-defect-mechanism/`. Decision needed: one small one, at the end.

## The three numbers

Counted from the engine's own counters (`metrics::WindowSnapshot` + `order_no_index_lookups`) on the same rebuilt append-heavy instance — 156 published tiles (3 customer, 153 stock), 2,785 movements appended after the last publish and still in the serving memtable, 188 scan requests through `request::execute_request`. No rate was measured.

1. **Index entries after the workload: 2,785 for 2,785 rows** (pre-fix code: **0 for 2,785**). The pre-fix arm is the real pre-fix tree (worktree at `f584ecc`, the commit before WP-1), and its adopted 0-entry index left both of its arms reporting the identical 188 scans entered — the defect in one line.
2. **Tile bytes per scan request: 616,428 → 0** (rows memtable-resident) and **→ 4,218** (rows published to a tile). Prediction was 638,938 → 4,096, i.e. one block per active tile before, one page probe after. Stated per request at the record's 0.047 scan mix and its own non-scan read work (13,268 B/req): **42,240 → 13,466 B/request** against the predicted **43,298 → 13,460**. So: the *after* figure hits (13,466 vs 13,460, +0.05%) and the *before* figure is 2.4% low only because my reproduced tiles' candidate blocks average 3,951 B against the record's 4,096 B — the mechanism is the one predicted, the reduction is 28,774 vs 29,838 B/req (96.4%).
3. **The path taken: 188 index consultations, 0 merged scans entered, 188 movement point probes** (the documented cross-check `get`), 188 `ok` answers identical to the no-index arm. `scan:StockMovement` never fires; `Engine::scan_primary` is never entered.

**Test name:** `engine/tests/index_defect_mechanism.rs::index_defect_mechanism_is_gone_on_the_append_heavy_instance`. It asserts the non-empty index (2,785 == rows), 0-of-188 scans entered, 188 consultations, identical answers, ≥ half a block per tile before and ≤ 2 blocks per scan after; run against pre-fix code it fails `left: 0, right: 2785` (`.scratch/84-index-defect-mechanism/logs/pre-fix.out`). The 3-row version of the same assertion already existed (`order_no_index.rs::index_build_covers_rows_that_live_only_in_the_memtable`); this one carries the M1 instance's scale and the counters. Suite: **340 passed, 0 failed** (`logs/workspace-tests.out`).

## What is in the record

`.scratch/84-index-defect-mechanism/` — `issues.md` (instance shape, both arms, the prediction table with its arithmetic, what is *not* measured), `run.sh` (re-runs both arms; the pre-fix arm is expected to fail its assertion), `probe-at-HEAD.rs` and `pre-fix-probe.rs` (byte-identical except the build call), `logs/{head,pre-fix,workspace-tests}.out`. `docs/plan/open-items.md` row T-83 now states the measured mechanism and keeps the throughput clause explicitly deferred — "M3 clause (b) … needs a quiet box this site does not have, so no throughput number is measured or claimed here". All twelve `.scratch/83-index-probe/runs/` files gained one `# INVALID - do not cite:` first line naming pair, arm, foreign load and the empty-index-in-both-arms reason; `cmp` against their committed predecessors below line 1 is IDENTICAL for all twelve.

## Residual risks

- The seeded 1.6 GB M1 snapshot is git-ignored and absent, so the instance is rebuilt to the record's *shape*, not restored. The scan term is a function of active-tile count and block fill (both reproduced, 3.5% low on block fill); the non-scan 13,268 B/req is the record's own figure and is labelled as such in every row. Nothing here re-measures the seeded instance.
- `README.md:67-75` still says the 200 rps comparison "has not been re-run with that fix, so whether it closes the gap is **unverified**" — true, but without the word *deferred* that `mail/2026-10-21-split-the-claim.md` asked for. README was outside my scope; that sentence and the M3 row wording are the parent's edit.
- The new test costs ~20 s of the suite in debug profile (156 publishes, 2,785 movement commits, 3 × 188 scans); it is the heaviest file in `engine/tests/` and could be `#[ignore]`d later if suite time matters, at the cost of losing the CI regression.
- Decision needed: whether to write the deferral wording into `README.md:68-75` (and the M3 row) now, since only `open-items.md` was in my scope.