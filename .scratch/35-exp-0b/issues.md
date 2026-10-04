# 35 — experiment 0b: the runner exists, the run does not yet

State on 2026-10-02: `harness/flashdb_harness/cold_0b.py` implements the arms, the lambda sweep, the
drift segment and a per-threshold verdict table with three honesty guards (noise-band downgrade;
"0 vs 0 compaction bytes" = NOT-MEASURABLE because the load driver never compacts; the 2-window drift
target reported as unreachable with the default parameters). 90 Python tests pass.

**No verdict has been produced.** The one partial run printed a T1 line whose configuration was a
smoke run (both arms sustained the top of the ladder, which is impossible for the 45%-scan win mix
cold, where the measured ceiling is 87-134 rps).

Two defects to fix before the run is evidence:

1. **Ladder under-power guard (must fix first).** The ladder is `reference_rate * (1,2,4,8)`. If
   every arm sustains the top step, the "highest sustained load level" metric ties by construction
   and a T1 tie says nothing. The runner must detect that case and emit
   NOT-MEASURABLE (ladder under-powered) with the observed rates, and it must raise the top step
   until at least one arm fails (or state that it cannot).
2. **Coldness evidence per arm.** Every arm must report the cgroup peak and the data:cap ratio, so a
   reader can see the run was actually cache-evicting. Warm runs on this box measured exactly the
   opposite conclusion for the page-size axis (slice 25 flat vs slice 29 +54% cold), so "cold" is not
   a detail - it is the difference between two contradictory results.

Also to state in the report: the win mix is 45% scans / 30% stock / 10% customer / 15% writes (the
scan share is what makes the layout axis matter); the instance is 1.65 GB at a 192 MiB cap = 8.17x
(below the pre-registration's 10x, flagged); and the drift target of 2 windows is an author-owned
threshold that the current parameters cannot reach (6 windows), so it must be reported as a failed
threshold or renegotiated - not silently relaxed.

## Verification item found while observing the first full matrix run (2026-10-02)

Watching `/tmp/cold0b.log` during the slice-36 matrix (instance `/tmp/cold33/per_region-mixed`,
data 1,647,135,638 B, cap 192 MiB, **ratio 8.18x, cgroup_peak == the cap on every arm, so
cold=True is evidenced**), two arms report numbers that are internally inconsistent and must be
explained before any verdict from this run is accepted:

```
[win/*]        achieved_rps=75.0  sustained=100.0  invalid=44
[adversarial/*] achieved_rps=107.0 sustained=400.0  invalid=0
```

`achieved_rps` appears to be the average over the WHOLE four-step ladder (50/100/200/400), and
`sustained=400` claims the top step was sustained while the average completed rate was 107 rps and
`invalid=0`. Those cannot all be true unless `highest_sustained_rate` is computed from something
other than per-step achieved-vs-offered. Also note the adversarial arm's command line carries no
`--ceiling-us`, so the latency-ceiling check is inert there by construction.

Required before a verdict is quoted: the report must show the PER-STEP curve (offered, achieved,
invalid-window count, lag) for every arm, and `sustained` must be derived from per-step
achieved-vs-offered with the same tolerance the pacer uses, with the noise band stated. If the
aggregation is wrong (as it looks), the first full 0b matrix produced no valid threshold verdict at
all - which is why this is being written down now rather than after the numbers are quoted.

## Actuated win result (slice 37, win arms COMPLETE; adversarial/drift still in flight)

`flashdb-load` now takes `--compact-bytes 67108864`, so the injected policy's `merge_min_tiles`
actually governs compaction during the offered load. Cold instance (1.647 GB / 192 MiB = 8.18x,
every arm's cgroup peak == the cap, so `cache_evicting=true`), rates [50,100,200,400], 2000 requests
per rung, 1 s windows:

| arm | lambda | achieved rps | highest sustained | invalid windows | reorg (compaction) bytes |
|---|---:|---:|---:|---:|---:|
| declared | - | 80 | 100 | 30 | 2,599,988 |
| learned@0.1 | 0.1 | 82 | 100 | 27 | 10,735,363 (4.13x) |
| learned@1.0 | 1.0 | 83 | 100 | 28 | 10,865,545 (4.18x) |
| learned@10.0 | 10.0 | 81 | 100 | 29 | 11,112,647 (4.27x) |

**T3 is now measurable and it FAILS**: the learner writes 4.1-4.3x the declared arm's compaction
bytes. **T1 ties at the top**: every arm sustains the same highest level (100 rps), and the
whole-run achieved difference is 80 vs 81-83 rps = +1.3% to +3.8%, inside the box's noise band, so
T1 cannot be called a win.

Two findings worth following up, neither of which is a thesis verdict:

1. **lambda barely moves the learned policy** (0.1 -> 10 changes reorg bytes only from 4.13x to
   4.27x). Either the reward proxy's write term is negligible against the service-time term at
   these magnitudes, or exploration/confidence dominates the choice. Either way the endurance price
   is not being priced in practice, which the objective intends it to be.
2. **The declared arm is already strong on this workload.** The learner's differentiated policy
   (defer `P|Order` and `P|StockMovement` to merge_min_tiles=200, keep `P|Stock` at 8 with 4 KiB
   pages, 64 KiB for `R|Order`) is *plausible* and matches the access pattern, yet it buys no
   measured throughput and costs 4x the writes - i.e. on this workload the declared default's small
   pages and eager merging are not the bottleneck the learner can improve.

Caveats, unchanged and required: 8.18x is below the 10x floor; one seed per arm (the 10-seed
median/IQR requirement is unmet); the calibration gate (<=25% p90) is not evaluable; and the ladder
tops out at 100 rps for every arm, so T1's "one load level higher" criterion cannot discriminate
here at all.

## Drift arm results (2026-10-02): the drift rate is above capacity, and the learner churns

The reduced matrix (adversarial + drift, arms declared / learned@1.0 / static-derived) ran to the
drift workload and produced two findings, both about the EXPERIMENT and the LEARNER rather than the
thesis:

| arm (drift, fixed 150 rps) | achieved | sustained | invalid windows | reorg bytes |
|---|---|---|---|---|
| declared | 58 | **0.0** | 148 | 1,673,926 |
| learned@1.0 | 54 | **0.0** | 151 | **402,984,851** |

1. **The drift workload's fixed rate is above the engine's capacity for its own mix.** The drift plan
   runs at a constant 150 rps, but its trace is 64% scans (7,697 of 12,000) versus the win mix's 45%,
   and scans are the slowest shape (21-40 ms each cold). Every arm saturates (`sustained=0`, ~150
   invalid windows), so "recover to within 10% of pre-drift throughput" is undefinable - there is no
   sustainable pre-drift baseline to recover to. The rate must be chosen below the capacity of the
   HEAVIEST pre-drift mix, or the drift claim cannot be evaluated at all. (This is also why the
   earlier log line "sustained=800" belonged to the adversarial arms, whose ladder is cheap, not to
   drift.)
2. **Under drift the learner churns: 402,984,851 reorganisation bytes against declared's 1,673,926 -
   a 240x difference.** That is the anti-overfitting failure mode the adversarial-uniform workload
   was designed to catch, appearing on the drift workload instead: drift triggers re-exploration,
   the re-exploration moves merge thresholds, and every threshold change makes the next compaction
   rewrite more. The pre-registration's "must not need >3x the reorganisation writes" is therefore
   already violated by a wide margin BEFORE a drift verdict is even defined.

Both are recorded as defects to fix (drift rate below the heaviest pre-drift mix's capacity; churn
control between re-explorations, e.g. a reorganisation budget or a cooldown), not as thesis
evidence. The adversarial arm's numbers are in the same run and will be read from the finished
report.
