# 70 - WP-R2: bound the single-region merge oscillation

## Contract

Work package R2 of the wave16 program. WP-Q (ticket 68) measured a recurring single-region
reorganisation blowup on the win workload, seed 42: the `U|Stock` region emitted ~75 MB over 8
merges on one run and 0 MB on the same-trace repeat, the learned win rung-50 lag-rejection hit
13/40 on that same run (windows 24-34 interleaved with the compaction bursts), and the win T3
reorganisation ratio blew up (seed42 15.19x). The same `U|Customer`/`P|Customer` oscillation
drove the adversarial seed-42 2.85x outlier (ticket 68) and the WP-P write-amplification variance
(ticket 67). The known cause, recorded in tickets 65/66/67/68: the driver calls the engine's
`maybe_compact_shape` once per window with a global 67 MB budget, and the engine chooses which
region compacts; a per-region reward/threshold cannot see that deferral in one region funnels
merges into an eager neighbour, and a tile-count threshold oscillating around its boundary can
emit several large merges.

**Objective:** make a single region's per-window reorganisation bounded and stable, so a threshold
sitting near the tile-count boundary cannot repeatedly emit large merges. Do not remove legitimate
compaction (the write path must still fold tiles), and do not special-case a workload; the rule
must be general.

**Acceptance (verbatim):**
- the single-region blowup no longer recurs (the same-trace repeat consistency of the max
  per-region output is bounded);
- win T3 ratio median <= 3x and no single-run blowup;
- learned win rung-50 lag-rej improves toward static-derived's band;
- no throughput regression and legitimate compaction still happens;
- OR the exact remaining cause with evidence. Never overstate.

**Judging:** SS7.1 lag-only primary; report both. Do not edit the pre-registration.

## Diagnosis (checkpoint, committed before implementation)

### The mechanism that fires, from the committed JSONs + `tileinfo`

The pre-built c51 instance `/tmp/cold51/per_region-mixed` (12.25x) already carries the hot
regions as MANY LARGE seed tiles (release `flashdb-scale tileinfo`, run on a `cp -a` copy):

| region | seed tiles | seed tile size |
|---|---:|---:|
| `U|Stock` | 121-127 | ~5.2 MB (70000 entries, 4 KiB pages) |
| `P|Stock` | 127 | ~5.8 MB |
| `U|Customer` / `P|Customer` | 26 each | ~5.4 / 5.9 MB |
| `U|Order` / `P|Order` / `U|StockMovement` / `P|StockMovement` | 6 each | ~5.4-5.9 MB |
| `R|Order` / `R|StockMovement` | 6 each | 8 KB - 1 MB mixed |

Per-window committed evidence (`.scratch/68-eval-final/seed42.json`, `win/learned@1.0`, the
policy in effect at each window's compaction is the PREVIOUS window's reload, and the async build
attributes the merge to the window where it is APPLIED, so window attribution lags selection):

| window | region | compaction_bytes | governing `merge_min_tiles` |
|---|---:|---:|---:|
| 22 | `U|Stock` | 5.1 MB | 8 |
| 24 | `U|Stock` | 10.1 MB | 8 |
| 26 | `U|Stock` | 9.9 MB | 8 |
| 28 | `U|Stock` | 9.9 MB | 8 |
| 31 | `U|Stock` | 10.2 MB | 8 |
| 33 | `U|Stock` | 10.2 MB | (selected at thr=8, applied after) |
| 45 | `U|Stock` | 9.9 MB | 2 |
| 47 | `U|Stock` | 9.9 MB | (selected at thr=2, applied after) |
| 37 / 40 / 42 | `P|Customer` | 10.2 / 10.3 / 10.3 MB | 8 / 8 / 8 |

The same-trace repeat (`seed42-repeat`) gives `U|Stock` 0 merges / 0 bytes and total reorg
3.3 MB: the blowup is exploration/timing-dependent, not trace-deterministic.

**Root cause (verified in `engine/src/engine.rs::select_region_run`):** the per-region automatic
trigger gates eligibility on the region's ABSOLUTE tile count (`idxs.len() >= merge_min_tiles`)
with no hysteresis. A region whose count is already far above its threshold (the seed phase left
`U|Stock` at 121 tiles, `P|Customer` at 26) is therefore eligible on EVERY window for every
threshold the learner tries (2/8/200 all sit below the seed count). `choose_run` (CheapestPair)
then merges exactly ONE adjacent pair per call, which lowers the count by exactly ONE, so the
region stays eligible and the global actuator re-selects it window after window. Once that
region's small freshly-published tiles are merged away, its cheapest pair is two ~5 MB seed
tiles, so each of those repeated calls emits ~10 MB. Deferring one region (a high threshold) does
not help while other regions are eager: the single global budget is simply spent on whichever
eligible region has the smallest pair, i.e. the merges funnel into one hot neighbour.

The observed `U|Stock`/`P|Customer` pattern is therefore not a boundary-crossing flip so much as
a hot region being merged one ~10 MB pair per window toward a threshold it will only reach after
~100 more windows; the learner's threshold exploration repeatedly re-arms it (2/8 make it
eligible, 200 stops it, 2 re-arms it, ...).

## Design (chosen): minimum-payback hysteresis on the per-region trigger

Reuse the existing `merge_min_tiles` knob but change what it gates: a region is eligible for its
NEXT automatic merge only after it has PUBLISHED at least `merge_min_tiles` NEW tiles since its
last automatic merge (or since open). Selecting a region resets its payback counter to zero.

- **First merge is unchanged.** For a region created during the run, the number of published tiles
  equals its tile count, so `payback >= threshold` reproduces the old `count >= threshold` gate
  exactly for the first merge.
- **Re-merge now requires new data.** After a merge the counter is 0, so the region must publish
  `threshold` more tiles before it is eligible again. A threshold at (or far below) the region's
  current count can no longer re-trigger a merge every window.
- **The merge always folds the cheapest pair** (unchanged `choose_run`), so the output is bounded
  by the region's smallest tiles — the freshly-published ones, not the ~5 MB seed tiers — and the
  seed tiers stay deferred until new data genuinely justifies folding them.
- **General, not workload-specific.** It is the same rule for every region and every threshold;
  it only changes the automatic re-merge cadence, never the merge mechanics, the explicit
  `compact()`, or the read/write/durability paths.

State: `Engine.merge_payback: RefCell<HashMap<Vec<u8>, usize>>`, keyed by the raw region prefix,
incremented by `record_published_tile` in the two publish paths (`build_tile`, `publish_tile`) and
read/reset in `select_region_run`. `RefCell` is required because `select_region_run` and the async
`compaction_job` both run under `&self`; the engine already uses `RefCell` for metrics and is
single-writer. It is never persisted: a reopen restarts every region's payback at zero, which only
defers (never forbids) the next merge.

### Rejected alternatives

- **(a) Per-region per-window output byte cap (a new knob).** Bounds each merge but not the
  cadence: the hot region would still be selected every window and would either re-merge capped
  pairs forever (churn remains) or be permanently skipped once every pair exceeds the cap
  (legitimate compaction stops for big regions). A new magic-byte threshold is less principled
  than reusing `merge_min_tiles`.
- **(b) Hard budget with no over-budget fallback.** The observed ~10 MB pairs are well under the
  67 MB budget, so this does not move them; the fallback is not the firing mechanism here.
- **(c) Merge the whole eligible region down to its threshold in one call.** Proper level collapse
  but it makes one window's actuation unbounded and can emit far more bytes in a single window
  (up to the whole region); it also changes the per-merge work bound that `CheapestPair` keeps
  O(log N).
- **(d) A per-region wall-clock cooldown.** Couples the trigger to time instead of to new data, so
  it either still re-merges large seed pairs (short cooldown) or stalls a hot region's legitimate
  folding (long cooldown). Payback-by-new-tiles is the quantity the threshold already names.
- **(e) Fair-share budget division across regions.** The actuator already merges only one region
  per window; the problem is the same region being re-selected across windows, which per-window
  budget division does not address.

### Invariants (state how each is preserved)

1. **Correctness / I2.** Only the automatic re-merge cadence changes. `merge_region_run` and the
   manifest/WAL/recovery protocols are untouched, so disjoint ranges, newest-first reads,
   fail-closed recovery and the per-region key restriction are unchanged.
2. **Durability.** No change to publish/checkpoint/barrier ordering; the payback counter is pure
   in-memory selection state and never feeds a durability or correctness decision.
3. **Legitimate compaction.** The write path still folds tiles: a steady region merges its
   cheapest pair every `threshold` new tiles. `compact()` still collapses a region on request.
4. **Bounded memory.** One `usize` per observed region, released with the engine; no new
   retention.

## Change

`engine/src/engine.rs`:
- New `Engine.merge_payback: RefCell<HashMap<Vec<u8>, usize>>`, documented, initialized empty in
  both constructors (`create`, `open_impl`).
- New `Engine::record_published_tile(&self, &TileHandle)` increments the tile's region counter;
  called from `build_tile` and `publish_tile` for each published tile (merge replacements and
  recovery do NOT call it).
- `select_region_run`: eligibility changed from `idxs.len() >= threshold` to
  `payback >= threshold` (still requires `>= 2` tiles); the selected region's counter is reset to
  0 at selection so the async `compaction_job` path cannot re-select the same region while its job
  is in flight. Doc comments updated.

`engine/tests/merge_threshold.rs`:
- `an_eager_region_is_not_held_back_by_a_lazy_neighbour` updated for the payback semantics.
- NEW `a_region_at_its_threshold_does_not_remerge_without_new_tiles` — fails pre-change (the old
  absolute-count gate re-merged on the very next call).

## Tests

- `cargo test --workspace --offline`: see gates below.
- `cd harness && uv run --with pytest pytest -q`: see gates below.

## Verbatim gates (run at final HEAD)

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q` — summary.
4. `git diff --check` — silent (exit 0).

## Measurement protocol

Release binary built once, copied to `/tmp/wave16-r2/`, run only from that copy, one process at a
time (no other measurement running). Instance `cp -a` of `/tmp/cold51/per_region-mixed` (12.25x).
Harness `cold_0b.py --workloads win adversarial --arms learned static-derived` (declared added if
cheap), full ladder 50/100/200/400, the ticket-68 exact flags (`--cap-mib 128
--checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864
--ceiling-us 10000 --lag-ceiling-us 10000 --lambdas 1.0`). At least 3 seeds + a repeat.

Report: per-region reorg totals, the max single-window actuation, learned/declared and
static/declared reorg ratios (T3), and rung-50 lag-rej. Record box load at every boundary.

## Before/after tables

(Pending the measurement; filled below after the runs.)

## Both judging views

(Pending the measurement.)

## Acceptance verdict

(Pending the measurement; honest, pilot-grade.)

## Residual items

(Pending the measurement.)
