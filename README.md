# flashdb

A relational store for single-node, IO-bound workloads whose **physical layout is learned from the workload rather than declared**.

Status: **design only.** Nothing is implemented. The repository currently contains contracts and the decision record behind them.

## What it is, in one paragraph

Storage is tiled — not rows, not columns — and every tile's arrangement is chosen by a per-tile learned policy and drifts continuously. No layout is ever guaranteed to a user; every intermediate state is fully servable, so layout never needs a migration project. A single scalar objective (angriness) prices every decision — layout, reorganization timing, promotion tier, speculation, compilation, advice — through one calibrated cost model. Requests are structured trees, not SQL text, with a canonicity rule that makes one intent expressible one way, because the request is also the unit of learning and of atomicity.

Motivating case: ~10 TB, ~128 GB RAM, heavily joined lookups, where the scarce resource is the number of *dependent* random reads per request rather than bytes.

## Documentation

| File | Read it when |
|---|---|
| `docs/design.md` | first. Thesis, goals/non-goals, architecture, the 13 invariants, open questions, prior art. |
| `docs/objective.md` | reasoning about any trade-off, or about what "better" means here. The angriness objective. |
| `docs/contracts.md` | before writing any code. The four frozen interfaces. |
| `docs/learning.md` | working on adaptation, promotion, canary, the advisor, or the human feedback loop. |
| `docs/dev-loop.md` | planning work, building the harness, or designing workloads and benchmarks. |
| `docs/glossary.md` | encountering an unfamiliar term. |
| `.scratch/01-design/decisions.md` | wanting the raw, attributed record of what was decided and why, round by round. |

## Goals

- Highest throughput on a single modern Linux node, without trading correctness or crash-atomicity.
- Layout, encoding and scheduling derived from the observed workload, per tile.
- One round trip per business request; the request is the unit of atomicity, learning and attribution.
- Continuous background adaptation with no migration project and no downtime.

## Non-goals

Multi-node and replication (deferred, not precluded). SQL or any text query language. Interactive multi-round-trip transactions. General-purpose engine parity. Any guarantee about physical layout, ever.
