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

## Design (chosen): minimum-payback hysteresis on the per-region RE-merge

Reuse the existing `merge_min_tiles` knob but split what it gates:

- **First merge is the slice-27 gate, unchanged.** A region is eligible for its
  FIRST automatic merge once its OWN tile count reaches `merge_min_tiles`
  (`count >= threshold`). A fresh region behaves exactly as before, and a
  recovered hot region (`U|Stock` at 121 tiles) is still eligible when the
  learner lowers its threshold.
- **Re-merge now requires new data.** Selecting a region records its post-merge
  tile count as a per-region **floor**; the region is re-admitted only once
  `count - floor >= merge_min_tiles`, i.e. after it has published `threshold`
  NEW tiles since its last merge. A threshold at (or far below) the region's
  current count can therefore trigger at most ONE merge and then must be repaid
  with new tiles before the next one - the ~10 MB-per-window repeat is removed.
- **The merge always folds the cheapest contiguous run** (unchanged
  `choose_run`), so a single actuation is bounded by one pair; the re-merge gate
  is what bounds the REPEAT.
- **General, not workload-specific.** It is the same rule for every region and
  every threshold; it only changes the automatic re-merge cadence, never the
  merge mechanics, the explicit `compact()`, or the read/write/durability paths.

State: `Engine.merge_floor: RefCell<HashMap<Vec<u8>, usize>>`, keyed by the raw
region prefix, recording the region's tile count immediately after its last
automatic merge; absent means "never merged" (the slice-27 count gate applies).
Written only in `select_region_run`. `RefCell` is required because
`select_region_run` and the async `compaction_job` both run under `&self`; the
engine already uses `RefCell` for metrics and is single-writer. It is never
persisted: a reopen forgets every floor, which only defers (never forbids) the
next merge.

### Measured correction (recorded before the code was committed)

The first design gated the FIRST merge on new-tiles-since-open too; a sanity run
showed that defers ALL compaction for a short load (declared reorg 0 B, because
the load publishes fewer than 2 new tiles per region). That fails "legitimate
compaction still happens" and makes the T3 ratio 0/0. The revised design keeps
the first merge on the absolute-count gate and applies the payback only to the
re-merge, which is the oscillation that actually fires.

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
- New `Engine.merge_floor: RefCell<HashMap<Vec<u8>, usize>>`, documented, initialized empty in
  both constructors (`create`, `open_impl`). It records a region's tile count immediately after
  its last automatic merge; absent = never merged.
- `select_region_run`: a never-merged region keeps the slice-27 absolute-count gate
  (`count >= threshold`); a merged region is re-admitted only when `count - floor >= threshold`
  (at least `merge_min_tiles` NEW tiles since its floor). The selected region's floor is recorded
  at selection (so the async `compaction_job` path cannot re-select the same region while its job
  is in flight). Doc comments updated.
- No publish-path change: the floor is derived from the region's current tile count and the chosen
  run length, so `build_tile`/`publish_tile` are untouched.

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
