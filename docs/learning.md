# flashdb — learning and adaptation

Budget: 220 lines / 21,000 chars

How the engine learns, how it changes, and how it talks to a human. Governed by Invariants I4 (off-path and scheduled), I10 (one scalar objective) and I11 (funded, proven, deployed).

## One rule above all

**A bad layout may be slow, never wrong** (I2). Everything below — promotion, expertise, drift, canary — is only possible because layout is never on the correctness path. Any proposal that makes correctness depend on a learned state is rejected by this rule, not by a heuristic.

## The trace

The trace is the training set. Three consumers read it: the learner, the advisor and the deploy history. **These are two tables, not one** (`review-02` §3): a `trace` table holding observations, and a separate `suggestions` table holding advice plus the human accept/reject record, which doubles as the deploy changelog. The earlier phrasing called both "one table", which left the identity undefined.

Recorded per pattern: shape (canonical form), request count, load window, bytes read, tiles touched, dependent misses, queue occupancy, latency histogram, wasted work, conflict/retry rate, rows returned, projection usage.

Two rules:

- **Shapes, not values.** Column *statistics* are permitted; raw values and per-row data are not. This is a privacy boundary and a size boundary at once.
- **Two classes of recorded data, with different durability** (`review-03` §3). *Measurements* — counters, histograms, residency, per-tile statistics — are `recomputable`: losing them costs learning time and nothing else. *Decisions* — the human accept/reject record and the deploy changelog — are **not** recomputable and must be durable, because they record an irreversible human act. Treating both as `recomputable` was a contradiction: it would drop the only record of what a human approved.
- **The recording path must be specified, or I4 is false.** Per-request counters and latency histograms are written on the request path; I4 permits that only as bounded measurement, never as analysis. The write path is per-CPU lock-free ring buffers flushed off-path, with a stated bound on the on-path cost (`review-02` F14).
- **The telemetry store obeys its own design.** It is a tiled, tiered, quantized store: sketch-based counters (HLL for cardinality, HDR/t-digest for latency), tiered rollup (minute → hour → day, recent fine, aged coarse, then evicted), percentiles never means, and a hard cap on its share of cache (target ≈1%; `design.md` still lists the exact share as an open question). **Open (`review-02` F14):** whether that share is carved out of the single shared budget that promotions compete for is unstated, and it changes the promotion economics. If the engine cannot store its own metrics efficiently, that is the first thing it has to say about itself.

## Pattern keys

- **Template versus instance.** Learn on shape with constants stripped; execute on instance. Otherwise every literal is a new pattern.
- `pattern_key = (normalizer_version, canonical_form)`. Without the version, a normalizer upgrade silently invalidates all accumulated learning.
- **Canonicalize before learning** (`contracts.md`). The same intent spelled five ways is five patterns, and the signal shatters.

## Weights are time series

A pattern's weight is not a scalar. It is a series over load windows, because deployments are long quiet periods punctuated by short intense bursts and optimizing the average is actively wrong (`objective.md`).

Consequences: the objective is over regimes; a regime needs a minimum sample count before it may influence a decision; and a rising weight is a *drift signal*. **Open (`review-01` F8):** the aggregator (worst regime / CVaR / which percentile) is unchosen; a maximum taken over many noisy windows is upward-biased by construction, so more windows manufacture apparently bad regimes; the minimum-sample gate **excludes the peak by construction**, since a short burst is by definition the least-sampled regime; and "earlier than the raw average" means acting on the fewest and noisiest points.

## The tier ladder

This is **profile-guided, offline, tiered specialization** — not JIT. Nothing is compiled or specialized at first execution; it is specialized only after a long observation window proves it important, and the more important it is, the more of the system's budget it may consume.

| Tier | Investment | Trigger |
|---|---|---|
| interpret | none | default; the long tail stays here |
| vectorize | small | pattern shows up repeatedly |
| compile | medium | pattern weight justifies compile + code-cache cost |
| specialize layout | large | weight **and** stability **and** a demonstrated win |
| replicate / co-locate | large | fan-out collapse pays for the write amplification |
| precompute | very large | the pattern is the workload |

Established precedents for this ladder: JVM tiered compilation (interpreter → C1 → C2 driven by profile counters), profile-guided optimization in compilers, and LSM tiering.

### Promotion requires three things, not one

1. **Weight** — it matters.
2. **Stability** — the shape has been stable long enough to *amortize the investment*. A longer observation window buys stability, but the higher the tier already reached, the more a drift hurts, because the investment is un-amortized the moment the pattern changes.
3. **Proof** — a demonstrated win, not a predicted one.

**Open — none of the three is decidable as written (`review-01` F12).** "Stability" has no statistic, window or threshold. "A demonstrated win" has no effect size, no significance criterion and no matched-durability rule at promotion time. And "proof, not a prediction" is self-contradictory as previously stated, because the only instruments offered are replay — which *is* the simulator whose trust this document admits is unknown — and a canary, which this document elsewhere declares to be a conservative lower bound and therefore incapable of demonstrating superiority. Promotion proof is circular until an explicit calibration order breaks it (for example: small randomised trial, calibrate from it, then promote).

The steepness estimate that this section previously hung on the accept/reject record is separately unidentifiable — see `objective.md`.

### One shared budget, and stop at the knee

Promotions draw from a **single finite budget** (cache, write amplification, reorg bandwidth, background IO). They compete. The top pattern may take nearly everything — but only by starving the rest, and that trade must be visible, because the patterns that lose are exactly the ones with nobody advocating for them.

Cost rises and gain shrinks with each tier, so the next increment must still pay for itself against the pattern's weight. A demotion path is mandatory and must reclaim the budget; a promoted pattern that drifts is a liability, not a stale asset.

**Open (`review-01` F12).** "The knee" was previously given as the stopping rule. It has no definition, no estimator and no confidence bound, and does not exist at all on a smooth diminishing-returns curve. The correct formulation is a budget-constrained allocation across competing patterns: promote while the marginal predicted angriness gain exceeds the **shadow price of the shared budget**, where that price is endogenous and rises as other patterns consume the budget. The shadow price, its estimator, and the confidence interval on the marginal gain are all unstipulated.

**Open (`review-01` F15).** Background work scheduled into quiet windows is also an accounting exploit: charging the cost in the window with the lowest weight while claiming the credit in a high-weight window makes any background workload appear nearly free. Background work must be charged at an opportunity cost in queue slots.

## Gray release

Gray release is nearly free here, and this is the payoff of the drift design: because the reader is layout-polymorphic, **mixed layouts are already a legal state**. A canary is simply "these key ranges get the new layout, everything else keeps working" — no migration, no dual write, no barrier.

| Property | How |
|---|---|
| scope | a fraction of tiles or key ranges |
| monitoring | regime-matched p99, retry/conflict rate, bytes read per satisfied pattern, angriness delta |
| rollback | O(1) — the previous version is still on disk, so rollback is a pointer flip |
| cost | retention window × space, charged to the budget |
| record | one row in the changelog table |

Two traps that make a canary lie:

- **Regime mismatching.** A canary measured in a quiet window against a baseline measured in a burst will lie confidently. Canary comparison must sample matched load regimes — a second reason the time-series weights are load-bearing rather than a nicety.
- **Contamination.** A canary shares the cache and the device queue with everything else, so it is not a clean A/B. Treat a canary result as a **conservative lower bound**, never as a measurement.
- **Which means a canary cannot serve as the promotion proof it was offered as (`review-01` F14, D7).** A conservative lower bound can never establish superiority. Together with the replay problem above, promotion proof is circular as written.
- **No power analysis exists anywhere (`review-01` F9, F12).** Nothing in this document or `dev-loop.md` states an effect size, a variance, a cluster count or a sample size — not for the sample-count gate, not for the canary, not for promotion, and not for a traffic split. A traffic split over tiles is a *cluster-randomised* design with a small number of Zipfian-weighted clusters, so the effect is confounded with which keys landed in treatment, and a request touching both arms has a mixed path belonging to neither.

## Layout and schema changes are deploys

Versioned, canaried, rollback-able, changelogged, exactly like software. The changelog is what lets a regression be correlated with a change. The suggestions/feedback table (`learning.md`, advisor) is the natural home: one table, three consumers.

## Prediction and predication

Default to **predication (if-conversion)**, not prediction. `contracts.md` marks blocks `harmless-if-not-applicable` precisely so the scheduler can run them unconditionally without deciding anything — this skips the guard, and in a database the guard is itself an IO (tens to hundreds of microseconds), so if-conversion is worth *more* here than in a CPU, not less.

The general form is **access prediction** (temporal), of which branch prediction is the conditional case. It subsumes speculative prefetch of the next stage. Layout optimization is the spatial half; this is the temporal half of the same solve.

Hard rules:

- **Speculate reads, never writes** (I9). A speculative write requires undo, which reintroduces exactly the MVCC/GC machinery this design removed.
- **Price waste in queue slots, not bytes.** A wasted read costs a slot a real request could have used. **Corrected (`review-05` SS-07):** the earlier claim that "a wasted read is free on an unsaturated device" is false for rotating media, where an unsaturated device is precisely where a wasted read costs *most* — it adds a seek and a rotation to the critical path of the demand read behind it, so 20 dependent reads on a 7200 rpm disk is ~180–240 ms rather than 1.6 ms. The device class must therefore enter the policy: **no speculative prefetch on seek-bound devices**, and the HDD policy (sorted batch sweep) must be written into the contracts rather than asserted in the glossary. **Also (`SS-23`):** speculative traffic must be tagged in the trace, or it inflates `bytes read` and `tiles touched` and biases the learner against layouts that were merely prefetched.
- **Only pays when the guard needs a fresh read.** If the condition is answerable from data already in hand, there is no stall to hide.
- **Gate on load mode.** The idle/latency mode is the speculation budget; under load, speculation is off or hard-bounded.

The predictor is per-pattern access statistics from the same trace table, scored by the same objective. No new subsystem. Probabilities feed back into layout: a cold fallback that stalls is *not* co-located; a hot ~50/50 stall is a candidate for co-locating both paths or restructuring the request.

## The advisor

Two layers, strictly ordered: **facts → LLM → validation.**

### Layer 1 — mechanical

A data-mining problem over the trace, with a bounded ceiling: it can only propose changes expressible *within the request language* — add a predicate, drop a projection, group fields, move a block to the next stage. Proposals that reach outside the language, such as a schema merge or a new derived layout, are layer 2's territory; the two layers previously disagreed about their own scope (`review-04` L-17).

- **Hidden predicates** — a projection column with near-zero entropy or one dominant mode that is not in the condition. "Field F is constant across 99.7% of these results; adding it as a condition enables pruning — estimated N×."
- **Correlated predicates** — mutual information between condition fields.
- **Unused projections** — the leanness lint. **Corrected (`review-04` L-04, L-17):** the engine can only observe *within-request* use — a column returned to the client and used client-side is invisible to it — so the lint is restricted to columns no later block consumes, and it can no longer claim to know what a requirement "really" was.
- **Independent-block violations** — a block that depends on a sibling's write, in a stage that asserts independence. "Move it to the next stage."
- **Column clusters** — association-rule mining over fields requested together.
- **Join paths that always co-occur** — derived-layout candidates.
- **Collapsible fan-out** — "you fetch N children per parent; ask in one request and we can co-locate."
- **Mergeable write shapes** — candidates for a counter/set encoding.
- **Dead fallbacks** — "this `ifEmpty` fires 99.7% of the time; you are paying twice. Make it the primary path."

Every candidate is ranked by predicted angriness delta from the same cost model. This is exactly the data-science toolkit — entropy, mutual information, association rules — applied to the engine's own trace.

### Layer 2 — LLM

Reaches what layer 1 cannot: *semantic* problems. "You compute stock three different ways across three requests and they disagree by 40% — is that intentional?" "`price` and `discount_price` are always used together; consider merging." "This pattern looks like a monthly report that wants a derived layout."

Constraints, each of which decides whether this is an asset or a liability:

- **Never in the data path.** Advice only. The LLM must never sit between a request and its answer.
- **The LLM proposes; the cost model disposes.** Layer 1's what-if model validates every structured suggestion before it is shown or applied. An LLM that invents a schema change and gets it applied is a data-loss bug with extra steps. Generator–verifier, and the verifier is mechanical.
- **Strict ordering: facts → LLM → validation.** Wrong in either direction and it is either a hallucination or a useless parrot.
- **Never in the hot path.** A budgeted periodic pass over the trace.
- **Structured output is the valuable half.** Because the request surface is canonical, a suggestion about a *request* is a machine-actionable diff the engine can validate. **Qualified (`review-04` L-17):** this holds only for request-shaped suggestions. A schema change has no canonical form to diff against (`contracts.md`, L-08), so layer 2's schema proposals are advice in prose, not a diff.
- **Nondeterminism and churn.** Version the advisor, keep output comparable over time, dedupe and rate-limit suggestions, or the advice itself oscillates exactly when the workload is drifting.

### Privacy tiers

The database never configures the LLM; a human does. Shield level is configurable, up to shielding nearly everything:

| Tier | LLM sees | Cost |
|---|---|---|
| full stats | shapes, statistics, distributions | none |
| coarse stats | bucketed/ordinal statistics only | some effectiveness |
| shapes only | schema and request structure, no statistics | must reason under assumptions |

**The shield governs egress only.** The validator reading a suggestion may always consult real data inside the database. So shielded mode never means unverified — it means *hypothesis proposed blind, then checked against ground truth*, with the provenance recorded (`inferred-under-shield` versus `observed`). Plenty of real problems are structural and need no values at all, which is why the effectiveness loss is partial rather than fatal.

## Push and the human loop

Fire-and-forget. **No delivery guarantee is needed, and the reason is structural rather than a risk accepted**: suggestions and feedback live in the database, so the webhook is a wake-up nudge (cache invalidation), not a delivery channel. A lost nudge self-heals on the next poll. This removes at-least-once, dedup, idempotency and retry-storm handling from the delivery path entirely.

What is still required:

- **A monotonic cursor**, so the receiver knows what is new without diffing the table.
- **A polling fallback**, so a lost nudge self-heals.
- **A bounded, non-blocking outbound queue** so a webhook can never stall or crash the writer. Drop and increment a counter on overflow; never grow unbounded.
- **HTTPS-only, explicit allowlist** — an outbound webhook from a database is an SSRF and exfiltration surface.
- **Shape/stat payloads only** — no row values.

### The human record is a label set

The feedback table records accept/reject and, crucially, **what is changeable and what is not**. That is a labelled training set for *which relaxations this user will actually concede* — so the engine stops asking for concessions the user never grants and asks for the ones he does. It also fits the angriness steepness (`objective.md`): the risk-aversion knob is learned from accepted versus rejected advice, on a much slower timescale than layout decisions.

### Do not build an alert-fatigue machine

A database that can page a human at 3 a.m. will, and then it will be muted — taking the one signal that matters with it. Push only for threshold crossings and rare-but-urgent events; everything else is a periodic digest. Consequently the rare-but-urgent pattern class needs a declared priority and a reserved budget, or the engine is correct and useless at the worst possible moment.

## Off-policy evaluation

The learner's feedback is **censored**: it only ever observes the cost of the layout it chose. Counterfactuals are unobservable in production. Three routes, all used:

1. **Simulator** — a calibrated cost model predicting the counterfactual (`dev-loop.md`).
2. **Deliberate small-scale experiment** — traffic split, bounded and reversible.
3. **Shadow tiles** — a small population of tiles maintained under an alternate layout purely to measure it.

**Open — none of the three identifies a counterfactual as stated (`review-01` F9).** No identification assumptions appear anywhere: positivity/overlap, ignorability, SUTVA, stationarity. The layout assignment is made by the policy under evaluation, so regressing cost on layout is confounded by design, and the simulator inherits that bias.

- The **estimator** is unnamed. It must be stated (direct method, inverse-propensity scoring, or doubly robust), with its variance.
- **Traffic split over tiles is cluster-randomised** with a small, Zipfian-weighted number of clusters, so the effect is confounded with *which keys* landed in treatment, and a request touching both arms has a mixed path that belongs to neither.
- **Interference violates SUTVA.** The arms share one cache and one device queue, so treating some tiles changes the environment for the rest — the same contamination this document already concedes for canaries.
- **Shadow tiles** are policy-selected and measured only for requests that happen to touch them; external validity to the tile population is unestablished, and their write amplification is charged to the shared budget, distorting the objective being evaluated.

Traffic-split randomisation must be independent of the policy, and the estimand must be named in advance — a per-tile average effect or a global angriness delta under a policy — because the two need different designs.

**Per-tile learning needs pooling (`review-01` F16).** With ~10⁶ tiles (10 TB at 8 MB) and several candidate layouts each, every arm has too few pulls. Per-tile-independent learning has no shrinkage, no sharing across tiles and no control for the number of simultaneous comparisons, so regret would be dominated by estimation variance rather than policy quality. A hierarchical or shrinkage prior, shared features, or a bucketed policy is required.

**Credit assignment is unspecified (`review-01` §2.14).** Angriness is a property of a *request*; the decision is per *tile*; one request touches many tiles that interfere through a shared cache. Which tile is credited or blamed for a request's angriness delta is stated nowhere, and without it the per-tile policy has no consistent reward signal.

Metric: **regret against an oracle layout**, not absolute throughput — with the caveat that `dev-loop.md` records serious problems with that metric. And the honest caveat that must not be forgotten: uncalibrated what-if prediction is optimistic in the literature, so replay and canary calibration keep the predictor honest. A promotion decided on an uncalibrated prediction is a guess wearing a number.
