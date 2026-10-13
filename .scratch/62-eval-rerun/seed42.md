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
| adversarial/declared | 7.369e+09 | 1.124e+06 | 2.872e+07 | 0.015% | 0.390% |
| adversarial/learned@1.0 | 9.856e+09 | 9.791e+05 | 7.202e+07 | 0.010% | 0.731% |
| adversarial/static-derived | 7.191e+09 | 1.124e+06 | 3.548e+07 | 0.016% | 0.493% |
| win/declared | 9.483e+10 | 1.106e+06 | 9.209e+06 | 0.001% | 0.010% |
| win/learned@1.0 | 9.683e+10 | 9.637e+05 | 2.209e+06 | 0.001% | 0.002% |
| win/static-derived | 9.988e+10 | 1.106e+06 | 1.199e+07 | 0.001% | 0.012% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| adversarial/learned@1.0 | 7 | 5 | 0 | 14 | 72,024,270 |
| win/learned@1.0 | 0 | 8 | 0 | 12 | 2,208,832 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| adversarial | declared | n/a | 50 | 50.0 | 100% | 553.0 | 38109 | 9903 | 37.0 | 4155 | 12557 | 123044 | 2000 | 1.00 | 1 | 2 | 107 | no | 1.000 | 1578 | 336 | 25025316 |
| adversarial | declared | n/a | 100 | 100.0 | 100% | 502.0 | 27399 | 9045 | 43.0 | 1161 | 21074 | 66635 | 1997 | 1.00 | 4 | 0 | 134 | no | 1.000 | 1578 | 336 | 25025316 |
| adversarial | declared | n/a | 200 | 200.0 | 100% | 2664.0 | 28102 | 47451 | 37.5 | 4191 | 19191 | 56753 | 1973 | 1.01 | 6 | 1 | 321 | no | 1.000 | 1578 | 336 | 25025316 |
| adversarial | declared | n/a | 400 | 377.9 | 94% | 8410.0 | 48822 | 70868 | 634.0 | 266022 | 271022 | 73891 | 1465 | 1.37 | 53 | 3 | 516 | no | 1.000 | 1578 | 336 | 25025316 |
| adversarial | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 553.0 | 140807 | 9476 | 72.0 | 71727 | 111727 | 190555 | 1995 | 1.00 | 6 | 0 | 104 | no | 1.000 | 1578 | 336 | 72024270 |
| adversarial | learned@1.0 | 1.0 | 100 | 100.0 | 100% | 1062.0 | 127105 | 164935 | 72.5 | 54394 | 91161 | 174935 | 1917 | 1.04 | 11 | 1 | 172 | no | 1.000 | 1578 | 336 | 72024270 |
| adversarial | learned@1.0 | 1.0 | 200 | 200.0 | 100% | 3579.0 | 120742 | 154059 | 76.0 | 75915 | 120915 | 160626 | 1877 | 1.07 | 27 | 3 | 278 | no | 1.000 | 1578 | 336 | 72024270 |
| adversarial | learned@1.0 | 1.0 | 400 | 382.4 | 96% | 9554.5 | 38134 | 50589 | 882.5 | 201766 | 206766 | 56205 | 1439 | 1.39 | 51 | 2 | 517 | no | 1.000 | 1578 | 336 | 72024270 |
| adversarial | static-derived | n/a | 50 | 50.0 | 100% | 507.5 | 128789 | 9777 | 54.5 | 122 | 5535 | 203716 | 2000 | 1.00 | 1 | 0 | 98 | no | 1.000 | 1578 | 336 | 30912180 |
| adversarial | static-derived | n/a | 100 | 100.0 | 100% | 517.0 | 89485 | 9834 | 59.5 | 2098 | 21776 | 164502 | 1997 | 1.00 | 4 | 1 | 124 | no | 1.000 | 1578 | 336 | 30912180 |
| adversarial | static-derived | n/a | 200 | 199.9 | 100% | 2033.5 | 42181 | 57181 | 59.5 | 17074 | 37074 | 60087 | 1959 | 1.02 | 10 | 1 | 253 | no | 1.000 | 1578 | 336 | 30912180 |
| adversarial | static-derived | n/a | 400 | 378.5 | 95% | 8460.5 | 239210 | 266710 | 756.0 | 258307 | 263307 | 274210 | 1411 | 1.42 | 64 | 2 | 489 | no | 1.000 | 1578 | 336 | 30912180 |
| win | declared | n/a | 50 | 50.0 | 100% | 37728.0 | 400080 | 420080 | 3751.5 | 182336 | 222336 | 420080 | 1368 | 1.46 | 12 | 0 | 826 | no | 1.000 | 1554 | 336 | 8024075 |
| win | declared | n/a | 100 | 90.6 | 91% | 1317724 | 2492826 | 2522826 | 669634 | 1862552 | 1872552 | 2522826 | 46 | 43.48 | 64 | 68 | 41 | no | 1.000 | 1554 | 336 | 8024075 |
| win | declared | n/a | 200 | 86.8 | 43% | 7820050 | 15236528 | 15261528 | 7495919 | 14916353 | 14936353 | 15266528 | 32 | 62.50 | 64 | 1539529 | 32 | no | 1.000 | 1554 | 336 | 8024075 |
| win | declared | n/a | 400 | 83.1 | 21% | 23162931 | 34130774 | 34143274 | 22993546 | 33964505 | 33974505 | 34145774 | 32 | 62.50 | 64 | 14824393 | 32 | no | 1.000 | 1554 | 336 | 8024075 |
| win | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 37775.0 | 179397 | 219397 | 4747.5 | 91699 | 131699 | 219397 | 1405 | 1.42 | 7 | 1 | 847 | no | 1.000 | 1554 | 336 | 2208832 |
| win | learned@1.0 | 1.0 | 100 | 87.6 | 88% | 1524572 | 2724042 | 2774042 | 873914 | 2073852 | 2123852 | 2784042 | 44 | 45.45 | 64 | 55 | 39 | no | 1.000 | 1554 | 336 | 2208832 |
| win | learned@1.0 | 1.0 | 200 | 81.1 | 41% | 8974490 | 17654464 | 17679464 | 8650162 | 17334168 | 17354168 | 17684464 | 32 | 62.50 | 64 | 1897205 | 32 | no | 1.000 | 1554 | 336 | 2208832 |
| win | learned@1.0 | 1.0 | 400 | 82.8 | 21% | 26075168 | 36650186 | 36657686 | 25911395 | 36487385 | 36487385 | 36657686 | 32 | 62.50 | 64 | 17247094 | 32 | no | 1.000 | 1554 | 336 | 2208832 |
| win | static-derived | n/a | 50 | 50.0 | 100% | 37421.0 | 297367 | 320487 | 3919.0 | 134031 | 174031 | 320487 | 1324 | 1.51 | 9 | 2 | 797 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 100 | 81.4 | 81% | 2384044 | 4196857 | 4226857 | 1739249 | 3552445 | 3582445 | 4226857 | 44 | 45.45 | 64 | 6 | 40 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 200 | 83.3 | 42% | 10591724 | 18655510 | 18680510 | 10267253 | 18334191 | 18354191 | 18685510 | 32 | 62.50 | 64 | 3510222 | 32 | no | 1.000 | 1554 | 336 | 10447055 |
| win | static-derived | n/a | 400 | 79.0 | 20% | 27098222 | 38784321 | 38796821 | 26928932 | 38618625 | 38628625 | 38799321 | 32 | 62.50 | 64 | 18248498 | 32 | no | 1.000 | 1554 | 336 | 10447055 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **482** (`adversarial/declared` 70, `adversarial/learned@1.0` 70, `adversarial/static-derived` 69, `win/declared` 91, `win/learned@1.0` 91, `win/static-derived` 91); windows the LAG ceiling rejected: **308** (`adversarial/declared` 8, `adversarial/learned@1.0` 20, `adversarial/static-derived` 8, `win/declared` 91, `win/learned@1.0` 91, `win/static-derived` 90). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| adversarial | declared | 50 | 10,562,228 |
| adversarial | declared | 100 | 8,110,759 |
| adversarial | declared | 200 | 4,637,837 |
| adversarial | declared | 400 | 1,714,492 |
| adversarial | learned@1.0 | 50 | 4,368,151 |
| adversarial | learned@1.0 | 100 | 52,000,231 |
| adversarial | learned@1.0 | 200 | 15,655,888 |
| adversarial | learned@1.0 | 400 | 0 |
| adversarial | static-derived | 50 | 14,178,597 |
| adversarial | static-derived | 100 | 9,781,705 |
| adversarial | static-derived | 200 | 4,418,623 |
| adversarial | static-derived | 400 | 2,533,255 |
| win | declared | 50 | 8,024,075 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 0 |
| win | learned@1.0 | 50 | 2,208,832 |
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
| adversarial | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77922304 | yes |
| adversarial | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 78299136 | yes |
| adversarial | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77901824 | yes |
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 76754944 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77156352 | yes |
| win | static-derived | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77008896 | yes |

## Per-region telemetry (summed over the run)

### adversarial/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 3,807,887 | 3 | 21.2691 |
| P|Stock | 76 | 54,064,108 | 108,882 | 3,881,780 | 5 | 35.6513 |
| P|StockMovement | 76 | 0 | 214,608 | 2,547,913 | 2 | 11.8724 |
| R|Order | 76 | 0 | 55,230 | 3,761,070 | 5 | 68.0983 |
| R|StockMovement | 76 | 0 | 67,854 | 4,409,650 | 5 | 64.9873 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 3,032,766 | 4 | 44.6955 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 3,584,250 | 4 | 42.0627 |

### adversarial/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,489,944 | 0 | 62,427,704 | 6 | n/a |
| P|Order | 76 | 0 | 179,034 | 0 | 0 | 0.0000 |
| P|Stock | 76 | 350,930,526 | 108,882 | 2,682,563 | 4 | 24.6373 |
| P|StockMovement | 76 | 0 | 214,608 | 0 | 0 | 0.0000 |
| R|Order | 76 | 0 | 55,230 | 323,715 | 1 | 5.8612 |
| R|StockMovement | 76 | 0 | 67,854 | 430,859 | 1 | 6.3498 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 426,775 | 1 | 6.2896 |
| U|Stock | 76 | 23,384,662 | 0 | 5,228,415 | 1 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 504,239 | 1 | 5.9175 |

### adversarial/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 76 | 0 | 126,240 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 23,494,076 | 0 | 0 | 0 | n/a |
| P|Order | 76 | 0 | 179,034 | 5,943,876 | 4 | 33.1997 |
| P|Stock | 76 | 48,696,198 | 108,882 | 3,881,780 | 5 | 35.6513 |
| P|StockMovement | 76 | 0 | 214,608 | 7,048,920 | 4 | 32.8456 |
| R|Order | 76 | 0 | 55,230 | 3,761,070 | 5 | 68.0983 |
| R|StockMovement | 76 | 0 | 67,854 | 4,409,650 | 5 | 64.9873 |
| U|Backorder | 76 | 0 | 74,166 | 0 | 0 | 0.0000 |
| U|Customer | 76 | 23,410,991 | 0 | 0 | 0 | n/a |
| U|Order | 76 | 0 | 67,854 | 0 | 0 | 0.0000 |
| U|Stock | 76 | 23,378,868 | 0 | 0 | 0 | n/a |
| U|StockMovement | 76 | 0 | 85,212 | 5,866,884 | 5 | 68.8504 |

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 91 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 91 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 91 | 0 | 176,195 | 0 | 0 | 0.0000 |
| P|Stock | 91 | 60,470,036 | 107,157 | 1,602,041 | 3 | 14.9504 |
| P|StockMovement | 91 | 10,858,394,372 | 211,208 | 0 | 0 | 0.0000 |
| R|Order | 91 | 0 | 54,355 | 2,299,446 | 4 | 42.3042 |
| R|StockMovement | 91 | 0 | 66,779 | 1,726,908 | 3 | 25.8600 |
| U|Backorder | 91 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 91 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 91 | 0 | 66,779 | 1,098,365 | 2 | 16.4478 |
| U|Stock | 91 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 91 | 0 | 83,862 | 1,297,315 | 2 | 15.4696 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 91 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 91 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 91 | 0 | 176,195 | 0 | 0 | 0.0000 |
| P|Stock | 91 | 261,132,556 | 107,157 | 977,093 | 2 | 9.1183 |
| P|StockMovement | 91 | 10,858,394,372 | 211,208 | 0 | 0 | 0.0000 |
| R|Order | 91 | 0 | 54,355 | 323,715 | 1 | 5.9556 |
| R|StockMovement | 91 | 0 | 66,779 | 430,859 | 1 | 6.4520 |
| U|Backorder | 91 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 91 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 91 | 0 | 66,779 | 477,165 | 1 | 7.1454 |
| U|Stock | 91 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 91 | 0 | 83,862 | 0 | 0 | 0.0000 |

### win/static-derived

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 91 | 0 | 124,320 | 0 | 0 | 0.0000 |
| P|Customer | 91 | 16,509,229 | 0 | 0 | 0 | n/a |
| P|Order | 91 | 0 | 176,195 | 835,904 | 1 | 4.7442 |
| P|Stock | 91 | 90,458,331 | 107,157 | 0 | 0 | 0.0000 |
| P|StockMovement | 91 | 10,838,704,082 | 211,208 | 1,032,297 | 1 | 4.8876 |
| R|Order | 91 | 0 | 54,355 | 2,299,446 | 4 | 42.3042 |
| R|StockMovement | 91 | 0 | 66,779 | 2,695,158 | 4 | 40.3594 |
| U|Backorder | 91 | 0 | 73,038 | 0 | 0 | 0.0000 |
| U|Customer | 91 | 16,372,225 | 0 | 0 | 0 | n/a |
| U|Order | 91 | 0 | 66,779 | 0 | 0 | 0.0000 |
| U|Stock | 91 | 19,810,051 | 0 | 0 | 0 | n/a |
| U|StockMovement | 91 | 0 | 83,862 | 3,584,250 | 4 | 42.7399 |

## Learner decisions and drift

### adversarial/learned@1.0

- gated switches: {'U|Stock': 1, 'P|Stock': 0, 'U|Customer': 1, 'P|Customer': 3, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- drift restarts: {'U|Stock': 2, 'P|Stock': 0, 'U|Customer': 3, 'P|Customer': 2, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {'U|Stock': 6, 'U|Customer': 6, 'P|Customer': 6}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (19): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w43: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w45: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w46: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w48: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w49: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w62: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w63: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w65: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w68: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w69: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w72: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w73: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w75: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

### adversarial/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}
- policy changes (1): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}

### win/learned@1.0

- gated switches: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 2, 'P|StockMovement': 0, 'U|StockMovement': 2, 'R|StockMovement': 0, 'P|Order': 2, 'R|Order': 2}
- drift restarts: {'U|Stock': 0, 'P|Stock': 0, 'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'P|Backorder': 0, 'P|StockMovement': 0, 'U|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (8): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w79: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w87: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}

### win/static-derived

- gated switches: None
- drift restarts: None
- recovery windows: None
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}
- policy changes (1): ["w0: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}"]
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **NOT-MEASURABLE** | declared sustained rate 0.0, learned 0.0: no sustained level to compare (the box could not complete even the reference level) |
| T2 learned loses <= 5% on adversarial-uniform | 1.0 | **PASS** | learned loses +0.0% vs declared on adversarial-uniform (declared sustained 0.0, learned 0.0) |
| T3 reorganisation <= 3x declared | 1.0 | **PASS** | learned reorg 2208832 B vs declared 8024075 B (ratio 0.28) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
