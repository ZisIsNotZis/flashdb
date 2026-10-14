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

<!-- filled after implementation; see the implementation commit -->

## Tests

<!-- filled after implementation -->

## Measurement

<!-- filled after measurement -->

## Gates (verbatim)

<!-- filled after the final HEAD -->

## Before/after tables

<!-- filled after measurement -->

## Acceptance verdict

<!-- filled after measurement -->

## Residual items

<!-- filled after measurement -->
