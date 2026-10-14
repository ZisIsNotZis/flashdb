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
persisted: a reopen forgets every floor, so a region re-enters the first-merge count gate (the
payback only ever defers a re-merge, never forbids a merge).

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

## Verbatim gates (run at final HEAD `1b13c8b`)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s
```
2. `cargo test --workspace --offline` — totals: **303 passed; 0 failed; 0 ignored**
(128 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 nvme_bench + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
140 passed in 4.09s
```
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

Box load (shared 12-core box; every run one flashdb process at a time, but the box carries other
users' work — a `llama-server` at ~100% CPU/10 GB RSS was present throughout):

| boundary | time | loadavg 1/5/15 |
|---|---|---|---|
| seed42 start | 11:08:02 | 4.89 / 6.11 / 6.76 |
| seed42 end | 11:18:09 | 8.73 / 7.23 / 6.86 |
| seed43 start | 11:18:40 | 7.58 / 7.10 / 6.83 |
| seed43 end | 11:27:43 | 7.74 / 8.69 / 7.88 |
| seed44 start | 11:27:56 | 7.37 / 8.56 / 7.85 |
| seed44 end | 11:36:59 | 6.89 / 7.52 / 7.60 |
| seed42-repeat start | 11:37:08 | 6.45 / 7.40 / 7.56 |
| seed42-repeat end | 11:46:12 | 6.27 / 7.00 / 7.38 |

**Serialization note (recorded after the orchestrator's `flock /tmp/wave16-measure.lock`
guidance arrived):** all four runs were already STARTED before that guidance, so **none of them was
lock-protected**; they ran one flashdb process at a time but not under the cross-lane lock. The
sibling lane `wave16/startwarm` (ticket 69) was measuring concurrently during the early part: its
`seed42-repeat` finished at 11:14, overlapping THIS ticket's `seed42` (11:08-11:18). This
package's `seed43`/`seed44`/`seed42-repeat` started after 11:18, i.e. after the sibling finished
at 11:14, so only the resident shared-box load (`llama-server` ~100% CPU) applied to them.
Consequence: the lag/timing numbers here are **direction-only under contention** (as directed);
the reorg/byte/merge-count numbers are deterministic and are the primary evidence. The decisive
clean serial measurement is WP-S after integration, not this lane.

## Before/after tables

"Before" = integrated pre-fix (ticket 68). "After" = this ticket (re-merge payback). All reorg
bytes are the summed per-region `compaction_bytes`.

### The single-region blowup is bounded (win learned, the acceptance target)

| run | `U|Stock` | `P|Customer` | `U|Customer` | total reorg | max single-window |
|---|---:|---:|---:|---:|---:|
| before seed42 | **75.2 MB / 8** | **30.8 MB / 3** | 0 | 109.3 MB | 10.16 MB |
| before seed42-repeat | 0 / 0 | 0 | 0 | 3.3 MB | ~0 |
| after seed42 | 5.23 MB / 1 | 10.29 MB / 1 | 0 / 0 | 17.12 MB | 10.29 MB |
| after seed43 | 5.23 MB / 1 | 0 / 0 | 0 / 0 | 6.88 MB | 5.23 MB |
| after seed44 | 5.23 MB / 1 | 0 / 0 | 0 / 0 | 6.83 MB | 5.23 MB |
| after seed42-repeat | 5.23 MB / 1 | 10.29 MB / 1 | 11.08 MB / 1 | 28.20 MB | 11.08 MB |

The oscillation is gone: `U|Stock` is now **5.23 MB / 1 merge on BOTH the seed42 run and the
same-trace repeat** (before: 75.2 MB / 8 vs 0 MB / 0). Each region does at most ONE merge per
low-threshold exposure, and the merge is a single cheapest contiguous pair, so the max
single-window actuation is 5.23-11.08 MB (one pair), never the repeated ~10 MB-per-window burst.

### win T3 reorg ratio (learned/declared and static/declared)

| seed | declared | learned | L/D | static-derived | S/D |
|---|---:|---:|---:|---:|---:|
| 42 | 30.72 MB | 17.12 MB | **0.56x** | 29.83 MB | 0.97x |
| 43 | 30.72 MB | 6.88 MB | **0.22x** | 24.70 MB | 0.80x |
| 44 | 30.72 MB | 6.83 MB | **0.22x** | 28.84 MB | 0.94x |
| 42-repeat | 30.72 MB | 28.20 MB | **0.92x** | 29.87 MB | 0.97x |

**win T3 (learned/declared) median = 0.39x** (all <= 1x; no single-run blowup).

### adversarial reorg ratio (learned/declared, T2 band)

| seed | declared | learned | ratio |
|---|---:|---:|---:|
| 42 | 30.72 MB | 29.00 MB | 0.94x |
| 43 | 30.72 MB | 17.03 MB | 0.55x |
| 44 | 30.72 MB | 29.79 MB | 0.97x |
| 42-repeat | 30.72 MB | 28.40 MB | 0.92x |

### win rung-50 lag-rejections (out of 40 windows) and achieved throughput

| seed | declared | learned | static-derived | learned ach | declared ach |
|---|---:|---:|---:|---:|---:|
| 42 | 40/40 | **3/40** | 4/40 | 107.0 | 71.0 |
| 43 | 39/40 | **6/40** | 5/40 | 107.0 | 70.0 |
| 44 | 40/40 | **3/40** | 5/40 | 107.0 | 72.0 |
| 42-repeat | 39/40 | **4/40** | 4/40 | 107.0 | 71.0 |

Before (ticket 68) learned win rung-50 lag-rej was 3-13/40, with seed42 = **13/40** driven by the
`U|Stock` reorg burst. After, learned sits **3/6/3/4** vs static-derived **4/5/5/4** — at or
better than static-derived's band on 3 of 4 seeds (seed43 is +1). The throughput win is intact
(learned 107.0 rps on every seed vs declared 70-72).

### adversarial rung-50 lag-rejections

| seed | declared | learned | static-derived |
|---|---:|---:|---:|
| 42 | 1/40 | 2/40 | 0/40 |
| 43 | 3/40 | 1/40 | 4/40 |
| 44 | 1/40 | 2/40 | 1/40 |
| 42-repeat | 7/40 | 2/40 | 2/40 |

## Both judging views

- **§7.1 lag-only (primary):** win rung-50 lag-rej learned 3/6/3/4 vs static-derived 4/5/5/4
  (pre-fix 3-13/40). The learner's reorg oscillation is removed, so learned now sits in
  static-derived's band; the remaining breaches are the read-ahead startup warm-up (windows 0-1)
  and shared-box noise, unchanged. No arm reaches 0 lag-rej, so the strict lag-only sustained
  score is still 0, exactly as ticket 68 — this package removes the REORG blocker, not the
  warm-up blocker.
- **Both ceilings (pre-registration):** sustained = 0 everywhere, unchanged — the per-group
  `fdatasync` durability floor keeps the service ceiling rejecting ~36-40/40 rung-50 windows on
  every arm (device-bound). Same open §7.1 reconciliation, untouched.

## Acceptance verdict (honest, pilot-grade)

**4 runs = 3 independent seeds (42/43/44) + one full seed-42 repeat, 12.25x c51, shared box,
one flashdb process at a time. NOT a 10-seed pre-registered verdict.**

1. **Single-region blowup no longer recurs — PASS.** `U|Stock` is 5.23 MB / 1 merge on both the
   seed42 run and the same-trace repeat (before: 75.2 MB / 8 vs 0 / 0); the same-trace max
   per-region output is now bounded and consistent. Max single-window actuation 5.23-11.08 MB
   (one cheapest pair), never a repeated burst.
2. **win T3 ratio median <= 3x, no single-run blowup — PASS.** learned/declared median **0.39x**
   (0.22-0.92x); static/declared median 0.96x. No run exceeds 1x.
3. **Learned win rung-50 lag-rej improves toward static-derived — PASS.** learned 3/6/3/4 vs
   static-derived 4/5/5/4 (pre-fix seed42 was 13/40).
4. **No throughput regression, legitimate compaction still happens — PASS.** learned achieved
   107.0 rps on all four runs (vs declared 70-72, the pre-fix +43-57% win intact); every arm
   still compacts (declared 30.72 MB, learned 6.83-28.20 MB, static 24.70-29.87 MB), one pair per
   region under the count gate.

**Honest note on the T3 denominator:** the declared control's absolute reorg changed from ~7 MB
(pre-fix) to **30.72 MB** (deterministic across seeds). The pre-fix global actuator STARVED the
big seed regions (`U|Customer` 26 × 5.9 MB, `U|Stock` 127 × 5.2 MB) — their pairs were never the
cheapest while the small regions kept re-merging. The fix removes that starvation: every region
does its FIRST merge under the unchanged slice-27 count gate, so declared now folds one pair in
each big region too (11.4 + 10.3 + 5.2 MB). That is legitimate folding (read-amplification
reduction), and the T3 ratio is re-measured against this new declared denominator. It is reported
verbatim rather than hidden.

## Residual items

- **Lag-only sustained still 0.** The remaining static-derived/learned rung-50 breaches are the
  read-ahead startup warm-up (windows 0-1) plus shared-box noise (ticket 67/68 residual), not the
  reorg oscillation this package removes.
- **Declared/static absolute reorg shifted up** (7 → 30.7 MB declared, 20.7 → 24.7-29.9 MB
  static) for the reason above; re-measured, not a regression in the T3 ratio.
- **4 seeds, shared box, no cross-lane lock** (loadavg 4.9-8.7, a resident `llama-server` at
  ~100% CPU; `seed42` overlapped the sibling `wave16/startwarm` `seed42-repeat` until 11:14):
  timings are direction-only under contention, per the orchestrator; the deterministic
  byte/count numbers are the primary evidence. The clean serial measurement is WP-S after
  integration.
- Service ceiling remains device-bound (both-ceilings 0 everywhere); the open §7.1 reconciliation
  is untouched and not edited.
