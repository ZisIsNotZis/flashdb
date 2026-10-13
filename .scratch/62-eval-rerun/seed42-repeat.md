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
| adversarial/declared | 7.026e+09 | 1.124e+06 | 3.319e+07 | 0.016% | 0.472% |
| adversarial/learned@1.0 | 9.055e+09 | 9.791e+05 | 6.036e+07 | 0.011% | 0.667% |
| adversarial/static-derived | 6.626e+09 | 1.124e+06 | 3.359e+07 | 0.017% | 0.507% |
| win/declared | 1.091e+11 | 1.106e+06 | 8.261e+06 | 0.001% | 0.008% |
| win/learned@1.0 | 1.083e+11 | 9.637e+05 | 2.713e+06 | 0.001% | 0.003% |
| win/static-derived | 1.065e+11 | 1.106e+06 | 1.199e+07 | 0.001% | 0.011% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| adversarial/learned@1.0 | 9 | 6 | 0 | 15 | 60,364,318 |
| win/learned@1.0 | 0 | 0 | 0 | 12 | 2,713,071 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| adversarial | declared | n/a | 50 | 50.0 | 100% | 511.5 | 74759 | 8619 | 62.0 | 1766 | 14999 | 218445 | 2000 | 1.00 | 1 | 1 | 96 | no | 1.000 | 1578 | 336 | 28923320 |
| adversarial | declared | n/a | 100 | 100.0 | 100% | 436.5 | 32550 | 8986 | 63.0 | 2885 | 23883 | 101817 | 1993 | 1.00 | 4 | 0 | 125 | no | 1.000 | 1578 | 336 | 28923320 |
| adversarial | declared | n/a | 200 | 200.0 | 100% | 2496.0 | 26518 | 40607 | 65.0 | 3704 | 13624 | 50436 | 1973 | 1.01 | 4 | 0 | 250 | no | 1.000 | 1578 | 336 | 28923320 |
| adversarial | declared | n/a | 400 | 379.5 | 95% | 7636.0 | 121705 | 136502 | 445.0 | 237689 | 242689 | 144764 | 1487 | 1.34 | 53 | 5 | 470 | no | 1.000 | 1578 | 336 | 28923320 |
| adversarial | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 486.5 | 80636 | 9624 | 63.0 | 366 | 3478 | 128898 | 2000 | 1.00 | 1 | 1 | 93 | no | 1.000 | 1578 | 336 | 60364318 |
| adversarial | learned@1.0 | 1.0 | 100 | 100.0 | 100% | 845.5 | 137467 | 177467 | 66.0 | 55693 | 86090 | 187467 | 1970 | 1.02 | 10 | 0 | 155 | no | 1.000 | 1578 | 336 | 60364318 |
| adversarial | learned@1.0 | 1.0 | 200 | 200.0 | 100% | 3274.0 | 219513 | 248405 | 66.5 | 132426 | 172426 | 253405 | 1866 | 1.07 | 36 | 0 | 264 | no | 1.000 | 1578 | 336 | 60364318 |
| adversarial | learned@1.0 | 1.0 | 400 | 378.3 | 95% | 7977.0 | 156365 | 206365 | 711.5 | 238122 | 243122 | 216365 | 1383 | 1.45 | 55 | 1 | 445 | no | 1.000 | 1578 | 336 | 60364318 |
| adversarial | static-derived | n/a | 50 | 50.0 | 100% | 464.5 | 53067 | 9790 | 59.0 | 1094 | 4020 | 140529 | 2000 | 1.00 | 1 | 0 | 103 | no | 1.000 | 1578 | 336 | 29265545 |
| adversarial | static-derived | n/a | 100 | 100.0 | 100% | 426.5 | 63289 | 125877 | 60.0 | 2468 | 33215 | 129558 | 1996 | 1.00 | 5 | 1 | 123 | no | 1.000 | 1578 | 336 | 29265545 |
| adversarial | static-derived | n/a | 200 | 200.0 | 100% | 1839.5 | 21655 | 33844 | 62.0 | 3015 | 9621 | 40464 | 1986 | 1.01 | 3 | 1 | 254 | no | 1.000 | 1578 | 336 | 29265545 |
| adversarial | static-derived | n/a | 400 | 379.6 | 95% | 7580.0 | 132647 | 160147 | 639.5 | 237285 | 242285 | 170147 | 1449 | 1.38 | 53 | 2 | 457 | no | 1.000 | 1578 | 336 | 29265545 |
| win | declared | n/a | 50 | 50.0 | 100% | 45068.0 | 312690 | 0 | 8625.0 | 189911 | 229911 | 332690 | 1241 | 1.61 | 12 | 0 | 760 | no | 1.000 | 1554 | 336 | 7198067 |
| win | declared | n/a | 100 | 76.4 | 76% | 2979582 | 5691887 | 5721887 | 2333014 | 5052981 | 5072981 | 5721887 | 47 | 42.55 | 64 | 9 | 41 | no | 1.000 | 1554 | 336 | 7198067 |
| win | declared | n/a | 200 | 73.3 | 37% | 13641673 | 23548110 | 23573110 | 13316724 | 23228029 | 23248029 | 23578110 | 32 | 62.50 | 64 | 5177618 | 32 | no | 1.000 | 1554 | 336 | 7198067 |
| win | declared | n/a | 400 | 69.4 | 17% | 35345715 | 47171310 | 47183810 | 35175671 | 47006159 | 47016159 | 47186310 | 32 | 62.50 | 64 | 23139614 | 32 | no | 1.000 | 1554 | 336 | 7198067 |
| win | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 46768.0 | 282981 | 7830 | 7833.5 | 136283 | 176283 | 342981 | 1248 | 1.60 | 9 | 0 | 757 | no | 1.000 | 1554 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 100 | 76.1 | 76% | 2943161 | 5884828 | 5914828 | 2298246 | 5248339 | 5268339 | 5914828 | 44 | 45.45 | 64 | 57 | 39 | no | 1.000 | 1554 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 200 | 73.1 | 37% | 14134033 | 23728640 | 23753640 | 13808192 | 23412441 | 23432441 | 23758640 | 32 | 62.50 | 64 | 5295031 | 32 | no | 1.000 | 1554 | 336 | 2713071 |
| win | learned@1.0 | 1.0 | 400 | 71.8 | 18% | 34059364.0 | 46399597 | 46412097 | 33892992.0 | 46239427 | 46249427 | 46414597 | 32 | 62.50 | 64 | 23323330 | 32 | no | 1.000 | 1554 | 336 | 2713071 |
| win | static-derived | n/a | 50 | 50.0 | 100% | 47598.0 | 376322 | 2172 | 11318.5 | 187725 | 227725 | 416322 | 1165 | 1.72 | 12 | 1 | 728 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 100 | 78.4 | 78% | 3030893 | 5674854 | 5704854 | 2381774 | 5051143 | 5061143 | 5704854 | 46 | 43.48 | 64 | 38 | 40 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 200 | 75.0 | 38% | 12889728 | 22272325 | 22297325 | 12563944 | 21952192 | 21972192 | 22302325 | 32 | 62.50 | 64 | 4834276 | 32 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 400 | 74.8 | 19% | 32101565.0 | 43833562 | 43846062 | 31934892.5 | 43673011 | 43683011 | 43848562 | 32 | 62.50 | 64 | 21861590 | 32 | no | 1.000 | 1554 | 336 | 10447055 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **495** (`adversarial/declared` 70, `adversarial/learned@1.0` 72, `adversarial/static-derived` 71, `win/declared` 94, `win/learned@1.0` 95, `win/static-derived` 93); windows the LAG ceiling rejected: **310** (`adversarial/declared` 9, `adversarial/learned@1.0` 13, `adversarial/static-derived` 6, `win/declared` 94, `win/learned@1.0` 95, `win/static-derived` 93). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| adversarial | declared | 50 | 12,521,687 |
| adversarial | declared | 100 | 9,130,287 |
| adversarial | declared | 200 | 5,303,594 |
| adversarial | declared | 400 | 1,967,752 |
| adversarial | learned@1.0 | 50 | 3,863,912 |
| adversarial | learned@1.0 | 100 | 30,766,017 |
| adversarial | learned@1.0 | 200 | 15,434,162 |
| adversarial | learned@1.0 | 400 | 10,300,227 |
| adversarial | static-derived | 50 | 12,651,199 |
| adversarial | static-derived | 100 | 10,990,870 |
| adversarial | static-derived | 200 | 3,655,724 |
| adversarial | static-derived | 400 | 1,967,752 |
| win | declared | 50 | 7,198,067 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 0 |
| win | learned@1.0 | 50 | 2,713,071 |
| win | learned@1.0 | 100 | 0 |
| win | learned@1.0 | 200 | 0 |
| win | learned@1.0 | 400 | 0 |
| win | static-derived | 50 | 10,447,055 |
| win | static-derived | 100 | 0 |
| win | static-derived | 200 | 0 |
| win | static-derived | 400 | 0 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| adversarial | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77795328 | yes |
| adversarial | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78991360 | yes |
| adversarial | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77590528 | yes |
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 76832768 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77225984 | yes |
| win | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77201408 | yes |

## Per-region telemetry (summed over the run)

### adversarial/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 3,807,887 | 3 | 21.2691 |
| P|Stock | 76 | 52,529,233 | 108,882 | 3,881,780 | 5 | 35.6513 |
| P|StockMovement | 76 | 0 | 214,608 | 4,515,665 | 3 | 21.0415 |
| R|Order | 76 | 0 | 55,230 | 3,761,070 | 5 | 68.0983 |
| R|StockMovement | 76 | 0 | 67,854 | 4,409,650 | 5 | 64.9873 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 4,963,018 | 5 | 73.1426 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 3,584,250 | 4 | 42.0627 |

### adversarial/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 74,344,708 | 0 | 51,366,471 | 5 | n/a |
| P|Order | 76 | 0 | 179,034 | 0 | 0 | 0.0000 |
| P|Stock | 76 | 352,559,816 | 108,882 | 2,682,563 | 4 | 24.6373 |
| P|StockMovement | 76 | 0 | 214,608 | 0 | 0 | 0.0000 |
| R|Order | 76 | 0 | 55,230 | 323,715 | 1 | 5.8612 |
| R|StockMovement | 76 | 0 | 67,854 | 430,859 | 1 | 6.3498 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 426,775 | 1 | 6.2896 |
| U|Stock | 76 | 86,181,458 | 0 | 5,133,935 | 1 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 0 | 0 | 0.0000 |

### adversarial/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 3,807,887 | 3 | 21.2691 |
| P|Stock | 76 | 51,833,423 | 108,882 | 3,881,780 | 5 | 35.6513 |
| P|StockMovement | 76 | 0 | 214,608 | 4,515,665 | 3 | 21.0415 |
| R|Order | 76 | 0 | 55,230 | 3,761,070 | 5 | 68.0983 |
| R|StockMovement | 76 | 0 | 67,854 | 4,409,650 | 5 | 64.9873 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 5,305,243 | 5 | 78.1861 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 3,584,250 | 4 | 42.0627 |

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 94 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 95 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 95 | 0 | 176,195 | 0 | 0 | 0.0000 |
| P|Stock | 95 | 60,985,754 | 107,157 | 1,602,041 | 3 | 14.9504 |
| P|StockMovement | 95 | 10,858,394,372 | 211,208 | 0 | 0 | 0.0000 |
| R|Order | 95 | 0 | 54,355 | 1,473,438 | 3 | 27.1077 |
| R|StockMovement | 95 | 0 | 66,779 | 1,726,908 | 3 | 25.8600 |
| U|Backorder | 95 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 95 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 95 | 0 | 66,779 | 1,098,365 | 2 | 16.4478 |
| U|Stock | 95 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 95 | 0 | 83,862 | 1,297,315 | 2 | 15.4696 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 96 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 96 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 96 | 0 | 176,195 | 0 | 0 | 0.0000 |
| P|Stock | 96 | 259,532,843 | 107,157 | 977,093 | 2 | 9.1183 |
| P|StockMovement | 96 | 10,858,394,372 | 211,208 | 0 | 0 | 0.0000 |
| R|Order | 96 | 0 | 54,355 | 323,715 | 1 | 5.9556 |
| R|StockMovement | 96 | 0 | 66,779 | 430,859 | 1 | 6.4520 |
| U|Backorder | 96 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 96 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 96 | 0 | 66,779 | 477,165 | 1 | 7.1454 |
| U|Stock | 96 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 96 | 0 | 83,862 | 504,239 | 1 | 6.0127 |

### win/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 93 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 94 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 94 | 0 | 176,195 | 835,904 | 1 | 4.7442 |
| P|Stock | 94 | 90,458,331 | 107,157 | 0 | 0 | 0.0000 |
| P|StockMovement | 94 | 10,838,923,862 | 211,208 | 1,032,297 | 1 | 4.8876 |
| R|Order | 94 | 0 | 54,355 | 2,299,446 | 4 | 42.3042 |
| R|StockMovement | 94 | 0 | 66,779 | 2,695,158 | 4 | 40.3594 |
| U|Backorder | 94 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 94 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 94 | 0 | 66,779 | 0 | 0 | 0.0000 |
| U|Stock | 94 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 94 | 0 | 83,862 | 3,584,250 | 4 | 42.7399 |

## Learner decisions and drift

### adversarial/learned@1.0

- gated switches: {'U|Stock': 2, 'P|Stock': 0, 'U|Customer': 2, 'P|Customer': 2, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- drift restarts: {'U|Stock': 2, 'P|Stock': 1, 'U|Customer': 3, 'P|Customer': 3, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Stock': 6, 'P|Stock': 6, 'U|Customer': 6, 'P|Customer': 6}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (18): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w43: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w46: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w49: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w56: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w59: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w62: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w64: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w68: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w70: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w72: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w73: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w74: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

### adversarial/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}
- policy changes (1): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

### win/learned@1.0

- gated switches: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'P|StockMovement': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- drift restarts: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'P|StockMovement': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (6): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

### win/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}
- policy changes (1): ["w0: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

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
