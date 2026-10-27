# Slice 43 - placement headroom: dependent random reads per request

**Question.** The learner's surface decides only the SHAPE of a region's storage
(merge threshold, page size, bloom bits). The request level supplies key
co-occurrence, and the physical decision that could exploit it is PLACEMENT - which
key families share a tile, and where region boundaries fall. Placement is a fixed
rule today, so co-occurrence has no action to influence; this is the leading
explanation of the measured zero throughput gain (slice 39: learner +0.0%,
4.13-4.27x write). Before any new axis is added, its headroom is measured here with
deterministic counts.

**Deliverable.** One number per placement: total dependent random reads, reads per
request, and reduction against today's layout. Verdict at the end.

## Method

Metric - **dependent random reads per request**: walk a request's reads in the
order `engine/src/request.rs` issues them; a read counts when it cannot be served
from the same tile as the previous read of that request. The first read always
counts (*dependent depth = 1 + independent fan-out*, `docs/8_DETAIL.md`), so a
request whose reads all land in one tile counts 1. Counts only - no wall-clock.

Trace - the repository's own deterministic inventory workload
(`flashdb_harness.generator.Generator`: an in-repo SplitMix64 rng and an integer
Zipf, the same stream `inventory_workload.py` replays and `load_profile.py` labels
with `classify`). Default: seed 42, 20,000 requests, 5,000 customers, 10,000 SKUs,
8 locations -> 267,651 store keys, 52,081 distinct read keys.

Read sequences are derived from each emitted request exactly as the adapter reads
it (`read_sequence`): a lookup is `unique_key` -> `primary_key`; `order_flow` also
probes the `Order` and `Backorder` unique keys. Shapes and their read counts:

| shape | reads (in order) | reads/req |
|---|---|---|
| order_flow (70%) | U\|Customer, P\|Customer, U\|Order, U\|Backorder, U\|Stock, P\|Stock | 6 |
| stock_lookup (15%) | U\|Stock, P\|Stock | 2 |
| customer_lookup (10%) | U\|Customer, P\|Customer | 2 |
| movements_scan (5%) | one sequential scan of P\|StockMovement | 1 |

Tiles - a fixed-capacity run of `tile_entries` **entries** in the placement's key
order. The key order comes from the placement; the tile capacity is an explicit
assumption and every result is printed for a sweep.

Placements:

- **(a) today** - the baseline. Keys ordered by their key-family prefix, cut at
  per-region `(marker, entity)` boundaries: one tile starts at every region change
  and every `tile_entries` entries within a region (`RegionMode::PerRegion`-style,
  `keys.rs::region_prefix`). **This is not the shipped default** - the shipped
  default is `RegionMode::Global` (`engine/src/engine.rs:57`), one whole-keyspace
  tile per publish. The per-region construction is kept because it encodes the
  region-axis placement decision that co-location would change and is the
  conservative (highest-count) baseline, so it cannot inflate the measured
  reduction. The verifier confirmed the reduction is invariant to the choice:
  recomputed as a Global-style whole-key-order placement the baseline is still
  **94,798** at every tile size and the co-location reduction is unchanged
  (34.76 / 34.76 / 34.77 / 34.82 / 37.08% at 64 / 256 / 1024 / 4096 / 16384).
- **(b) colocate** - family co-location: a row's derived `U|X` and `R|X` entries
  are placed in the same tile as its `P|X` row, so the row's keys are adjacent.
  Rows are grouped by entity; a tile is a contiguous run of rows.
- **(c) freq** - region boundaries chosen from the observed key distribution
  instead of the fixed rule: keys are ordered by observed access count (hottest
  first, ties by engine key order) and cut into `tile_entries`-entry tiles.

Command (from `harness/`):

```
uv run python -m flashdb_harness.placement_headroom
uv run python -m flashdb_harness.placement_headroom --requests 20000 --tile-entries 256 4096
```

Unit test (`uv run --with pytest pytest -q tests/test_placement_headroom.py`) pins
the counting logic on a hand-computed synthetic case (the hand computation is in
the test docstrings) and pins the measured verdict.

## Assumptions (explicit)

1. **Tile size.** A tile holds `tile_entries` entries. Results are shown for
   `tile_entries` in {64, 256, 1024, 4096, 16384}; the headline sizes are 256 and
   4096. The answer depends on it, so the sensitivity is shown, not asserted. The
    design's 8 MB tile is ~4096 entries at ~2 KiB/entry.
2. **Store composition.** The read keys' tile comes from their position in the
   placement's order over a deterministic enumeration of the store: every seeded
   Customer/Stock row, every StockMovement/Order row written by the trace, and
   every (would-be) unique key the flow probes. Composition shifts positions; it
   does not change the conclusion, because every request reads keys of distinct
   regions, so (a) is a region count and is independent of composition. **It is
   also degenerate:** because the module starts a new tile at every region change
   and zero of 20,000 requests reads two keys of one region, the `today` baseline
   is mathematically forced to equal the raw read count (94,798) and is flat at
   every tile size. That is a property of the modelled workload, not an
   independent measurement of today's layout.
3. **Modeled, not measured on the engine.** This is a deterministic count of tile
   touches under a stated layout model, not an engine run. No device or latency
   constants enter; the metric is placement structure only.
4. **Backorders.** The seeded `on_hand` (10^6) is never exhausted by this trace, so
   no Backorder row is written; the flow still probes the (empty) Backorder unique
   key, which is in the read sequence.

## Measured numbers

seed=42 requests=20000 store_keys=267651 distinct_read_keys=52081

| tile_entries | placement | dependent reads | reads/req | vs today |
|---:|---|---:|---:|---:|
| 256 | today (a) | 94,798 | 4.740 | 0.00% |
| 256 | colocate (b) | 61,848 | 3.092 | **34.76%** |
| 256 | freq (c) | 87,458 | 4.373 | 7.74% |
| 4096 | today (a) | 94,798 | 4.740 | 0.00% |
| 4096 | colocate (b) | 61,791 | 3.090 | **34.82%** |
| 4096 | freq (c) | 72,612 | 3.631 | 23.40% |

Sensitivity to tile size (reduction vs today, same trace):

| tile_entries | colocate (b) | freq (c) | tiles (today) |
|---:|---:|---:|---:|
| 64 | 34.76% | 6.84% | 4,184 |
| 256 | 34.76% | 7.74% | 1,051 |
| 1024 | 34.77% | 12.88% | 266 |
| 4096 | 34.82% | 23.40% | 72 |
| 16384 | 37.08% | 28.43% | 19 |

Reading: (a) is flat in `tile_entries` (94,798 -> 4.740 reads/request) because no
request reads two keys of one region; that makes the baseline degenerate - it is
mathematically forced to the raw read count and is flat at every tile size (a
property of the modelled workload, not an independent measurement of today's
layout). (b) is flat and large - family co-location
collapses every lookup's `U` -> `P` pair, which is every read of the three probe
shapes; only `movements_scan` (one region) cannot move. (c) is strongly tile-size
sensitive: ordering by access count separates a `U` from its `P` unless a tile is
large enough to hold both.

## Verdict

**Family co-location (b) would cut dependent random reads per request by about 35%
(34.76-34.82% at both headline tile sizes, 34.8-37.1% across the sweep), stable in
tile size. Whether that converts into a sustained-load (throughput) gain is NOT
established by this count and requires an engine-side experiment. No claim is made
against the pre-registration's 15% threshold: that threshold is defined on
sustained offered load - a throughput quantity - not on dependent reads, so this
count can neither clear nor fail it.** If a read-level criterion is wanted, the
34.8-37.1% reduction clears a 15% read-level bar at every tile size, but that bar
is explicitly a read-level criterion and is NOT the pre-registration's threshold.
Distribution-chosen boundaries (c) clear a 15% read-level bar only at the larger
tile sizes (23.4% at 4096 entries, 28.4% at 16384) and fall below it at 256 entries
(7.7%); today's layout (a) is the baseline (0%).
