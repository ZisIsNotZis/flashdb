# flashdb — the objective: angriness

**Angriness is the single scalar objective.** Every decision in the system — which layout, when to reorganize, what tier to promote to, whether to speculate, whether to compile — is evaluated as a predicted angriness delta through one calibrated cost model (`design.md` I10).

Angriness is a **rate**, not a total: angriness per unit of offered load, or per unit time. Reported as a total it can be made to look good by simply receiving less work.

## Why a soft penalty and not a hard constraint

A constraint returns a binary signal — feasible or not — that a search cannot trade against. A penalty returns a magnitude. The optimizer needs magnitudes: it must decide whether saving one byte is worth 2 µs of p99, and only a priced penalty makes that question answerable.

Curvature then encodes risk aversion: a 5% violation should be cheap, a 500% violation catastrophic. The curve is deliberately superlinear in the violation region — see the S-curve below.

## SLO(load): the anchor problem

The latency objective is **not** `observed_latency ≤ constant`. Latency necessarily rises with load (queueing), so a constant SLO is unsatisfiable at high concurrency and the engine would be permanently "failing" through no fault of its own.

But the naive fix is a trap. If `SLO(load)` is derived from observed latency, then angriness is identically zero and the objective is vacuous — the engine has defined its way out of the problem.

**The anchor must be the engine's measured capability, not its measured outcome:**

```
SLO(load) ≈ k · service_time / (1 − utilization)
```

`service_time` is measured work per request (bytes, CPU, IO) independent of how many requests were queued behind it. `utilization` is offered load over capacity. This is the shape of the classic queueing curve.

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
| `x < SLO` | near-zero, **tiny positive gradient** | still rewards headroom; no flat region for a search to stall in and no arbitrary ties between compliant layouts |
| `x ≈ SLO + a` | maximum slope | maximum sensitivity exactly where it matters |
| `x ≫ SLO` | saturates | bounded, so heavy-tailed latency cannot make the objective unstable |

Three parameters, three different sources:

- **location** — derived from the capacity model above, never hand-chosen;
- **scale** — normalization, so angriness stays a rate;
- **steepness `s`** — the learned risk-aversion knob, fitted from the human accept/reject record (`learning.md`), adapted on a **much slower timescale** than layout decisions or the moving target oscillates between lax and strict.

### Why the below-SLO gradient is load-bearing

It is the term that makes the engine keep buying **headroom** after it is already compliant. Without it the engine has no incentive to get faster once it is green — which is exactly wrong for a workload with short intense bursts, because headroom is what absorbs a burst. It is also what prevents the classic failure of a self-tuning system that stops optimizing the moment the dashboard is green and then collapses when load doubles.

### Why the curve must be bounded

Latency is heavy-tailed. A superlinear penalty applied to a tail heavier than the penalty's order has an unstable expectation — formally infinite for a Pareto tail with index below the order, and in practice an angriness estimate that swings wildly enough that the optimizer chases noise. Boundedness also closes the "average away a rare catastrophe" hole.

A constant baseline offset is harmless for `argmin` (constants vanish), so it does not affect decisions — but it is confusing to read. **Report angriness as a delta from the healthy baseline**, or normalize so perfect health reads as 0.

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

- **Pattern weights are time series, not scalars.** The objective is over load windows — either the worst regime, or a chosen percentile of windows.
- **Bursts are predictable** (diurnal, weekly, release-driven), so the engine can pre-position — warm the hot set, promote ahead of time — rather than reacting mid-burst.
- **Expensive background work is scheduled into quiet windows.** Reorgs, compiles and promotions do not run during a burst. "Promote now" is never the right answer to rising load.
- **Sample counts gate influence.** A regime with too few samples contributes noise, not signal; it must not be allowed to move a decision until it has enough evidence.

## Form of the decision

```
minimize   total angriness(load distribution)
subject to worst-regime angriness ≤ cap
```

The total gives a smooth objective the search can descend. The cap is the belt-and-braces guard a pure sum can never give: it stops the optimizer buying an aggregate win with a single unacceptable regime.

Convexity bonus: with a convex penalty the risk-neutral optimum spreads load rather than starving some requests, so fairness under contention falls out of the objective instead of needing a separate mechanism.

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

Both look "over SLO" in absolute terms; what differs is the residual against each layout's own capability. That is the comparison the objective is built to make. The numbers here are illustrative, not measurements.

## Operational reading

This objective is an SRE error budget under a different name: accumulated angriness **is** the budget consumed, and its derivative is the **burn rate**. That gives the operational rules for free — alert on burn rate rather than individual violations, and schedule promotions when the budget is healthy, which coincides with "do expensive work in quiet windows".
