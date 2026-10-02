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
