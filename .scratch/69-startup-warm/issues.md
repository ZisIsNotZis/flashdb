# 69 - WP-R: eagerly warm the win scan + its index at run start

## Contract

Work package R1 of the program. The only remaining rung-50 lag-ceiling breaches for the frozen
`static-derived` win layout are the read-ahead worker's STARTUP warm-up: at windows 0-2 the scan
prefix and the adopted `order_no` index tile are not yet warm, so the first scans take cold page-ins
on the pacer (`lag_breach_attribution reads=N boundary=0`). WP-Q (ticket 68) measured static-derived
win rung-50 = 3/2/2/3 out of 40, all in windows 0-2; learned 13/4/3/3 with the extra seed42 blowup
from a separate merge-oscillation (sibling lane R2).

**Objective:** eliminate the startup warm-up breaches with the smallest sound change: warm the
relevant scan prefix (and the adopted index tile) EAGERLY at engine open / run start, before the
first window, rather than waiting for the background worker's first cadence; or make the driver
perform a bounded synchronous warm-up pass before the timed windows begin.

**Preserve all invariants:** the warm path is read-only and off the correctness path; authoritative
reads stay inline; durability untouched; the warm work is bounded (no second full scan on the
request path). **Do NOT game the metric** (no extending/skipping the timed window, no pre-issuing
requests).

**Acceptance (verbatim):** on the win workload with the index adopted, `static-derived` rung-50
lag-rej reaches 0/40 (across >=3 seeds), and learned is within a small band of static-derived (its
extra breaches being the separate R2 oscillation); throughput must not regress. OR the exact
remaining cause with evidence. Never overstate; label pilot-grade.

Judging: the author's ADOPTED reading is §7.1 lag-only primary; report both that and both-ceilings.
Do not edit the pre-registration.

## Diagnosis (checkpoint, committed before implementation)

### Code path (this worktree, HEAD `e78d036`)

- `engine/src/readahead.rs::TileReadView` warms point lookups (`warm_lookup`) and full-entity scan
  prefixes (`warm_scan_prefix`, memoized per snapshot) off the pacer. `Engine::tile_read_view`
  (engine.rs) appends the adopted `order_no` index tile to the warm view (ticket 67's WP-P follow-on),
  and `request_read_lookups` emits the `order_no` index probe for a movement scan (request.rs).
- `engine/src/bin/flashdb-load.rs::run` startup order (HEAD):
  1. open engine + install policy (`engine.set_layout_policy` at open);
  2. spawn compaction worker;
  3. **spawn read-ahead worker with `engine.tile_read_view()` — view WITHOUT the index**;
  4. spawn the async `order_no` index build (if `order_no_index_wanted`);
  5. `t0 = Instant::now()`; run loop starts.

### The named cause, with the two distinct defects

The index build/adopt and the worker's index view both happen lazily, one window boundary too late:

1. **The index is adopted only at the FIRST window boundary.** `manage_order_no_index` runs inside
   `if args.metrics_json` at each window boundary, and the async build (spawned at open) is adopted
   there. So during window 0 the authoritative movement scan runs WITHOUT the index: the full
   30,000-doc `P|StockMovement` scan on the pacer (~38-43 ms/scan CPU + cold page-ins). WP-P measured
   window 0 `read_us` 503-757 ms — that is the full scan, not a single cold probe.
2. **The read-ahead worker's view lacks the index until the boundary AFTER adoption.** `ra.refresh`
   runs at the window boundary BEFORE `manage_order_no_index`, so after the window-0 adoption the
   worker still warms against the no-index view through all of window 1. Window 1's movement scans
   then cold-page-in the index tile on the pacer (WP-P measured window 1 `read_us` 26-28 ms).

Together they produce static-derived windows 0-1 (and the harness's observed 0-2). The device fsync
tail is NOT involved (WP-P proved no fsync >50 ms becomes a lag breach). The mechanism is
read-warm-up, exactly the scope of this package.

### Why the existing worker cadence is not enough

The worker is spawned just before `t0` with a stale (no-index) view and paces warm at 4 ms/request;
the first dispatch is ~20 ms later. Even a fast worker cannot fix defect 1 (the index is simply not
adopted during window 0) or defect 2 (its view excludes the index until the next boundary refresh).
Eager adoption + eager warm at open removes both, without touching the worker's steady-state cadence.

## Design (chosen)

1. **Eagerly build + adopt the index at open when the frozen policy wants it** (`order_no_index_wanted`
   at open). A bounded, synchronous, one-time build/fsync/adopt BEFORE `t0`, so window 0 already
   serves the index. The same build already runs async at open today; this only moves its completion
   ahead of the first dispatch. `record_order_no_index_build` still runs, so the build bytes land in
   the first window's metrics exactly as before (first snapshot is at window 0 boundary).
2. **Bounded synchronous warm-up pass before the timed windows** (`TileReadView::warm_startup`):
   walk the movement `P|StockMovement` primary prefix (the scan fallback read set) AND, when the
   index is adopted, the whole `U|StockMovement|order_no` index-tile prefix. ~4.78 MB + ~2.14 MB,
   one-time, read-only, results discarded. This makes the very first scan/index probe a page-cache
   hit independent of worker pacing.
3. **Spawn the read-ahead worker AFTER the eager adopt** so its initial view already includes the
   index tile, and it keeps the point-probe lead warm from window 0.

The steady-state `manage_order_no_index` (async build/adopt for the learned arm's later toggles) is
unchanged. The learned arm starts index-OFF (demand gate, ticket 66), so its own window 0-1 full
scan is the learner's designed observation window and is NOT warmed away here (out of scope; recorded
below).

### Rejected alternatives

- **(a) Lengthen the worker's initial lead / reduce the warm pace.** Does not fix defect 1 (the
  index is absent during window 0 regardless of pace) or defect 2 (stale view until the boundary
  refresh). Rejected as not addressing the cause.
- **(b) Refresh the worker's view immediately after `manage_order_no_index` at the boundary.** Fixes
  defect 2 but leaves defect 1 (window 0 full scan). Rejected as partial.
- **(c) Block the run loop's first boundary on the async build.** Still leaves window 0 without the
  index and turns the build into a boundary stall (lag risk). Rejected.
- **(d) Pre-issue / skip / extend the first timed window.** Explicitly forbidden (metric gaming).
- **(e) Warm per request on the request path.** Exactly the forbidden second full scan on the request
  path; the eager pass is a single bounded startup read off the request path. Rejected.

### Invariants argument (state how each is preserved)

1. **Correctness (index never on the correctness path).** Unchanged: the eager adopt installs the
   same `OrderNoIndex` via `adopt_order_no_index`; every hit is cross-checked and every miss/mismatch
   falls back to the authoritative scan. The warm pass discards its results.
2. **Durability.** Unchanged: the build writes via `TileWriter::finish` (sync_data) and the driver
   `sync_all`s the parent directory before adopting, exactly as `manage_order_no_index` does. No
   request ack depends on the index.
3. **Authoritative reads stay inline.** Unchanged: the warm pass only reads tile blocks into the
   page cache; the authoritative scan/probe still runs inline on the pacer.
4. **No second full scan on the request path.** The warm pass is a single bounded startup read
   (~6.9 MB total) BEFORE the timed windows; the request path is byte-for-byte unchanged.
5. **Bounded work.** The eager build is one 30k-entry sort + one tile write (the same build that
   already runs once per adoption); the warm pass is two bounded prefix walks. Both are one-time at
   open, off the timed windows.
6. **No metric gaming.** The timed windows are not extended/skipped; no request is pre-issued. The
   warm-up happens before `t0`/`window_start`, so it is simply off the timed clock (the same rule
   that already puts device probing and WAL replay outside the timed windows).

## Change

Two commits (`599027a` checkpoint, `25fd585` implementation). Files:

1. `engine/src/readahead.rs`:
   - `TileReadView::warm_prefix_range(prefix)` — walks a whole key prefix across every tile that
     could contain it and returns how many rows it yielded (read for page-cache priming only).
   - `warm_scan_prefix` now delegates its prefix walk to `warm_prefix_range` (behaviour unchanged).
   - `TileReadView::warm_startup()` — one bounded synchronous warm-up pass: walks the movement
     `P|StockMovement` primary prefix (the scan fallback read set) and, when adopted, the
     `U|StockMovement|order_no` index-tile prefix (every index block the point probe could touch).
     Returns the two entry counts via the new `StartupWarm` struct.
2. `engine/src/bin/flashdb-load.rs::run`:
   - When the frozen policy wants the index at open (`order_no_index_wanted`), build + fsync + adopt
     it EAGERLY (synchronous, before `t0`), then run `tile_read_view().warm_startup()` as a bounded
     synchronous warm-up pass, and only THEN spawn the read-ahead worker (so its initial view already
     includes the index tile). The steady-state async `manage_order_no_index` is unchanged.

## Tests

- `engine/src/readahead.rs::warm_startup_walks_the_scan_prefix_and_the_adopted_index` — NEW, fails
  pre-change: before adoption the pass walks the scan prefix but `index_entries == 0`; after
  `adopt_order_no_index` it walks the index tile (`index_entries >= 2`). Pins the eager warm the
  driver performs before the timed windows.

## Measurement

Release binary built once from HEAD `25fd585` (`flashdb-load` sha256
`e3c879efcbd8b86441939ae23b7b557b450f6373c11a2eb92e55106dd6031bc8`), used via the harness's
`--release` resolution of `target/release` (no rebuild between seeds; the harness's `build_binaries`
re-run is a no-op). Instance `cp -a` copies of `/tmp/cold51/per_region-mixed` (12.25x =
1,643,925,811 B / 128 MiB). Harness `cold_0b.py --workloads win --arms declared learned
static-derived --lambdas 1.0`, full ladder 50/100/200/400 rps, `--cap-mib 128
--checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864 --ceiling-us 10000
--lag-ceiling-us 10000`. Seeds 42/43/44 (3 independent) + seed 42 repeat (noise band), one arm
process at a time. Every static-derived run printed `startup_warm scan_entries=30000
index_entries=30000` (the eager pass deterministically walked all 30,000 movement docs and all
30,000 index entries).

Box load at boundaries (shared 12-core box; llama-server ~99% CPU + mpv ~91% CPU present
throughout):

| boundary | time | loadavg 1/5/15 |
|---|---|---|---|
| seed42 start | 10:43:40 | 8.19 / 7.40 / 6.80 |
| seed43 start | 10:51:13 | 10.16 / 8.67 / 7.51 |
| seed44 start | 11:00:37 | 6.62 / 6.97 / 7.21 |
| seed42-repeat start | 11:09:58 | 6.86 / 6.18 / 6.70 |
| seed42-repeat end | 11:19:50 | 7.96 / 7.29 / 6.91 |

### Serialization / contention note (supervisor guidance, added after the runs)

**All four runs above are UN-lock-protected** (they completed before the shared-box serialization
rule was issued). The sibling lane `wave16/oscillation` was measuring concurrently, which
contaminates the lag ceiling. Under the rule "wrap the whole run in `flock /tmp/wave16-measure.lock`",
these lag numbers must be read as **direction-only under contention**; the decisive serial
measurement is WP-S after integration. The deterministic evidence is unaffected by contention and
remains load-bearing: `startup_warm scan_entries=30000 index_entries=30000` in every run, and
windows 0-2 `lag_breach_attribution total=0` in every run (the startup warm-up is gone regardless of
contention). The mid-run residual (windows 28-36) is exactly the window class contention inflates,
so its attribution to "compaction-adjacent cold reads + device fsync tails" is provisional, not a
clean named cause.

## Gates (verbatim, final HEAD `25fd585`)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.44s
```
2. `cargo test --workspace --offline` — totals: **303 passed; 0 failed; 0 ignored**
   (129 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 12 group_commit + 6 manifest_engine + 7 merge_threshold + 15 metrics +
   33 multi_tile + 7 order_flow + 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0
   doc-tests; +1 test from this ticket).
3. `cd harness && uv run --with pytest pytest -q`:
```
140 passed in 4.60s
```
4. `git diff --check` — silent (exit 0).

## Before/after tables

"Before" = WP-Q integrated HEAD (ticket 68); "after" = this package (`25fd585`). Win workload,
rung 50, 40 windows.

**static-derived rung-50 lag-rejected windows** (the acceptance rung):

| run | before (all windows 0-2) | after (windows, all mid-run) |
|---|---:|---:|
| seed42 | 3/40 | 3/40 (28, 34, 36) |
| seed43 | 2/40 | 2/40 (29, 30) |
| seed44 | 2/40 | 1/40 (29) |
| seed42-repeat | 3/40 | **0/40** |

**First-3-window lag p99 (us) — the startup warm-up that was the named cause:**

| run | before window 0/1/2 | after window 0/1/2 |
|---|---|---|
| seed42 | breaching (WP-Q) | 2,626 / 4,407 / 2,616 |
| seed43 | breaching (WP-Q) | 931 / 2,399 / 110 |
| seed44 | breaching (WP-Q) | 446 / 1,608 / 129 |
| seed42-repeat | breaching (WP-Q) | 3,064 / 3,022 / 556 |

Every after-run's window 0/1/2 `lag_breach_attribution total=0` and `lag_ceiling_breached=false`:
the startup warm-up is gone. The residual breaches are all mid-run (windows 28-36), adjacent to the
compaction merges that grow ~826 KB-1.3 MB in the second half of the run (see residual cause).

**Achieved throughput (aggregate, the objective's throughput axis must not regress):**

| arm | before (4 seeds) | after (4 seeds) |
|---|---|---|
| declared | 68 / 75 / 69 / 71 | 69 / 69 / 71 / 72 |
| learned@1.0 | 107 / 107 / 106 / 107 | 106 / 105 / 106 / 106 |
| static-derived | 107 / 107 / 107 / 107 | 107 / 105 / 106 / 106 |

Per-rung achieved (50/100/200/400) static-derived after: 50 / 100 / 199.7-200 / 316.9-398.0 —
unchanged shape; the 400-rung 317-398 spread is the shared-box noise band, not a regression.
learned-vs-declared achieved-throughput win preserved (+43-57% aggregate, matching WP-Q).

## Acceptance verdict

**Startup warm-up: ELIMINATED (the package's objective).** Every after-run has windows 0-2 clean on
the lag ceiling (first-3 lag p99 110-4,407 us, all `total=0`), versus WP-Q where ALL of
static-derived's breaches were windows 0-2. The eager index build+adopt + the bounded synchronous
`warm_startup` pass removed the cold-start full scan and index-tile page-in.

**Rung-50 lag-rej = 0/40 across >=3 seeds: NOT reached — 1 of 4 runs (seed42-repeat) reached 0;
the residual 1-3/40 are a DIFFERENT, evidenced cause (mid-run, not startup).** The remaining
breaches are all in windows 28-36, adjacent to the larger compaction merges (~826 KB-1.3 MB in the
run's second half):
- seed43 window 29/30 `read_us` 116/152 ms with 2.38 MB / 0.9 MB actual device reads (cold
  page-ins on the pacer) and 63/189 ms device `fdatasync` tails;
- seed42 window 28 `read_us` 202 ms with a 437 ms device `fdatasync` tail (`max_lag_us` 158 ms);
- seed44 window 29 is the same single-window compaction-adjacent read stall.
This is the compaction tile-replacement cold-read + device-fsync-tail class ticket 68 called
"shared-box noise, not a named mechanism", NOT the startup warm-up. It is out of scope for a
read-ahead startup package (it is a compaction re-warm / device-durability-tail axis).

**Judging views:** §7.1 lag-only primary — static-derived 3/2/1/0 (was 3/2/2/3), with the startup
windows clean; the strict 0/40 is not reached across all seeds. Both-ceilings — 0 rps everywhere,
unchanged (service ceiling is device-bound; the open §7.1 reconciliation is untouched).

**Honest label:** pilot-grade, and direction-only for lag. 4 runs = 3 independent seeds + one 42
repeat on a shared box (loadavg 5.1-10.2), NOT a 10-seed pre-registered verdict, and NOT
lock-protected against the concurrent `wave16/oscillation` lane (see the serialization note above).
The fix removes the named startup warm-up deterministically (`startup_warm scan_entries=30000
index_entries=30000` and windows 0-2 `total=0` in every run); it does not and cannot claim the
mid-run device/compaction tail, whose lag magnitude is contention-inflated. The decisive serial
rung-50 verdict belongs to WP-S after integration.

## Residual items

- **Mid-run compaction-adjacent cold reads + device fsync tails (1-3/40):** the exact remaining
  cause of the non-zero seeds, evidenced above. Not the startup warm-up; a separate
  compaction-re-warm / device-durability axis, out of scope here.
- **learned's own demand-gated window 0-2 (index OFF until the first scan-demand flip):** the WP-O
  learner design (ticket 66). learned after: 5/4/4/7 vs static 3/2/1/0 — the extra windows are the
  demand-gate full scan (windows 0-2/3, CPU-bound, not warmable) plus the same mid-run noise; on
  seed42-repeat the learner also shows windows 24-39 from the separate `U|Stock` merge oscillation
  (R2 lane).
- Service ceiling remains device-bound (both-ceilings 0 everywhere); the open §7.1 reconciliation is
  untouched.
