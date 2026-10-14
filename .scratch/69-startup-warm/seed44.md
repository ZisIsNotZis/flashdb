# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/cold51/per_region-mixed`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `win` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | index write term ns | ingest/service | reorg/service | index/service |
|---|---:|---:|---:|---:|---:|---:|---:|
| win/declared | 9.886e+10 | 1.117e+06 | 7.351e+06 | 0 | 0.001% | 0.007% | 0.000% |
| win/learned@1.0 | 1.423e+10 | 9.735e+05 | 3.39e+06 | 2.143e+06 | 0.007% | 0.024% | 0.015% |
| win/static-derived | 6.253e+09 | 1.117e+06 | 5.247e+07 | 2.46e+06 | 0.018% | 0.839% | 0.039% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| win/learned@1.0 | 11 | 9 | 0 | 14 | 3,389,765 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| win | declared | n/a | 50 | 49.9 | 100% | 36981.5 | 331208 | 391208 | 2983.5 | 164845 | 204845 | 391208 | 1329 | 1.50 | 11 | 3 | 831 | no | 1.000 | 1569 | 336 | 6404991 |
| win | declared | n/a | 100 | 83.1 | 83% | 2550100.5 | 4366704 | 4416704 | 1903562.0 | 3731534 | 3771534 | 4426704 | 35 | 57.14 | 64 | 540 | 34 | no | 1.000 | 1569 | 336 | 6404991 |
| win | declared | n/a | 200 | 86.6 | 43% | 10517290 | 17147088 | 17162088 | 10191957 | 16837081 | 16837081 | 17162088 | 32 | 62.50 | 64 | 3356740 | 32 | no | 1.000 | 1569 | 336 | 6404991 |
| win | declared | n/a | 400 | 78.7 | 20% | 27203964 | 37619498 | 37631998 | 27038537 | 37459097 | 37469097 | 37634498 | 32 | 62.50 | 64 | 16950220 | 32 | no | 1.000 | 1569 | 336 | 6404991 |
| win | learned@1.0 | 1.0 | 50 | 49.5 | 99% | 522.0 | 841330 | 8381 | 65.5 | 64347 | 69199 | 999504 | 1948 | 1.03 | 4 | 0 | 293 | no | 1.000 | 1569 | 336 | 3389765 |
| win | learned@1.0 | 1.0 | 100 | 100.8 | 101% | 630.0 | 4463703 | 4463703 | 69.0 | 1686429 | 1796429 | 4463703 | 1532 | 1.31 | 64 | 0 | 155 | no | 1.000 | 1569 | 336 | 3389765 |
| win | learned@1.0 | 1.0 | 200 | 200.0 | 100% | 2657.0 | 235019 | 260019 | 69.0 | 156470 | 196470 | 270019 | 1891 | 1.06 | 42 | 0 | 348 | no | 1.000 | 1569 | 336 | 3389765 |
| win | learned@1.0 | 1.0 | 400 | 393.4 | 98% | 32418 | 246445 | 266518 | 1392 | 131731 | 171731 | 276518 | 1117 | 1.79 | 64 | 8 | 499 | no | 1.000 | 1569 | 336 | 3389765 |
| win | static-derived | n/a | 50 | 50.0 | 100% | 484.0 | 157659 | 9458 | 63.5 | 1949 | 17896 | 215088 | 2000 | 1.00 | 1 | 1 | 161 | no | 1.000 | 1569 | 336 | 45722243 |
| win | static-derived | n/a | 100 | 100.0 | 100% | 456.0 | 58203 | 97358 | 66.5 | 2976 | 42900 | 98008 | 1994 | 1.00 | 6 | 0 | 170 | no | 1.000 | 1569 | 336 | 45722243 |
| win | static-derived | n/a | 200 | 200.0 | 100% | 1053.5 | 43552 | 63486 | 64.5 | 15367 | 26762 | 68486 | 1964 | 1.02 | 7 | 1 | 246 | no | 1.000 | 1569 | 336 | 45722243 |
| win | static-derived | n/a | 400 | 382.7 | 96% | 10824.0 | 118473 | 125732 | 453.5 | 237470 | 237470 | 223111 | 1530 | 1.31 | 8 | 1 | 665 | no | 1.000 | 1569 | 336 | 45722243 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **232** (`win/declared` 90, `win/learned@1.0` 69, `win/static-derived` 73); windows the LAG ceiling rejected: **113** (`win/declared` 90, `win/learned@1.0` 15, `win/static-derived` 8). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| win | declared | 50 | 6,404,991 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 0 |
| win | learned@1.0 | 50 | 2,662,681 |
| win | learned@1.0 | 100 | 727,084 |
| win | learned@1.0 | 200 | 0 |
| win | learned@1.0 | 400 | 0 |
| win | static-derived | 50 | 10,586,369 |
| win | static-derived | 100 | 17,067,581 |
| win | static-derived | 200 | 11,023,536 |
| win | static-derived | 400 | 7,044,757 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77111296 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 79876096 | yes |
| win | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78295040 | yes |

## Per-region telemetry (summed over the run)

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 125,520 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 16,530,677 | 0 | 0 | 0 | 0 | n/a |
| P|Order | 90 | 0 | 178,012 | 0 | 0 | 0 | 0.0000 |
| P|Stock | 90 | 61,471,671 | 108,261 | 1,602,041 | 0 | 3 | 14.7980 |
| P|StockMovement | 90 | 10,647,466,436 | 213,384 | 0 | 0 | 0 | 0.0000 |
| R|Order | 90 | 0 | 54,915 | 1,473,438 | 0 | 3 | 26.8312 |
| R|StockMovement | 90 | 0 | 67,467 | 1,726,908 | 0 | 3 | 25.5963 |
| U|Backorder | 90 | 0 | 73,743 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,489,334 | 0 | 0 | 0 | 0 | n/a |
| U|Order | 90 | 0 | 67,467 | 1,098,365 | 0 | 2 | 16.2800 |
| U|Stock | 90 | 20,121,438 | 0 | 0 | 0 | 0 | n/a |
| U|StockMovement | 90 | 0 | 84,726 | 504,239 | 0 | 1 | 5.9514 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 73 | 0 | 125,520 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 73 | 16,530,677 | 0 | 0 | 0 | 0 | n/a |
| P|Order | 73 | 0 | 178,012 | 0 | 0 | 0 | 0.0000 |
| P|Stock | 73 | 390,548,109 | 108,261 | 1,704,177 | 0 | 3 | 15.7414 |
| P|StockMovement | 73 | 232,499,473 | 213,384 | 0 | 2,143,439 | 0 | 0.0000 |
| R|Order | 73 | 0 | 54,915 | 323,715 | 0 | 1 | 5.8948 |
| R|StockMovement | 73 | 0 | 67,467 | 430,859 | 0 | 1 | 6.3862 |
| U|Backorder | 73 | 0 | 73,743 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 73 | 16,489,334 | 0 | 0 | 0 | 0 | n/a |
| U|Order | 73 | 0 | 67,467 | 426,775 | 0 | 1 | 6.3257 |
| U|Stock | 73 | 20,121,438 | 0 | 0 | 0 | 0 | n/a |
| U|StockMovement | 73 | 0 | 84,726 | 504,239 | 0 | 1 | 5.9514 |

### win/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 125,520 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 16,530,677 | 0 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 178,012 | 9,734,566 | 0 | 5 | 54.6849 |
| P|Stock | 76 | 39,866,046 | 108,261 | 16,816,138 | 0 | 7 | 155.3296 |
| P|StockMovement | 76 | 26,592,479 | 213,384 | 0 | 2,143,439 | 0 | 0.0000 |
| R|Order | 76 | 0 | 54,915 | 3,761,070 | 0 | 5 | 68.4889 |
| R|StockMovement | 76 | 0 | 67,467 | 4,409,650 | 0 | 5 | 65.3601 |
| U|Backorder | 76 | 0 | 73,743 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 16,489,334 | 0 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,467 | 0 | 0 | 0 | 0.0000 |
| U|Stock | 76 | 77,536,232 | 0 | 5,133,935 | 0 | 1 | n/a |
| U|StockMovement | 76 | 0 | 84,726 | 5,866,884 | 0 | 5 | 69.2454 |

## Learner decisions and drift

### win/learned@1.0

- gated switches: {'P|StockMovement': 0, 'U|Customer': 1, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 1, 'P|Stock': 0, 'P|Backorder': 1, 'U|StockMovement': 2, 'R|StockMovement': 1, 'P|Order': 1, 'R|Order': 1}
- drift restarts: {'P|StockMovement': 0, 'U|Customer': 3, 'P|Customer': 3, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 3, 'P|Stock': 2, 'P|Backorder': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Customer': 6, 'P|Customer': 6, 'U|Stock': 6, 'P|Stock': 6}
- first policy: {'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}
- policy changes (14): ["w0: {'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}", "w3: {'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)'}", "w6: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)'}", "w9: {'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}", "w12: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)'}", "w15: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w41: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w44: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w62: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w65: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w68: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w69: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w70: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}", "w72: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}"]
- final policy: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}

### win/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)'}
- policy changes (1): ["w0: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)'}"]
- final policy: {'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **NOT-MEASURABLE** | declared sustained rate 0.0, learned 0.0: no sustained level to compare (the box could not complete even the reference level) |
| T3 reorganisation <= 3x declared | 1.0 | **PASS** | learned reorg 3389765 B vs declared 6404991 B (ratio 0.53) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
