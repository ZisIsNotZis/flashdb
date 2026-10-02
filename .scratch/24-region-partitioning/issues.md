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
