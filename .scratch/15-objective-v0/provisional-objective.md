# 15 — provisional objective and falsification contract (for the harness to be buildable)

> **SUPERSEDED on the estimand by [`objective-v1.md`](objective-v1.md)** (author decision,
> 2026-10-01: score sustained throughput under offered load with a latency ceiling; latency is a
> constraint, not the scored quantity). The measurement discipline, the calibration-gap analysis
> and the pre-registration skeleton here are still in force.

Status: **PROVISIONAL**, written 2026-09-30 while the author was away, under their instruction to
proceed with my recommendation. `docs/objective.md` marks F1–F5 Open; this document does not
claim to close them. Its purpose is narrower and honest: **the harness (simulator, cost model,
controls, scoring) cannot be built without SOME concrete objective, so this fixes a minimal,
internally consistent one, marks every choice with its alternative, and lists exactly what the
author must confirm or replace.** Nothing here should be quoted as the author's decision.

## 1. Estimand — the statistic that is scored

**Primary: per (pattern key, window) p99 request latency.** Secondary and reported but not
scored: mean latency, throughput, bytes written, RSS.

Why p99 rather than mean: the objective's penalty curve is defined on a per-request severity and
its inflection is placed in the tail, so the statistic that consumes the penalty must be a tail
statistic. Device calibration measured the quiet-device tail at **1.26× the mean** (0a-redo), i.e.
a mean-based score would hide exactly the damage the S-curve exists to price. Quantile sketches
(HDR/t-digest) are already specified in `docs/learning.md`.

*Alternative:* CVaR of the per-request latency distribution. Rejected for v0 only because it needs
the full tail and the sketch budget is ~1% of cache; p99 is the cheapest tail proxy with a known
estimator error.

## 2. Anchor (SLO) — what "too slow" means

`SLO(pattern, window) = inflation * p99_reference(pattern)`, where `p99_reference` is that
pattern's p99 measured at a **reference load** (a fixed, pre-registered mix) and `inflation = 2`
by default.

Why relative-to-measured rather than an absolute millisecond budget: absolute SLOs are
unfalsifiable across hardware, and our own constants moved by 35% between the first and second
0a runs (149 µs claimed → 96.3 µs measured). Relative anchors are also what makes
`relative_regret` comparable across workloads.

*Open (author):* the reference load definition and the inflation factor. Both are policy.

## 3. Penalty — a corrected piecewise form

The reviewed sigmoid had a contradiction (`review-01` F13): it claimed a small positive gradient
below the SLO while `σ(−a/s) ≈ 0.5` at `a = 0`, and near-zero only for `a/s ≳ 5`. Provisional fix,
stated as a formula rather than prose:

    penalty(x) = ε * (x / SLO)                                       for x <  SLO
    penalty(x) = ε + (1 - ε) * (σ((x - SLO)/s) - σ(0)) / (1 - σ(0))  for x >= SLO

with `ε = 0.05`, `s = SLO / 2`. The value at the SLO is exactly `ε` and the function is
**continuous and strictly increasing everywhere**.

*Corrected while implementing the model:* the first version put 0 at the SLO while the branch
below it evaluated to `ε·(x/SLO)` ≈ 0.045 just underneath, i.e. a layout at 0.9·SLO would have
scored *worse* than one exactly at the SLO. The model's own unit test caught it (this is the
second time a formula-level contradiction in this objective was found by writing the arithmetic
down; the first was `review-01` F13). The tail saturation problem
(F6/F7) is acknowledged, not solved: a severity-sensitive alerting statistic is a separate need
from a scoring penalty, and conflating them was the original error.

## 4. Aggregation over windows

`angriness(pattern) = CVaR_0.9 over windows of penalty(p99(pattern, window))`, reported as a
**delta from a per-regime healthy baseline** (the capacity model's prediction for that pattern at
that load, `review-01` F19), never as a raw mean.

Why CVaR rather than "worst regime": `review-01` F8 showed a maximum over noisy windows is
upward-biased and manufactures bad regimes as the window count grows, and that a minimum-sample
gate then excludes the peak by construction. CVaR is a coherent risk measure with a usable
estimator and no such bias; the minimum-sample gate becomes "windows with fewer than
`min_samples` requests contribute with weight 0 and are reported separately", which is honest
about what is being ignored.

## 5. Throughput term

`angriness_total = angriness_latency + λ * angriness_throughput`, with `λ = 1.0` default and the
throughput target per pattern pre-registered. Reported separately so a layout that wins latency by
destroying throughput is visible rather than netted out.

## 6. What this forces on the simulator (answers `review-01` F14 concretely)

The cost model must predict **p99 per (pattern, window)**, not a mean, and its calibration target
is therefore the same statistic the penalty consumes. Required reporting for every prediction:
point estimate, prediction interval, and the quantile-calibration error against measurements, plus
out-of-distribution diagnostics when a candidate layout is unlike anything measured.

First-version model terms, each with the measurement that validates it:

| term | source |
|---|---|
| dependent point read (4K QD1 96.3 µs; 16K 117.4 µs) | 0a-redo, both fio and O_DIRECT Rust |
| batched amortised read (4.9 µs/read at QD32 ≈ 203k IOPS) | 0a-redo |
| sequential bandwidth 831 MiB/s (device) / 605 MiB/s (engine scan) | 0a-redo + 12c run |
| per-block checksum CPU (CRC-32C 8.8–9.7 GB/s) | 08c measurement |
| fence index 0.20 MiB per GiB of tiles | 11 measurement |
| bloom false-positive rate 0.79% at 10 bits/key | 08d-2a measurement |
| write amplification (3.92× at 5 GB, tiered) | 12d measurement |
| window-bounded working set ≈ 2.3×W | 10a/10c measurements |

## 7. Falsification contract (draft for the author to pre-register)

- **Hypothesis under test:** with a trace-derived, drift-adapted per-tile layout, flashdb achieves
  lower angriness than `flashdb-declared` at equal durability.
- **Threshold:** at ≥10× cache-size data, the learned layout must beat `flashdb-declared` by more
  than **15%** in total angriness, and must not lose to `flashdb-static-derived` on the
  no-drift workloads. Failing that, the thesis is rejected as stated.
- **Primary metric:** total angriness over the run, with p99 per pattern as the underlying
  statistic; `relative_regret` against the relaxed oracle (per-tile best over the published
  candidate set, cardinality stated) as the secondary metric.
- **Minimum instance size:** the working set must exceed cache by ≥10×, otherwise the fan-out
  premise is untested.
- **Seeds:** 10 independent seeds; report median and IQR; a win must be visible in the median and
  not contradicted by the IQR.
- **Drift:** scored as time-to-recover (windows to return within X% of pre-drift angriness), with
  the reorg payback window stated; reference is an offline-best *sequence* over the published
  candidate set, never a single prescient layout.
- **Cost-model error tolerance:** predicted vs measured p99 within 25% at the 90th percentile of
  patterns, else the simulator's verdicts are reported as uncalibrated.
- **Postgres:** passive scoreboard only; a comparison is publishable only with its tuning protocol
  fixed in advance.

## 8. What the author must confirm or replace

1. The estimand (p99 vs CVaR) and the SLO's reference load + inflation factor.
2. The penalty's piecewise form and `ε`, `s`.
3. CVaR_0.9 as the aggregation, and how ignored low-sample windows are reported.
4. The throughput term, `λ`, and the target.
5. The falsification threshold (15%), the seed count (10), and the calibration tolerance (25%).
6. Whether any of this may be quoted outside the project before they sign off.
