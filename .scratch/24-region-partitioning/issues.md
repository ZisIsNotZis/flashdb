# 24 — region-partitioned tiles: the prerequisite for learning per-region policy

Opened 2026-10-01. The author's decision (`.scratch/15-objective-v0/objective-v1.md`) is to learn
**data and access patterns**, scored by sustained throughput under load. This ticket is the
structural prerequisite for that learning to have anything to act on.

## Why partitioning, when write amplification does not demand it

The partition simulation (`harness/flashdb_harness/partition_sim.py`, validated to 1.5% against
measured write_amp) says partitioning is write-neutral once the tile cap is large:

| tile cap | co-located write_amp | partitioned write_amp | partitioning gain |
|---|---|---|---|
| 12 | 4.092 | 7.745 | -89.3% |
| 96 | 1.661 | 1.519 | +8.6% |
| 1024 | 1.000 | 1.000 | +0.0% |

So write cost is NOT the argument. The argument is **reachability**: today one publish produces one
tile holding every region's entries interleaved by encoded key, so every per-region knob is
unreachable - page size, bloom bits and compaction trigger are effectively global. A learner that
observes per-region access patterns has no per-region actuator.

## The measurable hypothesis (why this is worth doing)

Our own measurements give the trade-off a real gradient:

- 64 KiB pages: sequential scan **+41%** (2,232 -> 3,155 MiB/s on scanbench) and fewer bytes, but a
  present-key probe is **4.6x slower** (19.3 -> 88.9 us on one tile, 24.1 -> 35.0 us at four).
- 4 KiB pages are probe-optimal (96.3 us device QD1 vs 117.4 at 16 KiB and 185.8 at 64 KiB).
- Bloom bits: 10 -> 14 removes ~88% of false-positive block reads for a few percent of space;
  disabling it costs orders of magnitude.

The inventory workload contains BOTH shapes by construction: `movements_scan` is a range scan over
one entity, `stock_lookup`/`customer_lookup` are index probes, and the write side has a write-hot
family (`P:Stock`, patched repeatedly) next to append-only ones (`R:Movement`, `U:*`). So the
assignment "which page size and bloom setting for which region" is a genuine function of the access
mix - exactly the "learn access patterns" the author asked for, with a measured gradient.

## Required work

1. **Region = (entity, key family).** Define it from the existing key encoding and publish the
   definition. A publish or compaction output then produces one tile per region (or groups
   regions sharing a policy) instead of one global tile.
2. **Per-region policy application.** `LayoutPolicy` already maps a region name to
   (page_size, bloom_bits); extend it to also carry the compaction/merge parameters, and make both
   write paths apply it per emitted tile. Keep the self-describing property: the reader must never
   need the policy to read a tile (I2) - a tile whose parameters differ from any policy still reads.
3. **Per-region compaction.** `select_run` must not merge across regions (a region's tiers are its
   own), which is what makes a per-region trigger meaningful. Keep the range-preserving manifest
   replacement and the disjoint, strictly-increasing cutoff invariant.
4. **Measurement, not assertion.** Report: write_amp and bytes at cap 1024 with and without
   partitioning on the real inventory flow; and the throughput effect of a mixed per-region page
   assignment (e.g. 64 KiB for the scan-heavy region, 4 KiB for the probe-heavy ones) versus a
   single global setting, using the offered-load pacer so the comparison is at equal offered load.
   State the else-branch count so we know the workload still exercises guards.
5. **Invariants to preserve and test:** every read returns what it returned before; recovery
   (`open_discover`) validates the larger tile list; a rotation or a crash mid-publish leaves no
   partially published region set; and the manifest cap accounting is stated (more tiles per
   publish means the cap is consumed faster - say by how much).

## Non-goal

Not a performance claim in itself. If the measurement shows partitioning costs write throughput
without enabling a better per-region assignment, that result is worth recording and the axis gets
closed - but the reachability argument means the alternative is having no per-region learner at all.

## Implemented (branch slice 24)

**Region rule.** A key's region is `(key family, entity)`: family = the first
byte of the encoded key (`P`/`U`/`R`), entity = the 0-terminated name right
after it. The region prefix is `MARKER | entity | 0` (`keys::region_prefix`),
equal for every key of a region, so key order groups a region's keys and orders
regions by `(marker, entity)`. Two `U` fields of the same entity are one region;
`P|Stock` and `U|Stock` are different regions.

**Ordering invariant.** The active tile list is ordered by `(cutoff, region)`
strictly increasing, where region is the tile's region prefix. A whole-keyspace
(Global) tile spans all regions, so for a legacy list this reduces to strictly
increasing cutoffs. A region-partitioned publish emits one tile per region
present in the `(previous cutoff, cutoff]` window, all sharing `cutoff`, ordered
by region. Each tile covers exactly its region's newest-version-per-logical-key
projection over `(lower, cutoff]`, where `lower` is that region's previous tile
cutoff (or the rotation floor); a whole-keyspace tile keeps the global lower.
Enforced in `open_impl` (region derived from each tile's first key), while
`manifest.rs` was relaxed from strictly-increasing to non-decreasing cutoffs
(on-disk format unchanged) because same-cutoff tiles are now legal.

**Compaction** never spans regions: `select_region_run` groups the list by region
and applies the same `CompactShape` policy to each region's own cutoff-ordered
sizes; `merge_region_run` replaces the chosen run with one tile at the run's
newest cutoff and inserts it at its `(cutoff, region)` position, rewriting the
full ordered reference list. The range-preserving replacement and the disjoint
per-region ranges are kept. `compact()` collapses each region's full sequence to
one tile.

**Mode.** `RegionMode::{Global (default), PerRegion}` (`Engine::set_region_mode`)
is a non-durable measurement/rollout switch; recovery derives each tile's region
from the tile itself, so both layouts reopen under either mode without the mode
being persisted. Per-region policy is read per emitted tile via the tile's region
name; the reader never consults the policy (each tile carries its own page size
and bloom bits).

**Measured (scale driver, 200k docs x 1 KiB, commit 5000, budget_prefix, cap
1024; deterministic):**

| mode | tiles/publish | write_amp | tile bytes total |
|---|---|---|---|
| Global, 4 KiB | 1.00 | 2.133 | 222,604,023 |
| PerRegion, 4 KiB all | 2.67 | 2.137 | 223,437,382 |
| PerRegion, 64 KiB P + 4 KiB U | 2.67 | 2.114 | 218,728,282 |

Partitioning at cap 1024 is essentially write-neutral (+0.4% bytes, from the
per-tile superblock/bloom overhead now duplicated per region); the mixed layout
saves ~1.7% bytes. The offered-load pacer comparison was NOT run: `flashdb-load`
opens with `Engine::open` (no tile discovery, no region/layout env) and
`seed-inventory`/`flashdb-replay` do not publish tiles, so the inventory flow does
not exercise tiles end-to-end in the current harness. The real inventory flow was
run to confirm correctness (20k requests, commit_rate 1.0000, family shares P
56.5% / U 37.2% / R 6.4%). The equal-load comparison needs a harness change:
publish tiles from `seed-inventory` and open the replay/load drivers via
`open_discover`, then run `flashdb-load` twice (global 4 KiB vs mixed per-region)
at one offered rate.

## Result (2026-10-01): partitioning works; the page-size axis is flat on this workload

Region partitioning is in and the harness can finally exercise it end to end: with tiles,
`active_tiles=45` and `serving_memtable_entries=0`, and the same reads return the same rows, while
`--no-tiles` shows 63,000 memtable entries - so reads demonstrably come from tiles (every earlier
"real workload" number could not distinguish the two paths).

The per-region page-size axis, however, measures **flat** here. At equal offered load (scan variants
at 100 and 200 rps, full flow at 120 rps, fresh engine per configuration, in-session device fsync
5.5-6.7 ms), mixed (64 KiB for the scan-heavy region, 4 KiB for probe-heavy) vs uniform 4 KiB is
indistinguishable on p50 (the sign flips between runs); p99 looks 45-50% lower but that is inside
this shared box's between-run spread; and the deterministic tie-break says mixed costs **+1.0% tile
bytes**. The reason is concrete: `movements_scan` is JSON-decode-bound, so block size never reaches
the critical path. Separately, the full-flow A/B is invalid because movements stay memtable-resident
there.

So the axes left with MEASURED gradients are **bloom bits per region** (in-range absent probes:
151.84 -> 1.59 us at 12 tiles) and **compaction parameters per region** (write_amp 4.09 -> 1.66 from
cap headroom). Page size stays available but is documented as flat for these access shapes, and the
axis reopens only with a block-bound scan workload (large-value sequential scan without per-document
JSON decode).

## Review outcome (2026-10-01, reviewer 34a8c2d5): BLOCK on the group contract, P2s fixed

The reviewer blocked on the deferred-barrier group claiming to be an all-or-nothing durability unit
while recovery replays any complete frame appended before the barrier. That was accurate against the
HEAD it read (`d76a9f8`); it was **my slice-23 requirement that was wrong**, and the contract had
already been corrected in `64dde22` (each block is atomic, the group is not; a crash persists the
sequential prefix; nothing unacked is lost; retries are idempotent through the business key).

The reviewer's substantive point that DID stand, and is now fixed: the test named
`torn_tail_before_the_barrier_loses_the_whole_group` truncated the file INSIDE the first group frame,
so it only ever proved that a torn first frame is dropped - it could not distinguish "whole group
lost" from "prefix survives". It is rewritten as
`a_crash_before_the_barrier_persists_exactly_the_complete_prefix`, asserting BOTH cases, including
the one the old test never constructed (a complete first frame survives while the rest of the group
does not).

Other review findings and their dispositions:

- **P2, fixed - region-publish cap overshoot.** `publish_tile` checked only the current count while a
  partitioned publish emits one tile per region, so near the cap it wrote its files, failed inside
  the manifest and **poisoned the writer** - for a condition that is merely "not enough room".
  `write_verified_tiles` now computes the remaining capacity and refuses BEFORE creating the region
  that would overflow, so the failure is a plain `InvalidInput` and the manifest is never touched.
- **P2, fixed - doc drift.** The `tiles` field and `active_tile_cutoffs` said "strictly increasing";
  the invariant is now `(cutoff, region)` strictly increasing with cutoffs merely non-decreasing.
- **P2, documented - `commit_batch` shares the append-then-sync shape.** A failed `sync` can leave
  complete frames that recovery replays even though the batch returned `Err`; the promise that
  "nothing is durable" is bounded to `Conflict`, and the doc comment now says so (nothing acked, so
  retries are idempotent, same rule as the group path).
- **P2, recorded only - a mixed-region tile is not caught.** `tile_single_region` and the ordering
  check look at the first/last key, so a crafted tile whose middle keys belong to another region
  would pass. Not reachable from the writer (region prefix is a key prefix); recorded as hardening.
- **P2, recorded only - a Global tile and a region tile at the same cutoff.** The manifest only
  enforces non-decreasing cutoffs, so a crafted root could admit both; a Global tile does not update
  `region_last`, and a following same-cutoff tile would get an overlapping `lower`. Crafted-only, and
  same-cutoff groups cannot arise from publishes because the CSN advances per commit; recorded.

Verified good by the review: the batch overlay is equivalent to sequential commits for the traced
cases (tombstones, one handle in two blocks, dedup exclusion, whole-batch rejection); chained-root
cross-slot reads are prevented and a half-written chain always fails its `seq`/CRC/index checks; the
A/B selection validates the whole chain before serving; and every other mutating path refuses while a
group is open.
