# 64 - WP-M: a learned SECONDARY ACCESS PATH (the axis the thesis is missing)

## Contract

Work package M of the program. The engine-side latency work is done (rung 50 lag-sustained on
point-lookup workloads). WP-K (ticket 62) measured that T1 is **UNDERIVABLE** because the win
workload's `movements_scan` (`find StockMovement where order_no=X`) has **no `order_no` index**, so
it runs a full-entity primary scan of `P|StockMovement` — JSON-parsing every movement doc on the
single pacer (~89-94% of wall time). Both arms fail identically, so the pre-registered win workload
offers **no layout-dependent headroom** to the current learner action space (merge threshold / page
size / bloom bits), and the learner churns. `docs/learning.md` already states the learned surface
must be able to act on PLACEMENT/co-location and that a new axis is added only after its headroom is
measured with deterministic counts, never by intuition.

This package adds exactly that missing axis: a **secondary access path keyed on the observed
predicate** (`order_no` -> StockMovement refs), built and maintained OFF the request path, used by
the authoritative read path only when present and valid, and exposed as a learner-selectable action.

**Acceptance (verbatim from the task):**
- Stage 1's deterministic headroom is measured and recorded (GO/NO-GO).
- If GO and implemented: all invariants preserved (state how each is preserved), all tests green,
  the axis is genuinely learnable (the learner can select it), and a measured before/after on the
  win workload shows material headroom (state the number and the seed count), OR an honest partial
  with the exact remaining cause. Never overstate.

## Stage 1 — deterministic headroom (measured, GO)

Measured on the c51 base instance `/tmp/cold51/per_region-mixed` (12.25x = 1,643,925,811 B / 128
MiB), built by `build_cold_51.py` with `write-requests=30000` (so 30,000 seeded StockMovement docs).
Deterministic counts come from `flashdb-scale tileinfo` on a `cp -a` copy (run 2026-10-14, release
binary `target/release/flashdb-scale`), from the harness trace builder (seed 42, win mix), and from
a release-mode `serde_json` parse micro-benchmark (not committed; `/tmp/parsebench`).

### The scan's current deterministic cost

`P|StockMovement` active tiles (6, in `(cutoff, region)` order; `tileinfo` verbatim):

| cutoff | entries | bytes | page_size |
|---:|---:|---:|---:|
| 5270 | 70 | 15,204 | 4096 |
| 11798 | 6,528 | 1,029,741 | 4096 |
| 18326 | 6,528 | 1,029,744 | 4096 |
| 24854 | 6,528 | 1,029,752 | 4096 |
| 31382 | 6,528 | 1,029,784 | 4096 |
| 35200 | 3,818 | 653,638 | 65536 |
| **total** | **30,000** | **4,777,863** | |

Deterministic facts (measured, not inferred):
- **docs scanned per movement scan = 30,000** (the whole entity; the `order_no` filter runs AFTER
  the read).
- **bytes scanned per movement scan = 4,777,863 B** (the whole `P|StockMovement` prefix).
- **win trace (seed 42) scan share = 2,471 requests** of 8,000 (30.9%; harness `build_trace`,
  `WIN_MIX = {stock:20, customer:10, scan:30, write:40}`; counts `stock=1608 customer=814 scan=2471
  write=3107`).
- **per-run doc parses = 2,471 × 30,000 = 74,130,000**; **per-run logical scan reads ≈ 2,471 ×
  4.78 MB ≈ 11.8 GB** (ticket 60 measured the run's `P|StockMovement` data-block reads at 10.86 GB —
  the delta is directory/bloom bytes that `bytes_read` excludes).
- **per-doc JSON parse = 796.7 ns/doc** (release mode, single thread, 106-byte movement doc,
  `/tmp/parsebench`, 10,000,000 parses / 7.967 s). The scan's inline cost per request is therefore
  dominated by ~30,000 × (~0.8 us parse + cursor-walk + alloc/filter), ≈ **38-43 ms/scan** — which
  is exactly the ticket-60 measured read phase (~103-117 s over ~117-125 s, ~89-94% scan).

### The headroom a `order_no -> handle` secondary path removes

An `order_no` index maps `order_no -> movement handle`. Every order has exactly one movement
(1:1 with `Order.order_no`, which the write path already enforces unique), and movement docs are
insert-once-never-updated, so `order_no -> handle` is a permanent bijection: a present index entry
can never go stale.

Per movement scan the index replaces the full scan with:
- 1 index point probe (`U|StockMovement.order_no` prefix, ~1-2 blocks), +
- 1 primary `get(handle)` (1 block), +
- 1 doc parse (vs 30,000).

| quantity | current scan | with `order_no` index | eliminated |
|---|---:|---:|---:|
| logical block reads / scan | ~4.78 MB | ~70 KB | **~98.5%** |
| docs JSON-parsed / scan | 30,000 | 1 | **29,999** |
| CPU parse / scan | ~38-43 ms | ~0.3-0.5 ms | **~99%** |

Write + space cost: one index tile of 30,000 `order_no -> handle` entries ≈ **~1 MB on disk**
(one-time, built OFF-path). Amortized against 2,471 scans, the build is ~0.4 KB of derived write per
~4.78 MB of scan read it removes.

### GO / NO-GO

**GO.** The deterministic headroom is material on all three axes the objective scores: the scan's
inline CPU (29,999 doc parses/scan) and read volume (~4.78 MB/scan) both collapse to a point probe,
and the one-time write cost (~1 MB) is priced in the existing reorg term. This is not intuition: it
is 30,000 -> 1 docs and 4.78 MB -> ~70 KB, per scan, with the scan measured as the rung-50 lag
blocker in WP-K.

## Stage 2 — design (derived `U|StockMovement.order_no` index tile, off the request path)

The secondary path is a **derived auxiliary tile** reusing the `U` (unique-index) key spelling, built
OFF the request path from the `P|StockMovement` tiles, and held OUTSIDE the engine's main
`self.tiles` list (which is WAL-projection-verified at recovery). It is a pure hint: a hit is
cross-checked against the document's own `order_no`, and a miss / mismatch / absent / corrupt index
always falls back to the existing authoritative scan.

- **Key reuse.** Index entries are written with the existing `keys::unique_key(StockMovement,
  "order_no", order_no_bytes, handle, csn)` / value = `handle.to_be_bytes()`; probed with
  `keys::unique_prefix(...)` + `Tile::prefix_cursor`. No new keyspace family, no change to
  `region_prefix`/`family_of`/recovery ordering.
- **Build (off-path).** A new `engine/src/index.rs`: `TileReadView::scan_primary(entity, |handle,
  doc|)` streams the whole `P|StockMovement` prefix tile-only (the exact prefix `scan_primary`
  walks), JSON-parses each doc once, extracts `(order_no, handle)`, and writes ONE `Tile` via
  `TileWriter` (64 KiB pages, no bloom). `TileWriter::finish` syncs the file; the caller syncs the
  parent dir before adoption, matching the compaction apply ordering.
- **Adopt (off-path, lazy).** The driver builds the index on a background thread after open
  (reusing the `TileReadView` already shared with the read-ahead worker) and calls
  `Engine::adopt_order_no_index(Arc<Tile>)` once the tile + its dirent are durable. Until then, and
  after any teardown, the read path falls back to the scan. No manifest change: the index is a
  sidecar optimization, never part of the durable keyspace (losing it on a crash costs the next
  open a rebuild, not correctness).
- **Read path.** `execute_request`'s `ReadKind::Movements` first calls
  `Engine::lookup_movement_by_order_no(order_no)`:
  - hit `Some(handle)` -> `engine.get(StockMovement, handle)` -> parse -> **cross-check
    `doc.order_no == order_no`** -> return (single row); on any mismatch, fall through to the scan;
  - miss / no index -> fall through to the existing full scan (unchanged).
- **Learner action.** `LayoutCandidate` gains `index_order_no: bool = False`; the policy schema and
  `LayoutPolicy` gain a per-region `index_order_no` field. Only the `P|StockMovement` region gets the
  indexed candidate variants (6 layouts × {off,on} = 12 candidates); every other region keeps the
  published 6 (index is a no-op there), so the global burn-in cost is unchanged. The engine acts on
  `index_order_no` only for `StockMovement`; the driver builds/retires the index off-path as the
  learner toggles it. The reward is the existing unit-anchored, reorg-priced cost: the scan speedup
  lands in the movement region's service term (`count * mean_ns`), and the index build's output
  bytes are attributed to the region's `compaction_bytes` (derived-output write amplification), so
  lambda prices it exactly like a merge.

### Rejected alternatives (recorded)

- **(a) write-path `U|StockMovement.order_no` index (add an op to every movement write).** Correct
  and crash-consistent trivially, but it adds a unique-index op + WAL byte + barrier work to every
  order flow on the single-writer request path, violating constraint (iii) and growing write
  amplification and the durability surface. Rejected.
- **(b) put the index tile into the main `self.tiles` list.** `open_impl` verifies every active tile
  against the WAL projection; an `I`/`U|order_no` tile has no WAL entries, so recovery would fail
  closed ("tile missing/extra WAL entry"). It would also need a new keyspace family and recovery
  ordering changes. Rejected: the index is derived, not WAL-authoritative.
- **(c) in-RAM `BTreeMap` index.** Bounded (30k entries ≈ a few MB) and fastest, but it does not
  reuse the tile machinery the task requires, it is not durable/recoverable, and it would grow with
  the entity. Rejected in favour of a disk tile with the same bounded read path.
- **(d) a bloom-only filter over `order_no`.** A bloom can only prove absence when it fires, and a
  full unique bloom over 30k values answered per scan still requires scanning when the filter
  passes; it removes no inline parse work. Rejected as not addressing the measured CPU-bound cause.
- **(e) overlapped/async scan (read+parse off the pacer).** Already rejected in WP-G/WP-I: it needs
  the `Engine` `Sync` refactor and breaks the "authoritative reads stay inline" invariant. Rejected.

### Invariants argument (state how each is preserved)

1. **Correctness (I2 — layout never on the correctness path).** The index is consulted only as a
   hint; every hit is cross-checked against the document's own `order_no`, and every miss / mismatch
   / absent / corrupt / stale index falls through to the authoritative full scan. A wrong answer
   requires both a corrupt index AND a wrong cross-check, which the scan never performs. The
   authoritative scan and `get` (newest-first memtable-over-tile) are unchanged.
2. **Crash-consistency.** The index tile is written via `TileWriter::finish` (`sync_data`) and its
   parent directory is synced before adoption. No request ack depends on the index (writes ack only
   after the covering WAL `fdatasync`); a crash that loses/tears the index is repaired by the next
   open's rebuild, and in the meantime the scan fallback serves correctly.
3. **No request-path write work.** The index build runs on a background thread; the request path's
   only change is the read-side point probe (strictly less work than the scan it replaces) and the
   cross-check. No op is added to the WAL append/apply/barrier.
4. **Newest-first memtable-over-tile.** Unchanged: hits go through the existing `get(handle)`; the
   fallback is the existing `scan_primary`. The index only short-circuits a positive hit.
5. **Bounded memory.** One index tile; its RAM footprint is the tile directory (fences) plus the
   superblock, independent of doc count beyond the dir size; the build streams and does not
   materialize the entity.

## Status

- [x] Stage 1 deterministic headroom measured and recorded (GO).
- [ ] Stage 2 engine: derived index tile build + read-path fallback + tests.
- [ ] Stage 2 driver: off-path build/adopt + policy knob + metrics attribution.
- [ ] Stage 2 harness: learner action (indexed candidates for `P|StockMovement`).
- [ ] Before/after on win (declared + learned, static-derived if cheap).
- [ ] Gates (verbatim) + acceptance verdict + residuals.
