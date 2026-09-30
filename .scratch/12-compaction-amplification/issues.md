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
