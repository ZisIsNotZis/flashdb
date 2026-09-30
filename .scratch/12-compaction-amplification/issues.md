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
