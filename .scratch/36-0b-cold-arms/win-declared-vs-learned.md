# 0b - the cold arms at equal offered load (Slices 35/36)

Instance `/tmp/cold33/per_region-mixed`: data 1,647,135,638 B, cgroup cap 201,326,592 B, **ratio 8.18x**

> **WARNING: data:cap 8.18x is below the pre-registration 10x floor.** The instance was reused from slices 29/30 rather than rebuilt larger; every throughput verdict is qualified by this.

Objective (objective-v1): score = sustained throughput at the offered load, latency a constraint, efficiency reported. Offered levels are the reference rate x [1, 2, 4, 8]. `lambda` is the endurance price in the learner's reward proxy (a policy choice, not a measurement); every verdict names it.

**Noise band: not measured this run** (the `--noise-repeat` control did not run); differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) must be treated as noise-ambiguous.

**Ladder under-power guard** (per workload): `win` bracketed (at least one arm failed the top step).

## Per-arm capacity curve

| workload | arm | lambda | offered | achieved rps | achieved % | p50 us | p99 us | sustained | commit | else | active tiles | reorg bytes |
|---|---|---|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|
| win | declared | n/a | 50 | 64.0 | 128% | 741199.5 | 995438 | yes | 1.000 | 94 | 324 | 0 |
| win | declared | n/a | 100 | 128.0 | 128% | 633070 | 894410 | yes | 1.000 | 94 | 324 | 0 |
| win | declared | n/a | 200 | 128.0 | 64% | 686329.0 | 989919 | no | 1.000 | 94 | 324 | 0 |
| win | declared | n/a | 400 | 128.0 | 32% | 675217 | 894258 | no | 1.000 | 94 | 324 | 0 |
| win | learned@1.0 | 1.0 | 50 | 64.0 | 128% | 720719.5 | 913156 | yes | 1.000 | 94 | 324 | 0 |
| win | learned@1.0 | 1.0 | 100 | 128.0 | 128% | 629052 | 942013 | yes | 1.000 | 94 | 324 | 0 |
| win | learned@1.0 | 1.0 | 200 | 128.0 | 64% | 711653.5 | 1126218 | no | 1.000 | 94 | 324 | 0 |
| win | learned@1.0 | 1.0 | 400 | 128.0 | 32% | 690490.0 | 948382 | no | 1.000 | 94 | 324 | 0 |

`commit` is completed/requests (1.0 = every offered request committed); `else` is the stock guard's else/backorder branch count; `reorg bytes` is the summed per-region `compaction_bytes` over the run. A row is `sustained` when the median window completed >= 95% of the offered rate with no rejected window.

## Coldness evidence (per arm)

The cgroup counts page cache, so a run whose peak reached its cap has been under memory pressure and evicted the working set. It is reported per arm because warm runs on this box reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure signal.

| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | cache-evicting |
|---|---|---:|---:|---:|---:|---:|---:|:--:|
| win | declared | 201326592 | 201326592 | 1.000 | 1647135638 | 8.18x | 73838592 | yes |
| win | learned@1.0 | 201326592 | 201326592 | 1.000 | 1647135638 | 8.18x | 73789440 | yes |

## Per-region telemetry (summed over the run)

### win/declared

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 60 | 0 | 7,440 | 0 | 0 | 0.0000 |
| P|Customer | 76 | 1,257,040 | 0 | 0 | 0 | n/a |
| P|Order | 52 | 0 | 9,347 | 0 | 0 | 0.0000 |
| P|Stock | 78 | 56,177,405 | 5,727 | 0 | 0 | 0.0000 |
| P|StockMovement | 78 | 10,428,691,447 | 11,212 | 0 | 0 | 0.0000 |
| R|Order | 52 | 0 | 2,905 | 0 | 0 | 0.0000 |
| R|StockMovement | 52 | 0 | 3,569 | 0 | 0 | 0.0000 |
| U|Backorder | 72 | 0 | 4,371 | 0 | 0 | 0.0000 |
| U|Customer | 78 | 1,247,148 | 0 | 0 | 0 | n/a |
| U|Order | 72 | 0 | 3,569 | 0 | 0 | 0.0000 |
| U|Stock | 78 | 10,899,492 | 0 | 0 | 0 | n/a |
| U|StockMovement | 52 | 0 | 4,482 | 0 | 0 | 0.0000 |

### win/learned@1.0

| region | windows | bytes_read | bytes_written | compaction_bytes | merges | rewrite factor |
|---|---:|---:|---:|---:|---:|---:|
| P|Backorder | 59 | 0 | 7,520 | 0 | 0 | 0.0000 |
| P|Customer | 77 | 1,265,310 | 0 | 0 | 0 | n/a |
| P|Order | 52 | 0 | 9,347 | 0 | 0 | 0.0000 |
| P|Stock | 79 | 56,653,087 | 5,727 | 0 | 0 | 0.0000 |
| P|StockMovement | 79 | 10,525,077,253 | 11,212 | 0 | 0 | 0.0000 |
| R|Order | 52 | 0 | 2,905 | 0 | 0 | 0.0000 |
| R|StockMovement | 52 | 0 | 3,569 | 0 | 0 | 0.0000 |
| U|Backorder | 73 | 0 | 4,418 | 0 | 0 | 0.0000 |
| U|Customer | 79 | 1,255,380 | 0 | 0 | 0 | n/a |
| U|Order | 73 | 0 | 3,569 | 0 | 0 | 0.0000 |
| U|Stock | 79 | 10,986,390 | 0 | 0 | 0 | n/a |
| U|StockMovement | 52 | 0 | 4,482 | 0 | 0 | 0.0000 |

## Learner decisions and drift

### win/learned@1.0

- gated switches: {'U|Stock': 4, 'P|Stock': 5, 'P|StockMovement': 8, 'U|Customer': 2, 'P|Customer': 4, 'U|Order': 6, 'U|Backorder': 5, 'P|Backorder': 2, 'U|StockMovement': 5, 'R|StockMovement': 5, 'P|Order': 5, 'R|Order': 5}
- drift restarts: {'U|Stock': 0, 'P|Stock': 1, 'P|StockMovement': 2, 'U|Customer': 4, 'P|Customer': 5, 'U|Order': 4, 'U|Backorder': 5, 'P|Backorder': 3, 'U|StockMovement': 2, 'R|StockMovement': 2, 'P|Order': 2, 'R|Order': 2}
- recovery windows: {'P|Stock': 6, 'P|StockMovement': 6, 'U|Customer': 6, 'P|Customer': 6, 'U|Order': 7, 'U|Backorder': 7, 'P|Backorder': 6, 'U|StockMovement': 6, 'R|StockMovement': 6, 'P|Order': 6, 'R|Order': 6}
- first policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Customer': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)', 'P|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=8)'}
- final policy: {'U|Stock': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Stock': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'P|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'U|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'P|Customer': 'LayoutCandidate(page_size=4096, merge_min_tiles=200)', 'U|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'U|Backorder': 'LayoutCandidate(page_size=65536, merge_min_tiles=200)', 'P|Backorder': 'LayoutCandidate(page_size=4096, merge_min_tiles=2)', 'U|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'R|StockMovement': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)', 'P|Order': 'LayoutCandidate(page_size=4096, merge_min_tiles=8)', 'R|Order': 'LayoutCandidate(page_size=65536, merge_min_tiles=2)'}

## Pre-registered thresholds

| threshold | lambda | status | detail |
|---|---:|:--:|---|
| T1 learned > declared by 15% on win | 1.0 | **FAIL** | both sustain 100.0/100.0 rps; throughput gain at the highest common level = +0.0% |
| T3 reorganisation <= 3x declared | 1.0 | **NOT-MEASURABLE** | both arms wrote 0 bytes of compaction output: the offered-load driver never calls maybe_compact_shape, so the merge threshold is inert on this path (0 vs 0 is not a pass) |

## What is NOT concluded

- data:cap is 8.18x, below the 10x pre-registration floor
- compaction reorganisation writes are 0 for every arm: the offered-load driver never calls maybe_compact_shape, so the learner's merge threshold is not actuated on this path
- per-region page size acts only on tiles written after the switch; the write fraction in these traces is small, so the measured page-size effect is a lower bound
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; no simulated verdict is quoted
- the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)

## Verbatim project verification

See the slice commit message / task report for: `cargo test --workspace --offline` (`test result:` lines), `cargo build --workspace --offline` (zero warnings), `uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.
