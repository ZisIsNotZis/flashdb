# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/cold44/per_region-mixed`: data 1,643,933,528 B, cgroup cap 201,326,592 B, **ratio 8.17x**

> **WARNING: data:cap 8.17x is below the pre-registration 10x floor.** The instance was reused from slices 29/30 rather than rebuilt larger; every throughput verdict is qualified by this.

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `adversarial` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | ingest/service | reorg/service |
|---|---:|---:|---:|---:|---:|
| adversarial/declared | 5.848e+09 | 7.492e+05 | 6.16e+05 | 0.013% | 0.011% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | p50 us | p99 us | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| adversarial | declared | n/a | 50 | 50.0 | 100% | 155484 | 0 | no | n/a | n/a | 336 | 536725 |
| adversarial | declared | n/a | 100 | 99.3 | 99% | 207522 | 412814 | no | n/a | n/a | 336 | 536725 |
| adversarial | declared | n/a | 200 | 196.6 | 98% | 161957.5 | 632343 | no | n/a | n/a | 336 | 536725 |

`commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the group-quantised per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error, no window the pacer rejected (schedule lag ran away), and no window over the latency ceiling.

**Latency ceiling:** `--ceiling-us 10000` per window. A window whose p99 exceeds it is rejected and its rung cannot be scored sustained. Windows the ceiling rejected this run: **57** (`adversarial/declared` 57). A run without a ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| adversarial | declared | 50 | 400,761 |
| adversarial | declared | 100 | 103,478 |
| adversarial | declared | 200 | 32,486 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| adversarial | declared | 201326592 | 201326592 | 1.000 | 1643933528 | 8.17x | n/a | yes |

## Per-region telemetry (summed over the run)

### adversarial/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 57 | 0 | 84,240 | 0 | 0 | 0.0000 |
| P|Customer | 57 | 15,669,365 | 0 | 0 | 0 | n/a |
| P|Order | 57 | 0 | 119,367 | 0 | 0 | 0.0000 |
| P|Stock | 57 | 215,209,702 | 72,588 | 0 | 0 | 0.0000 |
| P|StockMovement | 57 | 0 | 143,072 | 0 | 0 | 0.0000 |
| R|Order | 57 | 0 | 36,820 | 357,966 | 32 | 9.7221 |
| R|StockMovement | 58 | 0 | 45,236 | 99,349 | 18 | 2.1962 |
| U|Backorder | 61 | 0 | 49,491 | 34,468 | 6 | 0.6964 |
| U|Customer | 57 | 15,599,873 | 0 | 0 | 0 | n/a |
| U|Order | 59 | 0 | 45,236 | 44,942 | 8 | 0.9935 |
| U|Stock | 57 | 15,615,775 | 0 | 0 | 0 | n/a |
| U|StockMovement | 57 | 0 | 56,808 | 0 | 0 | 0.0000 |

## Learner decisions and drift

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|

## What is NOT concluded

- data:cap is 8.17x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
