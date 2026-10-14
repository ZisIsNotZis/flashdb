# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/wave21-rewarm/cold51`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `win` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | index write term ns | ingest/service | reorg/service | index/service |
|---|---:|---:|---:|---:|---:|---:|---:|
| win/declared | 9.5e+10 | 1.165e+06 | 3.526e+07 | 0 | 0.001% | 0.037% | 0.000% |
| win/learned@1.0 | 7.839e+09 | 1.015e+06 | 2.835e+07 | 2.143e+06 | 0.013% | 0.362% | 0.027% |
| win/static-derived | 6.77e+09 | 1.165e+06 | 3.429e+07 | 2.46e+06 | 0.017% | 0.506% | 0.036% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| win/learned@1.0 | 4 | 7 | 3 | 12 | 28,350,024 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| win | declared | n/a | 50 | 50.0 | 100% | 40346.5 | 256535 | 296535 | 7628.0 | 130466 | 170466 | 296535 | 1296 | 1.54 | 9 | 0 | 849 | no | 1.000 | 1636 | 336 | 30720572 |
| win | declared | n/a | 100 | 88.7 | 89% | 680371.5 | 2933697 | 2963697 | 226641.0 | 2309941 | 2309941 | 2963697 | 88 | 22.73 | 64 | 54 | 83 | no | 1.000 | 1636 | 336 | 30720572 |
| win | declared | n/a | 200 | 90.6 | 45% | 8344127.0 | 14724420 | 14749420 | 8013050.0 | 14399324 | 14419324 | 14754420 | 32 | 62.50 | 64 | 2130881 | 32 | no | 1.000 | 1636 | 336 | 30720572 |
| win | declared | n/a | 400 | 86.5 | 22% | 23792521.5 | 32517380 | 32524880 | 23622888.0 | 32359678 | 32359678 | 32524880 | 32 | 62.50 | 64 | 14355663 | 32 | no | 1.000 | 1636 | 336 | 30720572 |
| win | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 588.5 | 176992 | 9988 | 64.0 | 84663 | 104663 | 213149 | 1953 | 1.02 | 6 | 1 | 166 | no | 1.000 | 1636 | 336 | 28350024 |
| win | learned@1.0 | 1.0 | 100 | 100.0 | 100% | 649.0 | 73508 | 8794 | 67.5 | 971 | 3825 | 114277 | 2000 | 1.00 | 1 | 1 | 113 | no | 1.000 | 1636 | 336 | 28350024 |
| win | learned@1.0 | 1.0 | 200 | 199.9 | 100% | 1198.5 | 23374 | 30718 | 65.0 | 803 | 12633 | 32235 | 1987 | 1.01 | 4 | 2 | 287 | no | 1.000 | 1636 | 336 | 28350024 |
| win | learned@1.0 | 1.0 | 400 | 400.0 | 100% | 16928 | 397523 | 440023 | 791 | 366900 | 406900 | 450023 | 1386 | 1.44 | 64 | 1 | 592 | no | 1.000 | 1636 | 336 | 28350024 |
| win | static-derived | n/a | 50 | 50.0 | 100% | 597.5 | 142624 | 6992 | 64.0 | 214 | 38353 | 192942 | 1998 | 1.00 | 2 | 1 | 109 | no | 1.000 | 1636 | 336 | 29874895 |
| win | static-derived | n/a | 100 | 100.0 | 100% | 753.0 | 11972 | 9210 | 68.5 | 1130 | 2087 | 40672 | 2000 | 1.00 | 1 | 2 | 113 | no | 1.000 | 1636 | 336 | 29874895 |
| win | static-derived | n/a | 200 | 200.0 | 100% | 1110.5 | 78733 | 105107 | 66.5 | 2374 | 10951 | 119676 | 1988 | 1.01 | 4 | 1 | 282 | no | 1.000 | 1636 | 336 | 29874895 |
| win | static-derived | n/a | 400 | 399.2 | 100% | 29288 | 292743 | 330243 | 1200 | 252747 | 292747 | 340243 | 1243 | 1.61 | 64 | 1 | 619 | no | 1.000 | 1636 | 336 | 29874895 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **226** (`win/declared` 90, `win/learned@1.0` 68, `win/static-derived` 68); windows the LAG ceiling rejected: **107** (`win/declared` 90, `win/learned@1.0` 9, `win/static-derived` 8). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| win | declared | 50 | 30,720,572 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 0 |
| win | learned@1.0 | 50 | 28,350,024 |
| win | learned@1.0 | 100 | 0 |
| win | learned@1.0 | 200 | 0 |
| win | learned@1.0 | 400 | 0 |
| win | static-derived | 50 | 29,874,895 |
| win | static-derived | 100 | 0 |
| win | static-derived | 200 | 0 |
| win | static-derived | 400 | 0 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77201408 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 80633856 | yes |
| win | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77615104 | yes |

## Per-region telemetry (summed over the run)

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 130,880 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 17,006,891 | 0 | 10,290,339 | 0 | 1 | n/a |
| P|Order | 90 | 0 | 185,585 | 835,904 | 0 | 1 | 4.5042 |
| P|Stock | 90 | 83,336,015 | 112,884 | 368,160 | 0 | 1 | 3.2614 |
| P|StockMovement | 90 | 10,545,591,782 | 222,496 | 991,209 | 0 | 1 | 4.4550 |
| R|Order | 90 | 0 | 57,260 | 323,715 | 0 | 1 | 5.6534 |
| R|StockMovement | 90 | 0 | 70,348 | 379,269 | 0 | 1 | 5.3913 |
| U|Backorder | 90 | 0 | 76,892 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,936,850 | 0 | 11,372,547 | 0 | 1 | n/a |
| U|Order | 90 | 0 | 70,348 | 426,775 | 0 | 1 | 6.0666 |
| U|Stock | 90 | 20,176,474 | 0 | 5,228,415 | 0 | 1 | n/a |
| U|StockMovement | 90 | 0 | 88,344 | 504,239 | 0 | 1 | 5.7077 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 130,880 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 75 | 17,011,023 | 0 | 10,290,339 | 0 | 1 | n/a |
| P|Order | 75 | 0 | 185,585 | 0 | 0 | 0 | 0.0000 |
| P|Stock | 75 | 269,195,887 | 112,884 | 423,444 | 0 | 1 | 3.7511 |
| P|StockMovement | 75 | 198,937,519 | 222,496 | 0 | 2,143,439 | 0 | 0.0000 |
| R|Order | 75 | 0 | 57,260 | 323,715 | 0 | 1 | 5.6534 |
| R|StockMovement | 75 | 0 | 70,348 | 379,269 | 0 | 1 | 5.3913 |
| U|Backorder | 75 | 0 | 76,892 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 75 | 16,936,850 | 0 | 11,372,547 | 0 | 1 | n/a |
| U|Order | 75 | 0 | 70,348 | 426,775 | 0 | 1 | 6.0666 |
| U|Stock | 75 | 165,781,002 | 0 | 5,133,935 | 0 | 1 | n/a |
| U|StockMovement | 75 | 0 | 88,344 | 0 | 0 | 0 | 0.0000 |

### win/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | index_output_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 130,880 | 0 | 0 | 0 | 0.0000 |
| P|Customer | 75 | 34,033,343 | 0 | 10,165,563 | 0 | 1 | n/a |
| P|Order | 75 | 0 | 185,585 | 835,904 | 0 | 1 | 4.5042 |
| P|Stock | 75 | 276,178,940 | 112,884 | 423,444 | 0 | 1 | 3.7511 |
| P|StockMovement | 75 | 57,035,514 | 222,496 | 1,032,297 | 2,143,439 | 1 | 4.6396 |
| R|Order | 75 | 0 | 57,260 | 323,715 | 0 | 1 | 5.6534 |
| R|StockMovement | 75 | 0 | 70,348 | 379,269 | 0 | 1 | 5.3913 |
| U|Backorder | 75 | 0 | 76,892 | 0 | 0 | 0 | 0.0000 |
| U|Customer | 75 | 33,690,258 | 0 | 11,076,529 | 0 | 1 | n/a |
| U|Order | 75 | 0 | 70,348 | 0 | 0 | 0 | 0.0000 |
| U|Stock | 75 | 155,309,004 | 0 | 5,133,935 | 0 | 1 | n/a |
| U|StockMovement | 75 | 0 | 88,344 | 504,239 | 0 | 1 | 5.7077 |

## Learner decisions and drift

### win/learned@1.0

- gated switches: {'U|Customer': 1, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 1, 'P|Stock': 0, 'P|Backorder': 1, 'U|StockMovement': 1, 'P|StockMovement': 0, 'R|StockMovement': 1, 'P|Order': 1, 'R|Order': 1}
- drift restarts: {'U|Customer': 2, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 1, 'P|Stock': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Customer': 6, 'U|Stock': 6}
- first policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}
- policy changes (14): ["w0: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)'}", "w3: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)'}", "w6: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)'}", "w9: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=False)'}", "w12: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)'}", "w15: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w19: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w64: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w66: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w67: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w70: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w71: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w73: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)'}", "w74: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=False)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200, index_order_no=True)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)'}

### win/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}
- policy changes (1): ["w0: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8, index_order_no=False)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8, index_order_no=False)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2, index_order_no=True)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **NOT-MEASURABLE** | declared sustained rate 0.0, learned 0.0: no sustained level to compare (the box could not complete even the reference level) |
| T3 reorganisation <= 3x declared | 1.0 | **PASS** | learned reorg 28350024 B vs declared 30720572 B (ratio 0.92) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
