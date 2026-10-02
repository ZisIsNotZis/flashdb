# 31 — the engine's footprint when opening a many-tile instance bounds the data:RAM ratio

Opened 2026-10-02 while trying to run the pre-registration's ">= 10x cache" cold comparison
(slice 30). The comparison could not run, and the reason is a real engine property that needs its
own investigation before any cold result can be claimed.

## What was measured

Instance built by slice 30's runner: 5,000,000 dimension rows (2 M customers + 3 M stock rows),
**760-790 MB of tiles**, 35-73 active tiles, `serving_memtable_entries=0` after an explicit
`flashdb-scale checkpoint`. Device fsync p50 in session: 5.1-5.6 ms.

| instance | active tiles | data | open RSS (`/usr/bin/time -v`) |
|---|---|---|---|
| tiny (85 k docs, never checkpointed) | 0 | ~20 MB | 49 MB |
| global mode, 4 KiB pages | 36 | 777 MB | 242 MB |
| per-region, mixed pages | 73 | 789 MB | 255 MB |
| (earlier, 5 GB scale run, verify phase) | ~80 | 5.16 GB | **151 MB** |

Two things are clear and one is not:

- **Clear:** under a `MemoryMax` cap of 192 MiB (which counts page cache) every arm is OOM-killed,
  even with an empty memtable, and glibc tuning (`MALLOC_ARENA_MAX=1`,
  `MALLOC_TRIM_THRESHOLD_=65536`, `MALLOC_MMAP_THRESHOLD_=65536`) does not make it fit. So the
  footprint is anonymous memory, not reclaimable cache.
- **Clear:** `Tile` retains each tile's **bloom bits in RAM** (measured `bloom_len=385,024` bytes per
  10 MB tile), i.e. ~27 MB at 73 tiles - a real, per-tile cost that the 11-slice fence work did not
  remove (it removed the fence set, not the bloom).
- **Not clear:** why RSS is 255 MB here but 151 MB in the 5 GB run. It is not proportional to data
  bytes (5 GB -> 151 MB vs 760 MB -> 255 MB) and not obviously proportional to tile count (both
  ~36-80 tiles). The leading hypothesis is **glibc arena retention from streaming tile entries
  through transient `Vec` allocations** during `open_impl`'s `verify_projection` (the same
  phenomenon measured in slice 10a, where `malloc_trim` returned the memory) - but the 5 GB number
  contradicts it, so the hypothesis is NOT confirmed and must be tested, not assumed.

## Why it matters

- The pre-registration requires the instance to be >= 10x the memory budget. With an open footprint
  floor of ~250 MB, a valid cold experiment needs **>= 2.5-3 GB of data under a ~256 MiB cap**, and
  the earlier 5 GB/1 GiB run was only 5x.
- More importantly, the headline `data:RAM ~= 22-24:1` figure was measured during the **seed** phase
  (215 MB RSS for 5.16 GB), while **opening** a checkpointed many-tile instance costs 151-255 MB.
  Both numbers are real but they answer different questions, and a 20:1 claim must state which one it
  means, at what tile count and page size. As written it is ambiguous.

## Required work

1. **Locate the driver with a controlled experiment**, not a guess: vary (a) tile count at constant
   data, (b) data per tile at constant count, (c) bloom on/off, (d) page size, and report RSS after
   open for each. If the driver is allocator retention, prove it by calling `malloc_trim(0)` after
   open and re-measuring (a small, defensible engine change: trim after verification is pure
   allocator hygiene) - and if that is the fix, land it, because it is what makes the cold
   experiment runnable at all.
2. Then run slice 30's cold comparison properly at >= 10x: cap 256 MiB, data >= 2.6 GB (or a cap and
   data pair that reaches 10x with the measured floor), fully checkpointed before the load, with the
   device fsync p50 and the per-region `bytes_read`/block counts as the primary evidence.
3. Restate the memory claim precisely in `docs/` and the tickets: distinguish *open* footprint from
   *steady-state serving* footprint, and give the tile count and page size both were measured at.
4. If the bloom's RAM cost is material at high tile counts, say so and state the trade (bits/key vs
   RAM), since it is the knob we measured as dead-on-this-workload but it still costs memory.
