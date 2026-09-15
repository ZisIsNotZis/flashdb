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
- 2026-09-15 — pi / claude — **Review 03 (prior art, novelty) received and dispositioned.** **Verdict: accept — the closing novelty claim was false as written, and roughly a dozen mechanisms in this design are shipped prior work.**

Accepted: PA-01 (the "nothing combines these" sentence was false and contradicted the table directly above it), PA-02 (**SageDB**, CIDR 2019, is this thesis's actual neighbourhood and was absent from the entire repository), PA-03 (layout-as-materialized-view renames the view-selection problem — Harinarayan/Rajaraman/Ullman, Gupta, Agrawal et al., Gupta & Mumick, DBToaster — and never engages its NP-hardness), PA-04 (**H2O**, SIGMOD 2014, is the closest work in existence to the core mechanism: per-chunk adaptive layouts from the observed query mix, raw data as fallback, cost model deciding — and it was not in the table), PA-05 (`pg_stat_statements`-style constant stripping already gives a per-intent learning key), PA-06 (Oracle Automatic Indexing and SQL Server Automatic Tuning already ship funded, validated, auto-rolled-back promotion), PA-07 (`design.md` listed "error budget as the objective function" as open while `objective.md` concedes it is an error budget renamed), PA-08 (per-block offset directories are Parquet/ORC/SSTable format properties), PA-09 (BtrBlocks already selects encodings per chunk from data statistics), PA-10 (Monkey/Dostoevsky already do one scalar objective with per-component policies under one shared budget, and their optimality argument does not transfer to tiled storage), PA-11 (the trace-replay oracle *is* the AutoAdmin what-if lineage), PA-12 (Data Blocks pre-empts joint representation+execution; and the ICDE 2018 attribution was wrong), PA-13 (if-conversion is Allen/Kennedy 1983; selection vectors are X100), PA-14 (the Napa/Tesseract row was over-claimed and partly unverifiable), PA-15 (citation hygiene).

What changed:

- New `docs/prior-art.md`: a positioning ledger with an *established / delta / status* structure, the missing groups added (learned databases, view and index selection, IVM, workload compression, shipped automatic tuning, off-policy evaluation, tail latency), and the removal of the false claim.
- `docs/design.md` prior-art section replaced by a summary and pointer; the closing sentence deleted.
- `README.md` — settled-tense claims rewritten as design intent; citation caveat added there (it previously existed only in `design.md`, so a reader of any other file saw unqualified novelty claims).
- `design.md` I3 clarified: the shared budget is a global *resource constraint* arbitrated from local statistics, not global layout state. As written, I3 and the shared budget contradicted each other, which also undercut the novelty claim.
- `learning.md`: measurements are `recomputable`, but the human accept/reject record and the changelog are **not** — they record an irreversible human act. Treating both as recomputable would have dropped the only record of what a human approved.
- `objective.md`: the worked example also silently fixes `s = 1 ms`, so three of four parameters are pinned and none justified.

Citation verification — the reviewer's highest-value request, and it found more than the review did. A probe showed **`dblp.org` and `sigmod.org` return 200**, so the block was assumed rather than measured; Crossref queries then resolved seven items. **Kohn, Leis, Neumann, ICDE 2018 confirmed** (the "Menon" attribution was wrong, as the reviewer suspected). **BtrBlocks author list was wrong** (Kuschewski, Sauerwein, Alhomssi, Leis — not "Neumann, Freitag"). **"ArcaDB" deleted**: it exists as a 2024 disaggregated query engine, not adaptive-layout prior art, so pairing it with NoDB in the cracking lineage was simply wrong. **"Tesseract" deleted**: no database system of that name found. H2O, Monkey, FSST and Harinarayan et al. confirmed. Follow-up: `.scratch/02-citation-verification/`.

Stale, no action: the "13 rounds" count and the size-budget header ambiguity were already fixed at `0e1ac36`.

**Adopted claim (PA-01's recommendation, verbatim in substance).** The defensible claim is now: for a single-node IO-bound workload in the ~10 TB / ~128 GB regime, a single SLO-capability-anchored scalar penalty scored by a trace-replay cost model is *sufficient* to drive a per-tile **learned** layout policy — jointly with encoding, speculation and compilation — to within a measured regret of an oracle layout at equal durability, while grammar-enforced canonicity makes the learning key exact rather than a best-effort digest. Every component is established prior work; the contribution is **sufficiency and unification at tile granularity**, it is empirical, and it is falsifiable. Three sub-claims survive: a learned rather than heuristic per-tile policy; one shared budget across heterogeneous decision types (counterfactual: four independent budgets); and grammar-enforced canonicity measured as pattern-shatter rate against the constant-stripping baseline.

Diff reviewed: `1c8f148`. Reviewer: fresh-context subagent, aspect = prior art / novelty. Verification second-pass by the parent session with live Crossref access.
- 2026-09-15 — pi / claude — **Review 04 (language design, canonicity) received and dispositioned.** **Verdict: accept. 24 findings, 6 blockers — and this is the first review to hit the engine's own interface rather than the claims around it.** The three "blockers" it names are real and were invisible in three prior passes.

Accepted, and the important ones:

- **L-01 — `ifEmpty` is never defined, because no block shape has a declared result type.** `insert`, `counter-add` and `conditional-update-by-key` return no rows, so under the natural reading the fallback always fires — and `decrement iff available, else backorder` is therefore either inexpressible or silently corrupting. Genuine blocker; not previously noticed.
- **L-02 — the continuation contract is not implementable for reads.** The design guarantees idempotent *application* but not idempotent *observation*: on crash-before-ack, re-running a read block observes post-write state. Read-then-act is broken by prefix commit combined with having no payload WAL. This is the sharpest finding in four reviews.
- **L-03 — no predicate or expression language exists**, and the repository's own declared workloads (SCD-2 interval join, anti-join, `stock < reorder_point`) are inexpressible. The scope set the reviewer had to work from appears *nowhere in the repository* — it was stated in conversation and never written down.
- **L-04 / L-10 — canonicity as stated is a category error.** "Intent" is in the user's head; only requests can be canonicalized. The achievable property is an idempotent normal form over a declared, decidable equivalence relation `E`, with termination and confluence — none of which is specified. Six residual equivalence classes remain in the current grammar even after the distributivity ban.
- **L-05 — the stage-level fallback restriction silently reversed an author instruction.** The decision log records the author refusing to restrict the grammar for exactly this reason, and the contract then presented the restriction as a *payoff*. Two-stage conditional branches are inexpressible. Owned as a process failure, not just a design one.
- **L-09 — the normalizer's soundness condition is wrong.** "Is a read" is insufficient: hoisting a read out of a conditional changes which snapshot it observes. The condition must be data dependency. Block-boundary rewrites are also in unresolved tension with per-block idempotency.
- **L-11 — the pattern key was not a stable identity**: no schema version, no service class. `durable` and `lossy` requests were being aggregated into one latency histogram.
- **L-12 — my own worked grammar example was broken three ways**: it did not validate against the grammar above it, it did not express its own "else" (the fallback sat on stage 2), and it referenced `$1.customer_id`, which is empty on the path it claimed to support.
- **L-18 / L-19 / L-20** — "place cross-request invariants in one request" is false under per-block atomicity (correct rule: one *block*); serializability is unqualified despite `max_staleness`; and two parallel blocks can both read `stock == 1` and both decrement, so the headline use case can oversell. Read-set validation cannot catch that, because it validates tiles against the *request's* snapshot, not sibling writes.
- **L-16 / L-17** — "no string parsing of any kind" is false (JSON, string constants, string-encoded `$n.field` paths), and the SQL contrast is a category error: SQL is hard because of relational equivalence, not because it is text. And "canonicity makes the advisor computable" is unsupported — entropy and mutual information come from the trace; worse, the leanness lint needs client-side usage the engine cannot observe.
- Also accepted: L-06 (cannot branch on a write's outcome), L-07 (shape set mislabeled and missing predicate-addressed writes), L-08 (schema canonicity undefined), L-13 (join type and cardinality undefined), L-14 (no ordering, limit or cursor), L-15 (reject versus normalize unresolved), L-22, L-23 (I13 listed four reserved interfaces, the contract six), and all 16 missing-items including the absence of any request-size bound and of any golden canonical-form corpus.

Fixed in place: the broken example; the false "no string parsing" claim; the withdrawn advisor claim; the corrected soundness condition; the extended pattern key; serializability qualified by `max_staleness = 0`; the invariant remedy changed to "one block"; the shape set split into reads and writes with the missing shapes named; the leanness lint restricted to within-request usage; the layer-1/layer-2 advisor scope reconciled; `I13` reduced to a single pointer at the contract's list; and a 14-row Open table added to `contracts.md` recording every unresolved grammar item.

Already fixed at earlier revisions, no action: L-21 (the effect annotation is two orthogonal fields as of `0e1ac36`), L-24's aspect count (`spec.md` says "at least five" and records six), and the `R-DOC.6` location (noted in `README.md` at `c6b4c94`).

**Author decisions now outstanding — five, all recorded in `design.md`'s open questions:** program-level fallback (L-05); reject versus normalize (L-15); predicate-addressed writes (L-07); ordering and pagination (L-14); and request-level versus per-block atomicity (`review-02` F4). Each changes design truth and is reserved to the author under `R-DOC.1(a)`.

**Ticket size:** this file is approaching the ~150-line limit in `R-TKT.1.3`. After the final two reviews land, the disposition history moves to a successor ticket and this one closes.

Diff reviewed: `1c8f148`. Reviewer: fresh-context subagent, aspect = language design / canonicity.