# 76 - WP-W2: close the rung-50 lag residual (compaction-adjacent cold reads)

## Contract

Work package W2 of the program, dispatched to the single-writer `wave21/rewarm` worktree (branch
`wave21/rewarm`, based on master `8a4ea20`). WP-S (ticket 71) measured the adopted §7.1 lag-only
primary: rung 100 is sustained (static-derived 0/20 in 8/8 seeds), rung 50 is NOT (static-derived
median 2/40, learned median 5/40, declared 39-40/40). The startup warm-up (ticket 69) and the
single-region merge oscillation (ticket 70) are both removed; every residual rung-50 breach is
mid-run (windows 28-36 in WP-Q, scattered mid-run in the decisive run), adjacent to the larger
compaction merges, with cold page-ins on the pacer plus occasional device `fdatasync` tails.

**Objective:** warm newly published compaction output off the pacer so a post-compaction read is
not a cold page-in. Diagnose the exact path first (which reads go cold after a publish: the
movement scan prefix, the `order_no` index tile, both?), then implement the smallest sound fix.

**Preserve all invariants:** the warm path is read-only and off the correctness path; authoritative
reads stay inline; durability untouched; bounded work. **Do NOT game the metric.**

**Acceptance (verbatim):** on the win workload with the index adopted, `static-derived` rung-50
lag-rej reaches 0/40 across >=3 seeds (learned within a small band), with no throughput regression.
OR the exact remaining cause with evidence (e.g. a proven device floor). Never overstate; label
pilot-grade.

**Judging:** §7.1 lag-only primary; report both judging views. Do not edit the pre-registration.

## Diagnosis (checkpoint, committed before implementation)

### Code path (this worktree, HEAD `8a4ea20`)

- `engine/src/readahead.rs` runs a background worker that parses the same trace and warms tile
  blocks for the next `READ_AHEAD_LEAD = 64` requests ahead of the pacer's dispatched position
  (`position`, a `Relaxed` atomic the driver advances per dispatch group). Two warm paths:
  1. `warm_lookup` — per-request unique-index probe plus the follow-on primary `get` (point
     reads). NOT memoized: each request's point reads are warmed exactly once.
  2. `warm_scan_prefix` — the whole `P|StockMovement` primary-prefix walk, memoized per tile
     snapshot in `ScanWarmState` (`Arc::ptr_eq` on the view's `Arc<Vec<Arc<Tile>>>`).
- `engine/src/bin/flashdb-load.rs` applies a finished off-path compaction at the window boundary
  (`apply_compaction_job`, which REPLACES a region's tiles with a NEW tile = a new snapshot) and
  then sends `ReadAhead::refresh(engine.tile_read_view())`. `refresh` is sent EVERY window boundary
  (publish, compaction, and index adopt/clear all go through it).
- `Engine::tile_read_view` builds a FRESH `Arc<Vec<Arc<Tile>>>` on every call. Kept tiles keep
  their `Arc<Tile>` across a compaction apply (the engine moves the kept `TileHandle` as-is and
  only the replacement tile gets a fresh `Arc`); the adopted `order_no` index tile is an
  `Arc<Tile>` sidecar appended OUTSIDE `self.tiles`, so compaction never touches it.

### The named cause, precisely

When the worker adopts a refreshed snapshot, its warm cursor `warm_index` is already
`position + READ_AHEAD_LEAD` (64 requests ahead). Those 64 requests' point reads were warmed
against the OLD snapshot. On refresh the worker keeps `warm_index` and continues warming request
`position + 65` onward against the NEW snapshot, so the 64 requests the pacer is about to serve are
NEVER re-warmed against the new tiles. Their point reads (the unique-index probe and the primary
`get` for whichever region compaction just merged) hit the replacement tile cold, on the pacer.

### Which reads go cold after a publish

- **Point reads (unique-index probes + primary document gets) — THE systematic gap.** Any region
  compaction leaves ~64 requests' point reads cold on the new tile. This matches the decisive-run
  attribution: seed43 static-derived win rung-50 has its single lag breach at window 16, adjacent
  to the 10.16 MB `P|Customer` merge at the window 16 boundary (from the committed
  `.scratch/71-eval-decisive/seed43.json`); the pacer's next ~64 customer/write requests probe the
  new `P|Customer` tile cold.
- **Movement scan prefix — NOT the systematic gap.** `warm_scan_prefix`'s memo resets on every
  refresh because the driver sends a fresh `Arc<Vec<Arc<Tile>>>` each window boundary, so the whole
  `P|StockMovement` prefix is re-walked (off the pacer) at the first movement scan the worker
  processes after each boundary. That re-walk pages in the replacement `P|StockMovement` tile even
  when that region was the one compacted.
- **`order_no` index tile — NOT the systematic gap in static-derived.** It is a sidecar outside
  `self.tiles`, adopted once at open, and never compacted. (A mid-run index rebuild/adopt in the
  learned arm would hit the same stale-lookahead point-read gap, but that is the same root cause,
  not a separate path.)

### Why the existing cadence is not enough

The worker's lookahead is forward-only: it warms `warm_index..position+64` and then advances. A
snapshot change invalidates the blocks it already warmed for that range, but nothing rewinds the
cursor, so the invalidated range is simply skipped. The scan-prefix memo self-heals (it re-walks on
the next movement scan), but the per-request point reads have no such re-warm.

## Design (chosen)

When the worker adopts a `Refresh` whose tile set differs from its current view, rewind the warm
cursor to the pacer's current position so the `READ_AHEAD_LEAD` lead is re-warmed against the new
tiles. A refresh carrying the SAME tile set (the common case: a window boundary where nothing was
published/compacted/rebuild) leaves the cursor alone, so no redundant re-warm is added.

1. `TileReadView::same_tile_set(&self, other)` — compares the two views' `Arc<Tile>` vectors
   element-wise by `Arc::ptr_eq`. Kept tiles keep their `Arc`; replaced/new tiles get a fresh one,
   so pointer identity detects a real snapshot change without touching file contents.
2. `readahead::adopt_snapshot(view, position, warm_index, next)` — the single adoption path used by
   BOTH refresh sites in the worker loop. If `!view.same_tile_set(&next)`, sets
   `warm_index = position.load(Relaxed)` before swapping the view.

### Rejected alternatives

- **(a) Warm the replacement tile inside `apply_compaction_job`.** Puts a read on the request path
  (apply runs on the pacer) and requires the engine to know the warm prefixes; more invasive and
  risks the "authoritative reads stay inline / warm path off the pacer" invariant.
- **(b) Rewind `warm_index` on EVERY refresh.** Re-warms 64 requests at every window boundary even
  when nothing changed (~2x the warm work at rung 50), degrading the worker's lead at higher rungs
  for no benefit.
- **(c) Refresh the worker only when the driver thinks tiles changed.** Duplicates the
  snapshot-change detection the worker needs anyway and adds engine/driver coupling; the worker
  already holds both views and can compare them directly.

### Invariants argument

1. **Warm path read-only and off the correctness path.** Unchanged: the worker still only reads
   tile blocks and discards results; the re-warm is the same warm read, just issued again for the
   invalidated lead.
2. **Authoritative reads stay inline.** Unchanged: the pacer still runs the authoritative
   probe/scan; the worker only pre-primes the page cache.
3. **Durability untouched.** No write/fsync/manifest change anywhere.
4. **Bounded work.** The re-warm is at most the existing `READ_AHEAD_LEAD` (64 requests) per
   ACTUAL snapshot change; a snapshot change happens only on publish/compaction/index-adopt, so the
   total extra warm work is bounded by `leads × changes`, not by window count. The comparison is
   O(tile count) pointer checks per refresh.
5. **No metric gaming.** The timed windows, offered schedule, and dispatch path are byte-for-byte
   unchanged; no window is skipped/extended and no request is pre-issued.

## Change

Two commits (`52d5a5e` checkpoint, `8aa7b29` implementation). Files:

1. `engine/src/readahead.rs`:
   - `TileReadView::same_tile_set(&self, other)` — true when the two views name the
     same immutable tile set (same length, every `Arc<Tile>` pointer-identical). Kept tiles keep
     their `Arc` across a publish/compaction; replaced/new tiles get a fresh one.
   - `readahead::adopt_snapshot(view, position, warm_index, next)` — the single adoption path
     used by BOTH refresh sites in the worker loop. On a real snapshot change it rewinds
     `warm_index = position.load(Relaxed)` before swapping the view, so the already-warmed
     `READ_AHEAD_LEAD` is re-warmed against the new tiles. A refresh carrying the same tile set
     leaves the cursor alone.
   - Both `Msg::Refresh` arms in `worker` now call `adopt_snapshot`.

## Tests

- `engine/src/readahead.rs::adopt_snapshot_rewinds_the_warm_cursor_only_when_the_tile_set_changes`
  — NEW, fails pre-change: `adopt_snapshot` did not exist and the worker adopted a refresh by
  only replacing the view, leaving the warm cursor past the stale lead. The test pins both the
  snapshot-change detection (`same_tile_set`) and the rewind (cursor reset on a real change,
  untouched on a same-set refresh).

## Measurement

Release binary built once from HEAD `8aa7b29` (`flashdb-load` sha256
`7be6b92a3c1a3ec83eb0e3c542de9ef12f096619fb5fe93dfe05fd7308a33849`), copied to
`/tmp/wave21-rewarm-bin/` and verified byte-identical to `target/release/flashdb-load` (the path
`--release` resolves). Instance `cp -a` copy of `/tmp/cold51/per_region-mixed`
(`/tmp/wave21-rewarm/cold51`, 1,643,925,811 B / 134,217,728 B = **12.25x**). Harness
`cold_0b.py --workloads win --arms declared learned static-derived --lambdas 1.0`, full ladder
50/100/200/400 rps, `--cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608
--compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000`. Seeds 42/43/44 (3
independent) + seed 42 repeat (noise band), one arm process at a time. Every seed invocation was
wrapped in `flock /tmp/wave21-measure.lock -c '<full command>'` (all 4 runs lock-protected).

Box load at boundaries (shared 12-core box):

| boundary | time | loadavg 1/5/15 |
|---|---|---|---|
| seed42 start | 14:51:59 | 6.67 / 6.62 / 6.65 |
| seed42 end | 14:56:49 | 5.67 / 6.98 / 6.85 |
| seed43 start | 14:58:37 | 9.15 / 7.75 / 7.13 |
| seed43 end | 15:03:20 | 5.08 / 6.21 / 6.66 |
| seed44 start | 15:05:21 | 6.37 / 6.23 / 6.62 |
| seed44 end | 15:10:38 | 6.16 / 5.97 / 6.39 |
| seed42-repeat start | 15:10:59 | 5.61 / 5.86 / 6.35 |
| seed42-repeat end | 15:16:08 | 3.75 / 5.21 / 5.99 |

No other `flashdb-load/scale/replay` process ran concurrently with any seed (each seed serialized
under the lock; the harness itself runs one arm process at a time).

## Gates (verbatim, final HEAD `8aa7b29` + the record commit)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.73s
```
2. `cargo test --workspace --offline` — totals: **305 passed; 0 failed; 0 ignored**
   (130 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics +
   33 multi_tile + 7 order_flow + 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0
   doc-tests; +1 test from this ticket).
3. `cd harness && uv run --with pytest pytest -q`:
```
140 passed in 4.53s
```
4. `git diff --check` — silent (exit 0).

## Before/after tables

"Before" = WP-S integrated master (ticket 71); "after" = this package (`8aa7b29`). Win workload,
rung 50, 40 windows, `--lambdas 1.0`.

**static-derived rung-50 lag-rejected windows** (the acceptance rung):

| run | before (windows) | after (windows) |
|---|---:|---:|
| seed42 | 3/40 | 1/40 (19) |
| seed43 | 1/40 | 2/40 (20, 22) |
| seed44 | 1/40 | 1/40 (17) |
| seed42-repeat | 2/40 | 1/40 (14) |
| **median** | **2/40** | **1/40** |

**learned@1.0 rung-50 lag-rejected windows** (reported; not the acceptance arm):

| run | before | after |
|---|---:|---:|
| seed42 | 4 | 6 |
| seed43 | 6 | 8 |
| seed44 | 5 | 4 |
| seed42-repeat | 4 | 5 |
| median | 4.5 | 5.5 |

learned's band is unchanged within noise; its extra windows over static-derived are its
demand-gated index-OFF observation window (the WP-O design, ticket 66) plus the same
build-window device tail.

**Achieved throughput (aggregate achieved rps) — must not regress:**

| arm | before (42/43/44/42-rpt) | after (42/43/44/42-rpt) |
|---|---|---|
| declared | 72 / 72 / 70 / 72 | 69 / 73 / 72 / 71 |
| learned@1.0 | 107 / 107 / 106 / 107 | 107 / 107 / 106 / 107 |
| static-derived | 107 / 107 / 106 / 107 | 107 / 107 / 106 / 107 |

No throughput regression on either indexed arm; declared is within its measured noise band
(ticket 71 measured 2.48% max relative diff on the same-trace repeat).

**Mid-run window attribution of the residual rung-50 breaches (after, static-derived)** — every
residual breach is the compaction BUILD window, not the post-publish window:

| seed | breach window | compaction at that window | stall attribution (stderr) |
|---|---|---|---|
| 42 | 19 | none (P|Customer applied at 18, U|Customer at 20) | `stall_fsync_us=12597` `stall_completion_us=13232` `max_lag_us=12580` |
| 43 | 20 | `P|Customer` 10.17 MB | `stall_fsync_us=145356` `stall_completion_us=146205` `max_lag_us=38878` |
| 43 | 22 | `U|Customer` 11.08 MB | `stall_append_us=91243` `stall_fsync_us=9257` `max_lag_us=31673` |
| 44 | 17 | `P|Customer` 10.17 MB | `stall_fsync_us=63071` `read_bytes=59506688` `max_lag_us=34081` |
| 42-rpt | 14 | `P|Customer` 10.17 MB | `stall_fsync_us=313146` `read_bytes=32804864` `max_lag_us=72695` |

The POST-publish windows (the first window after each apply) are clean in all four runs
(lag_p99 0.1-6.2 ms, `lag_ceiling_breached=false`), which is the fix's effect: the first reads of
the replacement tile are now page-cache hits, not cold page-ins.

## Acceptance verdict

**Post-publish cold read (the package's named objective): ELIMINATED.** In all four after-runs the
first window after each compaction apply is clean on the lag ceiling (`lag_ceiling_breached=false`,
lag_p99 0.1-6.2 ms), versus WP-S where the post-publish first reads were the named rung-50
residual. The re-warm also collapsed the build-window lag where a stale lead had been served
(seed42 build windows 77/49 ms -> 1.5/0.2 ms).

**Rung-50 lag-rej = 0/40 across >=3 seeds: NOT reached — static-derived 1/2/1/1 (median 1),
learned 6/8/4/5.** The remaining breaches are a DIFFERENT, evidenced cause: the compaction BUILD
window's device tail, not the post-publish read. Every residual breach window's `window_stall`
line shows the stall group dominated by the device `fdatasync` tail (63-313 ms; the device fsync
p99 is 13.4 ms idle and 71-498 ms under load per tickets 54/56) and/or a 91 ms WAL append plus the
build's own 30-60 MB device I/O under the 128 MiB cap. This is a proven device/working-set floor,
not a named engine mechanism, and it cannot be removed without weakening durability (forbidden) or
reducing the compaction build's own full-tile read set.

**Judging views:** §7.1 lag-only primary — static-derived rung-50 1/2/1/1 (was 3/1/1/2), rung 100
0/20 in 4/4 seeds; the strict 0/40 is not reached. Both-ceilings — 0 rps everywhere, unchanged (the
service ceiling remains device-bound; the open §7.1 reconciliation is untouched).

**Honest label:** pilot-grade. 4 runs = 3 independent seeds (42/43/44) + one seed-42 repeat on a
shared box (loadavg 3.75-9.15), all lock-protected, one process at a time. NOT a 10-seed
pre-registered verdict. The fix removes the post-publish cold read deterministically (post-merge
windows clean in every run); it does not and cannot claim the device fdatasync/append tail of the
compaction build window.

## Residual items

- **Compaction build-window device tail (1-2/40):** the exact remaining cause of the non-zero
  seeds, evidenced above (fdatasync 63-313 ms + 91 ms append + 30-60 MB build I/O under the
  128 MiB cap). Not the post-publish read; a device-durability / compaction-I/O axis, out of scope
  here.
- **learned's demand-gated window 0-2** (index OFF until the first scan-demand flip, WP-O design,
  ticket 66) remains the systematic difference between learned and static-derived.
- Service ceiling remains device-bound (both-ceilings 0 everywhere); the open §7.1 reconciliation
  is untouched.
