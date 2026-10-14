# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/wave21-driftshape/per_region-mixed`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `drift` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | index write term ns | ingest/service | reorg/service | index/service |
|---|---:|---:|---:|---:|---:|---:|---:|
| drift/learned@1.0 | 1.125e+10 | 2.643e+06 | 0 | 2.143e+06 | 0.024% | 0.000% | 0.019% |
| drift/static-derived | 1.068e+10 | 3.034e+06 | 0 | 2.46e+06 | 0.028% | 0.000% | 0.023% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Drift shape (WP-W):** the post-drift segments multiply the `write` WIN_MIX weight x10 (a write-spike drift).
**Drift rate (Slice 39):** resolved to **1000 rps** by rule `explicit`. Rule: the fixed drift rate is `--drift-rate-fraction` = 70% of the PRE-DRIFT (win) mix's highest sustained level, measured by a short pre-flight ladder; pre-flight highest sustained = n/a rps.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| drift/learned@1.0 | 0 | 0 | 0 | 0 | 0 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| drift | learned@1.0 | 1.0 | 1000 | 957.1 | 96% | 1539268.0 | 3859637 | 3890010 | 1469352.0 | 3794603 | 3831105 | 3906010 | 191 | 62.83 | 64 | 56 | 188 | no | 1.000 | 4261 | 336 | 0 |
| drift | static-derived | n/a | 1000 | 997.6 | 100% | 32365.5 | 399051 | 413051 | 8474.5 | 339176 | 355382 | 419051 | 744 | 16.13 | 64 | 1 | 719 | no | 1.000 | 4261 | 336 | 0 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **24** (`drift/learned@1.0` 12, `drift/static-derived` 12); windows the LAG ceiling rejected: **24** (`drift/learned@1.0` 12, `drift/static-derived` 12). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| drift | learned@1.0 | 1000 | 0 |
| drift | static-derived | 1000 | 0 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| drift | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 79437824 | yes |
| drift | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78110720 | yes |

## Per-region telemetry (summed over the run)

### drift/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 12 | 0 | 340,880 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 12 | 38,279,472 | 0 | 0 | 0 | 0 | n/a |
| P|Order | 12 | 0 | 483,356 | 0 | 0 | 0 | 0.0000 |
| P|Stock | 12 | 369,366,236 | 293,940 | 0 | 0 | 0 | 0.0000 |
| P|StockMovement | 12 | 536,950,852 | 579,360 | 0 | 2,143,439 | 0 | 0.0000 |
| R|Order | 12 | 0 | 149,100 | 0 | 0 | 0 | 0.0000 |
| R|StockMovement | 12 | 0 | 183,180 | 0 | 0 | 0 | 0.0000 |
| U|Backorder | 12 | 0 | 200,267 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 12 | 38,046,161 | 0 | 0 | 0 | 0 | n/a |
| U|Order | 12 | 0 | 183,180 | 0 | 0 | 0 | 0.0000 |
| U|Stock | 12 | 40,613,636 | 0 | 0 | 0 | 0 | n/a |
| U|StockMovement | 12 | 0 | 230,040 | 0 | 0 | 0 | 0.0000 |

### drift/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 12 | 0 | 340,880 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 12 | 38,279,472 | 0 | 0 | 0 | 0 | n/a |
| P|Order | 12 | 0 | 483,356 | 0 | 0 | 0 | 0.0000 |
| P|Stock | 12 | 371,727,087 | 293,940 | 0 | 0 | 0 | 0.0000 |
| P|StockMovement | 12 | 19,932,889 | 579,360 | 0 | 2,143,439 | 0 | 0.0000 |
| R|Order | 12 | 0 | 149,100 | 0 | 0 | 0 | 0.0000 |
| R|StockMovement | 12 | 0 | 183,180 | 0 | 0 | 0 | 0.0000 |
| U|Backorder | 12 | 0 | 200,267 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 12 | 38,046,161 | 0 | 0 | 0 | 0 | n/a |
| U|Order | 12 | 0 | 183,180 | 0 | 0 | 0 | 0.0000 |
| U|Stock | 12 | 40,613,636 | 0 | 0 | 0 | 0 | n/a |
| U|StockMovement | 12 | 0 | 230,040 | 0 | 0 | 0 | 0.0000 |

## Learner decisions and drift

### drift/learned@1.0

- gated switches: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'P|StockMovement': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- drift restarts: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'P|StockMovement': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}
- policy changes (4): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}

### drift/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}
- policy changes (1): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T4 drift recovery within 2 windows | 1.0 | **NOT-MEASURABLE** | the learner recorded no drift-triggered re-exploration window count (the drift segment may have been too short to move an active region's cost past drift_margin for drift_windows). |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal
- the drift rate is a stated fraction (70%) of the pre-drift mix's highest sustained level measured by the pre-flight; it is not the win mix's true capacity and the fraction is an author-owned choice, not a measurement

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
