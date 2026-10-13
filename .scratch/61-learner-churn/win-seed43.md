# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored reward, churn control)

Instance `/tmp/cold51/per_region-mixed`: data 1,643,925,811 B, cgroup cap 134,217,728 B, **ratio 12.25x**

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance weight in the learner's reward proxy (a policy choice, not a measurement); it is anchored to the device write cost in slice 39 and every verdict names it.

**Offered mix** (integer weights; every arm gets the same trace): `adversarial` `{'customer': 30, 'scan': 0, 'stock': 30, 'write': 40}`; `win` `{'customer': 10, 'scan': 30, 'stock': 20, 'write': 40}`

**Compaction actuation (Slice 37):** the driver calls the engine's existing `maybe_compact_shape` once per metrics window with a `67,108,864`-byte merge budget and the active per-region policy (so the learner's `merge_min_tiles` governs).

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `win` bracketed (at least one arm failed the top step).

**Reward units (Slice 39).** The learner's cost per region-window is `count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: 1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write time and lambda is the explicit endurance weight on top. The write term prices the reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.

| arm | service ns | ingest write term ns | reorg write term ns | ingest/service | reorg/service |
|---|---:|---:|---:|---:|---:|
| win/declared | 9.219e+10 | 1.165e+06 | 9.209e+06 | 0.001% | 0.010% |
| win/learned@1.0 | 9.211e+10 | 1.015e+06 | 2.836e+06 | 0.001% | 0.003% |

A write term whose ratio to the service term is a tiny fraction of a percent cannot move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only 4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.

**Learner churn control (Slice 39):** per-region cooldown 3 windows after a change; per-window reorganisation budget = the declared arm's observed rate x 1.0 (a region is refused a switch whose projected added reorganisation rate exceeds the remaining budget). An unexplored challenger projects 0 added bytes, so the bounded burn-in is never blocked.

| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |
|---|---:|---:|---:|---:|---:|
| win/learned@1.0 | 0 | 17 | 0 | 12 | 2,835,572 |

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | group us | groups | grp mean | grp max | min lag | grp>ceil | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| win | declared | n/a | 50 | 50.0 | 100% | 37564 | 217346 | 257346 | 5358 | 107739 | 130296 | 257346 | 1369 | 1.46 | 7 | 1 | 881 | no | 1.000 | 1636 | 336 | 8024075 |
| win | declared | n/a | 100 | 88.9 | 89% | 593716.5 | 2503617 | 2543617 | 193366.0 | 1856868 | 1896868 | 2553617 | 80 | 25.00 | 64 | 5 | 73 | no | 1.000 | 1636 | 336 | 8024075 |
| win | declared | n/a | 200 | 94.1 | 47% | 7688066.5 | 13831874 | 13856874 | 7361031.0 | 13515617 | 13535617 | 13861874 | 32 | 62.50 | 64 | 1652364 | 32 | no | 1.000 | 1636 | 336 | 8024075 |
| win | declared | n/a | 400 | 89.6 | 22% | 22529259 | 30853108 | 30860608 | 22354007 | 30690226 | 30690226 | 30860608 | 32 | 62.50 | 64 | 13456585 | 32 | no | 1.000 | 1636 | 336 | 8024075 |
| win | learned@1.0 | 1.0 | 50 | 50.0 | 100% | 37322 | 206285 | 241778 | 6636 | 98783 | 135521 | 241778 | 1358 | 1.47 | 7 | 0 | 848 | no | 1.000 | 1636 | 336 | 2835572 |
| win | learned@1.0 | 1.0 | 100 | 90.3 | 90% | 423552.5 | 2315063 | 2365063 | 141810.5 | 1684276 | 1724276 | 2375063 | 102 | 19.61 | 64 | 43 | 94 | no | 1.000 | 1636 | 336 | 2835572 |
| win | learned@1.0 | 1.0 | 200 | 93.7 | 47% | 7523081.5 | 13568597 | 13593597 | 7198571.0 | 13249095 | 13269095 | 13598597 | 32 | 62.50 | 64 | 1491517 | 32 | no | 1.000 | 1636 | 336 | 2835572 |
| win | learned@1.0 | 1.0 | 400 | 89.7 | 22% | 22646322 | 30560736 | 30568236 | 22481619 | 30393824 | 30393824 | 30568236 | 32 | 62.50 | 64 | 13202059 | 32 | no | 1.000 | 1636 | 336 | 2835572 |

`svc p50`/`svc p95`/`svc p99` are PER-REQUEST service-latency percentiles (group completion minus each request's OWN due time); `lag p50`/`lag p95`/`lag p99` are PER-REQUEST schedule-lag percentiles (issue minus due). `group us` is a separate diagnostic (the largest group completion minus its first member's due time); it bounds the per-request latencies and is never one of them. `groups` is the number of groups dispatched over the rung, `grp mean` their mean size (total requests / total groups) and `grp max` the largest group seen; `min lag` is the ABSOLUTE minimum per-request lag over the rung's windows (the percentiles are clamped at 0 and cannot show the floor); `grp>ceil` is how many groups' elapsed exceeded the service ceiling. `commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over the longer of its offered schedule span and its elapsed wall time (not a median of the per-window rate), so it cannot exceed the offered rate; a row is `sustained` when it is within 5% of the offered rate with no error and no window rejected by either ceiling (service latency or schedule lag).

**Latency ceilings (per window, named separately):** `--ceiling-us 10000` bounds the PER-REQUEST service-latency p99 and `--lag-ceiling-us 10000` bounds the PER-REQUEST schedule-lag p99. A window over either is rejected (`reject_reason` names which) and its rung cannot be scored sustained. Windows the SERVICE ceiling rejected: **180** (`win/declared` 90, `win/learned@1.0` 90); windows the LAG ceiling rejected: **180** (`win/declared` 90, `win/learned@1.0` 90). A run without a service ceiling is not scored.

## Compaction actuation (`compact_bytes`, driver field summed per offered level)

| workload | arm | offered | compact_bytes |
|---|---|---:|---:|
| win | declared | 50 | 7,198,067 |
| win | declared | 100 | 0 |
| win | declared | 200 | 0 |
| win | declared | 400 | 826,008 |
| win | learned@1.0 | 50 | 2,835,572 |
| win | learned@1.0 | 100 | 0 |
| win | learned@1.0 | 200 | 0 |
| win | learned@1.0 | 400 | 0 |

`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed over the windows of that offered level; the same merges are split by region in the telemetry section below.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| win | declared | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 77123584 | yes |
| win | learned@1.0 | 134217728 | 134217728 | 1.000 | 1643925811 | 12.25x | 76873728 | yes |

## Per-region telemetry (summed over the run)

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 130,880 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 16,986,231 | 0 | 0 | 0 | n/a |
| P|Order | 90 | 0 | 185,585 | 0 | 0 | 0.0000 |
| P|Stock | 90 | 60,323,264 | 112,884 | 1,602,041 | 3 | 14.1919 |
| P|StockMovement | 90 | 10,553,452,477 | 222,496 | 0 | 0 | 0.0000 |
| R|Order | 90 | 0 | 57,260 | 2,299,446 | 4 | 40.1580 |
| R|StockMovement | 90 | 0 | 70,348 | 1,726,908 | 3 | 24.5481 |
| U|Backorder | 90 | 0 | 76,892 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,920,442 | 0 | 0 | 0 | n/a |
| U|Order | 90 | 0 | 70,348 | 1,098,365 | 2 | 15.6133 |
| U|Stock | 90 | 20,218,680 | 0 | 0 | 0 | n/a |
| U|StockMovement | 90 | 0 | 88,344 | 1,297,315 | 2 | 14.6848 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 90 | 0 | 130,880 | 0 | 0 | 0.0000 |
| P|Customer | 90 | 16,986,231 | 0 | 0 | 0 | n/a |
| P|Order | 90 | 0 | 185,585 | 0 | 0 | 0.0000 |
| P|Stock | 90 | 241,085,780 | 112,884 | 1,654,223 | 3 | 14.6542 |
| P|StockMovement | 90 | 10,553,452,477 | 222,496 | 0 | 0 | 0.0000 |
| R|Order | 90 | 0 | 57,260 | 323,715 | 1 | 5.6534 |
| R|StockMovement | 90 | 0 | 70,348 | 430,859 | 1 | 6.1247 |
| U|Backorder | 90 | 0 | 76,892 | 0 | 0 | 0.0000 |
| U|Customer | 90 | 16,920,442 | 0 | 0 | 0 | n/a |
| U|Order | 90 | 0 | 70,348 | 426,775 | 1 | 6.0666 |
| U|Stock | 90 | 20,218,680 | 0 | 0 | 0 | n/a |
| U|StockMovement | 90 | 0 | 88,344 | 0 | 0 | 0.0000 |

## Learner decisions and drift

### win/learned@1.0

- gated switches: {'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 0, 'P|Stock': 0, 'P|Backorder': 4, 'U|StockMovement': 3, 'P|StockMovement': 0, 'R|StockMovement': 3, 'P|Order': 4, 'R|Order': 3}
- drift restarts: {'U|Customer': 0, 'P|Customer': 0, 'U|Order': 0, 'U|Backorder': 0, 'U|Stock': 0, 'P|Stock': 0, 'P|Backorder': 0, 'U|StockMovement': 0, 'P|StockMovement': 0, 'R|StockMovement': 0, 'P|Order': 0, 'R|Order': 0}
- recovery windows: {}
- first policy: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- policy changes (14): ["w0: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}", "w3: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w6: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w9: {'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w12: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)'}", "w15: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w63: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)'}", "w64: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w68: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)'}", "w69: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)'}", "w76: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w82: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w86: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}", "w87: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}"]
- final policy: {'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|StockMovement': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **NOT-MEASURABLE** | declared sustained rate 0.0, learned 0.0: no sustained level to compare (the box could not complete even the reference level) |
| T3 reorganisation <= 3x declared | 1.0 | **PASS** | learned reorg 2835572 B vs declared 8024075 B (ratio 0.35) |

## What is NOT concluded

- data:cap is 12.25x, below the 10x pre-registration floor
- compaction is actuated once per metrics window with a 67108864-byte merge budget, so the learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile count still never merges (a true 0), so the reorganisation comparison is bounded by the observed tile counts, not by the knob's full range
- per-region page size acts only on tiles written after the switch; the write share was raised to 40% so compaction and the write segment republish regions under the arm's active policy, but a policy switched late still acts only on what is written after it
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)
- the churn budget is a per-region projection (challenger's observed reorganisation rate minus the incumbent's); the engine chooses which region compacts globally, so deferral in one region can funnel merges into an eager neighbour (slice 37/38), which a per-region reward cannot see. The drift reorganisation result is therefore a bound under this control, not a proof that the learner is reorganisation-optimal

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
