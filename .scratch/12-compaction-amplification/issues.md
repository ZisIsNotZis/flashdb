# 12 — compaction rewrites the whole dataset (write amplification ∝ data²/window)

Opened 2026-09-30 while watching the 10d scaled run (10 M docs x 1 KiB, ~10.4 GB).

## Observed evidence

`flashdb-scale seed` calls `Engine::compact()` every few checkpoints (it must: the active
tile list is capped at `manifest::MAX_TILES = 8`, so publishing refuses once the list is
full). Our `compact()` merges **all** active tiles into **one**, which is a full rewrite of
the entire dataset, and the driver then keeps publishing, so it compacts again a few tiles
later. Throughput therefore decays as the dataset grows, exactly as an O(data^2) pattern
predicts:

| elapsed | docs | on disk | docs/s |
|---|---|---|---|
| 60 s | 500 k | 0.8 GB | 8 300 |
| 260 s | 1.75 M | 2.4 GB | 6 700 |
| 810 s | 3.5 M | 5.8 GB | 4 300 |

The directory listing shows the result: `compact-676.tile`, `compact-728.tile`, … next to
`tile-689/702/715/728.tile`, i.e. each compaction rewrote everything already on disk.

Memory is *not* the problem any more (RSS stayed 205 → 246 MB across 5 GB of data; part of
that drift is glibc arena retention, see 10c's `malloc_trim` measurements).

## Why it matters

- The objective is an IO-dense store; rewriting the whole dataset every few hundred MB of
  new data puts write amplification at roughly `data / window` per compaction pass, i.e.
  quadratic total writes, and burns device endurance for no benefit.
- It also hides the "learned layout" question: with a full-rewrite every time, layout
  decisions cannot be evaluated incrementally.
- It makes long seed runs (and any benchmark that must reach multi-GB scale) an
  endurance test rather than a measurement.

## Design constraints the fix must respect

The current read path and recovery verification depend on the active tile list having
**strictly increasing, non-overlapping CSN ranges**: `verify_projection` checks each tile
against its own `(lower, cutoff]` WAL slice, and the merge relies on a key appearing in at
most one tile. A leveled design with overlapping ranges would break both. So the fix must
preserve disjoint ranges, which rules out classic leveled compaction and points at:

**Size-budgeted tiered merge of the oldest contiguous run.** Keep the increasing ranges, and
when the tile count would exceed the cap, merge only a *bounded subset* - the oldest
contiguous run whose combined size is at most `max_merge_bytes`, at least two tiles. The
output covers exactly the merged run's union range (so cutoffs stay strictly increasing and
disjoint), and only the merged inputs are retired. With geometrically growing tile sizes
this rewrites each byte O(log N) times instead of O(N / window), and it is the usual tiered
argument.

Consequences to handle in the same design:

- `manifest::MAX_TILES` is bounded by the 4092-byte manifest payload (a tile ref is
  `4 + name + 8 + 32` bytes), i.e. roughly 90 tiles at short names. A tiered policy wants
  more than the current 8, so raise the cap to what the page can hold (and record that the
  manifest page, not the policy, is the real limit - multi-page manifests are a later
  concern).
- Compaction must become a policy: `maybe_compact(budget)` chosen by the caller (or by the
  engine from a declared budget), instead of "merge everything" being the only option and the
  benchmark driver calling it every few checkpoints.
- The streaming merge from 10c-1 already bounds *memory* for the merge set; this ticket
  bounds *work*.
- Acceptance: (a) bytes written per byte ingested, measured at multi-GB scale, reported as a
  number and compared with today's; (b) a seed run whose throughput does not decay with
  dataset size; (c) the rotate-then-compact-then-reopen regression and the disjoint-range
  verification stay green.

## Direction (needs a design decision)

- **Leveled/tiered compaction**: merge only a bounded subset (e.g. the K oldest tiles or
  tiles whose combined size is under a budget), writing one output tile and retiring only
  the merged inputs, so per-compaction work is bounded by the merge set rather than by the
  dataset. Growth then costs amortized O(log N) rewrites per byte, the usual LSM argument.
- **Trigger policy as a first-class knob**: compaction should be driven by tile count /
  overlap ratio, not by a fixed "every few checkpoints" in a benchmark driver.
- **Acceptance**: a stated bytes-written per byte-ingested figure at multi-GB scale
  (measure WAL bytes + tile bytes written vs payload ingested), and a seed run whose
  throughput does not decay with dataset size.

## Note on scope

This is NOT a memory or correctness defect: every compaction output is verified and the
durable states are consistent. It is a cost/endurance defect, and it is the write-side
analogue of the read-side O(n²) found earlier in ticket 07.

## 12a/12b outcome: the policy still rewrites a growing tile (measured)

12a added `maybe_compact(budget, cap)` and `publish_replacing`; 12b wired the driver to it
(budget = 8 x 64 MiB = 512 MiB, cap = `MAX_ACTIVE_TILES` = 12) and added cumulative
accounting. The 2 M-doc x 1 KiB demo finished in 471.9 s and **falsified the fix**:

| docs | cumulative docs/s | compactions | write_amp |
|---|---|---|---|
| 200 k | 19,976 | 0 | 2.13 |
| 1 M | 7,357 | 1 | 2.63 |
| 2 M | 4,238 | 14 | **8.92** |

`payload_bytes` 2.06 GB, `wal_written_bytes` 2.17 GB, `tile_bytes_publish` 2.22 GB,
`tile_bytes_compact` **14.0 GB**. The reason is selection, not the manifest operation:
`select_run` always starts at the OLDEST tile, so once the first merge output exceeds the
512 MiB budget, every later merge is "the one giant tile + one window", and the giant keeps
growing - still O(data^2/window). Merge outputs grew 518 MB -> 593 MB -> ~740 MB -> ~960 MB
-> ~1.2 GB -> ~1.4 GB across 14 compactions.

**12c fix (policy only, no format/manifest change): select by size, never lead with a giant.**
Choose the contiguous run of length >= 2 that minimises combined size, ignoring tiles
already larger than the budget wherever a cheaper run exists (an oversized tile is a finished
tier, not a merge input); fall back to the two smallest adjacent tiles only when no eligible
run exists. That bounds per-merge work by the budget, and makes each byte's rewrite count
O(log N) instead of O(N/window). Acceptance: flat `write_amp` at ~5 GB (target <= 4x) and no
throughput decay in a 5 GB seed run; plus unit tests for the selection rule (giant skipped,
cheapest run chosen, fallback when everything is oversized).

## 12b also exposed a second, separate cost

`commit_block` probes uniqueness with `scan_merged` per unique op across EVERY active tile
(up to 12). That is bounded by the tile count, not by data size, but it is a large constant:
at 12 tiles each unique op pays ~12 indexed seeks + block reads. The measured justification
for the planned bloom slice (08d) is exactly this: a per-tile bloom answers "no published
owner" in RAM for the append-mostly case, turning that constant into ~12 hash probes.

## 12c result: cost-based run selection works, and the bottleneck moved

Same driver, same machine, 1 KiB docs, budget 512 MiB, cap 12:

| docs | payload | 12b write_amp | 12c write_amp | 12c compaction output |
|---|---|---|---|---|
| 2.0 M | 2.06 GB | 8.92x | 3.82x | 3.48 GB |
| 2.25 M | 2.32 GB | - | 3.99x | 4.30 GB |
| 3.75 M | 3.87 GB | - | 5.17x | 11.71 GB |

So 12c removed the pathological "giant + window" rewrite (8.92x -> 3.99x at 2 M docs, a 3.3x
reduction in total bytes written) and the remaining growth is the expected tiered behaviour:
each byte is rewritten ~log2(data / budget) times, i.e. ~3 at 3.9 GB with a 512 MiB budget.
The earlier "<= 4x flat" target was optimistic; the honest claim is O(log N) rewrites, and the
absolute number should be quoted together with the budget and the data size.

**The bottleneck has moved to the probe path.** At 3.75 M docs the interval rate is ~2.4 k
docs/s for ~5 MB of payload per commit, i.e. ~2 s per block. Decomposing one 5000-doc block
(all numbers page-cache warm):
- ~5000 distinct unique values -> ~5000 `scan_merged` calls, each opening one clipped cursor
  per active tile (12): ~60 k cursor constructions, each allocating a 4 KiB block buffer
  (~245 MB of allocation churn per block) plus ~60 k directory+data block reads;
- WAL append + fsync for 5 MB;
- amortized publish (every ~64 MiB of WAL): WAL replay + map + tile write;
- amortized compaction: ~3-5x the publish bytes.

The probe terms dominate. Two cheap fixes, in order of cost:
1. **Per-tile key-range fast reject (no format change).** Cache each tile's first and last key
   in RAM (bounded by the tile count, not by data) at open, and skip a tile for a prefix
   outside that range without creating a cursor at all. For append-mostly keys (monotonic
   order numbers) this rejects nearly every tile instantly.
2. **Per-tile bloom (format bump).** For prefixes that fall inside the range but are absent,
   a bloom answers "no key with this prefix" in RAM. This is the measured justification
   recorded under 12b.

Both are read-path changes and none of them need the author's D1-D5 decision, which only
affects how the *partial-unique* predicate is resolved (outside the filter, by row
materialization).

## End-to-end 5 GB run on the 12c driver (all phases OK)

5,000,000 docs x 1 KiB, budget 512 MiB, cap 12, `MemoryMax=1G` scope:

| phase | result |
|---|---|
| seed | 5.16 GB payload in 1545 s, cumulative 3238 docs/s, 65 compactions, **write_amp 5.87x** |
| memory | process `rss_peak` 215 MB -> **data:RAM ~= 24:1**; scope `memory.peak` = 1,073,741,824 B (exactly the cap, i.e. the cap was consumed by reclaimable page cache; the process peaked far below it and the run completed normally) |
| verify | 34 s, docsA 2.5 M + docsB 2.5 M, 40 unique probes + 6 full scans, rss 151 MB |
| scanbench | 5.12 GB in 8.07 s = **605 MiB/s** on one pass (device sequential reference: 831 MiB/s) |

write_amp 5.87 matches the tiered model (log2(5.16 GB / 512 MiB) ~= 3.3 compaction rewrites
plus publish ~1.1 and WAL ~1.05), which is the honest claim to make: O(log N) rewrites, with
the absolute number quoted together with budget and data size.

Remaining known cost: the probe path (see above). 3238 docs/s cumulative is CPU-bound on
per-unique-op probing across up to 12 tiles, not on I/O, disk, or memory.

## Probe path after the bloom (08d-2b), measured on one binary

`probe_cost spread` builds tiles whose keys are spread across the whole keyspace, so probes land
INSIDE tile ranges and are absent - the case the key-range filter cannot reject. Same binary,
same run parameters; the only difference is whether the writer attaches a bloom (measured by
temporarily skipping `set_bloom` and restoring it):

| tiles | no bloom (µs/probe) | with bloom (µs/probe) | speedup |
|---|---|---|---|
| 1 | 15.59 | 0.93 | 17x |
| 4 | 94.57 | 0.56 | 169x |
| 12 | **151.84** | **1.59** | **95x** |

Without a bloom the cost grows with the tile count (15.6 -> 151.8 µs) because every probe reads a
directory block and a data block per tile; with a bloom it is ~1 µs and **tile-count
independent**, because the answer comes from RAM. Combined with 08d-1 (sequential-key probes:
130.89 -> 0.46 µs, range reject without touching the bloom), the probe path is now cheap in both
shapes.

Remaining probe cost: PRIMARY-key probes (`get(entity, handle)`) are not covered by this bloom
(its keys are unique-index prefixes), so a point read still costs one indexed seek per tile.
Measured motivation for a future slice: ~12 µs per tile at 12 tiles, i.e. ~150 µs before any
match. It is not the current bottleneck (the workload is write-heavy), so it is recorded rather
than scheduled.

## Budget sweep: the budget is inert, the merge SHAPE is the knob (2026-09-30)

2 M docs x 1 KiB, cap 12, budget = 2 / 8 / 32 rotation windows (a new
`FLASHDB_COMPACT_WINDOWS` env override made the sweep possible without rebuilds):

| budget (windows) | write_amp | compactions | tile_bytes_compact | docs/s | elapsed |
|---|---|---|---|---|---|
| 2 (128 MiB) | 3.827 | 19 | 3,491,862,107 | 12,506 | 160 s |
| 8 (512 MiB) | 3.827 | 19 | 3,491,862,107 | 13,049 | 153 s |
| 32 (2 GiB) | 3.827 | 19 | 3,491,862,107 | 9,866 | 203 s |

Byte-for-byte identical outcomes: the budget never binds. `select_run` always picks the run with
the SMALLEST combined size, which at this tile count is invariably the two smallest adjacent
tiles, so the policy is effectively "merge the cheapest pair" (a Huffman-style tier merge) and
the budget only matters if a single merge would exceed it. The wall-clock differences are noise
plus cache warmth from the preceding run, not policy effects.

**Consequence: the real compaction knob is the merge SHAPE, not the budget.** Candidate policies
to measure against the current one, all implementable inside `select_run` with no format or
protocol change:

1. **cheapest pair** (today) - baseline;
2. **size-ratio group**: merge every run of adjacent tiles whose sizes are within a factor
   (e.g. 2x) of the run's smallest, capped by the budget - the standard universal-compaction
   shape, which bounds tier waste instead of always paying a pairwise merge;
3. **budget-maximal prefix of the smallest**: merge as many of the smallest tiles as fit the
   budget in one pass, which cuts the number of rewrites per byte at the cost of bigger single
   merges.

Acceptance for the follow-up: measure all three at 2 M and 5 M docs (write_amp, compaction count,
docs/s) and make the winner the default, with the losing numbers recorded rather than deleted.
The tile CAP (12, page-bound) is the other half of tier headroom and needs multi-page manifests;
that is a separate, larger decision.

## Measurement caveat: the machine is shared (2026-09-30)

While comparing compaction policies I found that this box runs other work in parallel (another
project's sweeps, and at one point two policy seeds side by side). The consequence is visible in
the numbers already recorded: the SAME 1 M-doc x 1 KiB workload measured **6137**, **15060** and
**16264 docs/s** in different runs. That spread is load, not policy.

Therefore, for every comparison in this ticket and in the paper:

- **Primary metrics are the deterministic ones** - `write_amplification`, compaction count,
  `tile_bytes_publish` / `tile_bytes_compact`, `payload_bytes`, `wal_written_bytes`,
  `rss_peak_kb`, probe microseconds. These reproduced byte-for-byte across repeated runs
  (the budget sweep produced identical byte counts three times) and are safe to compare.
- **Wall-clock and docs/s are indicative only** unless (a) runs are sequential, (b) no other
  workload is active, and (c) the run is long enough to amortise cache warmth. Any timing claim
  must state the concurrency conditions it was taken under.
- The design docs already demand a pre-registered measurement protocol; this is a concrete
  instance of why: a docs/s table taken on this box without that control would have been noise
  presented as a policy effect.

## 12d result: the merge shape was the real knob; BudgetPrefix is now the default

Three shapes measured with the same driver, payload and config; **deterministic metrics only**
(timing on this shared box is indicative - see the caveat above).

2,000,000 docs x 1 KiB (payload 2,061,777,780 B):

| shape | compaction output | tile bytes total | compactions | write_amp |
|---|---|---|---|---|
| `cheapest_pair` (old default) | 3,491,862,107 | 5,719,943,853 | 19 | 3.827 |
| `ratio_group` | 1,782,336,058 | 4,010,417,804 | 4 | 2.998 |
| `budget_prefix` | 1,782,336,058 | 4,010,417,804 | 4 | 2.998 |

5,000,000 docs x 1 KiB (payload 5,157,777,780 B):

| shape | compaction output | tile bytes total | compactions | write_amp |
|---|---|---|---|---|
| `cheapest_pair` | 19,204,614,555 | 24,840,172,661 | 65 | 5.868 |
| `ratio_group` | 10,259,877,823 | 15,907,787,753 | 25 | 4.137 |
| **`budget_prefix`** | **9,144,257,067** | **14,792,166,997** | **21** | **3.920** |

So the pairwise "cheapest" merge was paying roughly 1.5-2x the necessary writes: absorbing as
many of the smallest adjacent tiles as fit the budget per merge halves the compaction output at
5 GB and cuts the number of merges from 65 to 21. **BudgetPrefix is now the shipped default**,
with the measured numbers recorded in the code comment and the other two shapes reachable via
`FLASHDB_COMPACT_SHAPE` for re-measurement.

Consequences to keep in mind:
- The budget *does* bind under BudgetPrefix (unlike under CheapestPair), so the budget is now a
  real policy knob again; a later sweep can tune it.
- The tile cap (12) still bounds how many tiles a single merge can absorb, so multi-page
  manifests remain the way to buy more tier headroom.
- Memory is unaffected: rss_peak 220-232 MB at 5.16 GB of payload across all shapes, i.e.
  data:RAM ~= 22:1 holds.

## Budget sweep under BudgetPrefix: there is a real optimum (2026-09-30)

2 M docs x 1 KiB, shape budget_prefix, deterministic metrics only:

| budget (windows / bytes) | write_amp | compactions | compaction output |
|---|---|---|---|
| 2 / 128 MiB | 3.827 | 19 | 3,491,862,107 |
| **8 / 512 MiB** | **2.998** | **4** | **1,782,336,058** |
| 32 / 2 GiB | 3.394 | 2 | 2,599,539,326 |

Both ends are worse, for opposite reasons: a small budget cannot absorb a tier's worth of tiles,
so the policy degenerates back toward pairwise merges (matching the CheapestPair numbers), while
a budget far larger than the tier structure lets one merge swallow nearly the whole active list -
which is the full-dataset rewrite this ticket exists to remove. The 512 MiB default happens to sit
at the optimum at this scale and cap (12 tiles).

Open follow-ups (recorded, not scheduled): the optimum is a function of data size and the tile
cap, so a future policy could derive the budget from the tile-size distribution instead of fixing
it - and the cap (multi-page manifests) is what limits how good any budget can be.
