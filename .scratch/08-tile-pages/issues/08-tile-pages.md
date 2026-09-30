# 08 — tile pages: indexed access, cheap verification, honest scan rate

Opened 2026-09-30 from the blocked data:RAM experiment (see 07) and `probe_cost`
measurements (07/evidence/probe-cost.md, `b718fab`).

## Findings to fix (all measured, not inferred)

1. **No tile index.** `next_matching` (engine.rs:885) + `Entries` (tile.rs:200) walk
   from the tile's first entry, allocating two `Vec`s and checksumming every entry
   until a prefix hit. Measured probe cost is linear in tile size:
   29 ms @1.4 MB → 1.65 s @90.8 MB (memtable probe: ~2 µs). With `K ≤ MAX_TILES = 8`
   tiles, a point/unique probe is O(total data).
2. **Sequential tile scan rate ≈ 55 MB/s** (90.8 MB / 1.65 s, page cache warm),
   i.e. ~15× below the 831 MiB/s measured device rate. Cause: per-entry `Vec`
   allocation + per-entry checksum, and a whole-file digest instead of per-page CRCs.
   This caps the prototype's *strongest* path (scan) below the device, so no
   throughput claim from this engine is meaningful yet.
3. **Publish/compaction/rotation rewrite and re-verify whole tiles** (O(N) per event,
   ~K× write amplification). Publish re-derives the full projection; open re-verifies
   every tile against the WAL.

## Planned slices (each independently reviewable)

- **08a — page-structured tile format (FDBTILE2)**: 4 KiB pages, per-page CRC, page
  header with entry count + first key, fence-key directory (sparse, ~1 entry/page)
  with its own CRC, fixed trailer. Immutable pages: publish appends only new pages.
  No users exist, so this is a format bump with regeneration from the trace corpus
  rather than a migration path.
- **08b — zero-copy buffered reader**: borrow entries from a page buffer instead of
  allocating per entry; per-page CRC means a probe verifies only the page it read.
  Acceptance: sequential scan ≥ 400 MB/s on cached data, probe = directory binary
  search + one page read.
- **08c — per-tile bloom over declared unique values**: probe answers "definitely
  absent" in RAM (no I/O) or "maybe" (one page read); bits/key a tunable with a
  documented RAM cost.
- **08d — publish verifies only new pages** (whole-tile digest retained at open),
  then re-measure write amplification.
- **08e — re-run the data:RAM experiment** (the 07 run) on the fixed engine.

## Open author decision — constraint enforcement strategy (BLOCKS 08c/08e scope)

See the analysis in the reply of 2026-09-30; options:

- **A (parent recommendation)**: keep strict constraints validated **at publish,
  batched over the block** — one uniform mechanism (guard probe set) for unique,
  foreign-key and expression constraints; execution plan (bloom / page index /
  merge join) chosen per guard set and measured. Append-mostly workloads pay ~0 I/O
  (bloom says absent); only genuine duplicates pay ≤1 page read/tile, parallelizable.
- **B**: defer constraint validation past the ack (MVCC-style / async audit).
  Buys: no read coupling on the publish path. Costs: a "success now, violation later"
  contract, compensation/undo machinery, and a conflict-report channel — and undo
  fights the latest-only, no-history design.
- **C**: RAM-budgeted learned unique index (hot-value cache, learned admission).

Author's concerns to answer with measurements before choosing: (i) does eager-on-
publish hurt throughput; (ii) is "learned index/filter" real or mystical.
