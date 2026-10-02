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

## Result (slice 31) — driver located, allocator-hygiene fix landed

The controlled experiment is `harness/flashdb_harness/open_footprint.py` (one knob off `base` at a
time; `flashdb-scale openinfo <dir>` reports `VmHWM` = open-time peak, `VmRSS` = live after open).
One 800 k-row / ~114 MB inventory trace, global mode, 4 KiB pages, 10 bloom bits unless varied:

| variant | axis | tiles | data MB | open peak (VmHWM) MB | live RSS MB |
|---|---|---:|---:|---:|---:|
| base | 4 MiB checkpoint+rotate | 22 | 113.7 | 9.9 | 5.1 |
| manytiles | 1 MiB checkpoint+rotate | 80 | 113.8 | 5.1 | 5.1 |
| fewtiles | 16 MiB checkpoint+rotate | 6 | 120.5 | 85.9 | 5.1 |
| bigsuffix | 4 MiB ckpt / 16 MiB rotate | 292 | 122.1 | 65.4 | 5.5 |
| nosuffix | no rotation (full WAL replay) | 381 | 202.8 | 457.4 | 7.2 |
| nobloom | bloom bits = 0 | 22 | 111.8 | 8.1 | 3.2 |
| page64k | 64 KiB pages | 22 | 112.3 | 9.9 | 5.0 |

**The driver is the retained WAL suffix, i.e. the rotate threshold — not tile count and not total
data.** `open_impl` replays the retained WAL suffix into `records`/`covered`, then for every active
tile builds `newest_projection(&covered, lower, cutoff)` and copies it into `verify_projection`'s
`expect` map before streaming the tile's entries. That working set is O(suffix) (plus one tile's
entries), so:

* at a 16 MiB suffix the open-time peak is ~65-86 MB; at 4 MiB it is ~10 MB; at 1 MiB it is ~5 MB;
* the *tile count* is irrelevant (6 tiles peak at 86 MB, 80 tiles at 5 MB, 292 tiles at 65 MB at the
  same 16 MiB suffix);
* with rotation **disabled** (`FLASHDB_SEED_ROTATE_BYTES` unset) the suffix is the whole WAL, so open
  replays the entire dataset: 381 tiles / 203 MB on disk (tile bytes plus the un-retired WAL) →
  open peak **457 MB** (an ad-hoc 16 MiB-cadence run without rotation peaked at 554 MB). This is the
  5 GB/760 MB contradiction: those runs differed in rotate cadence, not in tile count.

**It is glibc arena retention, and the fix is allocator hygiene.** Pre-fix, RSS *stayed* at the peak
after open (6-tile instance: 82 MB live; 292-tile/16 MiB-suffix: 63 MB live) because glibc keeps the
freed arena pages. `open_impl` now drops the suffix temporaries (`records`, `covered`) and calls
`malloc_trim(0)` (`Engine::trim_retained_heap`, glibc-only, no-op elsewhere) before returning. Live
RSS after open is now a ~5 MB floor in every rotated variant above (a 16x cut on the 6-tile case),
and 5.2 MB for the earlier 82 MB case. This changes **no** on-disk format, read/write semantics, or
durability; it returns already-freed pages to the OS.

**Bloom trade (material at high tile counts, measured):** bloom RAM is ~10 bits per stored
unique-prefix, so it scales with total live entries, not with tile count per se: at 22 tiles the
bloom accounts for ~1.9 MB of the 5.1 MB live RSS (nobloom 3.2 MB), and the 10-bits/key default is
what slice 26 measured dead on these workloads. Turning the bloom off (bits/key = 0) saves that
memory and costs the in-range-absent device read the bloom would skip.

## Restated memory claim

Two different numbers, each with its own measurement conditions:

* **Steady-state serving footprint** (the 22-24:1 headline): measured during the **seed** phase of
  the slice-10d generic `seed` path (10 M docs × 1 KiB, 1 KiB docs, rotate/checkpoint window
  W = 64 MiB), so the instance there had 1 KiB documents and tile pages were not exercised.
  Live RSS ≈ `2.3 × W` + sparse directory index (0.20 MiB/GiB) + bloom (~10 bits per live entry) +
  ~5 MB process floor; it is a function of the *window*, not of total data.
* **Open footprint** (what this ticket is about): the floor to open a checkpointed instance is now
  **~5 MB plus bloom/directory**, independent of total data and tile count, *provided the WAL is
  rotated* (retained suffix ≤ rotate threshold). Without rotation the open-time peak is ~4-5× the
  full WAL and total-data-proportional. Both numbers above were measured at 4 KiB pages, global
  mode, 10 bloom bits, at tile counts of 6/22/80/292 on a ~114 MB instance; the 10d number used
  1 KiB docs.

Consequence for the pre-registration: with the fix, an instance only needs its **retained WAL suffix
and tile bloom** to fit under the cap, so a >= 10x-data:256-MiB-cap cold run is no longer blocked by
the open footprint.

## Load-path footprint (same day, follow-up): open is fixed, the LOAD is now the blocker

After the open-time trim fix, the engine opens a 760 MB / 36-tile instance at ~5 MB live, but a
6000-request read load on it peaks at **~243 MB RSS** (measured with `/usr/bin/time -v`, warm, no
cgroup). Consequences and evidence:

- The load fits under a **384 MiB** cgroup cap but is OOM-killed under **192 MiB**, even with
  `MALLOC_ARENA_MAX=1`. So for a >=10x-cache cold run the instance must be >= ~3.9 GB (at a 384 MiB
  cap) or the load's footprint must come down.
- The peak is **not** glibc retention that a boundary trim can undo: I added a per-window
  `malloc_trim(0)` to the pacer and the maximum RSS was unchanged (243,176 KB vs 243,244 KB), because
  `/usr/bin/time` reports the maximum and the peak occurs *inside* a window. (The per-window trim is
  kept: it is harmless hygiene, but it is not the fix, and the earlier "arena retention" reading of
  this number was wrong.)
- **Leading hypothesis (NOT isolated):** the scan path materialises a whole-entity result set. This
  instance holds 3 M stock rows, and 243 MB / 3 M is ~80 bytes per row, which is the size of a row's
  key+value - i.e. one `movements_scan` over an entity returning its entire row set would explain the
  peak exactly. Consistent with `reverse_lookup` collecting into a `Vec` and sorting it before
  returning, and with the adapter building the row list. A dedicated isolation test (probes only vs
  scans only vs writes only, and with the pacer streaming rather than holding the trace) was NOT run,
  so this remains a hypothesis with a plausible mechanism and matching arithmetic, not a finding.
- This matters beyond the cold comparison: it is a **bounded-memory claim** issue. If a scan
  materialises its result set, then scan memory is proportional to the result set, not to the window,
  and the data:RAM story needs that stated (or the scan streamed).

Next: isolate the load footprint (probes-only / scans-only / writes-only; pacer streaming), fix or
bound it (streaming scan results is the likely fix), and only then run the >=10x cold comparison.
