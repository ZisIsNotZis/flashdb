# 14 — 0b prerequisite gap analysis (what stands between us and the layout experiment)

Written 2026-09-30, after the engine reached its first honest measurement point. This is a gap
analysis against the plan the author already wrote (`docs/dev-loop.md`, `docs/learning.md`,
`docs/objective.md`) — not a new plan.

## Already in place

| prerequisite (from `docs/dev-loop.md`) | status |
|---|---|
| Frozen, hashed replay corpora | DONE — `mixed_seed42_20.jsonl` (sha 92982259…), `hot_orders_seed42.jsonl` (sha d803afa5…) |
| Deterministic replay | DONE — ticket 06, `flashdb-replay` CLI |
| A real engine with the tile architecture | DONE — tickets 07/08/10/11/12: measured data:RAM ≈ 24:1 at 5.16 GB, scan 605 MiB/s, write_amp 5.87, indexed probes (in-range absent 151.8 → 1.6 µs at 12 tiles) |
| Device calibration constants | DONE — 0a-redo (4K QD1 96.3 µs, ≈203 k IOPS QD32, 117.4 µs 16K, 831 MiB/s) |

## Missing, and who owns it

| gap | owner | blocker for 0b? |
|---|---|---|
| **Layout space**: what a layout *is* per tile and how it is declared/applied | author (research design) | YES — without it there is nothing to learn or vary |
| **Objective function F1–F5** (estimand, anchor, parameterisation, aggregation, oracle) | author — `docs/objective.md` marks them Open after review-01 | YES — scoring and promotion cannot be implemented |
| **Pre-registered falsification contract** (threshold, metric + reference, minimum instance size > cache, seed count, CI, acceptable cost-model error, Postgres tuning protocol) | author — required before any engine existed; still not registered | YES — results without it are not evidence |
| **Simulator + calibrated cost model** | parent can build *after* the objective fixes the calibrated statistic (`review-01` F14) | partially |
| **Controls**: `flashdb-declared` (one fixed layout) and `flashdb-static-derived` (offline layout from the trace) | needs the layout space first | YES as comparison, no as construction |
| **Telemetry store** (sketch counters, tiered rollup, ≈1% budget) | parent can build; the budget-share question is author-owned (`review-02` F14) | no, but the learner needs it |

## Proposed layout space, cheapest first (needs the author's pick)

The engine's keys are already ordered `entity | key | ~csn`, so the physical layout questions are
concrete and testable:

- **L1 — per-tile clustering choice (smallest useful space).** Which key family a tile is built
  around and in what order within the tile: primary (entity, handle), a declared unique value,
  a reverse-index target, or a time bucket; plus page size and bloom bits/key. The learner picks
  one option per tile from a published enumerated set. Everything needed already exists in the
  engine except the ability to *declare* the choice and to build a tile in a non-default order.
- **L2 — key-family co-location.** Which families share a tile (e.g. an order row next to its
  lines and its stock row) versus being separated. This is what the join fan-out hypothesis in
  `docs/dev-loop.md` actually tests, and it is the first option that cannot be a per-tile choice
  alone — it needs a grouping decision.
- **L3 — per-tile heterogeneity with learned packing** (the ambition): mixed families, learned
  page layout, quantized widths. Most powerful, most invasive, and it needs L1's declaration
  surface anyway.

Recommendation: adopt **L1** as the first layout space (enumerable, small enough to brute-force a
relaxed oracle over, and sufficient for the star-schema/fan-out workloads), with L2 as the
second increment. That also matches the relaxed-oracle decision from `review-02` F1 (per-tile
best over a small published candidate set).

## What the parent can build without any decision

- Telemetry/measurement plumbing that both the controls and the simulator need (per-window cost
  samples keyed by pattern, emitted by the real engine).
- Engine-side enablers for L1 that are contract-neutral: a declarative, per-tile layout
  descriptor that today has exactly one valid value (the current ordering), so the code path
  exists and is exercised before any learned value is allowed.
- Further efficiency work with measured payoffs (compaction tier capacity, bounded open,
  point-read probes) — none of it moves the 0b critical path.

## Headroom measurement (2026-09-30): the safe layout space cannot win

`harness/flashdb_harness/layout_headroom.py` scores the knobs that are safe to implement today
(per-tile page size and bloom bits/key) with the measured constants and the provisional
objective, over a documented inventory-shaped pattern mix at 12 probed tiles. Base = today
(4 KiB blocks, 10 bloom bits) - note the engine's real block size is 4 KiB, which is also the
**probe-optimal** page size (96.3 us vs 117.4 us at 16 KiB and 185.8 us at 64 KiB), so larger
pages only help scans, which are a minority of the mix.

| candidate | vs base |
|---|---|
| 4 KiB + 14 bloom bits | **+2.98%** (best) |
| 4 KiB + 10 bits (base) | 0.00% |
| 16 KiB + 14 bits | -14.8% |
| 64 KiB + 14 bits | -72.3% |
| 4 KiB, no bloom | -3150% |

Sweeping the unmeasured scan-rate gain of large pages (1.0-1.5x per doubling) does not change the
ranking. **So the maximum win available from the safe knob set is ~3%, far below the provisional
15% threshold: 0b cannot be run on this space, and no learner should be written for it.**

The space therefore needs an axis that can move more than 15%. The candidate that preserves
correctness (per the orchestrator correction in the layout proposal: nothing may be *omitted*,
because the v0 doc payload is opaque to the engine) is **family partitioning** - keeping the
entries of different key families in different tiles so a slowly-changing family is not
rewritten when a hot family compacts. Its headroom is on the write side, and the same model
gives a usable criterion:

| slow-family byte share | write_amp | total gain vs base (at 0.02 us/byte endurance weight) |
|---|---|---|
| 5% | 3.92 -> 3.72 | +4.5% |
| 10% | 3.92 -> 3.53 | +8.3% |
| **20%** | 3.92 -> 3.14 | **+15.9%** |
| 40% | 3.92 -> 2.35 | +31.1% |

**Decision rule: implement family partitioning only if a real workload's slow-family byte share
is >= ~20%.** That is a measurement we do not have yet (the scale driver writes only P and U,
where U is small; the inventory trace writes an R family per movement). Next step: measure the
per-family byte shares of the real workload before building the axis - and, per the oracle's
risk #3, validate the cost model against a measured pair of layouts at the same time.

## Cost-model calibration status (2026-09-30) — not yet evaluable, and why

Slice 17 turned page size into a real per-tile parameter and measured the trade-off
(`.scratch/14-layout-harness/layout-headroom` notes it too): on the same content, 64 KiB pages
lower bytes slightly and raise scan throughput (2231.7 -> 3154.9 MiB/s on scanbench) but make a
present-key probe 4.6x slower (19.3 -> 88.9 us on one tile, 24.12 -> 35.02 us at four tiles).
The probe penalty is ~1.16 us per extra KiB of block handled, so the measurement is CPU-bound,
not device-bound.

Consequences recorded honestly:
- The model in `layout_headroom.py` prices the **device** regime (L_dep from 0a-redo). The
  measured points are the **warm** regime. Neither the device nor the CPU terms can be
  separated without a cold-cache measurement.
- Two path-specific CPU constants would be needed (probe vs scan); fitting both on the same
  points is circular, so no calibration claim is made.
- **review-01 F14's quantile-calibration error cannot be evaluated yet**, and therefore neither
  can the 25% calibration tolerance pre-registered in doc 15. The cheapest closure is a
  cold-cache measurement: working set > RAM under a cgroup with the cache dropped, probes and
  scans at 4 KiB and 64 KiB.
- The headroom conclusion is unaffected: it depends on the layout ranking, and the warm numbers
  make larger pages look *worse* for probes, strengthening it.

## Finding (slice 18): the request surface cannot create dimension rows

While building the first runnable inventory workload the worker found - and I verified from the
adapter - that `engine/src/request.rs` accepts only four request templates (`order_flow` with its
`cust` -> `take` guard, a `Stock` get, a `Customer` find, a `StockMovement` scan). There is **no
create/seed shape**: the header itself says dimension rows must be seeded as P/U pairs at the
engine level. So dimensions are creatable only through engine ops, never through the documented
request grammar.

Consequences:
- Any workload that needs pre-existing dimensions (which is the whole inventory scenario, and
  therefore every 0b workload) must be seeded out-of-band, which is what the harness will now do.
- The gap is a grammar/contract question, not an engine one: `docs/contracts.md` already carries
  open decisions about the request surface (L-04, L-07, the expression grammar, and the
  "one intent must not be expressible two ways" rule). Widening the grammar is the author's call,
  so I deliberately did NOT add an ad-hoc `put` shape. It is recorded here for them.

Decision taken instead: a `seed-inventory` applier in the scale driver commits the harness seeding
trace as raw P/U/R ops, with the requirement that it derives keys by exactly the rules the
adapter's lookups probe (same unique-prefix encoding, same x-ref reverse entries), proven by the
flow's commit rate rather than asserted.
