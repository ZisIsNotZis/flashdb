# 01-design — write the flashdb design of record and challenge it

Status: claimed
need-review: yes
need-test-cases: no (documentation deliverable; no behavior to regress)
Spec: `.scratch/01-design/spec.md`

## Issue

A multi-round design conversation exists only as chat. Nothing is written down, so nothing is reviewable, and the interfaces that cannot be cheaply retrofit are not yet frozen. The design has never been adversarially attacked.

## Acceptance criteria

See `spec.md` — nine items. In short: six documents written to the project's doc conventions, every budget and style rule respected, an adversarial review from at least five distinct aspects with every finding dispositioned, and a committed revision identifiable by hash.

## Deliverable

- `docs/design.md`, `docs/objective.md`, `docs/contracts.md`, `docs/learning.md`, `docs/dev-loop.md`, `docs/glossary.md`
- `.scratch/01-design/decisions.md` — raw attributed decision log, thirteen rounds
- Adversarial review findings and their dispositions, in `## Comments` below

## Know-how learned

- Adversarial review is worth running before implementation, not after. The optimisation/learning-theory aspect alone invalidated the implementability of the objective layer, which is the layer everything else is priced against.
- Recording findings as `Open` markers at the point of use — rather than deleting the wrong claim — preserves the reasoning that produced it while preventing a reader from trusting it.

## Comments

- 2026-09-15 — pi / claude — Wrote the six documents, the spec and this ticket. Recorded a curated portion of the prior `.scratch/decisions.md` design-log content into the topic-centric documents (R-DOC.1(c), write-it-down go given by the user). The log itself was retained, trimmed to decisions only, so that no information is duplicated between `docs/` and `.scratch/` (R-DOC.3).
- 2026-09-15 — pi / claude — Spawned six fresh-context adversarial reviewers (fork=false: no inherited conversation, prompts scoped to the written documents only), one per aspect: database internals and crash correctness; storage systems and hardware; language design and canonicity; optimisation and learning theory; prior art and novelty; feasibility, methodology and adoption. Findings and dispositions to follow.
- 2026-09-15 — pi / claude — **Review 01 (optimisation, learning theory, statistics) received and dispositioned.** 19 findings (F1–F19), 16 document defects (D1–D16), 7 undecidable rules (R1–R7) and 14 missing-items. Verified against the text before acceptance; the review's own arithmetic was rechecked. **Verdict: accept the substance — the objective layer is not well-posed and cannot be implemented as written.**

Dispositions:

| Finding | Verdict | Action taken |
|---|---|---|
| F1 x undefined (mean anchor vs percentile penalty) | **accept** | `objective.md` marked Open with the requirement stated |
| F2 `k` unsourced, silently 1 in the example | **accept** | same |
| F3 `service_time` not independent of the decisions it prices | **accept** | same |
| F4 M/M/1 wrong here; ρ ≥ 1 undefined | **accept** | same, with Pollaczek–Khinchine named as the first-order correction |
| F5 aggregation unspecified; cap contradicts I10 | **accept** | `objective.md` decision-form section corrected; I10 conflict recorded |
| F6 bounded penalty makes shedding optimal; burn rate decoupled from magnitude | **accept** | operational-reading section corrected |
| F7 convexity → fairness false (sigmoid is concave past the inflection) | **accept** | false claim removed, replaced with the correct curvature |
| F8 time-series gate biases toward noise and excludes the peak | **accept** | `objective.md` and `learning.md` |
| F9 OPE routes do not identify counterfactuals | **accept** | `learning.md` — assumptions, estimator and randomisation now required |
| F10 oracle is not ground truth; regret ill-defined | **accept** | `dev-loop.md` rewritten |
| F11 steepness `s` unidentifiable; two-timescale unsupported | **accept** | `objective.md`, `learning.md` |
| F12 knee/stability/proof undecidable; proof circular | **accept** | replaced with a shadow-price rule, marked Open |
| F13 parameterisation mismatch; placement contradiction | **accept, one correction** | reviewer wrote `σ(−20) ≈ 0` in float32; 6e-10 is not zero. The mechanism (underflow at large `a/s`) is real and recorded; the specific value was wrong |
| F14 canary lower bound cannot be proof; calibration functional unspecified | **accept** | `learning.md`, `dev-loop.md` |
| F15 gameability (window exemption, regime starvation, shedding, advisory self-manipulation, measurement ownership) | **accept** | recorded as an open question in `design.md`; background charged at opportunity cost |
| F16 per-tile bandit, no pooling, no multiplicity control | **accept** | `learning.md` + open question |
| F17 saturating sigmoid is risk-insensitive in the tail | **accept** | corrected |
| F18 worked example drops `a`, fixes `k` | **accept** | example relabelled as illustrating the comparison only |
| F19 load-dependent baseline is not a harmless constant | **accept** | corrected |
| D1–D16, R1–R7 | **accept as documentation defects** | fixed where mechanical (round count removed, glossary, budget headers, I4 wording, telemetry share reconciled, policy-location note in README); the remainder re-marked `Open` rather than papered over |
| §2.14 credit assignment unspecified | **accept** | `learning.md` + open question — a request touches many tiles, so the per-tile reward signal was never defined |

Deliberately **not** done: no design decision was taken unilaterally on the objective. `R-DOC.1(a)` reserves changed design truth to the author, so the findings are recorded as decisions-needed rather than resolved. A consolidated correction pass follows once the remaining five reviews return.

Diff reviewed: `1c8f148` (revision under review). Reviewer: fresh-context subagent, aspect = optimisation / learning theory / statistics, no inherited conversation.
