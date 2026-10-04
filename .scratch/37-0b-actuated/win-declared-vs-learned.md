# 0b with compaction ACTUATED — win declared vs learned (Slice 37, PARTIAL)

Committed mid-run so the result is durable. The runner is still going (adversarial ladder
extension, then drift); the FINAL generated report lands at `report.md`/`results.json` when it
finishes. Nothing here is superseded silently: the numbers are the runner's own per-arm summary
lines for the completed `win` arms.

## What changed

- `flashdb-load --compact-bytes N`: the offered-load driver now calls the engine's existing
  `maybe_compact_shape` once per metrics window, with the ACTIVE per-region policy (the one
  installed by `--policy-file`/`--policy-reload`) and its tile gate at the floor (2), so the
  learner's `merge_min_tiles` governs which regions merge. It compacts at the window boundary
  before the policy reload and the snapshot, and the window line carries the driver's own
  `compact_bytes`; the engine's per-region `compaction_bytes`/`compaction_merges` counters are
  populated (they were always 0 on this path before).
- `harness/flashdb_harness/cold_0b.py`: passes `--compact-bytes` (default 64 MiB) to every arm;
  raises the write share from 15% to 40% (win: stock20/customer10/scan30/write40;
  adversarial-uniform: stock30/customer30/scan0/write40) so a page-size switch reaches newly
  written tiles. No engine format/manifest/WAL/tile change.

## Instance

`/tmp/cold33/per_region-mixed`: data 1,647,135,638 B, cgroup cap 201,326,592 B, **ratio 8.18x**
(below the pre-registration 10x floor — flagged). Every arm's cgroup peak == the cap
(`cache_evicting=true`).

## win arms (COMPLETE) — `--segment-requests 2000`, rates [50,100,200,400], window 1 s

| arm | lambda | achieved rps | highest sustained | invalid windows | reorg bytes | cgroup peak | cold |
|---|---:|---:|---:|---:|---:|---:|:--:|
| win/declared | n/a | 80 | 100 | 30 | 2,599,988 | 201326592 | yes |
| win/learned@0.1 | 0.1 | 82 | 100 | 27 | 10,735,363 | 201326592 | yes |
| win/learned@1.0 | 1.0 | 83 | 100 | 28 | 10,865,545 | 201326592 | yes |
| win/learned@10.0 | 10.0 | 81 | 100 | 29 | 11,112,647 | 201326592 | yes |

`reorg bytes` is the summed per-region `compaction_bytes` over the run — **non-zero for every
win arm**, which is the whole point of this slice: the merge knob is no longer inert.

## Provisional threshold reading (win only; adversarial/drift still running)

| threshold | lambda | provisional | detail |
|---|---:|---|---|
| T1 learned > declared by 15% on win | 0.1/1.0/10.0 | **tie at the top** | all four arms sustain the same highest level (100 rps); whole-run achieved 80/82/83/81 rps. The exact common-level gain is filled from the finished report. |
| T3 reorganisation <= 3x declared | 0.1 | **FAIL (provisional)** | learned 10,735,363 B vs declared 2,599,988 B = 4.13x |
| T3 reorganisation <= 3x declared | 1.0 | **FAIL (provisional)** | learned 10,865,545 B vs declared 2,599,988 B = 4.18x |
| T3 reorganisation <= 3x declared | 10.0 | **FAIL (provisional)** | learned 11,112,647 B vs declared 2,599,988 B = 4.27x |

**T3 is now MEASURABLE**: declared and learned both wrote non-zero compaction output, so the
0-vs-0 NOT-MEASURABLE of slices 35/36 is gone. The measured reading is a FAIL (learned rewrites
~4.1-4.3x declared), pending the finished report's exact per-region split.

## Not concluded (unchanged)

- data:cap 8.18x is below the 10x pre-registration floor.
- one seed per arm on a shared box; the 10-seed median/IQR requirement is not met.
- the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable.
