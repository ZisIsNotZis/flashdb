Budget: 200 lines / 15,000 chars

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
- **Inject drift mid-run** — a new forecast query appears at 10× weight, a flash sale spikes one SKU, a monthly close report arrives with low weight but a hard deadline. That exercises drift response, evidence-gated promotion and the rare-but-urgent escape hatch in one experiment. **Open (`review-02` F13):** one run is one sample, and deterministic replay deliberately removes variance, so a single run cannot separate learner quality from a lucky seed. Pre-register N seeds and report median and IQR; score drift as *time to recover* (windows to return within X% of pre-drift angriness, with the reorg payback window stated) rather than as a single end-of-run number; and note that under drift an offline-best-*single*-layout oracle biases the comparison *against* the learner — the reference must be an offline-best *sequence* over a small state space.
- **Pre-register a falsification contract.** As specified, almost any measurement can be read as progress, and every candidate refutation is already pre-labelled as not-a-measurement: a canary is "a conservative lower bound", an untuned Postgres is "a strawman", the simulator is "uncalibrated", the worked example is "illustrative" (`review-02` F18). Therefore, before any engine exists, pre-register: the falsification hypothesis with its threshold (for example — at ≥10× cache-size data, trace-derived layout does not beat `flashdb-declared` by more than 15%, therefore the thesis is rejected), the primary metric and its reference, the minimum instance size (which must exceed cache, or the fan-out premise is untested), the seed count, the confidence interval, the acceptable cost-model error, and the Postgres bar with its tuning protocol. A benchmark set that can disconfirm nothing is not a benchmark set.

## Harness

```
generator → trace → deterministic replay → pluggable storage simulator → layout search → regret
```

- **Metric: relative regret versus an oracle layout**, not absolute throughput. **Corrected and still open (`review-01` F10):** raw regret is not comparable across workloads without normalisation, because a workload with little layout headroom shows small regret for any learner — normalise by `A_baseline − A_oracle` (or by `A_oracle`), and state whether the metric is static, dynamic or per-window regret, since a fixed-trace oracle penalises tracking a moving optimum while a per-window oracle is prescient.
- **Oracle: brute-force layout search on small instances — which is not ground truth.** Brute-forcing the *same* cost model removes optimisation error only, not model error, so it cannot deliver the purpose it was given: it cannot tell whether the learner is good or merely as good as its own cost model. Ground truth would require executing layouts on hardware. Worse, the layout space is per-tile and heterogeneous, so a brute-forceable instance cannot exhibit the per-tile heterogeneity, drift or shared budget the design is about — the phenomenon under study is absent from the oracle's domain. The oracle's independence from the learner's model, and its scalarisation of the constrained problem, must be stated before regret means anything. **And it is either infeasible or rigged (`review-02` F1):** if the oracle is the best *uniform* layout, a per-tile learner beats it routinely and regret goes ≤ 0, making the metric vacuous or perverse; if it is the best *per-tile* layout, the space is combinatorial and "small instances" means tens of tiles. Either way the oracle regime and the motivating regime are mutually exclusive — a brute-forceable instance is smaller than cache, so the ~1.3% residence premise is never exercised. Replace it with a declared baseline plus a **relaxed oracle**: per-tile best over a small, published enumerated candidate set, reported as `relative_regret = (cost_learner − cost_best_candidate) / cost_best_candidate`, with the candidate-set cardinality and the minimum representative instance size stated.
- **Deterministic replay.** The same trace, byte for byte, against different layouts. Determinism is a hard requirement, because every promotion decision depends on it.
- **Pluggable storage simulator with an explicit, calibratable cost model.** This is what makes counterfactual evaluation possible at all (`learning.md`, off-policy evaluation), and it is the reason millions of layout experiments can be run without touching a real device.
- **Frozen, versioned replay corpora.** A trace is a dataset: freeze it, commit it with its hash, and treat changes to it as dataset changes. Results without a corpus hash are not comparable to anything.
- **The cost model must be calibrated against measurements**, and its error tracked, because uncalibrated what-if prediction is optimistic — a promotion decided on an uncalibrated prediction is a guess wearing a number. **Open (`review-01` F14):** which *functional* is calibrated is unspecified. A mean-calibrated simulator systematically mispredicts the percentile the penalty consumes, so the calibration target must be the same statistic `x` that `objective.md` penalises, with a quantile-calibration error metric, prediction intervals and drift/out-of-distribution diagnostics.

## Baselines and the gap-attribution protocol

The bar, the KPI ladder and what makes a baseline setup *reasonable* are owned by `objective.md` §0. This section owns only the method that makes them measurable.

Every comparison runs at **equal durability**. An engine that fsyncs less is not faster; it is a different product. The client is **open-loop** (offers the target rate regardless of completion), because a closed-loop client measures itself; every arm gets the same trace, the same frozen corpus hash, the same memory cap, the same quiet box (loadavg < 2 sustained before each arm), and a fresh restored instance.

| Baseline | Isolates |
|---|---|
| LMDB | closest design relative — single writer, copy-on-write, A/B checksummed meta pages, no redo log |
| SQLite | the page-cache comparison; its default WAL autocheckpoint stays **on** |
| RocksDB | the merge/compaction comparison |
| Postgres | the general-purpose scoreboard; tuned the way a competent operator would tune it inside the same memory budget |
| **`flashdb-declared`** | **the missing control (`review-02` F9):** flashdb with one fixed, author-chosen layout. Comparing learned layout against *other engines* conflates three variables — the tile architecture, the declared-versus-learned choice, and implementation quality — so a win over RocksDB proves nothing about learning. |
| **`flashdb-static-derived`** | **the second missing control (`review-02` F9):** a layout derived offline from the trace, built once, no drift. The thesis is that learning *and* drift beat declaration, so both must be measured against this, and the reported quantity is the incremental gain over it. |

### Gap attribution

The rule that a gap must be attributed before anything is tuned is in `objective.md` §0. The procedure:

1. **Freeze the record set.** The arm-run JSON, the arm's stderr, the trace (`trace.jsonl`), region telemetry, the revision and the corpus hash. A number without its record is not evidence.
2. **Build the per-request phase ledger.** Dispatch → validate → WAL append → barrier → apply → reply, with the wall time of each phase per request. The engine already emits `lag_breach_attribution reads= read_us=`, and the driver records `group_elapsed_us`; where a phase is invisible, that blindness is itself a finding.
3. **Compute each phase's theoretical floor** from the calibrated device constants (`engine.md`, experiment 0a-redo): dependent read 89-174 µs by granularity, batched independent read 4.9 µs, bandwidth 827 MiB/s, quiet-device tail 1.26x mean, plus the machine's own fsync p50 measured in the same session. The floor of a request is the minimum IOs, bytes and barriers its semantics require — not the floor of the current design.
4. **Take the residual.** measured − floor, per phase, with the error band measured in the same session (a repeated-control block, not an assumption).
5. **Classify.** Residual inside the band on every phase → the cost is the work itself: **architecture** (category c), and the only lever is doing less work per request. A phase outside the band with a replicable mechanism naming a code path → **defect** (b). Every phase at floor while the chosen layout loses to the oracle-best candidate set by more than the band → **not learned enough** (a).
6. **Record it** in the milestone report with the evidence and the revision, before any fix is proposed. A fix without an attribution is a guess and is labelled as one.

Format forensics is part of step 3-4, not a separate study: on-disk (tile directory and page overhead, index bytes, WAL bytes, manifest bytes) and in-memory (memtable `BTreeMap<Vec,Vec>` overhead, fence RAM, page cache, allocator arenas) are each compared against the format's declared minimum bytes per row, because that ratio is where a 10x data:memory target is actually won or lost.

`ponytail:` start with a small thread pool for IO; add `io_uring` when a profile shows the syscall tax. `io_uring` is aligned with the design (many small independent reads, completion batching, registered fixed buffers and files) but it is the last 20%, and it maps cleanly onto the batch scheduler as an upgrade rather than a rewrite.

## Build order

The order is chosen by risk, not by appeal.

| # | Step | Why here |
|---|---|---|
| 0 | Write the four contracts (`contracts.md`) and freeze the reserved interfaces | Cheap now, a rewrite later |
| 1 | **Trace + replay + cost-model simulator + generators + oracle search — with no engine at all** | Brings the objective function, the metrics and the workload descriptors before any engine exists. **Corrected (`review-02` F7):** this step cannot output a *calibrated* model, because step 2 is its calibration target, and it cannot model reorg (step 3) faithfully, so its evidence is not evidence until step 3. Its output is an *uncalibrated* model plus an oracle. |
| 2 | Dumb correct engine: immutable tiles, single writer, one fixed layout, WAL-free commit record, single-round-trip transactions | Correctness first; a baseline to calibrate the simulator against |
| 3 | **Mechanism**: relocatable tiles, atomic root flip, background reorg, orphan GC, crash-safe resume | See the fatal risk below |
| 4 | Policy: bandit over candidate layouts, replay-gated, with hysteresis, a reorg budget, promotion/demotion | The fun part, and the reason step 3 must exist first |
| 5 | Learned per-tile encoding and co-location, as independently measurable axes | Each is a hypothesis, not a bundle |

Step 1 is deliberately engine-free: **do not build a database to learn what a database should do.** Its output is a workload descriptor, a metric definition, an **uncalibrated** cost model and an oracle — prerequisites for judging an engine, not a calibrated instrument (`review-02` F7; "calibrated (initially rough)" was self-contradictory, since step 2 is the calibration target).

**Proposed reorder (`review-02` F7) — recorded, not yet adopted.** The order above front-loads the work with the least thesis value: by its own estimate ~90% of the effort is the mechanism, and the thesis experiment comes last. Two zero-engine experiments should precede everything:

| # | Experiment | Why first |
|---|---|---|
| 0a | Device microbenchmark: dependent-serialized reads versus batched independent reads at the target queue depth | can falsify the entire fan-out premise in a day, with no engine |
| 0b | Offline trace → static layout builder, measured against the declared-layout control, with no reorg and no drift | tests the thesis claim directly, before any mechanism exists |

### The fatal risk
Steps 3 and 4 are where ~90% of the effort lives, and they are asymmetric: **the mechanism is boring and enormous, the policy is fun and small.** Building the policy first means the project never ships. Build the mechanism minimal, then the policy.

### Deferred implementations, frozen interfaces

Deferring an implementation is cheap; deferring a boundary is not. The reserved interfaces in `contracts.md` are frozen at step 0. Everything else — encodings, tier ladder, advisor, canary — can be deferred, stubbed, or omitted from an early build without cost.

## Evidence

- Every completion claim cites the command run and its observed result, or the artifact inspected, together with the commit it ran on. A claim without evidence is false.
- Test/benchmark runs leave trackable artifacts naming the producing branch, the ticket, and the corpus hash.
- Every fixed bug gets a regression test before closure.
- `ponytail:` non-trivial logic leaves one runnable check behind — the smallest thing that fails when the logic breaks. No frameworks, no fixtures, no per-function suites.
