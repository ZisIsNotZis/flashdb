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
