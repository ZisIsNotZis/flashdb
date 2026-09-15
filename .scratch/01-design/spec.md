# spec — flashdb design contracts

Ticket: `.scratch/01-design/issues/01-design.md`

## Problem

The author has a design thesis for a new database engine (tiled storage whose physical layout is learned from the workload, IO-bound single-node, one round trip per business request, one scalar objective pricing every decision) developed over an extended multi-round brainstorming session. It exists only as conversation. It must become a written, reviewable design with frozen interfaces before any implementation begins, and it must be challenged adversarially before it is trusted.

## Outcome

A written design of record under `docs/`, plus a raw attributed decision log, plus an adversarial review record naming every hole found and its disposition.

## Acceptance criteria

1. `docs/design.md` states the thesis, goals and non-goals, the architecture, the load-bearing invariants, the open questions, and the prior art with an explicit verify-before-citing caveat.
2. `docs/contracts.md` states the four interfaces that cannot be cheaply retrofit (logical model, transaction model, durability and failure, layout) and the frozen reserved interfaces.
3. `docs/objective.md` states the single scalar objective precisely enough to implement: the SLO anchor, the penalty curve and its parameters, the time-series weights, and the decision form.
4. `docs/learning.md` states how the engine adapts, promotes, rolls out, predicts, advises and talks to a human.
5. `docs/dev-loop.md` states the workloads, the harness, the baselines, the build order and the fatal risk.
6. `docs/glossary.md` defines every term the other documents use, for a reader who is not a database engineer.
7. Every doc respects the size budget of `R-DOC.6` (≤200 lines or 12,000 chars, or an explicit `Budget:` header) and the markdown style of `R-DOC.6.1` (one item per physical line, no manual wrapping). Both rules live in the parent workspace policy, outside this repository; `README.md` records that.
8. A fresh-context adversarial review has been run from at least five distinct aspects (six were spawned, one per aspect), every finding is recorded with its disposition, and no finding is left undispositioned.
9. The repository is committed so the reviewed revision is identifiable by hash.

## Non-goals

- Any implementation, benchmark or prototype.
- Resolving the open questions listed in `docs/design.md`. They are recorded, not answered.
- Verifying the prior-art citations. They are flagged as needing verification instead.

## Open questions carried forward

See the "Open questions" section of `docs/design.md` — six items, all genuine design forks rather than research niceties.
