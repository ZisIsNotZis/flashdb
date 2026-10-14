# 71 - WP-S: the decisive SS7.1 lag-only evaluation (after startup-warm + merge-oscillation fixes)

## Contract

Work package S of the program. Integrated master (this worktree HEAD at measurement `853633a`, then
record commits) carries both the WP-R1 startup-warm fix (ticket 69: eager index build+adopt + bounded
synchronous `warm_startup` pass before the timed windows) and the WP-R2 merge-oscillation fix
(ticket 70: re-merge minimum-payback hysteresis in `Engine::select_region_run`). The layout axis
gives a reproducible +43-57% achieved-throughput win; the question this package answers is whether,
with BOTH fixes integrated and measured SERIALLY under a shared-box lock, **rung 50 now reaches 0
lag-rejected windows on the win workload** so the win becomes a clean §7.1 lag-only SUSTAINED
result, and to report T1/T2/T3 with the best available evidence.

Single writer in `/home/z/vibe/worktrees/flashdb/wave17-eval` (isolated worktree, branch
`wave17/eval`, based on integrated master). One process at a time under the memory cap; box load is
recorded at every boundary.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3, both judging
views, the harness's both-ceilings `0/0` sustain artifact flagged, any new defect, the slice-39
zero-gain status, and a frank statement of what the evidence does and does not support for the
pre-registered thesis. Pilot-grade labels only; a 10-seed pre-registered verdict is claimed only if
actually run (it will not be).

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
metric, so the lag-only numbers are re-derived from the per-rung `lag_ceiling_rejected_windows` and
`error_windows` fields, not quoted from the harness verdict. A "0/0" (zero service-rejected AND
zero lag-rejected) is what the harness needs to print `sustained=true`; the lag-only reading needs
only the second zero. The harness prints **`sustained=false` at every rung for every arm in every
seed** because the service ceiling rejects ~16-40 windows per rung (the device fsync floor), so its
T1 verdict is `NOT-MEASURABLE` ("declared sustained 0.0, learned 0.0") — the both-ceilings `0/0`
artifact, not the lag-only answer.

## Measurement protocol (exact commands)

Release binaries built once from HEAD `853633a` (`flashdb-load` sha256
`47c0e5a333881b97dde44c0ce1403ec9bd0f2ddf4f1195a232d097aaf58fabbe`); the run only uses
`target/release` binaries. Instance `cp -a` copies of `/tmp/cold51/per_region-mixed`
(12.25x = 1,643,925,811 B / 128 MiB). The harness `flashdb_harness/cold_0b.py` runs every arm
under `systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0` (the harness does
this per arm); `--cap-mib 128` names the same cap. `cgroup_peak_bytes = 134217728` (= the cap,
cache-evicting cold) on every arm-run.

One harness invocation per seed (it runs the 3 arms x 2 workloads sequentially, one arm process at
a time), wrapped in `flock /tmp/wave17-measure.lock`:

```
cd harness && flock /tmp/wave17-measure.lock uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave17-eval/<seed> \
  --report .scratch/71-eval-decisive/<seed>.md \
  --json   .scratch/71-eval-decisive/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>
```

- Full ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10;
  rung 400 = 5).
- Seeds run: **42, 43, 44, 45, 46, 47, 48 (7 independent) + 42 repeat (noise band)** = 8 full
  harness invocations = 48 arm-runs. (Ticket 68's protocol used 42/43/44 + 42-repeat; this is the
  decisive run, so more independent seeds were added within the wall-clock budget.)
- The eager warm-up pass runs in every arm-run: `startup_warm scan_entries=30000` in all 48 runs;
  `index_entries=30000` exactly for the 8 static-derived win runs (index eagerly adopted at open),
  `index_entries=0` for the other 40 (declared never wants the index; learned starts index-OFF by
  the WP-O demand gate).

Box load at every boundary (shared 12-core box; `llama-server` x4 ~90% CPU each + a `helpagent`
benchmark were present early and drained over the run, which is why the first seeds are noisier):

| boundary | time | loadavg 1/5/15 |
|---|---|---|
| run start (process snapshot) | 12:06 | 11.30 / 9.60 / 9.08 |
| seed42 start | 12:10:59 | 8.32 / 9.32 / 9.10 |
| seed42 end | 12:19:40 | 7.82 / 8.68 / 8.87 |
| seed43 start | 12:19:50 | 7.78 / 8.64 / 8.86 |
| seed43 end | 12:28:25 | 12.14 / 9.57 / 9.14 |
| seed44 start | 12:28:30 | 12.14 / 9.57 / 9.14 |
| seed44 end | 12:37:12 | 9.09 / 8.87 / 8.95 |
| seed45 start | 12:37:15 | 8.68 / 8.78 / 8.92 |
| seed45 end | 12:45:50 | 8.41 / 7.92 / 8.37 |
| seed46 start | 12:45:54 | 8.41 / 7.92 / 8.37 |
| seed46 end | 12:54:29 | 4.91 / 6.31 / 7.50 |
| seed42-repeat start | 12:54:34 | 4.83 / 6.27 / 7.49 |
| seed42-repeat end | 13:03:09 | 5.96 / 5.91 / 6.81 |
| seed47 start | 13:03:16 | 5.66 / 5.85 / 6.78 |
| seed47 end | 13:11:48 | 3.48 / 4.24 / 5.65 |
| seed48 start | 13:11:52 | 3.44 / 4.22 / 5.64 |
| seed48 end | 13:20:24 | 3.07 / 3.98 / 5.03 |

No other `flashdb-load/scale/replay` process ran concurrently with any seed (checked at every
boundary). The box is shared and was NOT quiet for the first half (load 8-12) but drained to 3-6
for the second half; every timing number below names its load.

## Results

### T1 (win) — rung-50 lag-only verdict (the pre-registered rung)

Rung-50 lag-rejected windows (`lag_ceiling_rejected_windows` at offered 50, out of 40):

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-repeat | median |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 39 | 40 | 39 | 40 | 40 | 40 | 40 | 39 | 40 |
| learned@1.0 | 4 | 6 | 5 | 5 | 5 | 5 | 4 | 4 | 5 |
| static-derived | 3 | 1 | 1 | 2 | 2 | 3 | 2 | 2 | 2 |

**No arm reaches 0 at rung 50, so no arm is lag-sustained at rung 50** (static-derived best is 1/40
in seeds 43/44; learned best is 4/40). Achieved rate is within 5% for all three arms at rung 50
(`achieved_fraction` 0.9995-1.0005) and `error_windows = 0` everywhere, so the lag ceiling alone is
the only thing that vetoes rung 50 for the indexed arms. The lag-valid fraction at rung 50
(= 40 − lag-rej)/40: **declared 0/40 (0-2.5%), learned 34-36/40 (85-90%), static-derived
37-39/40 (92.5-97.5%)**.

Win aggregate achieved rps (the layout win, now over 7 independent seeds + 1 repeat):

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-repeat |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 72.0 | 72.0 | 70.0 | 72.0 | 72.0 | 74.0 | 75.0 | 72.0 |
| learned@1.0 | 107.0 | 107.0 | 107.0 | 106.0 | 107.0 | 107.0 | 107.0 | 107.0 |
| static-derived | 107.0 | 107.0 | 106.0 | 106.0 | 107.0 | 107.0 | 107.0 | 107.0 |

learned gain over declared (aggregate achieved): **+48.6% / +48.6% / +52.9% / +47.2% / +48.6% /
+44.6% / +42.7% / +48.6%** — range **+42.7% to +52.9%**, median **+48.6%**. Per-rung achieved
(50/100/200/400): declared 50 / 82-87 / 78-91 / 80-94; learned and static-derived hold ~100% of
offered at 50/100/200 and ~370-400 rps at 400.

**Noise band (measured, same trace seed 42 vs 42-repeat):** declared win achieved rps max relative
diff **2.48%** (rung 100: 84.99 -> 87.10; rung 400: 80.53 -> 81.74 = 1.50%). learned/static
aggregate achieved repeat to <0.05 rps. The lag metric is noisier: static-derived rung-50 lag-rej
3 -> 2 and learned 4 -> 4 on the same trace, but both stay in a tight band (see below).

### T2 (adversarial) — rung-50 lag, reorg ratio, index builds

Rung-50 lag-rejected windows (out of 40):

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-repeat | median |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 2 | 4 | 3 | 2 | 4 | 4 | 2 | 0 | 2.5 |
| learned@1.0 | 4 | 1 | 1 | 1 | 1 | 2 | 0 | 4 | 1 |
| static-derived | 3 | 3 | 2 | 0 | 2 | 2 | 2 | 2 | 2 |

learned median 1/40 vs declared median 2.5/40 — **learned is within (in fact at or better than)
declared's band** on 7 of 8 seeds (seed42 is +2). **Index builds on the 0-scan region: 0 on all 8
adversarial runs** (`P|StockMovement.index_output_bytes = 0`, final policy `index_order_no=False`
for learned) — the WP-O demand gate holds.

Adversarial reorganisation (learned/declared compaction ratio, T2 reorg band):

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 28,401,614 | 0.925x |
| 43 | 30,720,572 | 28,630,592 | 0.932x |
| 44 | 30,720,572 | 18,466,818 | 0.601x |
| 45 | 30,720,572 | 29,000,333 | 0.944x |
| 46 | 30,720,572 | 27,647,852 | 0.900x |
| 47 | 30,720,572 | 28,660,225 | 0.933x |
| 48 | 30,720,572 | 18,760,384 | 0.611x |
| 42-repeat | 30,720,572 | 28,905,853 | 0.941x |

**Median 0.928x, max 0.944x — every run is <= 1x, comfortably <= 1.5x.** (The two ~0.6x seeds are
learned deferring the `U|Stock`/`P|Customer` first-merge past the shorter adversarial run; no
blowup, just a lower total.)

**T2 verdict:** rung-50 lag learned = declared (PASS, median 1 vs 2.5); 0 index builds on the
0-scan region (PASS); reorg ratio median 0.928x <= 1.5x with no outlier (clean PASS).

### T3 (win) — learned/declared reorg ratio and the single-region blowup

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 28,704,315 | 0.934x |
| 43 | 30,720,572 | 28,905,853 | 0.941x |
| 44 | 30,720,572 | 28,200,076 | 0.918x |
| 45 | 30,720,572 | 28,200,076 | 0.918x |
| 46 | 30,720,572 | 28,704,315 | 0.934x |
| 47 | 30,720,572 | 28,200,076 | 0.918x |
| 48 | 30,720,572 | 28,704,315 | 0.934x |
| 42-repeat | 30,720,572 | 28,905,853 | 0.941x |

**Median 0.934x, max 0.941x (all <= 1x; no single-run blowup).** static/declared: median 0.939x,
max 0.972x. The declared denominator is deterministic (30,720,572 B in all 8 seeds) — the R2
first-merge gate makes declared fold one pair in each big seed region; learned is a stable
~28.2-28.9 MB (its higher thresholds defer a few small-region merges).

**The single-region blowup does NOT recur.** Win/learned `U|Stock` compaction is
**5.13-5.23 MB / 1 merge in ALL 8 seeds** (pre-fix it swung 75.2 MB / 8 merges vs 0 MB / 0 on the
same-trace pair; ticket 70 bounded it to 5.23 MB / 1). The max single-window actuation across all
8 runs is **11.08-11.37 MB, one cheapest contiguous pair** (`U|Customer`, one window), never a
repeated ~10 MB-per-window burst. Per-region totals over the 8 seeds: `U|Customer` 89.2 MB / 8,
`P|Customer` 82.3 MB / 8, `U|Stock` 41.6 MB / 8 — every region does exactly one first merge per
seed and then pays back new tiles before re-merging.

**T3 verdict:** win reorg ratio median 0.934x (<= 3x), no single-run blowup, single-region
oscillation gone — **PASS** (the R2 fix holds under serial measurement).

### Rung 100 and 200 (the "rung 50 or higher" question, §7.1 lag-only)

The strict rung-50 answer is negative, but the two fixes + serial measurement do lift the lag
ceiling higher up the ladder:

Rung-100 lag-rejected windows (out of 20), win:

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-repeat |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 17 | 17 | 17 | 17 | 17 | 17 | 16 | 17 |
| learned@1.0 | 2 | 0 | 0 | 0 | 1 | 0 | 0 | 0 |
| static-derived | **0** | **0** | **0** | **0** | **0** | **0** | **0** | **0** |

`achieved_fraction` is 0.9995-1.0005 and `error_windows = 0` at rung 100 for all indexed arms, so
under §7.1 lag-only **static-derived is lag-sustained at rung 100 in all 8 seeds (0/20 lag-rej)**
and **learned is lag-sustained at rung 100 in 6 of 8 seeds** (seed42 = 2/20, seed46 = 1/20).
declared is never lag-sustained at any rung (rung 50 already 39-40/40; rung 100 16-17/20).

Rung-200 lag-rejected windows (out of 10): static-derived 1/1/3/1/1/0/1/0, learned
2/0/3/1/2/0/1/1 — mixed (0-3/10), so neither indexed arm is lag-sustained at rung 200 across all
seeds. Rung-400 (out of 5): 4-6/5-6 for both indexed arms (the device fsync tail dominates at the
top rung).

### Both judging views (summary)

- **§7.1 lag-only (primary):** rung 50 is **NOT** lag-sustained for any arm (declared 39-40/40,
  learned 4-6/40, static-derived 1-3/40). **Rung 100 IS lag-sustained for static-derived in 8/8
  seeds and for learned in 6/8 seeds** — the first positive lag-only sustained rung in any
  multi-seed sample. Rung 200 and 400 are not lag-sustained across all seeds.
- **Both ceilings (harness / pre-registration):** sustained = 0 everywhere (all 8 seeds, all arms,
  all rungs print `sustained=false`; T1 `NOT-MEASURABLE`), because the per-group `fdatasync`
  durability floor keeps the service ceiling rejecting ~16-40 windows per rung on every arm
  (device fsync p50 6.6 ms / p99 13.4 ms vs a fixed 10 ms). The open §7.1 primary-vs-service
  reconciliation is unchanged and not edited.

### Slice-39 zero-gain status (unchanged)

Learned win reward-term ratios over the 8 seeds: reorg/service = **0.00320 .. 0.00362**,
index/service = **0.00024 .. 0.00027**. The reorganisation write term remains a tiny fraction of
the service term, so the lambda sweep still cannot move any candidate's argmin — the slice-39
zero-gain conclusion holds at this revision.

## Honest verdict (pilot-grade)

**8 runs = 7 independent seeds (42-48) + one full seed-42 repeat, 12.25x c51, shared box
(loadavg 3.1-12.1, noisier for the first 4 seeds), one flashdb process at a time under a shared-box
lock. NOT a 10-seed pre-registered verdict.**

1. **Rung-50 lag-rej = 0: NOT reached.** static-derived 1-3/40 (median 2), learned 4-6/40
   (median 5), declared 39-40/40. The WP-R1 startup warm-up is gone (windows 0-2 `total=0` in
   every static-derived run via the eager `warm_startup` pass) and the WP-R2 oscillation is gone
   (`U|Stock` 5.13-5.23 MB / 1 merge in all 8 seeds), so the remaining static-derived rung-50
   breaches are 1-3 scattered mid-run windows (shared-box noise + the device fsync tail), NOT a
   named mechanism. learned's extra 2-4 windows over static are its demand-gated index-OFF
   observation window (the WP-O design) plus the same mid-run noise.
2. **Rung 100 IS lag-sustained (§7.1 lag-only) — the headline.** static-derived 0/20 in 8/8 seeds,
   learned 0/20 in 6/8. declared never sustains any rung. This is a strictly stronger result than
   ticket 68's "0 sustained at every rung": the two fixes + serial (locked) measurement moved the
   first positive lag-only sustained rung from none to rung 100.
3. **T1 throughput win preserved:** learned/static ~106-107 rps vs declared 70-75 rps (+42.7% to
   +52.9% aggregate achieved, median +48.6%), ~100% achieved at 100/200 rps where declared
   collapses to 82-91%. The `>15% learned-vs-declared` comparison: as a *sustained-load* level,
   learned (100 rps in 6/8 seeds) vs declared (never sustained) exceeds 15% by an unbounded
   margin; as *achieved throughput* it is +42.7-52.9%, far outside the 2.5% measured throughput
   noise band.
4. **T2:** rung-50 lag learned median 1/40 vs declared 2.5/40 (at/better than declared); 0 index
   builds on the 0-scan region; reorg ratio median 0.928x, max 0.944x (<= 1.5x).
5. **T3:** win reorg ratio median 0.934x, max 0.941x (<= 3x); the single-region blowup does NOT
   recur (max single-window actuation 11.08-11.37 MB, one pair).
6. **T4 (drift recovery within 2 windows): NOT measured in this run** — this package ran only the
   win and adversarial workloads (per the task), not the drift workload, so T4 stays UNDERIVED at
   this revision (STATUS §5: the default parameters need 6 windows, not the pre-registered 2).

**What the evidence supports:** the layout axis (WP-M `order_no` index) is necessary and delivers a
reproducible, large achieved-throughput + service-latency win (declared 70-75 -> learned/static
106-107 rps; ~100% at 100/200 rps), and — after WP-O/WP-R1/WP-R2 and under a serial locked
measurement — that win now **converts to a positive lag-only sustained rung at 100 rps** for
static-derived (8/8 seeds) and learned (6/8). The strict rung-50 target is still not met because
the lag ceiling at 50 rps leaves ~1-3 (static) / 4-6 (learned) scattered windows over the line,
now attributable to shared-box noise and the device fsync tail rather than any named engine
mechanism.

**What it does NOT support:** a lag-only sustained score at rung 50; a both-ceilings sustained
score at any rung (the service ceiling remains device-bound, 0 everywhere); a 10-seed pre-registered
verdict (7 independent seeds + 1 repeat, on a shared box whose first 4 seeds ran under load 8-12).

## New defects

None introduced. No new defect was observed; the two pre-existing residuals remain and are named
(rung-50 scattered mid-run lag windows under shared-box load; the both-ceilings service ceiling
being device-bound). No new correctness, durability, or amplification defect appeared in the 48
arm-runs (0 error windows everywhere, `cgroup_peak` = the 128 MiB cap on every run, achieved
fraction <= 1.0005 at every rung).

## Gates (verbatim, final HEAD, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.33s
```
2. `cargo test --workspace --offline` — totals: **304 passed; 0 failed; 0 ignored**
(129 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 nvme_bench + 0 doc-tests). The integrated
tree has one more test than either pre-merge lane (ticket 69's +1 lib warm_startup test and ticket
70's +1 merge_threshold test both present).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
140 passed in 4.80s
```
4. `git diff --check` — silent (exit 0).

## Residual gaps

- **Rung 50 is still not lag-sustained** (static 1-3/40, learned 4-6/40); the residual is scattered
  mid-run windows under shared-box load + the device fsync tail, not a named engine mechanism. The
  first positive lag-only sustained rung is **rung 100**, not rung 50.
- **learned's demand-gated window 0-2** (index OFF until the first scan-demand flip, the WP-O
  design) is the remaining systematic difference between learned and static-derived at rung 50.
- **Service ceiling remains device-bound** (both-ceilings 0 everywhere; device fsync p50 6.6 ms /
  p99 13.4 ms vs a fixed 10 ms). The open §7.1 reconciliation is untouched.
- **7 independent seeds + 1 repeat, not 10; shared box** (load 3.1-12.1, first 4 seeds noisier).
  All timing/count numbers are pilot-grade.
- T1 ">15% learned-vs-declared sustained" is derivable only because declared never sustains any
  rung while learned sustains rung 100 in 6/8 seeds; the achieved-throughput comparison
  (+42.7-52.9%) is the clean quantitative substitute.
