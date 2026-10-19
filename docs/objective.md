Budget: 180 lines / 18,000 chars

# flashdb — the objective

## 0. The product bar: an end-to-end win over sqlite and postgres (author-decided)

The product is judged by one end-to-end comparison: flashdb against sqlite and postgres, on one reasonably specified machine, each engine in its own *reasonable* configuration, on the same frozen trace, through the same open-loop client, at equal durability and under the same memory cap. **The bar is that flashdb wins that comparison.** Nothing else is acceptance — not a learned-vs-declared ratio, not a microbenchmark, not a model's verdict.

"Reasonably specified machine" means hardware a user would actually run this on (the measured box: 12-core, NVMe, 128 MiB memory cap for the engine, 1.57 GiB instance = 12.25x data:memory). "Reasonable configuration" means each baseline runs as its authors intend it to run for this workload, disclosed and un-handicapped: sqlite with its default WAL and automatic checkpoint on, postgres with the shared-buffer/work-mem figures a competent operator would set inside the same memory budget, both driven open-loop because a closed-loop client measures the client. A win obtained by drawing a baseline back — disabling its checkpointing, capping its buffers below its own recommendation, or pacing it with a synchronous client — is not a win and is recorded as an invalid arm.

**Withdrawn (author-decided):** T6's "at least 3x the best baseline's sustained throughput" and the related "do not try to beat Postgres early" guidance. The 3x gate is *structurally unpassable*: two arms that both sustain a common offered level are each within 5% of it, so their ratio is confined to [0.95, 1.0526] (`.scratch/82-controlled-m1/report.md` §T6). The deferral rule was strawman-avoidance; it is inverted here — the baselines *are* the bar, and strawman avoidance is achieved by giving them a fair setup, never by postponing the comparison.

### The KPI ladder (strict priority)

| # | KPI | Instrument | Must be |
|---|---|---|---|
| 1 | **Efficiency** | bytes written / byte ingested (`summary.compact_bytes_total` / logical ingest); bytes read / request (region telemetry `bytes_read`, `count`); CPU µs / request (cgroup `cpu.stat` delta / requests); resident bytes / data byte (`memory.peak`); recovery seconds (open → first served request) | no worse than the best baseline on every component, and **strictly better outside the measured error band on at least one** |
| 2 | **Sustained throughput** | the highest offered level sustained with 0 rejected windows and 0 errors, over paired blocks, quiet box | **at least the best baseline's level** |
| 3 | **Tail latency** | schedule-lag p99 <= 10 ms (primary, `STATUS.md` §7.1) and service p99 reported with the device fsync floor beside it | reported; a breach while a baseline holds the same level is a loss, and throughput may never be bought by abandoning it |

A comparison resolves to **win** (efficiency no worse with one strict gain, throughput at least equal), **not-worse**, or **loss**. A throughput tie with worse efficiency is a **loss**. Every number carries its condition: revision, corpus hash, machine state, memory cap, durability setting and the device's measured floor at that moment.

### The gap-attribution rule (required before any tuning)

When flashdb loses or ties, the gap is attributed from the saved records — run JSON/JSONL trajectories, stderr, region telemetry, on-disk format bytes, in-memory format bytes — to exactly one of:

- **(a) not learned enough** — the policy chose a layout that the oracle-best candidate set beats by more than the error band, while the engine executes the chosen layout at its floor;
- **(b) a defect** — a phase exceeds its theoretical floor by more than the error band, with a replicable mechanism that names a code path;
- **(c) architecture** — every phase is at its floor and the remaining cost is the work itself (bytes moved, IOs issued, barriers per request), i.e. the competitor's design does less work per request.

Each category has its own remedy, and an unattributed gap is not a licence to tune thresholds. The procedure and its instruments are in `dev-loop.md` §Gap attribution.

## 1. The internal objective: angriness

**The internal objective is the constrained program** — maximise the highest sustained offered load, subject to the latency ceiling and the write-amplitude budget as hard feasibility conditions (`Endurance is a constraint, not a price`). **Angriness is the internal pricing form**, not a competing objective. Every decision in the system — which layout, when to reorganize, what tier to promote to, whether to speculate, whether to compile — is evaluated as a predicted angriness delta through one calibrated cost model (`design.md` I10).

Angriness is a **rate**, not a total: angriness per unit of offered load, or per unit time. Reported as a total it can be made to look good by simply receiving less work.

> **Status: not yet well-posed.** 设备层校准常量已由实验 0a-redo 取得（见 `docs/engine.md` 与 `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`）：依赖点读 89–174 µs（按粒度）、批量摊薄 4.9 µs/读、带宽 827 MiB/s、安静设备尾 1.26× 均值。**仍然未决的是 estimand/锚定/聚合**——常量就位不改变 F1–F5 的未决状态。 An adversarial review (aspect: optimisation and learning theory) found that the estimand, the anchor, the parameterisation, the aggregation, the oracle and the estimators are all undefined or self-contradictory. Findings and dispositions are recorded as `review-01` in `.scratch/01-design/issues/01-design.md`. The defects are marked `Open` inline below rather than deleted, because the decisions they need are the author's, not the reviewer's. **The objective cannot be implemented or falsified until F1–F5 are resolved.**

**How these statements relate (2026-10-06).** The **internal objective** — what the pre-registered comparison scores — is a **constrained program**: maximise the **highest sustained offered load**, subject to the latency ceiling and the write-amplitude budget as **hard feasibility conditions** (`Endurance is a constraint, not a price`). **Angriness is the internal pricing / soft-penalty form**, not a competing objective, and is **not yet well-posed** (F1–F5 open; the banner above stays true). The **adopted decision (2026-10-09, `.scratch/STATUS.md` §7.1)** judges the primary metric on the per-request **schedule lag** p99 against a lag ceiling (default 10 ms — the number `pre-registration.md` writes the metric with); per-request **service latency** has its own ceiling and is reported separately as the latency/efficiency measure, so a throughput result can never be won by abandoning latency. **Decided (2026-10-19, delegated author authority):** lag-only p99 <= 10 ms is the primary throughput-feasibility rule; the service ceiling has no pre-registered veto and is reported beside the device's own fsync floor. Under the KPI ladder (§0) a service-latency breach loses the external comparison only when a baseline holds the same offered level without breaching. The §7.1 reconciliation is closed by that rule and by §0, not by re-reading the old thresholds. The *soft-penalty* section below explains why the internal form is priced; the *endurance* section explains why the ceiling and the budget are feasibility, not prices. The tie-breaks and the empty-feasible-set rule are stated with the endurance decision.

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

**Partly resolved (author, 2026-10-06); the rest remains open (`review-01` F5, F7; D1–D3).**

- **The aggregation is unspecified and self-contradictory.** "Either the worst regime, or a chosen percentile of windows" is not a definition, and the constraint uses a third (worst-regime). If angriness is integrated over the load distribution, the total is dominated by whichever regime occupies the most *time* — the long quiet periods, which is exactly the average this document calls actively wrong. **Still open:** this is the *angriness* aggregator, which only bites once angriness is well-posed.
- **The cap is now a constrained program, not a contradiction (resolved 2026-10-06).** The objective *is* a constrained program: highest sustained offered load under the latency ceiling and the write-amplitude budget as hard feasibility conditions, with I10 corrected (`design.md`) and the scalarisation and empty-feasible-set rule stated in *Endurance is a constraint, not a price*. The `minimize total angriness subject to a worst-regime cap` form above is superseded; angriness remains the internal pricing form and stays not-well-posed.
- **The convexity → fairness claim is false for this form.** `d²σ/dx² ∝ σ'(z)(1 − 2σ(z))`, so the penalty is convex for `x < SLO + a` and **concave for `x > SLO + a`** — concave in the far violation region. The load-spreading argument requires global convexity and does not apply. Fairness must be supplied separately, or the penalty form changed.

## Endurance is a constraint, not a price (author, 2026-10-06)

Measured on the slice-39 **adversarial** workload (service-time term): the reorganisation write term is **0.083%** for the `declared` arm, **0.669%** for `learned@1.0`, and **0.118%** for `static-derived`; ingest writes are **0.003%** on every arm. No drift-arm ratio was reported — slice 39 ran the adversarial arm only. A priced term of that size cannot move any candidate's argmin however `lambda` is swept, so lambda was never a calibration problem — on these arms endurance is **not** the scarce resource; device service time is.

The form therefore becomes:

```
maximise   sustained load
subject to latency ceiling       (hard)
           write-amplitude budget (hard)
```

**Scalarisation.** Among candidates that satisfy both hard conditions, rank by **highest sustained offered load**; break ties first by **latency headroom** (the larger measured margin below the ceiling), then by the **reported efficiency metrics** in the pre-registered order (write amplification, bytes read per op per pattern, RSS per data byte, CPU per op, recovery time — `pre-registration.md`). Angriness does not enter this ordering until it is well-posed.

**Empty feasible set.** If no candidate satisfies both hard conditions, the program has no solution: the comparison reports infeasibility and names the binding constraint, and no arm is scored as a winner — the metric cannot be won by abandoning a condition (matching the pre-registration's rule that invalid windows are not counted).

The budget is enforced as a **per-window reorganisation allowance** — the window is the load window the objective aggregates over (1 s in the reported slice-39 runs). The slice-39 churn control is that mechanism: on the adversarial arm it moved `learned@1.0` reorganisation bytes from **33,141,775** (slice-38 adversarial arm) to **28,191,940** (slice-39 adversarial arm), about **1.18×**. (The slice-38 **drift** arm's `learned@1.0` figure was 402,984,851, but the churn control's effect **under drift is not yet measured** — slice 39 ran the adversarial arm only; what was observed on the adversarial arm are the control's counters: **52 restarts, 45 gated switches, 10 budget blocks, 42 cooldown holds**.) The deliverable is a **capacity-versus-budget curve** ("under a reorganisation budget of B bytes per load window, sustained load is X"), not a lambda. A small lambda may remain only as a tie-break, and must be described as such.

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

**It is also arithmetically inconsistent with its own tile size (`review-05` SS-03, SS-12).** It bills a tile read as one ~80 µs miss — 4 KB-class latency — while the design's tile is 8 MB: 8 MB at ~7 GB/s is **~1.14 ms of transfer alone**, roughly the entire claimed un-collapsed fan-out, and the stated 1 MB read is ~143 µs, already above the stated 0.09 ms before any latency is added. The comparison therefore prices 20 small reads against one read priced as if it too were small. Two things must be defined before the arithmetic means anything: the **access granule** (device block, sub-tile page, or the whole tile — and if sub-tile, locating the page requires the tile directory, which is a *second* dependent read), and a **bandwidth term** beside the latency term. Also, with `a = 0, s = 1` the sigmoid *is* centred on the SLO, which the section above forbids; and because it saturates, a 17× service-time improvement scores 0.72 against 1.00, giving the objective almost no gradient exactly where the storage decisions are made.

## Operational reading

This objective is an SRE error budget under a different name: accumulated angriness **is** the budget consumed, and its derivative is the **burn rate**.

**Corrected after `review-01` F6:** with a bounded penalty, once `x ≫ SLO` the per-request penalty is constant, so the burn rate approaches the arrival rate and becomes **independent of violation magnitude** — a slightly-over-SLO and a catastrophically-over-SLO system burn at the same rate. "Alert on burn rate" is therefore least sensitive exactly where this document says violations are catastrophic, and the sigmoid's derivative is maximal at the inflection rather than in the tail. An unsaturated, severity-sensitive statistic is needed for alerting; `σ` alone does not carry it.

Scheduling expensive background work into quiet windows is also an **accounting exploit**, not only a benefit (`review-01` F15): if the cost is booked in the window with the lowest weight and the credit claimed in a high-weight window, the learner can make any background workload appear nearly free. Background work must be charged at an opportunity cost (queue slots), not at the window's measured angriness.
