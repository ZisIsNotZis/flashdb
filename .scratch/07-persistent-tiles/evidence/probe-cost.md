# Probe cost vs. tile size — measured 2026-09-30

`cargo run --release --example probe_cost` on the v0 engine at `dfb147c`
(200 random `unique_lookup` probes per row; one tile published covering all docs;
OS page cache warm, so the numbers are CPU-bound, not device-bound):

| docs | tile bytes | memtable probe | tile probe | slowdown |
|---|---|---|---|---|
| 5 000 | 1.4 MB | 1.4 µs | **29.0 ms** | 21 338× |
| 20 000 | 5.6 MB | 1.6 µs | **110.6 ms** | 70 511× |
| 80 000 | 22.6 MB | 2.0 µs | **450.2 ms** | 225 062× |
| 320 000 | 90.8 MB | 2.4 µs | **1 645.3 ms** | 690 989× |

Two independent facts fall out of the table:

1. **Probe cost is linear in tile size** (≈ 18 ns/byte ⇒ ≈ 55 MB/s effective), because
   `next_matching` (engine.rs:885) walks the tile from its first entry and
   `Entries` (tile.rs:200) *allocates two `Vec`s and checksums every entry* until the
   prefix matches. The memtable side is already indexed (`BTreeMap` range seek, ~2 µs):
   the asymmetry is a missing tile index, not a design defect — `docs/engine.md`
   already prescribes 16 KiB pages.
2. **Even the sequential scan rate is only 55 MB/s** (vs 831 MiB/s measured device
   sequential), i.e. the whole engine currently runs ~15× below the device on its
   strongest path. Cause: per-entry allocation + checksum in the reader, and a single
   whole-file digest instead of per-page CRCs. This is a second, independent fix
   (buffered/zero-copy page reader), and it also makes the data:RAM numbers meaningless
   until fixed.

## What the index does and does not fix

Fixes (implementation gaps):
- unique/point probe: O(log pages + 1 page) per tile instead of O(tile bytes); with
  `K ≤ MAX_TILES = 8` tiles, ~8 page reads worst case.
- per-page CRC so a probe verifies only the page it touches.
- zero-copy buffered reader → scan throughput toward device rate.

Does **not** fix (structural, needs a contract decision):
- Strict global synchronous uniqueness is a read-modify-write on the write path: every
  unique op must consult each tile unless a per-tile bloom says "definitely absent".
  Cost model after the fix ≈ 1 bloom probe, plus ≤1 page read per tile only for
  values that may exist. At the calibrated 96 µs 4K QD1 random read: ~10 k unique
  ops/s single-threaded, ~200 k/s if probes are issued in parallel (they are
  read-only, and the block DAG only constrains ordering — parallelism is legal).
- Index+bloom RAM ∝ number of *distinct unique keys*, not data: fence keys ≈ 1 per
  16 KiB page (≈ 1.6 MB per 2 M docs), bloom ≈ 10 bits/key (≈ 2.5 MB per 2 M unique
  values per tile). Compatible with data:RAM ≥ 20:1 for KB-sized documents, but it is
  a *declared* cost that the learned-layout story must account for.
- Publish/compaction/rotation still rewrite whole tiles and re-verify whole
  projections: O(N) per event, ~K× write amplification overall. Immutable pages +
  append-only publish (verify only new pages) is the deeper fix and is a prerequisite
  for honest write-amplification numbers at scale.

## Design consequence worth keeping (candidate for the paper)

A block that declares `needs: unique(order_no)` over all its rows can be validated
either as **N indexed point probes** or as **one merge join** (sort the block's claimed
values, range-scan the tiles' value ranges). For clustered/monotonic keys the merge
join is sequential and nearly free; for uniform-random keys the block's values span the
whole keyspace and the merge join degenerates to a full scan, where N point probes win.
Choosing per workload is a concrete, measurable instantiation of "the physical layout is
learned from the workload" — and it rides on the existing `needs`/`else` separation
(guards are declarative; execution strategy is free), so it needs no semantic change.
