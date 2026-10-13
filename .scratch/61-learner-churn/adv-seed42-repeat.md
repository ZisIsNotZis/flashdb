# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/cold51/per_region-mixed`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `adversarial` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | ingest/service | reorg/service |
|---|---:|---:|---:|---:|---:|
| adversarial/declared | 7.581e+09 | 1.124e+06 | 2.675e+07 | 0.015% | 0.353% |
| adversarial/learned@1.0 | 7.292e+09 | 9.791e+05 | 4.368e+06 | 0.013% | 0.060% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| adversarial/learned@1.0 | 5 | 3 | 0 | 14 | 4,368,151 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| adversarial | declared | n/a | 50 | 50.0 | 100% | 525.0 | 138207 | 9338 | 36.5 | 100959 | 140959 | 160830 | 1992 | 1.00 | 8 | 1 | 99 | no | 1.000 | 1578 | 336 | 23310824 |
| adversarial | declared | n/a | 100 | 100.0 | 100% | 457.5 | 20368 | 9984 | 43.0 | 2568 | 23857 | 66961 | 1997 | 1.00 | 4 | 3 | 127 | no | 1.000 | 1578 | 336 | 23310824 |
| adversarial | declared | n/a | 200 | 199.9 | 100% | 1812.0 | 25068 | 114588 | 38.5 | 2345 | 30608 | 119634 | 1979 | 1.01 | 9 | 0 | 261 | no | 1.000 | 1578 | 336 | 23310824 |
| adversarial | declared | n/a | 400 | 377.6 | 94% | 9095.5 | 49035 | 67861 | 898.0 | 250950 | 255950 | 75358 | 1435 | 1.39 | 51 | 3 | 533 | no | 1.000 | 1578 | 336 | 23310824 |
| adversarial | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 504.5 | 92882 | 8744 | 64.0 | 30370 | 70370 | 168122 | 1997 | 1.00 | 4 | 0 | 117 | no | 1.000 | 1578 | 336 | 4368151 |
| adversarial | learned@1.0 | 1.0 | 100 | 100.0 | 100% | 530.5 | 128009 | 8964 | 67.0 | 305 | 2862 | 235230 | 2000 | 1.00 | 1 | 1 | 112 | no | 1.000 | 1578 | 336 | 4368151 |
| adversarial | learned@1.0 | 1.0 | 200 | 199.9 | 100% | 2271.5 | 21170 | 109865 | 66.5 | 4206 | 10013 | 122406 | 1982 | 1.01 | 4 | 1 | 269 | no | 1.000 | 1578 | 336 | 4368151 |
| adversarial | learned@1.0 | 1.0 | 400 | 378.3 | 95% | 8245.5 | 133041 | 158041 | 541.5 | 259013 | 264013 | 168041 | 1419 | 1.41 | 52 | 5 | 446 | no | 1.000 | 1578 | 336 | 4368151 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **137** (`adversarial/declared` 68, `adversarial/learned@1.0` 69); windows the LAG ceiling rejected: **16** (`adversarial/declared` 8, `adversarial/learned@1.0` 8). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| adversarial | declared | 50 | 11,530,478 |
| adversarial | declared | 100 | 8,564,792 |
| adversarial | declared | 200 | 3,215,554 |
| adversarial | declared | 400 | 0 |
| adversarial | learned@1.0 | 50 | 4,368,151 |
| adversarial | learned@1.0 | 100 | 0 |
| adversarial | learned@1.0 | 200 | 0 |
| adversarial | learned@1.0 | 400 | 0 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| adversarial | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77799424 | yes |
| adversarial | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 76910592 | yes |

## Per-region telemetry (summed over the run)

### adversarial/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 3,807,887 | 3 | 21.2691 |
| P|Stock | 76 | 52,919,706 | 108,882 | 3,881,780 | 5 | 35.6513 |
| P|StockMovement | 76 | 0 | 214,608 | 2,547,913 | 2 | 11.8724 |
| R|Order | 76 | 0 | 55,230 | 3,761,070 | 5 | 68.0983 |
| R|StockMovement | 76 | 0 | 67,854 | 2,695,158 | 4 | 39.7200 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 3,032,766 | 4 | 44.6955 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 3,584,250 | 4 | 42.0627 |

### adversarial/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 0 | 0 | 0.0000 |
| P|Stock | 76 | 352,557,361 | 108,882 | 2,682,563 | 4 | 24.6373 |
| P|StockMovement | 76 | 0 | 214,608 | 0 | 0 | 0.0000 |
| R|Order | 76 | 0 | 55,230 | 323,715 | 1 | 5.8612 |
| R|StockMovement | 76 | 0 | 67,854 | 430,859 | 1 | 6.3498 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 426,775 | 1 | 6.2896 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 504,239 | 1 | 5.9175 |

## Learner decisions and drift

### adversarial/learned@1.0

- gated switches: {'U|Stock': 1, 'P|Stock': 0, 'U|Customer': 1, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- drift restarts: {'U|Stock': 2, 'P|Stock': 0, 'U|Customer': 2, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Stock': 6, 'U|Customer': 6, 'P|Customer': 6}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (11): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w63: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w66: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w69: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w72: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w75: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T2 learned loses <= 5% on adversarial-uniform | 1.0 | **PASS** | learned loses +0.0% vs declared on adversarial-uniform (declared sustained 0.0, learned 0.0) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
