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

- 2026-09-15 — pi / claude — **Review 02 (feasibility, methodology, adoption) received and dispositioned.** The reviewer read revision `1c8f148`; every defect was re-checked against the current tree at `ec0b8c1`, and findings already fixed there are marked as such rather than re-fixed. **Verdict: accept the substance — the plan as written is untestable, and its own motivating workload is unsupported.**

Accepted, 18 findings (F1–F18). The load-bearing ones:

| Finding | Core claim | Action |
|---|---|---|
| F1 | the oracle is either infeasible (per-tile, combinatorial) or rigged (uniform, and a per-tile learner beats it, so regret ≤ 0); a brute-forceable instance is smaller than cache, so the fan-out premise is never exercised | relaxed-oracle replacement recorded in `dev-loop.md` |
| F2 | the harness cannot measure the objective: no service-time decomposition, no deadline field, no per-tile footprint, no reorg accounting — and group commit makes per-request fsync load-dependent, contradicting the anchor's load-independence claim | recorded; trace-v0 field list required |
| F3 | the objective is not numerically frozen: `s` needs a user label set a thesis has no source for, and `cap` is undefined, so I10 is unenforceable | open question |
| F4 | per-block atomicity contradicts the motivating `decrement stock iff insert lines`, so the inventory correctness oracle fails under crash injection | **needs an author decision** — see below |
| F5 | `recomputable` derived tiles reachable from a durable root mean a crash yields wrongness, not slowness; `max_staleness` puts layout on the correctness path, so I2 is false as written | fixed in `contracts.md` (coverage/version rule, reachability rule) |
| F6 | ten subsystems, no cut list, no schedule, no kill criterion | open; proposed v0 cut list recorded |
| F7 | build order is not risk-ordered; step 1 cannot output a calibrated model when step 2 is its calibration target | corrected, with a proposed 0a/0b reorder recorded |
| F8 | equal durability is unachievable at benchmark time, since the fault-injection harness is deferred | open |
| F9 | the declared-layout control is missing, so a win over any other engine proves nothing about learning | two control baselines added |
| F10 | one workload per hypothesis is not attribution — workloads differ on many axes | open; ablation family required |
| F11 | the as-of join and anti-join workloads may not be expressible in the frozen grammar | open; write all five request trees before freezing |
| F12 | TPC-C's distribution claim is overbroad (NURand hot set) and the reference is not free | recorded |
| F13 | drift injection is a single unreplicated run with an undefined oracle | seeds/IQR/time-to-recover recorded |
| F14 | recording is on-path as specified; the telemetry carve-out is unstated | recording path specified; carve-out marked open |
| F15 | the effect annotation is ill-typed (a third "additionally" value), so read/write eligibility is undecidable | fixed: two orthogonal fields plus a snapshot rule |
| F16 | a `normalizer_version` bump can change which requests are accepted, so a routine upgrade can refuse production traffic | fixed: acceptance grammar versioned separately from the learning key |
| F17 | the LLM advisor violates I10 (no angriness price), has no success metric and no label source | open; recommended cut from v1 |
| F18 | no observation can refute the thesis, because every candidate refutation is pre-labelled not-a-measurement | falsification contract recorded |

**Needs an author decision — F4.** Per-block atomicity (the author's Round 6 instruction) contradicts the motivating workload, whose correctness oracle requires `decrement stock iff insert lines` to be atomic. Either (a) make request-level atomicity the default and per-block the opt-in — recommended, because one commit record already covers the whole request so only *publishing* a partial prefix costs extra, and it inverts the default at no cost — or (b) keep per-block atomicity and define a documented client-side compensation contract, with a reference client included in the correctness oracle. `design.md` I5 was also asserting request-level atomicity in contradiction to I6; corrected to remove the claim.

Already fixed at `ec0b8c1`, no action: size-budget headers now state lines and chars, the telemetry-share contradiction, the glossary's angriness definition, the `I4` wording, and the round count in `design.md`.

Rejected, with reason:

- *"Normalizer is mandatory or conditional"* — not a contradiction. I12 settles that a server-side normalizer exists; the open question is only whether it must additionally be *effect-aware*, which depends on whether program-level fallback is ever added.
- *"Reviewer count disagreement"* — `spec.md` says "at least five" and six were spawned. A satisfied floor is not a contradiction.
- *"`contracts.md` exceeds its character budget"* — true of `1c8f148`; the header at `ec0b8c1` declares both lines and chars and both are within it. The ambiguity the reviewer identified was real, and stating both is the fix.

Missing-items list (§2, twelve items) accepted in full. Highest value: the absent client-side contract for prefix commits (the client is part of the safety claim and does not exist in this repository), the absence of any effort, schedule or kill criterion, the absence of a measurable performance target, and the absence of any write-side budget analysis — every workload hypothesis in `dev-loop.md` is read-side.

Diff reviewed: `1c8f148`. Reviewer: fresh-context subagent, aspect = feasibility / methodology / adoption.