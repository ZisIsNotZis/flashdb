# 60 - warm the win workload's movement scan reads (wave9 WP-I)

Contract: make the win workload's SCAN reads part of the read-ahead warm set so scan latency stops
becoming schedule lag. WP-G (ticket 58) warmed unique-prefix lookups off the pacer; WP-H (ticket 59)
found the win mix is ~30% `movements_scan`, and `request_read_lookups` returns `[]` for it, so the
scan does cold full-entity reads synchronously on the pacer and no arm sustains rung 50. Acceptance:
on the win workload, scan-attributed lag breaches are removed (rung-50 lag-rej drops materially
toward 0) with no regression on the point-lookup workloads and no read/write amplification blowup;
OR the exact remaining cause with evidence. Never overstate.

## Diagnosis (checkpoint before implementation)

The win mix is `{stock: 20, customer: 10, scan: 30, write: 40}` (`harness/flashdb_harness/cold_0b.py`,
`WIN_MIX`), and each shape's read path (`engine/src/request.rs`):

| shape | share | read path | read set |
|---|---:|---|---|
| stock | 20% | `get Stock where sku/loc` | `unique_lookup(U|Stock)` + `get(P|Stock)` — bounded point probe |
| customer | 10% | `find Customer email` | `unique_lookup(U|Customer)` + `get(P|Customer)` — bounded point probe |
| **scan** | **30%** | `find StockMovement where order_no=X order=[movement_no] limit=100` | **`scan_primary(P|StockMovement)` — a full-entity primary scan** |
| write | 40% | `cust -> take` order flow | customer/order/backorder/stock unique probes + writes |

The movement scan shape carries `where.order_no` but the catalog indexes `StockMovement` only by
`movement_no` (unique) and `stock` (reverse) — there is **no `order_no` index** (`request.rs`
`MOVEMENT` catalog). So `execute_request`'s `ReadKind::Movements` branch runs
`engine.scan_primary(MOVEMENT.entity, …)`: it walks the **whole `P|StockMovement` primary prefix**,
parses every live movement document, keeps only those whose `order_no` matches, and retains the
top-100 by `(movement_no, handle)`. Each order has exactly one movement, so each scan returns ≤1 row
but still reads and parses the entire entity.

Measured read set (decoded from the active tiles of `/tmp/cold51/per_region-mixed`, marker/entity
read from each tile's first fence key): `P|StockMovement` = **6 tiles, 4,786,257 B** at seed ≈ ~1200
4 KiB blocks (15 KB + 4 × 1.03 MB + 0.65 MB). It reads only the `P` family of this one entity; the
memtable slice is RAM-resident. The filter/order are applied after the read, so the read set is
independent of the request's `order_no`: every movement scan reads the same whole prefix.

## Design (chosen): memoized per-snapshot full-prefix scan warm

Extend the WP-G read-ahead worker with a second warm kind, still purely observational (read-only,
results discarded, never feeds a decision); the authoritative scan stays inline on the pacer.

- `engine/src/request.rs`: `request_scan_entities(req) -> Vec<&'static str>` — `["StockMovement"]`
  for the movement-scan shape, `[]` otherwise. `request_read_lookups` is unchanged.
- `engine/src/readahead.rs`: `TileReadView::warm_scan_prefix(entity, &mut ScanWarmState)` walks the
  entity's whole primary prefix (`P|entity|0`, the exact prefix `scan_primary` walks) across every
  tile that `could_contain_prefix` admits, using the same `prefix_cursor` block reads; a
  `ScanWarmState` memoizes per tile snapshot so a full-entity scan is warmed **once per published
  tile set, not once per scan request** (2400 × 4.8 MB would be a second full-scan stream).
- The worker calls it for each `request_scan_entities` result after the existing `warm_lookup` loop.

### Rejected alternatives (recorded)

- **(a) extract the scan's read set in `request_read_lookups` and warm it per request**: the read set
  is the whole entity, so warming it per scan request is exactly the forbidden second full scan. The
  memoized version here is the sound form.
- **(b) add a `U|StockMovement.order_no` index** so the scan becomes a bounded point probe: changes
  the catalog, adds a unique-index op to every movement write, and grows write amplification and the
  durability surface. Out of scope for this bounded read-ahead package.
- **(c) make the scan read/parse itself overlapped/async**: needs the `Engine` `Sync` refactor and
  shared serving-memtable snapshotting WP-G already rejected, and it breaks the invariant that
  authoritative reads stay inline. Rejected.
- **(d) warm only a bounded predicted subrange**: there is no range — the `order_no` filter runs
  after the read, so any correct warm must page in every movement document. Rejected.

## Change (implemented, commits `6695493`)

1. `engine/src/request.rs`: `request_scan_entities` + test
   `request_scan_entities_names_only_the_full_entity_scans`.
2. `engine/src/readahead.rs`: `ScanWarmState`, `TileReadView::warm_scan_prefix`, the worker call, and
   test `scan_prefix_warm_is_memoized_per_snapshot_and_reads_the_whole_entity`.

## Measurement (c51, 12.25x, `cp -a` copies, one process at a time)

Protocol: release binaries built once each and copied
(`before` = base `59b01bc`, sha256 `77d3b0ef…`; `after` = `6695493`, sha256 `16ef9b58…`). Harness
`cold_0b.py --workloads win --arms declared --seed 42`, `--cap-mib 128 --window-ms 1000
--group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864
--reference-rate 50 --segment-requests 2000 --ceiling-us 10000 --lag-ceiling-us 10000`. Load
recorded at run boundaries: before `13.5` (a parallel 6-way evaluation + llama-server + mpv), after
`9.4` (llama-server + mpv only). A separate wave9-churn evaluation was running later (not during
these two win runs).

### Rung 50 (the acceptance rung)

| run | achieved_rps | frac | lag-rej | svc-rej | lag p99 (us) | svc p99 (us) |
|---|---:|---:|---:|---:|---:|---:|
| before (base `59b01bc`) | 49.98 | 0.9995 | **38/38** | 38/38 | 427,250 | 856,857 |
| after (`6695493`) | 50.00 | 0.9995 | **39/40** | 39/40 | 298,616 | 8,316 |

**The scan warm does not move the lag ceiling: 38/38 vs 39/40 lag-rejected.** The difference is box
noise, not a regression and not an improvement.

### Whole run

| run | achieved_rps | late_total | lag-rej | svc-rej | compact_bytes | merges | elapsed |
|---|---:|---:|---:|---:|---:|---:|---:|
| before | 64.0 | 7,368 | 96 | 96 | 3,662,893 | 8 | 124.8 s |
| after | 69.0 | 6,918 | 94 | 94 | 7,198,067 | 13 | 116.7 s |

### Why warming is the wrong lever (the exact remaining cause, measured)

The authoritative scan still reads `P|StockMovement` 10,858,394,372 B (10.86 GB) in BOTH runs
(identical — the inline read is unchanged, as required). But the scan's page-in was never the
bottleneck:

| run | logical reads (rchar) | device reads (read_bytes) | page-cache hit rate | pacer read phase (read_us) |
|---|---:|---:|---:|---:|
| before | 11.77 GB | 128.4 MB | **98.9%** | **117.3 s** |
| after | 12.17 GB | 131.4 MB | **98.9%** | **103.4 s** |

- **98.9% page-cache hit rate before AND after**: the scan re-reads the same ~4.8 MB entity every
  ~67 ms, so it was already self-warming; the device did only ~128-131 MB of actual reads over the
  whole run, not the 10.9 GB of logical scan reads. The `request_read_lookups`/warm path was never
  the dominant scan cost.
- The pacer's read phase (`read_us` = group submit elapsed − WAL append − memtable apply, i.e. the
  inline `execute_request` cost) is **~103-117 s over a ~117-125 s run** (~89-94% of wall time) and
  is **unchanged** by page-cache warming. That phase is **CPU-bound**: for a scan it is the cursor
  iteration + `serde_json::from_slice` of every movement document + the `order_no` filter, all on
  the single pacer thread.
- `P|StockMovement` logical reads (10.86 GB) are 89% of the run's rchar, so the scan dominates this
  inline CPU cost. The scan shape has no `order_no` index, so this full parse is inherent.

**Exact remaining cause:** the win scan's inline CPU cost (full-entity cursor walk + per-document
JSON parse + filter on the single pacer) saturates the pacer, independent of page-cache warmth.
Read-ahead warming cannot remove inline CPU work. Removing it requires either (b) an `order_no`
index (a write-path/schema change, out of scope) or (c) overlapping the scan's read+parse off the
pacer (breaks the "authoritative reads stay inline" invariant).

### Regression / amplification

- Point-lookup workloads: no regression mechanism — `request_scan_entities` returns `[]` for every
  adversarial shape (stock get, customer find, order flow; pinned by the new Rust test), and the
  point-lookup warm path is byte-for-byte unchanged. No live adversarial re-run was attempted
  because a separate wave9-churn adversarial evaluation was running on the same box (an
  uninterpretable concurrent measurement); WP-H already measured adversarial declared 0/40 (both
  seeds) under this engine.
- Read/write amplification: no blowup. Device reads 128.4 → 131.4 MB (+2.3%, the scan re-warm's
  cold portion). Compaction 3.7 → 7.2 MB / 8 → 13 merges is within the box's run-to-run compaction
  noise (WP-H measured win declared 4.3-11.5 MB across runs); the change does not touch compaction.

## Judging views (both reported; the §7.1 reconciliation stays OPEN)

- **Sustained-by-lag-ceiling-only:** rung 50 NOT sustained — before 38/38, after 39/40 lag-rejected.
  The scan warm did not remove the scan-attributed breaches because they are CPU-bound, not
  page-in-bound.
- **Sustained-by-both-ceilings:** NOT sustained (service ceiling still rejected in ~38-39/40 rung-50
  windows by the durability barrier itself, unchanged). The primary-vs-service ceiling reconciliation
  remains the OPEN AUTHOR ITEM; nothing here edits the pre-registration or `docs/2_GOAL.md`.

## Acceptance verdict

**NOT MET on the lag metric, with the exact remaining cause evidenced.** The scan-attributed rung-50
lag breaches are NOT removed (38/38 → 39/40) because the win scan's cost is inline CPU (full-entity
parse), not page-in; the read-ahead warm path was already covering the page-in implicitly via the
scan's self-warming (98.9% hit rate before AND after). No point-lookup regression and no read/write
amplification blowup. The sound fixes for the remaining cause are the out-of-scope (b) `order_no`
index or (c) overlapped scan; both are recorded above.

## Gates (verbatim, run at final HEAD `6695493`)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.56s
```
2. `cargo test --workspace --offline` — totals: **293 passed; 0 failed; 0 ignored**
   (126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests;
   +2 tests from this ticket).
3. `cd harness && uv run --with pytest pytest -q`:
```
132 passed in 6.92s
```
4. `git diff --check` — silent (exit 0).

## Residual / next steps (not implemented here)

- The real fix for the win scan is **out of scope for a read-ahead package**: add a
  `U|StockMovement.order_no` index (write-path change) or overlap the scan's read+parse off the
  pacer (breaks the inline-read invariant). Both are product/architecture decisions to be planned as
  their own work package.
- The scan-warm extension itself is sound and bounded and is retained: it warms the scan pages off
  the pacer and re-warms on tile refresh, which removes the first-scan page-in and any transient
  eviction, at ~+2.3% device reads. It is necessary-but-insufficient.
