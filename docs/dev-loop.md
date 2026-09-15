# flashdb — development loop

How the project is developed, measured and de-risked. The organizing principle: **the database guides its own development, and the objective function is built before the engine.**

## Workloads

The first scenario is an **inventory system**. It is a good first case for concrete reasons, not just familiarity:

- **Natural extreme hot/cold skew** — active SKUs, current stock and open orders are small; history is huge. That is the 10 TB / 128 GB regime by construction.
- **Real multi-table joins** — order → order_line → product → stock → location.
- **Conditional multi-row writes** — "decrement stock iff available, insert two lines" — exercising single-round-trip atomicity without interactive transactions.
- **A strong external correctness oracle** — stock equals the sum of movements, stock never negative, no lost or duplicated orders, retries idempotent. A research storage engine without a correctness oracle is untestable.
- **A free reference workload** — TPC-C's warehouse/district/customer/stock/order schema is an inventory system, so a boring baseline (Postgres) can be run against the same shape.

TPC-C alone is insufficient: it is point-lookup heavy with negligible joins, whereas the motivating case is by construction a large fact plus dimensions. The heavy-join set, **one workload per hypothesis** so that wins can be attributed to mechanisms rather than to a single big number:

| Workload | Shape | Hypothesis tested |
|---|---|---|
| Replenishment / days-of-cover | big fact (order lines) → small dimensions (product, supplier, lead time) | star-schema co-location along `(sku, loc, time-bucket)` collapses the fan-out |
| Recall / order traceability | lot → products → order_lines → orders → customers → returns | **multi-hop** traversal: the case that breaks a single learned clustering key, since co-locating for hop 1 punishes hop 3 |
| As-of join | fact joined to a time-versioned (SCD-2) dimension on `(sku, t)` intervals | range/predicate validation and time-based tiling |
| Lost sales (anti-join) | demanded SKUs with no supply movement | the co-location trap: you cannot cluster for *absence* |
| ABC / demand by SKU × region × month | flat scan plus aggregate | that specialization has not wrecked scans |
| Uniform-random point lookups (adversarial) | no locality exists to exploit | the learner must recognise there is nothing to win and **not churn** — the anti-overfitting workload, and the one that fails a learner that reads noise as signal |

Two harness rules:

- **Zipfian access, not uniform.** Real inventory is skewed; TPC-C's generated distribution is not. A layout learned on a distribution that will never occur is worthless.
- **Inject drift mid-run** — a new forecast query appears at 10× weight, a flash sale spikes one SKU, a monthly close report arrives with low weight but a hard deadline. That exercises drift response, evidence-gated promotion and the rare-but-urgent escape hatch in one experiment.
- **Pre-register at least one falsification condition.** As specified, almost any measurement can be read as progress. At least one experiment must state, in advance, an observation that would prove the thesis wrong — for example: on the replenishment workload, collapsing fan-out must reduce dependent reads per request by at least the predicted factor, or the thesis fails. A benchmark set that cannot disconfirm anything is not a benchmark set.

## Harness

```
generator → trace → deterministic replay → pluggable storage simulator → layout search → regret
```

- **Metric: relative regret versus an oracle layout**, not absolute throughput. **Corrected and still open (`review-01` F10):** raw regret is not comparable across workloads without normalisation, because a workload with little layout headroom shows small regret for any learner — normalise by `A_baseline − A_oracle` (or by `A_oracle`), and state whether the metric is static, dynamic or per-window regret, since a fixed-trace oracle penalises tracking a moving optimum while a per-window oracle is prescient.
- **Oracle: brute-force layout search on small instances — which is not ground truth.** Brute-forcing the *same* cost model removes optimisation error only, not model error, so it cannot deliver the purpose it was given: it cannot tell whether the learner is good or merely as good as its own cost model. Ground truth would require executing layouts on hardware. Worse, the layout space is per-tile and heterogeneous, so a brute-forceable instance cannot exhibit the per-tile heterogeneity, drift or shared budget the design is about — the phenomenon under study is absent from the oracle's domain. The oracle's independence from the learner's model, and its scalarisation of the constrained problem, must be stated before regret means anything.
- **Deterministic replay.** The same trace, byte for byte, against different layouts. Determinism is a hard requirement, because every promotion decision depends on it.
- **Pluggable storage simulator with an explicit, calibratable cost model.** This is what makes counterfactual evaluation possible at all (`learning.md`, off-policy evaluation), and it is the reason millions of layout experiments can be run without touching a real device.
- **Frozen, versioned replay corpora.** A trace is a dataset: freeze it, commit it with its hash, and treat changes to it as dataset changes. Results without a corpus hash are not comparable to anything.
- **The cost model must be calibrated against measurements**, and its error tracked, because uncalibrated what-if prediction is optimistic — a promotion decided on an uncalibrated prediction is a guess wearing a number. **Open (`review-01` F14):** which *functional* is calibrated is unspecified. A mean-calibrated simulator systematically mispredicts the percentile the penalty consumes, so the calibration target must be the same statistic `x` that `objective.md` penalises, with a quantile-calibration error metric, prediction intervals and drift/out-of-distribution diagnostics.

## Baselines

Every comparison at **equal durability**. An engine that fsyncs less is not faster; it is a different product, and a raw throughput win against a stricter baseline is not a win.

| Baseline | Isolates |
|---|---|
| LMDB | closest design relative — single writer, copy-on-write, A/B checksummed meta pages, no redo log |
| SQLite | the page-cache comparison |
| RocksDB | the merge/compaction comparison |
| Postgres | the general-purpose scoreboard — run from day one, **never** a step-1 goal |

Do not try to beat Postgres early. Beating it while it is untuned proves only that a strawman was beaten, and teaches nothing about the design.

`ponytail:` start with a small thread pool for IO; add `io_uring` when a profile shows the syscall tax. `io_uring` is aligned with the design (many small independent reads, completion batching, registered fixed buffers and files) but it is the last 20%, and it maps cleanly onto the batch scheduler as an upgrade rather than a rewrite.

## Build order

The order is chosen by risk, not by appeal.

| # | Step | Why here |
|---|---|---|
| 0 | Write the four contracts (`contracts.md`) and freeze the reserved interfaces | Cheap now, a rewrite later |
| 1 | **Trace + replay + cost-model simulator + generators + oracle search — with no engine at all** | Brings the objective function, the metrics and the workload descriptors before any engine exists. Highest value per unit of work, and the only step that de-risks the thesis |
| 2 | Dumb correct engine: immutable tiles, single writer, one fixed layout, WAL-free commit record, single-round-trip transactions | Correctness first; a baseline to calibrate the simulator against |
| 3 | **Mechanism**: relocatable tiles, atomic root flip, background reorg, orphan GC, crash-safe resume | See the fatal risk below |
| 4 | Policy: bandit over candidate layouts, replay-gated, with hysteresis, a reorg budget, promotion/demotion | The fun part, and the reason step 3 must exist first |
| 5 | Learned per-tile encoding and co-location, as independently measurable axes | Each is a hypothesis, not a bundle |

Step 1 is deliberately engine-free: **do not build a database to learn what a database should do.** The output of step 1 is a workload descriptor, a metric definition, a calibrated (initially rough) cost model and an oracle — all of which are prerequisites for judging any engine.

### The fatal risk

Steps 3 and 4 are where ~90% of the effort lives, and they are asymmetric: **the mechanism is boring and enormous, the policy is fun and small.** Building the policy first means the project never ships. Build the mechanism minimal, then the policy.

### Deferred implementations, frozen interfaces

Deferring an implementation is cheap; deferring a boundary is not. The reserved interfaces in `contracts.md` are frozen at step 0. Everything else — encodings, tier ladder, advisor, canary — can be deferred, stubbed, or omitted from an early build without cost.

## Evidence

- Every completion claim cites the command run and its observed result, or the artifact inspected, together with the commit it ran on. A claim without evidence is false.
- Test/benchmark runs leave trackable artifacts naming the producing branch, the ticket, and the corpus hash.
- Every fixed bug gets a regression test before closure.
- `ponytail:` non-trivial logic leaves one runnable check behind — the smallest thing that fails when the logic breaks. No frameworks, no fixtures, no per-function suites.
