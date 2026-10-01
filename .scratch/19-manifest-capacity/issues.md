# 19 — the tile cap is the biggest measured writing lever (raise it via a multi-page manifest)

Opened 2026-09-30 from `harness/flashdb_harness/partition_sim.py`, a simulation of the shipped
`budget_prefix` merge policy that **reproduces the measured write_amp of 3.920 within 1.5%** on
the slice-12d configuration, so its comparisons are usable.

## Finding

The manifest's 4092-byte A/B page bounds the active tile list at **12** (`.scratch/12`,
compile-time asserted). Sweeping that cap in the simulation, at the inventory workload's measured
family composition (P 58.5%, U 30.8%, R 10.7% of written bytes) and the same payload:

| tile cap | co-located write_amp | partitioned write_amp | partitioning "gain" |
|---|---|---|---|
| **12 (today)** | 4.092 | 7.745 | -89.3% |
| 24 | 2.075 | 2.808 | -35.3% |
| 48 | 1.771 | 2.102 | -18.7% |
| 96 | 1.661 | 1.519 | +8.6% |
| 192 | 1.095 | 1.000 | +8.7% |

Two conclusions, both actionable:

1. **Cap headroom is the real lever: 12 -> 96 cuts write amplification by ~59%** (4.09 -> 1.66).
   More active tiles means a better tier factorization, so each byte is rewritten fewer times.
   This is far above the provisional 15% threshold and it is contract-neutral - it is internal
   capacity, not a product-visible layout choice.
2. **Family partitioning is closed as an axis.** It only turns positive at cap >= ~96 and even
   then only +8.7%; at today's cap it is catastrophically worse (partitioning multiplies the tile
   count against a fixed cap, so every family hits the cap sooner and compacts its own smaller
   tiers more often). Note the model gives partitioning the OPTIMISTIC bound (each family gets
   half the cap to itself); a shared cap would be stricter, so the negative conclusion is robust.

## Why it matters for 0b

The layout space needs a >=15% axis to be falsifiable at all. Page size and bloom bits top out at
~3% (`.scratch/14-layout-harness`); family partitioning is closed by this measurement. Raising the
cap is (a) a large measured win by itself and (b) a **prerequisite for any layout choice that
trades write cost against read locality**, because those choices need tile headroom to exist.
It also makes "which tiles to merge, when" a meaningful learned decision (the design already lists
"compaction trigger per prefix-region" as learner-controlled in v0).

## Required work

Multi-page manifest:

- Keep the A/B page protocol and its ordering rule (inactive page -> sync file -> sync dir ->
  flip), but let the root chain pages so the tile list is no longer bounded by one page. Options:
  a chained page list with a next-page pointer (each page CRC-protected), or a fixed small
  directory page pointing at tile-record pages.
- Define the failure model precisely and fail closed: a chain whose CRC fails at any link makes
  the whole root invalid (so the other A/B page, if valid, wins); a chain that does not terminate
  at a bounded length is corruption; the highest valid seq still wins.
- Recovery (`open_discover`) must validate the entire chain before serving, and `MAX_TILES` becomes
  a policy constant (say 1024) rather than a page-size artefact.
- Keep the invariant that a tile's durable directory entry exists before the manifest references
  it, and that the manifest never references a name twice.
- Measure for real: re-run the 12d/12c 5 M-doc configuration at cap 12 vs the new cap and report
  the measured write_amp, then the simulation's prediction error (this is also a second,
  independent calibration point for the cost model - `review-01` F14).
- Acceptance: measured write_amp at the new cap materially below the 3.920 recorded for cap 12,
  all fault-injection tests green, and recovery fails closed on every malformed chain shape the
  tests can construct.
