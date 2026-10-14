# 68 - WP-Q: the final SS7.1 lag-only evaluation after the churn + write-stall fixes

## Contract

Work package Q of the program. Integrated master (`b7958bc`) carries both the WP-O learner-churn
fix (ticket 66: demand-gated, one-shot sticky `index_order_no` axis) and the WP-P write-stall fix
(ticket 67: the grouped WAL append is deferred onto the barrier worker, and the WP-M index tile is
warmed in the read-ahead view). The layout axis gives a +40-51% throughput win; the question this
package answers is whether, with both fixes integrated, **rung 50 now reaches 0 lag-rejected
windows on the win workload** so the win becomes a clean §7.1 lag-only SUSTAINED result, and to
report T1/T2/T3 with the best available evidence.

Single writer in `/home/z/vibe/worktrees/flashdb/wave15-eval` (isolated worktree, branch
`wave15/eval`, based on integrated master). One process at a time under the memory cap; box load is
recorded at every boundary.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3, both judging
views, the harness's both-ceilings `0/0` sustain artifact flagged, any new defect, the slice-39
zero-gain status, and a frank statement of what the evidence does and does not support for the
pre-registered thesis. Pilot-grade labels only; a 10-seed pre-registered verdict is claimed only if
actually run (it will not be).

## Measurement protocol (exact commands)

Release binaries built once from HEAD `b7958bc` and the run only uses `target/release` binaries.
`flashdb-load` sha256 `29222e0b83b45f79024fa30e745eafbc50479520b207f29007b0e23922509b84` — byte-
identical to the WP-P "after" binary, so the integrated tree compiles to the same load binary.
Instance `cp -a` copies of `/tmp/cold51/per_region-mixed` (12.25x = 1,643,925,811 B / 128 MiB).
The harness `flashdb_harness/cold_0b.py` runs every arm under
`systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0` (the harness does this per
arm); `--cap-mib 128` names the same cap.

One harness invocation per seed (it runs the 3 arms x 2 workloads sequentially, one arm process at
a time):

```
cd harness && uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave15-eval/<seed> \
  --report .scratch/68-eval-final/<seed>.md \
  --json   .scratch/68-eval-final/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>
```

- Full ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10;
  rung 400 = 5).
- Seeds: **42, 43, 44 (3 independent) + 42 repeat (noise band)**; the box did not allow more within
  the wall-clock budget (4 x ~9-10 min serial harness runs).
- Box load (`uptime`) recorded before and after each seed invocation.

Gates (verbatim, run at final HEAD after the measurement; see bottom):

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q` — summary.
4. `git diff --check` — silent (exit 0).

## Judging views (both reported; neither edited)

- **§7.1 lag-only (primary).** A rung is lag-sustained iff its achieved rate is within 5% of the
  offered rate, it has zero error windows, and **zero schedule-lag-ceiling rejected windows**
  (`lag_ceiling_rejected_windows == 0`). The service ceiling is reported separately as the
  efficiency measure and does not veto the lag-only reading.
- **Both ceilings (the harness's built-in `sustained` / T1 verdict).** A rung is `sustained` only
  if BOTH the service ceiling and the lag ceiling reject zero windows. The harness's `sustained`
  field and its T1 threshold are computed this way.

**Flagged artifact:** the harness's `sustained`/`highest_sustained_rate` and the T1 threshold table
are the both-ceilings reading. Under the adopted §7.1 lag-only primary they are NOT the primary
metric, so the lag-only numbers are re-derived below from the per-rung `lag_ceiling_rejected_windows`
and `error_windows` fields, not quoted from the harness verdict. A "0/0" (zero service-rejected AND
zero lag-rejected) is what the harness needs to print `sustained=true`; the lag-only reading needs
only the second zero.

## Results

### Box load at every boundary (shared 12-core box; timings name their load)

| boundary | time | loadavg 1/5/15 |
|---|---|---|
| seed42 start | 09:33:48 | 4.25 / 5.78 / 5.53 |
| seed42 end | 09:44:06 | 3.91 / 4.92 / 5.39 |
| seed43 end (observed 9.11 mid-run) | 09:53:40 | 8.62 / 7.11 / 6.10 |
| seed44 end | 10:03:10 | 7.28 / 7.15 / 6.68 |
| seed42-repeat end | 10:12:39 | 5.68 / 5.57 / 5.99 |

Every arm-run recorded `cgroup_peak = 134,217,728 B` (= the cap, cache-evicting cold). Declared's
achieved rps swung 68-75 across the four runs and learned rung-50 lag-rej swung 3-13 on the SAME
trace (seed42 vs seed42-repeat), so every timing/count number below is pilot-grade on this box.

### T1 (win) — rung-50 lag-only verdict

Rung-50 lag-rejected windows (`lag_ceiling_rejected_windows` at offered 50, out of 40):

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 39/40 | 39/40 | 40/40 | 40/40 |
| learned@1.0 | 13/40 | 4/40 | 3/40 | 3/40 |
| static-derived | 3/40 | 2/40 | 2/40 | 3/40 |

**No arm reaches 0 at rung 50; no arm is lag-sustained at rung 50 (or any rung) under the §7.1
reading.** The residual lag breaches are:

- **static-derived** (index on from window 0, no learner churn): windows **0-1** in 3/4 runs
  (seed43/seed44 = 2/40), plus a single mid-run window 28 in seed42 and seed42-repeat (lag p99
  42 / 25 ms). Windows 0-1 are the read-ahead worker's startup warm-up recorded in ticket 67 (the
  pacer dispatches the first window before the warm view fills the lead); window 28 is shared-box
  noise, not a named mechanism.
- **learned@1.0**: windows **0-2** in the 3 clean runs (3-4/40) — one extra warm-up window over
  static because the learner starts index-OFF and flips index-ON only at the first scan-demand
  window. seed42 is the blowup outlier below.

Win aggregate achieved rps and per-rung achieved (the +40-51% layout win, now over 4 seeds):

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 68.0 | 75.0 | 69.0 | 71.0 |
| learned@1.0 | 107.0 | 107.0 | 106.0 | 107.0 |
| static-derived | 107.0 | 107.0 | 107.0 | 107.0 |

learned gain over declared: **+57.4% / +42.7% / +53.6% / +50.7%** (aggregate achieved). Per-rung
achieved (50/100/200/400): declared 50 / 80-90 / 77-94 / 76-88; learned and static-derived hold
~100% of offered at 50/100/200 (declared collapses to 40-90% at 100/200 and ~20% at 400).

**T1 verdict (lag-only, honest):** NOT lag-sustained at rung 50 for any arm. The layout win is
real and reproducible as an **achieved-throughput** win (learned/static ~107 rps vs declared
68-75 rps, +43-57%), but it does NOT convert to a positive lag-sustained score because static-
derived still breaches 2-3/40 windows (startup warm-up + noise) and learned breaches 3-13/40
(warm-up +, on seed42, the reorg oscillation below). The `>15% learned-vs-declared` threshold is
UNDERIVABLE as a *sustained-load* comparison (both arms are 0 sustained); as an achieved-throughput
comparison learned beats declared by +43-57%, far outside the ~9% measured throughput noise band.

**Noise band (measured):** declared win achieved rps, seed42 vs seed42-repeat (same trace): max
relative diff **9.0%** (rung 400: 75.54 -> 82.37). Learned/static throughput repeats to <0.05 rps
(noise far smaller than the win). The lag metric is far noisier: learned rung-50 lag-rej swung
3 -> 13 on the same trace, driven by the reorg blowup (next).

### T2 (adversarial) — rung-50 lag, reorg ratio, index builds

Rung-50 lag-rejected windows (out of 40):

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 1/40 | 1/40 | 1/40 | 0/40 |
| learned@1.0 | 0/40 | 0/40 | 0/40 | 0/40 |
| static-derived | 1/40 | 1/40 | 1/40 | 1/40 |

learned matches declared (0/40 on all four; declared 0-1/40). **Index builds on the 0-scan region:
0 on all four adversarial runs** (`index_output_bytes = 0`, `P|StockMovement.index_order_no=False`
in every final policy) — the demand gate holds.

Adversarial reorganisation (learned/declared compaction ratio, T2 reorg band):

| seed | declared bytes | learned bytes | ratio |
|---|---:|---:|---:|
| seed42 | 26,955,568 | 76,700,180 | 2.85x |
| seed43 | 23,310,824 | 39,405,039 | 1.69x |
| seed44 | 21,651,974 | 21,148,825 | 0.98x |
| seed42-repeat | 25,025,316 | 3,392,045 | 0.14x |

**Median 1.34x** (toward <=1.5x), but seed42 is a 2.85x blowup (learned `P|Customer` 72.3 MB /
3 merges + `U|Stock` oscillation), the same pre-existing merge oscillation as ticket 66's seed43
outlier, not the index axis (index bytes = 0).

**T2 verdict:** rung-50 lag = declared (PASS); no index build on the 0-scan region (PASS); reorg
ratio median 1.34x toward <=1.5x with a 2.85x noise outlier (direction PASS, not a clean pass).

### T3 (win) — learned/declared reorg ratio and the single-region blowup

| seed | declared bytes | learned bytes | ratio |
|---|---:|---:|---:|
| seed42 | 7,198,067 | 109,308,744 | 15.19x |
| seed43 | 7,198,067 | 14,940,441 | 2.08x |
| seed44 | 5,654,480 | 15,444,680 | 2.73x |
| seed42-repeat | 7,198,067 | 3,339,811 | 0.46x |

**Median 2.41x** (within the pre-registered 3x), with a **15.19x single-run blowup on seed42.**
The blowup is the pre-existing `U|Stock` merge oscillation, and it **does recur**:

- seed42 win/learned: `U|Stock` **75.2 MB / 8 merges** + `P|Customer` **30.8 MB / 3 merges** =
  106 MB of the 109 MB total; the rung-50 lag-rej 13/40 in that same run is windows 24-34
  interleaved with those compaction bursts (5-10 MB/window on the shared device cascades into lag).
- seed42-repeat win/learned (SAME trace): `U|Stock` 0 merges / 0 bytes, total reorg 3.3 MB, lag-rej
  3/40. The oscillation is timing/exploration-dependent, not trace-deterministic.

**T3 verdict:** the single-region blowup RECURS (up to 75 MB in `U|Stock`), is noise-dominated
(3.3 MB -> 109 MB on the same trace), and is the main thing that keeps learned's rung-50 lag from
matching static-derived on every seed. It is pre-existing (tickets 65/66/67), not introduced by
this integration.

### Both judging views (summary)

- **§7.1 lag-only (primary):** sustained rps = **0 at every rung for every arm**. Rung 50:
  declared 39-40/40, learned 3-13/40, static-derived 2-3/40. The index removes the scan but the
  two-fix integrated tree still breaches the lag ceiling in 2-13 windows (startup warm-up +
  `U|Stock` reorg oscillation + shared-box noise).
- **Both ceilings (harness / pre-registration):** sustained = 0 everywhere, as pre-registered —
  the per-group `fdatasync` durability floor keeps the service ceiling rejecting ~34-40/40 rung-50
  windows on every arm (device-bound; the open §7.1 primary-vs-service reconciliation is unchanged
  and not edited).

### Slice-39 zero-gain status (unchanged)

Learned win reward-term ratios: reorg/service = **0.00039 .. 0.01032** (even the 109 MB blowup is
1.03% of service), index/service = 0.00020 .. 0.00027. The reorganisation write term remains a
tiny fraction of the service term, so the lambda sweep still cannot move any candidate's argmin —
the slice-39 zero-gain conclusion holds at this revision.

## Honest verdict (pilot-grade)

**4 runs = 3 independent seeds (42/43/44) + one full seed-42 repeat, 12.25x c51, shared box,
one process at a time. NOT a 10-seed pre-registered verdict.**

1. **Rung-50 lag-rej = 0: NOT reached.** static-derived 2-3/40 (windows 0-1 warm-up, one
   noise window 28), learned 3-4/40 in 3 runs (windows 0-2) and 13/40 in the seed42 reorg-blowup
   run. The WP-P write-path acceptance holds (no fsync tail becomes lag); the residual is the
   read-ahead STARTUP WARM-UP + the learner's `U|Stock` merge oscillation, neither of which is a
   write-path or device-durability stall.
2. **T1 throughput win preserved:** learned/static ~107 rps vs declared 68-75 rps (+43-57%
   achieved), ~100% at 100/200 rps where declared collapses. Learned vs declared as a *sustained-
   load* comparison is UNDERIVABLE (both 0); as achieved throughput it is +43-57%.
3. **T2:** rung-50 lag learned = declared (0/40); 0 index builds on the 0-scan region; reorg ratio
   median 1.34x (toward <=1.5x) with a 2.85x noise outlier.
4. **T3:** win reorg ratio median 2.41x (<=3x), but the single-region blowup RECURS — `U|Stock`
   75 MB / 8 merges on seed42 vs 0 MB on the same-trace repeat.

**What the evidence supports:** the layout axis (WP-M `order_no` index) is necessary and delivers a
reproducible, large achieved-throughput + service-latency win (declared 68-75 -> learned/static
107 rps; svc p99 collapses), and the WP-O churn fix removes the learner's own index toggling (1
build, 0 toggles on win; 0 builds on adversarial). **What it does NOT support:** a positive lag-only
SUSTAINED score. The strict sustained metric is still 0 because (a) the read-ahead worker's cold
startup breaches windows 0-2 even with the index, and (b) the learner's `U|Stock` merge exploration
can trigger a multi-merge reorg blowup that cascades into rung-50 lag. Both residuals are named and
pre-existing; the lag ceiling remains the binding constraint on a positive sustained verdict.

## Residual gaps

- The startup warm-up (windows 0-2) is still un-fixed: eager warm of the movement scan prefix +
  `order_no` index before the first dispatch (or a longer initial lead) is the obvious candidate and
  is out of scope here (a read-ahead tuning change).
- The `U|Stock`/`P|Customer` merge oscillation is un-bounded: the learner explores merge thresholds
  that can fire a ~75 MB compaction in one window. Out of scope; it is what kept learned at 13/40 on
  seed42.
- Service ceiling remains device-bound (both-ceilings 0 everywhere); the open §7.1 reconciliation is
  untouched.
- 4 seeds, not 10; shared box (load 3.9-9.1); all timing/count numbers are pilot-grade.
- T1 ">15% learned-vs-declared sustained" is UNDERIVABLE as written because the sustained rate is 0
  for both arms; the achieved-throughput comparison (+43-57%) is the honest substitute.

## Gates (verbatim, final HEAD, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.78s
```
2. `cargo test --workspace --offline` — totals: **302 passed; 0 failed; 0 ignored**
(128 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 7 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
140 passed in 5.10s
```
4. `git diff --check` — silent (exit 0).
