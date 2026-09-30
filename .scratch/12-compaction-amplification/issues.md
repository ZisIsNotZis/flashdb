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
