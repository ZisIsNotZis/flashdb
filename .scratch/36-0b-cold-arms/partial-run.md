# Slice 36 - 0b cold run (PARTIAL: captured mid-run)

The full run writes its report at the end; this partial captures the per-arm summary
lines emitted so far so the win comparison is durable. ratio=8.18x.

| arm | rc | achieved rps | commit | sustained | invalid | reorg B | cgroup peak | data:cap |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| win/declared | 0 | 75.0 | 1.0 | 100.0 | 44 | 0 | 201326592 | 8.18 |
| win/learned@0.1 | 0 | 75.0 | 1.0 | 100.0 | 45 | 0 | 201326592 | 8.18 |
| win/learned@1.0 | 0 | 75.0 | 1.0 | 100.0 | 44 | 0 | 201326592 | 8.18 |
| win/learned@10.0 | 0 | 74.0 | 1.0 | 100.0 | 44 | 0 | 201326592 | 8.18 |
| win/static-derived | 0 | 75.0 | 1.0 | 100.0 | 44 | 0 | 201326592 | 8.18 |
| adversarial/declared | 0 | 107.0 | 1.0 | 400.0 | 0 | 0 | 201326592 | 8.18 |
| adversarial/learned@0.1 | 0 | 107.0 | 1.0 | 400.0 | 0 | 0 | 201326592 | 8.18 |
| adversarial/learned@1.0 | 0 | 107.0 | 1.0 | 400.0 | 0 | 0 | 201326592 | 8.18 |
| adversarial/learned@10.0 | 0 | 107.0 | 1.0 | 400.0 | 0 | 0 | 201326592 | 8.18 |

## Ladder notes (partial)

- **win**: top step is 400 rps; every arm sustained only up to **100 rps** (200/400 not sustained),
  so the win ladder is BRACKETED - not under-powered. T1 will be decided on throughput at the
  common sustained level, not on a tie.
- **adversarial**: every arm sustained the full 50-400 ladder at `--segment-requests 2000`
  (sustained=400) at the time this partial was captured, so the ladder is UNDER-POWERED and the
  guard will raise the top step to 800 and rerun the adversarial arms (the live extension).

## Win results so far (declared vs learned, lambda {0.1, 1.0, 10.0})

| arm | achieved rps | sustained level | commit | invalid windows | reorg bytes | cgroup peak | data:cap | cold |
|---|---:|---:|---:|---:|---:|---:|---:|:--:|
| declared | 75 | 100 | 1.0 | 44 | 0 | 201326592 | 8.18x | yes |
| learned@0.1 | 75 | 100 | 1.0 | 45 | 0 | 201326592 | 8.18x | yes |
| learned@1.0 | 75 | 100 | 1.0 | 44 | 0 | 201326592 | 8.18x | yes |
| learned@10.0 | 74 | 100 | 1.0 | 44 | 0 | 201326592 | 8.18x | yes |
| static-derived | 75 | 100 | 1.0 | 44 | 0 | 201326592 | 8.18x | yes |

All five win arms reach the same highest-sustained load level (100 rps) and land within ~1% of each
other on achieved throughput - the layout axis did not separate the arms on the win mix at this
scale. Every arm was cache-evicting (cgroup peak == cap).
