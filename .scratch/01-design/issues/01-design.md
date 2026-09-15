# 01-design — write the flashdb design of record and challenge it

Status: claimed
need-review: yes
need-test-cases: no (documentation deliverable; no behavior to regress)
Spec: `.scratch/01-design/spec.md`

## Issue

Thirteen rounds of design conversation exist only as chat. Nothing is written down, so nothing is reviewable, and the interfaces that cannot be cheaply retrofit are not yet frozen. The design has never been adversarially attacked.

## Acceptance criteria

See `spec.md` — nine items. In short: six documents written to the project's doc conventions, every budget and style rule respected, an adversarial review from at least five distinct aspects with every finding dispositioned, and a committed revision identifiable by hash.

## Deliverable

- `docs/design.md`, `docs/objective.md`, `docs/contracts.md`, `docs/learning.md`, `docs/dev-loop.md`, `docs/glossary.md`
- `.scratch/01-design/decisions.md` — raw attributed decision log, thirteen rounds
- Adversarial review findings and their dispositions, in `## Comments` below

## Know-how learned

To be filled as the review returns.

## Comments

- 2026-09-15 — pi / claude — Wrote the six documents, the spec and this ticket. Recorded a curated portion of the prior `.scratch/decisions.md` design-log content into the topic-centric documents (R-DOC.1(c), write-it-down go given by the user). The log itself was retained, trimmed to decisions only, so that no information is duplicated between `docs/` and `.scratch/` (R-DOC.3).
- 2026-09-15 — pi / claude — Spawned six fresh-context adversarial reviewers (fork=false: no inherited conversation, prompts scoped to the written documents only), one per aspect: database internals and crash correctness; storage systems and hardware; language design and canonicity; optimisation and learning theory; prior art and novelty; feasibility, methodology and adoption. Findings and dispositions to follow.
