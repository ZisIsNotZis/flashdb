Budget: 180 lines / 18,000 chars

# flashdb — the objective: angriness

**Angriness is the single scalar objective.** Every decision in the system — which layout, when to reorganize, what tier to promote to, whether to speculate, whether to compile — is evaluated as a predicted angriness delta through one calibrated cost model (`design.md` I10).

Angriness is a **rate**, not a total: angriness per unit of offered load, or per unit time. Reported as a total it can be made to look good by simply receiving less work.

> **Status: not yet well-posed.** An adversarial review (aspect: optimisation and learning theory) found that the estimand, the anchor, the parameterisation, the aggregation, the oracle and the estimators are all undefined or self-contradictory. Findings and dispositions are recorded as `review-01` in `.scratch/01-design/issues/01-design.md`. The defects are marked `Open` inline below rather than deleted, because the decisions they need are the author's, not the reviewer's. **The objective cannot be implemented or falsified until F1–F5 are resolved.**

## Why a soft penalty and not a hard constraint

A constraint returns a binary signal — feasible or not — that a search cannot trade against. A penalty returns a magnitude. The optimizer needs magnitudes: it must decide whether saving one byte is worth 2 µs of p99, and only a priced penalty makes that question answerable.

Curvature encodes risk aversion: a violation just past the SLO should be cheap, a large one expensive.

**Corrected after `review-01` (F7, F17).** The sigmoid is *convex below its inflection and concave above it*, so it is superlinear only between the SLO and the inflection and **sublinear beyond it**: once saturated, a 5× and a 50× violation are nearly indistinguishable, so the intended severity ordering in the tail is *not* delivered by this form. A saturating penalty also makes shedding attractive under overload (F6). And a violation expressed as a percentage has no fixed meaning while the argument is in absolute time — the unit convention (`(x − SLO)/SLO` versus absolute residual) must be chosen explicitly. **Open.**

## SLO(load): the anchor problem

The latency objective is **not** `observed_latency ≤ constant`. Latency necessarily rises with load (queueing), so a constant SLO is unsatisfiable at high concurrency and the engine would be permanently "failing" through no fault of its own.

But the naive fix is a trap. If `SLO(load)` is derived from observed latency, then angriness is identically zero and the objective is vacuous — the engine has defined its way out of the problem.

**The anchor must be the engine's measured capability, not its measured outcome:**

```
SLO(load) ≈ k · service_time / (1 − utilization)
```

`service_time` is measured work per request (bytes, CPU, IO) independent of how many requests were queued behind it. `utilization` is offered load over capacity. This is the shape of the classic queueing curve.

**Open — this anchor is not yet well-posed, and the objective cannot be implemented until it is (`review-01` F1–F4).**

- **Which statistic is `x`?** The formula above is the M/M/1 *mean* response time, while the rest of this document penalizes a percentile — `glossary.md` insists on p99 precisely because the mean hides failures. Under exponential service `p99 ≈ 4.6 × mean`, so the residual would be dominated by the tail ratio rather than by layout. The penalized functional must be named (mean, p95, p99, CVaR) and the anchor must be a capability model *for that functional*.
- **`k` has no stated source.** The worked example silently uses `k = 1`, at which the anchor *is* the M/M/1 mean outcome — the "measured outcome" this section claims to avoid. If `k` is fitted to observed latency, the tautology returns in full. Is `k` a fixed physical constant, a fitted parameter, or a prior?
- **`service_time` is not independent of the decisions it prices.** It bundles three unit systems (bytes, CPU, IO) through a conversion that *is* the cost model, and it is causally downstream of batching, layout and bandwidth saturation. A measurement protocol is required: sampled at what concurrency, from which counters, converted by what calibrated device model, validated against directly measured latency.
- **The queueing model is wrong for this system, and the overload case is undefined.** The system is batched, bursty, heterogeneous and has many outstanding IOs; M/M/1 assumes Poisson arrivals, exponential service and one server. The first-order correction is Pollaczek–Khinchine, `W_q = λE[S²] / (2(1 − ρ))` — the *second moment* of service time matters, and collapsing fan-out is precisely a change to that second moment. `1/(1 − ρ)` also diverges at `ρ = 1` and changes sign for `ρ > 1`, so the anchor and the worst-regime cap are undefined exactly during the transient overload the design deliberately provokes.

Three consequences:

1. **Angriness becomes comparable across load.** The right question stops being "did latency rise" (it always will, with concurrency) and becomes "did the curve move". That is the correct question for comparing two layouts.
2. **The residual is diagnostic.** Large residual concentrated at high load means a *scheduling or contention* problem. Angriness spread across all loads means a *layout or work* problem. Same scalar, two different subsystems to blame.
3. **Lowering service time is always rewarded**, because it shifts the whole curve down. This is what aligns the objective with capacity rather than mere compliance.

## The penalty curve

A sigmoid, placed so the SLO sits **just below the inflection point**:

```
penalty(x) = σ((x − SLO(load) − a) / s),   a > 0
```

Never a sigmoid centred *on* the SLO — that scores half the maximum penalty at exactly the SLO, which is wrong.

| Region | Behaviour | Why |
|---|---|---|
| `x < SLO` | small positive gradient | still rewards headroom and avoids exact ties between compliant layouts. **Open:** "near-zero" and "just below the inflection" cannot both hold — `penalty(SLO) = σ(−a/s)` is ≈0.5 for small `a/s`, and near-zero only for `a/s ≳ 5` (`review-01` F13). At large `a/s` the value underflows and the promised gradient becomes exactly zero, restoring the ties this row claims to avoid. |
| `x ≈ SLO + a` | maximum slope | maximum sensitivity exactly where it matters |
| `x ≫ SLO` | saturates | bounded, so heavy-tailed latency cannot make the objective unstable |

**Open — the parameterisation does not yet match the formula (`review-01` F13).** The formula carries `SLO`, `a` and `s`; this list names location, scale and steepness, so `a` has no source at all and "scale" has no symbol. Only `(x − SLO)/s` and `a/s` are jointly identified from curve data.

- **location** — `SLO(load)`, derived from the capacity model above;
- **offset `a`** — **source not yet stated**; it fixes where the inflection falls relative to the SLO;
- **steepness `s`** — the risk-aversion knob. It was previously said to be fitted from the human accept/reject record; that is **not identified** (`review-01` F11), because an accept/reject label constrains the predicted *gain* of an advice item, not the slope of a latency penalty, and because the engine chooses which advice to show, making the labels policy-selected. `review-01` F11 also rejects the two-timescale claim as unsupported: slower adaptation relocates the instability rather than removing it, and the "environment" here is a human who responds to the setting.

The remaining "scale" role is normalization, so that angriness stays a rate.

### Why the below-SLO gradient is load-bearing

It is the term that makes the engine keep buying **headroom** after it is already compliant. Without it the engine has no incentive to get faster once it is green — which is exactly wrong for a workload with short intense bursts, because headroom is what absorbs a burst. It is also what prevents the classic failure of a self-tuning system that stops optimizing the moment the dashboard is green and then collapses when load doubles.

### Why the curve must be bounded

Latency is heavy-tailed. A superlinear penalty applied to a tail heavier than the penalty's order has an unstable expectation — formally infinite for a Pareto tail with index below the order, and in practice an angriness estimate that swings wildly enough that the optimizer chases noise. Boundedness also closes the "average away a rare catastrophe" hole.

A baseline offset is harmless for `argmin` **only if it is constant and known exactly**. Here the baseline is load-dependent — the SLO moves with load — so it is not constant across regimes: subtracting it changes the argmin across windows and doubles the estimator variance, being a difference of two noisy rates. **Report angriness as a delta from a per-regime healthy baseline**, and state that baseline's estimator (`review-01` F19).

## The other terms

One primitive for every term: penalty uses `σ`, credit (served fraction, deadline met) uses `1 − σ`.

- **Unserved** — shed, dropped, or timed-out requests, penalized the same way.
- **Deadline** — requests that exceed a completion deadline.
- **Wasted work** — speculation and predication spend (see below), priced in queue slots rather than bytes.
- **Space / cache pollution** — a tile's footprint is a latency cost, not just a disk cost.
- **Reorg and compile cost** — charged now, credited later, and required to amortize.

Everything converts into predicted angriness through the load model. That conversion *is* the cost model's job: the cost model is an angriness predictor, not a separate abstraction.

## Bursty load: weights are time series

Real deployments look like long quiet periods punctuated by short intense bursts. Optimizing the average is actively wrong: a layout that is excellent on average and bad at the peak causes the outage, and one that is excellent at the peak may be wasteful off-peak.

- **Pattern weights are time series, not scalars.** The objective is over load windows. **Open:** which aggregator (worst regime / CVaR / which percentile) is still unchosen, and a "worst" taken over many noisy windows is a maximum, which is upward-biased by construction — more windows manufacture apparently bad regimes (`review-01` F8).
- **Bursts are predictable** (diurnal, weekly, release-driven), so the engine can pre-position — warm the hot set, promote ahead of time — rather than reacting mid-burst.
- **Expensive background work is scheduled into quiet windows.** Reorgs, compiles and promotions do not run during a burst. "Promote now" is never the right answer to rising load.
- **Sample counts gate influence.** A regime with too few samples contributes noise, not signal; it must not move a decision until it has enough evidence. **But this gate is backwards for the stated purpose (`review-01` F8):** the peak is by definition the regime with the fewest samples, so a minimum-sample gate guarantees the peak can *never* influence a decision — and it lets the learner push a regime out of consideration entirely, which also disables the cap. The numeric threshold, its power basis, and the interaction with the cap are unresolved.

## Form of the decision

```
minimize   total angriness(load distribution)
subject to worst-regime angriness ≤ cap
```

**Open — this form contradicts the rest of the document (`review-01` F5, F7; D1–D3).**

- **The aggregation is unspecified and self-contradictory.** "Either the worst regime, or a chosen percentile of windows" is not a definition, and the constraint uses a third (worst-regime). If angriness is integrated over the load distribution, the total is dominated by whichever regime occupies the most *time* — the long quiet periods, which is exactly the average this document calls actively wrong.
- **The cap contradicts Invariant I10.** "Angriness is the only currency" and a second, non-currency criterion cannot both hold, and the cap is precisely the non-tradeable binary constraint argued against in the first section. Either the cap goes, or the objective is a constrained program that must state its scalarisation (shadow price) and its behaviour when the feasible set is empty.
- **The convexity → fairness claim is false for this form.** `d²σ/dx² ∝ σ'(z)(1 − 2σ(z))`, so the penalty is convex for `x < SLO + a` and **concave for `x > SLO + a`** — concave in the far violation region. The load-spreading argument requires global convexity and does not apply. Fairness must be supplied separately, or the penalty form changed.

## Worked example

One request, 10 ms base service time, 1 MB read per request.

| | Layout A (base copy only) | Layout B (fan-out collapsed into one tile) |
|---|---|---|
| dependent reads | 20 | 1 |
| service time | ~1.6 ms | ~0.09 ms |
| `SLO(load)` at 80% utilization | ~8 ms | ~0.45 ms |
| p99 at peak load | 22 ms | 1.4 ms |
| residual `x − SLO` | 14 ms | 0.95 ms |
| angriness at peak (steepness 1) | σ(≈14) ≈ 1.00 | σ(≈0.95) ≈ 0.72 |

Both look "over SLO" in absolute terms; what differs is the residual against each layout's own capability. That is the comparison the objective is built to make.

The numbers are illustrative, not measurements, and they silently set **`k = 1`, `s = 1 ms` and `a = 0`** — `a = 0` contradicts the formula's own `a > 0`, and feeding milliseconds straight into σ contradicts `s` being the normalisation that keeps angriness a rate (`review-03` §3). Three of the four parameters are therefore pinned by the example and none is justified. The example cannot validate the parameterisation; it only illustrates the comparison.

## Operational reading

This objective is an SRE error budget under a different name: accumulated angriness **is** the budget consumed, and its derivative is the **burn rate**.

**Corrected after `review-01` F6:** with a bounded penalty, once `x ≫ SLO` the per-request penalty is constant, so the burn rate approaches the arrival rate and becomes **independent of violation magnitude** — a slightly-over-SLO and a catastrophically-over-SLO system burn at the same rate. "Alert on burn rate" is therefore least sensitive exactly where this document says violations are catastrophic, and the sigmoid's derivative is maximal at the inflection rather than in the tail. An unsaturated, severity-sensitive statistic is needed for alerting; `σ` alone does not carry it.

Scheduling expensive background work into quiet windows is also an **accounting exploit**, not only a benefit (`review-01` F15): if the cost is booked in the window with the lowest weight and the credit claimed in a high-weight window, the learner can make any background workload appear nearly free. Background work must be charged at an opportunity cost (queue slots), not at the window's measured angriness.
