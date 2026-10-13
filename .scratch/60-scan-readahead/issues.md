# 60 - warm the win workload's movement scan reads (wave9 WP-I)

Contract: make the win workload's SCAN reads part of the read-ahead warm set so scan latency stops
becoming schedule lag. WP-G (ticket 58) warmed unique-prefix lookups off the pacer; WP-H (ticket 59)
found the win mix is ~30% `movements_scan`, and `request_read_lookups` returns `[]` for it, so the
scan does cold full-entity reads synchronously on the pacer and no arm sustains rung 50. This is a
readiness gap, not a thesis verdict. Acceptance: on the win workload, scan-attributed lag breaches
are removed (rung-50 lag-rej drops materially toward 0) with no regression on the point-lookup
workloads and no read/write amplification blowup; OR the exact remaining cause with evidence. Never
overstate.

## Diagnosis (checkpoint before implementation)

The win mix is `{stock: 20, customer: 10, scan: 30, write: 40}` (`harness/flashdb_harness/cold_0b.py`,
`WIN_MIX`), and each shape's read path (`engine/src/request.rs`):

| shape | share | read path | read set |
|---|---:|---|---|
| stock | 20% | `get Stock where sku/loc` | `unique_lookup(U|Stock)` + `get(P|Stock)` — bounded point probe |
| customer | 10% | `find Customer email` | `unique_lookup(U|Customer)` + `get(P|Customer)` — bounded point probe |
| **scan** | **30%** | `find StockMovement where order_no=X order=[movement_no] limit=100` | **`scan_primary(P|StockMovement)` — a full-entity primary scan** |
| write | 40% | `cust -> take` order flow | customer/order/backorder/stock unique probes + writes |

The movement scan shape (`_movements_scan` in `cold_0b.py`) carries `where.order_no` but the catalog
indexes `StockMovement` only by `movement_no` (unique) and `stock` (reverse) — there is **no
`order_no` index** (`request.rs` `MOVEMENT` catalog). So `execute_request`'s `ReadKind::Movements`
branch runs `engine.scan_primary(MOVEMENT.entity, …)`: it walks the **whole `P|StockMovement` primary
prefix**, parses every live movement document, keeps only those whose `order_no` matches, and retains
the top-100 by `(movement_no, handle)`. Each order has exactly one movement, so each scan returns ≤1
row but still reads the entire entity.

Measured read set (decoded from the active tiles of `/tmp/cold51/per_region-mixed`, marker/entity read
from each tile's first fence key):

- `P|StockMovement` = **6 tiles, 4,786,257 B** at seed (15 KB + 4 × 1.03 MB + 0.65 MB) ≈ **~1200
  4 KiB blocks**. It grows modestly over a run (the run writes ~3200 more movements; compaction keeps
  the tile count bounded).
- The scan reads **only** the `P` family of this one entity — no `U`/`R`, and the memtable slice is
  already RAM-resident (nothing to page in). The filter/order are applied after the read, so the read
  set is **independent of the request's `order_no`**: every movement scan reads the same whole prefix.
- WP-H measured `P|StockMovement` read **10.9 GB over the run** ≈ 2400 scans × ~4.5 MB, confirming the
  full-entity scan is the dominant scan cost.

Why it breaches the lag ceiling: at 128 MiB cap vs 1.6 GB data every page-cache line is cold. The
4.8 MB scan page-in happens synchronously on the single pacer and cascades into the next group's
schedule lag (same mechanism WP-G removed for point probes). `request_read_lookups` returns `[]` for
this shape (asserted by `request_read_lookups_extracts_the_probes_each_shape_pays`), so the read-ahead
worker never warms it.

## Design (chosen): memoized per-snapshot full-prefix scan warm

Extend the WP-G read-ahead worker with a second warm kind. The worker still parses the same trace and
stays purely observational (read-only, results discarded, never feeds a decision); the authoritative
scan stays inline on the pacer with newest-first memtable-over-tile precedence.

- `engine/src/request.rs`: add `request_scan_entities(req) -> Vec<&'static str>` — returns
  `["StockMovement"]` for the movement-scan shape and `[]` otherwise. `request_read_lookups` is
  unchanged (the scan still has no unique prefix).
- `engine/src/readahead.rs`:
  - `TileReadView::warm_scan_prefix(entity, &mut ScanWarmState)`: walks the entity's whole primary
    prefix (`P|entity|0`, the exact prefix `scan_primary` walks) across every tile that
    `could_contain_prefix` admits, using the same `prefix_cursor` block reads the authoritative scan
    performs, and discards the entries.
  - `ScanWarmState` memoizes "which entity prefixes are already warm for the current tile snapshot"
    (`Arc<Vec<Arc<Tile>>>` identity + a `HashSet<Vec<u8>>`). A snapshot change (publish/compaction
    replaced tiles) resets the memo on the next scan warm.
  - The worker calls `warm_scan_prefix` for each `request_scan_entities` result, right after the
    existing `warm_lookup` loop.

**The bound.** The warm work is the entity's own primary data (~4.8 MB at seed), warmed **once per
published tile set**, not once per scan request. Without the memo the worker would re-read the entity
on every movement scan (2400 × 4.8 MB ≈ 11.5 GB), i.e. a second full-scan stream that saturates the
device; the memo is what keeps read-ahead from becoming that. The one-time cost lands off the pacer,
in the worker's headroom (at rung 50 the worker runs ~5× ahead of the pacer), and subsequent scans in
the same snapshot are page-cache hits re-touched by the authoritative scans themselves.

**Contingency (not implemented speculatively).** If the first scan warm's one-time ~4.8 MB read
contends with the early WAL fsync/append the way WP-G's un-paced startup warm did, spread it across
worker ticks with a per-tick tile/byte budget (a persistent scan-warm cursor). The measurement will
tell; do not add the pacing before it is shown to be needed.

### Rejected alternatives (recorded)

- **(a) extract the scan's read set in `request_read_lookups` and warm it per request**: the scan's
  read set is the whole entity, so warming it on every scan request is exactly the forbidden second
  full scan. The memoized version here is the sound form of this idea.
- **(b) add a `U|StockMovement.order_no` index** so the scan becomes a bounded point probe: changes
  the catalog, adds a unique-index op to every movement write, and grows write amplification and the
  durability surface. It is a schema/write-path change, not a read-ahead warm change, and is out of
  scope for this bounded package.
- **(c) make the scan read itself overlapped/async**: needs the `Engine` `Sync` refactor and shared
  serving-memtable snapshotting that WP-G already rejected for a decision-returning reader, and it
  would break the invariant that authoritative reads stay inline. Rejected.
- **(d) warm only a bounded predicted subrange of the scan**: there is no range to predict — the
  `order_no` filter runs after the read, so any correct warm must page in every movement document; a
  partial warm would still leave cold blocks on the pacer. Rejected.

## Change (to be implemented and committed after this checkpoint)

1. `engine/src/request.rs`: `request_scan_entities` + a test that the movement scan is the only scan
   entity and point shapes return `[]`.
2. `engine/src/readahead.rs`: `ScanWarmState`, `TileReadView::warm_scan_prefix`, the worker call, and
   a test that a fresh snapshot warms once and the memo suppresses a second warm until the snapshot
   changes.

## Measurement protocol (run only after the implementation is built once and committed)

- Instance: `cp -a` copies of `/tmp/cold51/per_region-mixed` (12.25x); if time permits also
  `/tmp/cold44/per_region-mixed` (8.17x). Build once, copy the binary, run only that copy, one process
  at a time.
- Harness: `cold_0b.py --workloads win --arms declared` for a before/after on the win workload; the
  point-lookup workloads must not regress (the adversarial-uniform workload, scan share 0, is the
  regression sentinel if time permits).
- Report per-rung `achieved_rps`, `lag-rej`, `svc-rej`, lag p99, and the breach attribution. The box
  is shared/noisy: record load and repeat a run. Judging is reported BOTH sustained-by-lag-only and
  sustained-by-both-ceilings; the primary-vs-service ceiling reconciliation stays the OPEN AUTHOR
  ITEM (nothing edits the pre-registration or `docs/objective.md` thresholds).

## Gates (verbatim, to be run at final HEAD)

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q`.
4. `git diff --check` — silent.
