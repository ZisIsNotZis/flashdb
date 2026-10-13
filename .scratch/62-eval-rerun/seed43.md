# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/cold51/per_region-mixed`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `adversarial` bracketed (at least one arm failed the top step); `win` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | ingest/service | reorg/service |
|---|---:|---:|---:|---:|---:|
| adversarial/declared | 7.62e+09 | 1.159e+06 | 2.485e+07 | 0.015% | 0.326% |
| adversarial/learned@1.0 | 7.849e+09 | 1.01e+06 | 2.888e+06 | 0.013% | 0.037% |
| adversarial/static-derived | 7.987e+09 | 1.159e+06 | 4.053e+07 | 0.015% | 0.507% |
| win/declared | 9.353e+10 | 1.165e+06 | 8.261e+06 | 0.001% | 0.009% |
| win/learned@1.0 | 9.216e+10 | 1.015e+06 | 2.713e+06 | 0.001% | 0.003% |
| win/static-derived | 9.704e+10 | 1.165e+06 | 1.589e+07 | 0.001% | 0.016% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| adversarial/learned@1.0 | 5 | 15 | 0 | 15 | 2,887,806 |
| win/learned@1.0 | 0 | 18 | 2 | 12 | 2,713,071 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| adversarial | declared | n/a | 50 | 50.0 | 100% | 608.5 | 57031 | 8285 | 66.0 | 2732 | 4731 | 111923 | 2000 | 1.00 | 1 | 0 | 142 | no | 1.000 | 1628 | 336 | 21651974 |
| adversarial | declared | n/a | 100 | 100.0 | 100% | 589.0 | 13950 | 40922 | 71.0 | 1597 | 4229 | 48076 | 2000 | 1.00 | 1 | 1 | 142 | no | 1.000 | 1628 | 336 | 21651974 |
| adversarial | declared | n/a | 200 | 199.9 | 100% | 2574.0 | 68366 | 94238 | 70.0 | 17965 | 37965 | 101732 | 1965 | 1.02 | 10 | 1 | 363 | no | 1.000 | 1628 | 336 | 21651974 |
| adversarial | declared | n/a | 400 | 399.2 | 100% | 9679 | 201799 | 241799 | 646 | 163136 | 203136 | 249299 | 1428 | 1.40 | 64 | 1 | 555 | no | 1.000 | 1628 | 336 | 21651974 |
| adversarial | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 604.5 | 174723 | 9389 | 68.0 | 139 | 4799 | 217971 | 2000 | 1.00 | 1 | 0 | 112 | no | 1.000 | 1628 | 336 | 2887806 |
| adversarial | learned@1.0 | 1.0 | 100 | 100.0 | 100% | 697.0 | 29649 | 7928 | 70.0 | 1635 | 29707 | 69033 | 1997 | 1.00 | 4 | 0 | 110 | no | 1.000 | 1628 | 336 | 2887806 |
| adversarial | learned@1.0 | 1.0 | 200 | 199.9 | 100% | 2148.0 | 92789 | 127789 | 69.5 | 62429 | 102429 | 132789 | 1963 | 1.02 | 22 | 1 | 294 | no | 1.000 | 1628 | 336 | 2887806 |
| adversarial | learned@1.0 | 1.0 | 400 | 399.6 | 100% | 13410 | 204859 | 235814 | 963 | 159924 | 199924 | 243314 | 1295 | 1.54 | 64 | 2 | 458 | no | 1.000 | 1628 | 336 | 2887806 |
| adversarial | static-derived | n/a | 50 | 50.0 | 100% | 690.0 | 199338 | 7669 | 72.0 | 1425 | 9192 | 253856 | 2000 | 1.00 | 1 | 1 | 148 | no | 1.000 | 1628 | 336 | 35317938 |
| adversarial | static-derived | n/a | 100 | 100.0 | 100% | 757.0 | 63560 | 154224 | 73.0 | 16709 | 36709 | 157559 | 1988 | 1.01 | 5 | 2 | 159 | no | 1.000 | 1628 | 336 | 35317938 |
| adversarial | static-derived | n/a | 200 | 199.9 | 100% | 2441.5 | 41779 | 71779 | 71.5 | 11854 | 41854 | 81779 | 1959 | 1.02 | 11 | 1 | 277 | no | 1.000 | 1628 | 336 | 35317938 |
| adversarial | static-derived | n/a | 400 | 399.2 | 100% | 8920 | 225618 | 265618 | 586 | 177801 | 217801 | 273118 | 1485 | 1.35 | 64 | 1 | 484 | no | 1.000 | 1628 | 336 | 35317938 |
| win | declared | n/a | 50 | 49.9 | 100% | 38457.0 | 273818 | 313818 | 6018.5 | 147407 | 187407 | 313818 | 1343 | 1.49 | 10 | 1 | 851 | no | 1.000 | 1636 | 336 | 7198067 |
| win | declared | n/a | 100 | 86.4 | 86% | 776687 | 3101077 | 3131077 | 265904 | 2430404 | 2480404 | 3141077 | 80 | 25.00 | 64 | 54 | 74 | no | 1.000 | 1636 | 336 | 7198067 |
| win | declared | n/a | 200 | 95.7 | 48% | 8103465.0 | 14109838 | 14134838 | 7774477.5 | 13792358 | 13812358 | 14139838 | 32 | 62.50 | 64 | 2248917 | 32 | no | 1.000 | 1636 | 336 | 7198067 |
| win | declared | n/a | 400 | 86.9 | 22% | 23382479 | 31769853 | 31777353 | 23217241 | 31609404 | 31609404 | 31777353 | 32 | 62.50 | 64 | 13752396 | 32 | no | 1.000 | 1636 | 336 | 7198067 |
| win | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 39711.0 | 397477 | 417477 | 6681.5 | 191888 | 211888 | 417477 | 1358 | 1.47 | 11 | 4 | 864 | no | 1.000 | 1636 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 100 | 91.7 | 92% | 644039.5 | 2227303 | 2257303 | 212024.5 | 1607153 | 1607153 | 2267303 | 82 | 24.39 | 64 | 53 | 78 | no | 1.000 | 1636 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 200 | 91.3 | 46% | 7532631.5 | 13806075 | 13831075 | 7207881.5 | 13483736 | 13503736 | 13836075 | 32 | 62.50 | 64 | 1415441 | 32 | no | 1.000 | 1636 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 400 | 88.8 | 22% | 22816614 | 31039257 | 31046757 | 22649719 | 30877723 | 30877723 | 31046757 | 32 | 62.50 | 64 | 13447424 | 32 | no | 1.000 | 1636 | 336 | 2713071 |
| win | static-derived | n/a | 50 | 50.0 | 100% | 44460.5 | 271950 | 311950 | 6105.0 | 146280 | 186280 | 311950 | 1256 | 1.59 | 10 | 2 | 790 | no | 1.000 | 1636 | 336 | 13848365 |
| win | static-derived | n/a | 100 | 86.2 | 86% | 942972 | 3296239 | 3346239 | 317363 | 2664692 | 2704692 | 3356239 | 63 | 31.75 | 64 | 46 | 56 | no | 1.000 | 1636 | 336 | 13848365 |
| win | static-derived | n/a | 200 | 87.2 | 44% | 9130871.5 | 16188829 | 16213829 | 8801008.5 | 15862326 | 15882326 | 16218829 | 32 | 62.50 | 64 | 2529324 | 32 | no | 1.000 | 1636 | 336 | 13848365 |
| win | static-derived | n/a | 400 | 84.0 | 21% | 25744455 | 34690550 | 34698050 | 25574592 | 34528060 | 34528060 | 34698050 | 32 | 62.50 | 64 | 15839447 | 32 | no | 1.000 | 1636 | 336 | 13848365 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **488** (`adversarial/declared` 74, `adversarial/learned@1.0` 69, `adversarial/static-derived` 74, `win/declared` 90, `win/learned@1.0` 91, `win/static-derived` 90); windows the LAG ceiling rejected: **295** (`adversarial/declared` 7, `adversarial/learned@1.0` 7, `adversarial/static-derived` 10, `win/declared` 90, `win/learned@1.0` 91, `win/static-derived` 90). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| adversarial | declared | 50 | 9,704,772 |
| adversarial | declared | 100 | 8,968,215 |
| adversarial | declared | 200 | 2,978,987 |
| adversarial | declared | 400 | 0 |
| adversarial | learned@1.0 | 50 | 2,887,806 |
| adversarial | learned@1.0 | 100 | 0 |
| adversarial | learned@1.0 | 200 | 0 |
| adversarial | learned@1.0 | 400 | 0 |
| adversarial | static-derived | 50 | 11,719,100 |
| adversarial | static-derived | 100 | 15,311,300 |
| adversarial | static-derived | 200 | 8,287,538 |
| adversarial | static-derived | 400 | 0 |
| win | declared | 50 | 6,404,991 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 793,076 |
| win | learned@1.0 | 50 | 2,713,071 |
| win | learned@1.0 | 100 | 0 |
| win | learned@1.0 | 200 | 0 |
| win | learned@1.0 | 400 | 0 |
| win | static-derived | 50 | 12,189,515 |
| win | static-derived | 100 | 0 |
| win | static-derived | 200 | 0 |
| win | static-derived | 400 | 1,658,850 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| adversarial | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78716928 | yes |
| adversarial | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 79683584 | yes |
| adversarial | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78528512 | yes |
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77139968 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77217792 | yes |
| win | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77676544 | yes |

## Per-region telemetry (summed over the run)

### adversarial/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 130,240 | 0 | 0 | 0.0000 |
| P|Customer | 75 | 23,656,980 | 0 | 0 | 0 | n/a |
| P|Order | 75 | 0 | 184,679 | 2,149,037 | 2 | 11.6366 |
| P|Stock | 75 | 55,213,751 | 112,332 | 3,881,780 | 5 | 34.5563 |
| P|StockMovement | 75 | 0 | 221,408 | 2,547,913 | 2 | 11.5078 |
| R|Order | 75 | 0 | 56,980 | 3,761,070 | 5 | 66.0068 |
| R|StockMovement | 75 | 0 | 70,004 | 2,695,158 | 4 | 38.5001 |
| U|Backorder | 75 | 0 | 76,516 | 0 | 0 | 0.0000 |
| U|Customer | 75 | 23,548,700 | 0 | 0 | 0 | n/a |
| U|Order | 75 | 0 | 70,004 | 3,032,766 | 4 | 43.3228 |
| U|Stock | 75 | 23,591,564 | 0 | 0 | 0 | n/a |
| U|StockMovement | 75 | 0 | 87,912 | 3,584,250 | 4 | 40.7709 |

### adversarial/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 130,240 | 0 | 0 | 0.0000 |
| P|Customer | 75 | 23,656,980 | 0 | 0 | 0 | n/a |
| P|Order | 75 | 0 | 184,679 | 0 | 0 | 0.0000 |
| P|Stock | 75 | 586,903,889 | 112,332 | 1,706,457 | 3 | 15.1912 |
| P|StockMovement | 75 | 0 | 221,408 | 0 | 0 | 0.0000 |
| R|Order | 75 | 0 | 56,980 | 323,715 | 1 | 5.6812 |
| R|StockMovement | 75 | 0 | 70,004 | 430,859 | 1 | 6.1548 |
| U|Backorder | 75 | 0 | 76,516 | 0 | 0 | 0.0000 |
| U|Customer | 75 | 23,548,700 | 0 | 0 | 0 | n/a |
| U|Order | 75 | 0 | 70,004 | 426,775 | 1 | 6.0964 |
| U|Stock | 75 | 23,591,564 | 0 | 0 | 0 | n/a |
| U|StockMovement | 75 | 0 | 87,912 | 0 | 0 | 0.0000 |

### adversarial/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 75 | 0 | 130,240 | 0 | 0 | 0.0000 |
| P|Customer | 75 | 23,656,980 | 0 | 0 | 0 | n/a |
| P|Order | 75 | 0 | 184,679 | 9,734,566 | 5 | 52.7107 |
| P|Stock | 75 | 110,623,179 | 112,332 | 0 | 0 | 0.0000 |
| P|StockMovement | 75 | 0 | 221,408 | 11,545,768 | 5 | 52.1470 |
| R|Order | 75 | 0 | 56,980 | 3,761,070 | 5 | 66.0068 |
| R|StockMovement | 75 | 0 | 70,004 | 4,409,650 | 5 | 62.9914 |
| U|Backorder | 75 | 0 | 76,516 | 0 | 0 | 0.0000 |
| U|Customer | 75 | 23,548,700 | 0 | 0 | 0 | n/a |
| U|Order | 75 | 0 | 70,004 | 0 | 0 | 0.0000 |
| U|Stock | 75 | 23,591,564 | 0 | 0 | 0 | n/a |
| U|StockMovement | 75 | 0 | 87,912 | 5,866,884 | 5 | 66.7359 |

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 130,880 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 16,986,231 | 0 | 0 | 0 | n/a |
| P|Order | 90 | 0 | 185,585 | 0 | 0 | 0.0000 |
| P|Stock | 90 | 60,736,657 | 112,884 | 1,602,041 | 3 | 14.1919 |
| P|StockMovement | 90 | 10,553,452,477 | 222,496 | 0 | 0 | 0.0000 |
| R|Order | 90 | 0 | 57,260 | 1,473,438 | 3 | 25.7324 |
| R|StockMovement | 90 | 0 | 70,348 | 1,726,908 | 3 | 24.5481 |
| U|Backorder | 90 | 0 | 76,892 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,920,442 | 0 | 0 | 0 | n/a |
| U|Order | 90 | 0 | 70,348 | 1,098,365 | 2 | 15.6133 |
| U|Stock | 90 | 20,218,680 | 0 | 0 | 0 | n/a |
| U|StockMovement | 90 | 0 | 88,344 | 1,297,315 | 2 | 14.6848 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 91 | 0 | 130,880 | 0 | 0 | 0.0000 |
| P|Customer | 91 | 16,986,231 | 0 | 0 | 0 | n/a |
| P|Order | 91 | 0 | 185,585 | 0 | 0 | 0.0000 |
| P|Stock | 91 | 254,539,874 | 112,884 | 977,093 | 2 | 8.6557 |
| P|StockMovement | 91 | 10,553,452,477 | 222,496 | 0 | 0 | 0.0000 |
| R|Order | 91 | 0 | 57,260 | 323,715 | 1 | 5.6534 |
| R|StockMovement | 91 | 0 | 70,348 | 430,859 | 1 | 6.1247 |
| U|Backorder | 91 | 0 | 76,892 | 0 | 0 | 0.0000 |
| U|Customer | 91 | 16,920,442 | 0 | 0 | 0 | n/a |
| U|Order | 91 | 0 | 70,348 | 477,165 | 1 | 6.7829 |
| U|Stock | 91 | 20,218,680 | 0 | 0 | 0 | n/a |
| U|StockMovement | 91 | 0 | 88,344 | 504,239 | 1 | 5.7077 |

### win/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 130,880 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 16,986,231 | 0 | 0 | 0 | n/a |
| P|Order | 90 | 0 | 185,585 | 3,807,887 | 3 | 20.5183 |
| P|Stock | 90 | 89,788,753 | 112,884 | 0 | 0 | 0.0000 |
| P|StockMovement | 90 | 10,553,452,477 | 222,496 | 0 | 0 | 0.0000 |
| R|Order | 90 | 0 | 57,260 | 3,761,070 | 5 | 65.6841 |
| R|StockMovement | 90 | 0 | 70,348 | 2,695,158 | 4 | 38.3118 |
| U|Backorder | 90 | 0 | 76,892 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,920,442 | 0 | 0 | 0 | n/a |
| U|Order | 90 | 0 | 70,348 | 0 | 0 | 0.0000 |
| U|Stock | 90 | 20,218,680 | 0 | 0 | 0 | n/a |
| U|StockMovement | 90 | 0 | 88,344 | 3,584,250 | 4 | 40.5715 |

## Learner decisions and drift

### adversarial/learned@1.0

- gated switches: {'U|Customer': 1, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 1, 'P|Stock': 0, 'P|Backorder': 2, 'U|StockMovement': 2, 'P|StockMovement': 2, 'R|StockMovement': 2, 'P|Order': 2, 'R|Order': 2}
- drift restarts: {'U|Customer': 2, 'P|Customer': 1, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 2, 'P|Stock': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Customer': 6, 'P|Customer': 6, 'U|Stock': 6}
- first policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (16): ["w0: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w63: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w64: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w65: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w66: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w67: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w68: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w69: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w70: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w71: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w74: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

### adversarial/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}
- policy changes (1): ["w0: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

### win/learned@1.0

- gated switches: {'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 0, 'P|Stock': 0, 'P|Backorder': 4, 'U|StockMovement': 4, 'P|StockMovement': 0, 'R|StockMovement': 3, 'P|Order': 4, 'R|Order': 3}
- drift restarts: {'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 0, 'P|Stock': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {}
- first policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (15): ["w0: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w66: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w68: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w71: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w75: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w78: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w82: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w83: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w85: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w87: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}

### win/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}
- policy changes (1): ["w0: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **NOT-MEASURABLE** | declared sustained rate 0.0, learned 0.0: no sustained level to compare (the box could not complete even the reference level) |
| T2 learned loses <= 5% on adversarial-uniform | 1.0 | **PASS** | learned loses +0.0% vs declared on adversarial-uniform (declared sustained 0.0, learned 0.0) |
| T3 reorganisation <= 3x declared | 1.0 | **PASS** | learned reorg 2713071 B vs declared 7198067 B (ratio 0.38) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
