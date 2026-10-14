# 78 - WP-X: the combined post-wave21 evaluation (rung-50 residual + T4 + T1/T2/T3 regression)

## Contract

Work package X of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave22-eval`
(isolated worktree, branch `wave22/eval`, based on integrated master `ce66f3e` carrying the
wave21/tilecap fix (ticket 75) and the wave21/driftshape write-spike drift shape (ticket 77)).
Do NOT cd to `/home/z/vibe/flashdb`. One flashdb process at a time under
`MemoryMax=128MiB`; `flock /tmp/wave21-measure.lock -c '<full command>'` serialises every
measurement command on the shared box; box load recorded at every boundary. Do NOT edit
`.scratch/15-objective-v0/pre-registration.md`, and do NOT change any pre-registered threshold
(the 2-window T4 target, the 10% band, any ceiling, the 15%/5%/3x numbers).

This is the **combined confirmation run** on the fully integrated tree. Ticket 71 re-derived
T1/T2/T3 (rung-100 lag-only sustain, rung-50 residual = compaction-adjacent cold reads + device
fsync tail) and ticket 77 re-derived T4 with the new write-spike drift shape (EVALUABLE, measured
NOT SUPPORTED). This package re-runs all of it in one pass against the integrated HEAD to confirm
the combined result, then records the per-arm/rung tables, both judging views, T1-T4 verdicts with
seed counts and noise band, an overall statement, and the residual.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3/T4, per-arm
per-rung lag-only tables (is rung 50 sustained now? rung 100?), T1 throughput, T2 adversarial,
T3 win reorg, T4 with the new drift shape, both judging views, a frank overall statement at the
achieved seed count, new-defect status, and slice-39 status. Pilot-grade labels only; a 10-seed
pre-registered verdict is claimed only if actually run (it will not be).

## Judging views (both reported; neither edited)

- **§7.1 lag-only (primary).** A rung is lag-sustained iff its achieved rate is within 5% of the
  offered rate, it has zero error windows, and **zero schedule-lag-ceiling rejected windows**
  (`lag_ceiling_rejected_windows == 0`). The service ceiling is reported separately as the
  efficiency measure and does not veto the lag-only reading.
- **Both ceilings (the harness's built-in `sustained` / T1 verdict).** A rung is `sustained` only
  if BOTH the service ceiling and the lag ceiling reject zero windows. The harness's `sustained`
  field and its T1 threshold table are computed this way. Under the adopted §7.1 lag-only primary
  they are NOT the primary metric, so the lag-only numbers are re-derived from the per-rung
  `lag_ceiling_rejected_windows` and `error_windows` fields, not quoted from the harness verdict.
  The harness prints `sustained=false` at every rung for every arm because the service ceiling
  rejects ~16-40 windows per rung (the per-group `fdatasync` device floor vs a fixed 10 ms), so
  its both-ceilings T1 verdict is `NOT-MEASURABLE` — the 0/0 artifact, not the lag-only answer.

## Measurement protocol (exact commands)

Build once from HEAD, copy the release binaries, verify the byte hash, and run only that copy (the
harness re-verifies no rebuild by byte-comparing `target/release/flashdb-load` against the pinned
copy before each invocation):

```
source $HOME/.cargo/env
cargo build --offline --release --bin flashdb-load --bin flashdb-scale --bin flashdb-replay
mkdir -p /tmp/wave22-eval-bin
cp target/release/flashdb-load /tmp/wave22-eval-bin/
cp target/release/flashdb-scale /tmp/wave22-eval-bin/
cp target/release/flashdb-replay /tmp/wave22-eval-bin/
sha256sum target/release/flashdb-load /tmp/wave22-eval-bin/flashdb-load
# flashdb-load   d2bd37ba9d5215d6b7e21410ea7db05f097aae58b49bc89751bba0ef43ee51b9
# flashdb-scale  c2d19eb4d0f3340b6246fe5cbdca5411cbd6e28b03c0e9fd6cc0d8e37076739f
# flashdb-replay 70338bea390605e8b40fcef3c90efa340a6ab5da41b27207cf5b8122154a80a9
cp -a /tmp/cold51/per_region-mixed /tmp/wave22-eval/per_region-mixed   # 1,643,925,811 B, 12.25x
```

`flashdb-load` is byte-different from WP-S HEAD (`47c0e5a3…`, tickets 71/73/77) because ticket 75
changed the engine (`manifest::MAX_TILES` 1024 -> 4096, `MAX_CHAIN_PAGES` 96 -> 316,
`Engine::set_max_tiles`, `--max-tiles`); the harness runs `target/release` which is byte-identical
to the pinned copy (sha256 re-checked at the end). Per arm the harness makes its own `cp -a` of
the base instance under `MemoryMax=128MiB`.

The T1/T2/T3 win+adversarial runs use all three arms; the T4 drift run uses learned + static-derived
only (declared is not a T4 subject — it saturates ~85 rps on the win mix, tickets 73/77 — and its
1450-rps end-of-run drain would be expensive and useless). Every measurement command was wrapped in
`flock /tmp/wave21-measure.lock -c '<full command>'`; `--drift-rate` was always explicit (never
`auto`).

Per seed, run 1 (win + adversarial; T1/T2/T3):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave22-eval/per_region-mixed \
  --workdir /tmp/wave22-eval/<seed> \
  --report /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>.md \
  --json   /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>'
```

Per seed, run 2 (drift; T4, write-spike shape):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave22-eval/per_region-mixed \
  --workdir /tmp/wave22-eval/<seed>-drift \
  --report /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>-drift.md \
  --json   /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>-drift.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift \
  --arms learned static-derived \
  --drift-shape write --drift-factor 10 --drift-pre-segments 6 --drift-rate 1450 \
  --seed <seed>'
```

- Win/adversarial ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10; rung 400 = 5).
- Drift: `write` weight x10, `pre=6` pre-drift segments (drift boundary at request 12,000), fixed
  1450 rps, `segment_requests 2000` (10 segments = 20,000 requests).
- Seeds run: **42, 43, 44 (3 independent) + 42 repeat (noise band)** = 4 runs per workload set.
- The eager warm-up pass runs in every arm-run (`startup_warm`); static-derived win runs adopt the
  index eagerly (`index_entries=30000`), declared never wants it, learned starts index-OFF by the
  WP-O demand gate.
- The learner runs without the declared-derived reorg budget on the drift runs (declared excluded
  there); `--learner-cooldown-windows 3` stays active.
- Every measurement command was lock-protected with `flock /tmp/wave21-measure.lock` (8 lock-protected
  harness invocations = 4 win+adversarial + 4 drift).

Box load at every boundary (shared 12-core box, 1-minute loadavg; the run was fairly quiet
throughout, unlike WP-S's noisy first half):

| boundary | time | loadavg 1/5/15 |
|---|---|---|
| seed42 win+adv start | 15:47:44 | 5.64 / 5.82 / 6.02 |
| seed42 win+adv end | 15:56:18 | 6.43 / 6.27 / 6.04 |
| seed42 drift start | 15:59:45 | 5.06 / 5.81 / 5.91 |
| seed42 drift end | 16:00:29 | 5.48 / 5.80 / 5.90 |
| seed43 win+adv start | 16:01:26 | 6.02 / 5.89 / 5.92 |
| seed43 win+adv end | 16:09:59 | 5.96 / 6.40 / 6.33 |
| seed43 drift start | 16:10:04 | 5.80 / 6.36 / 6.32 |
| seed43 drift end | 16:15:25 | 4.94 / 5.47 / 5.95 |
| seed44 win+adv start | 16:15:37 | 5.77 / 5.63 / 5.99 |
| seed44 win+adv end | 16:24:12 | 6.27 / 5.57 / 5.74 |
| seed44 drift start | 16:24:16 | 5.93 / 5.51 / 5.72 |
| seed44 drift end | 16:29:34 | 5.75 / 5.33 / 5.58 |
| seed42-repeat win+adv start | 16:29:39 | 5.93 / 5.37 / 5.59 |
| seed42-repeat win+adv end | 16:42:40 | 5.40 / 5.31 / 5.32 |
| seed42-repeat drift start | 16:42:45 | 5.13 / 5.26 / 5.30 |
| seed42-repeat drift end | 16:48:02 | 6.18 / 5.39 / 5.30 |

## Results

`error_windows = 0` on every one of the 96 win/adversarial rung-arm rows and every drift window;
`cgroup_peak = 134217728` (= the 128 MiB cap, cache-evicting cold) on every arm-run.

### T1 (win) — rung-50 and rung-100 lag-only (the primary metric)

Rung-50 lag-rejected windows (`lag_ceiling_rejected_windows` at offered 50, out of 40):

| arm | 42 | 43 | 44 | 42-rpt | median |
|---|---:|---:|---:|---:|---:|
| declared | 40 | 39 | 40 | 40 | 40 |
| learned@1.0 | 4 | 9 | 5 | 5 | 5 |
| static-derived | 2 | 2 | 2 | 2 | 2 |

**Rung 50 is NOT lag-sustained for any arm.** static-derived is 2/40 in every seed (38/40 valid,
95%); learned is 4-9/40 (median 5); declared never sustains any rung. Achieved rate is within 5%
for all three arms at rung 50 (`achieved_fraction` 0.9995-1.0005) and `error_windows = 0`, so the
lag ceiling alone is the only veto — the residual is 2 (static) / 4-9 (learned) scattered windows,
the named compaction-adjacent cold reads + device fsync tail + shared-box noise.

Rung-100 lag-rejected windows (out of 20), win:

| arm | 42 | 43 | 44 | 42-rpt |
|---|---:|---:|---:|---:|
| declared | 17 | 18 | 17 | 17 |
| learned@1.0 | 0 | 5 | 1 | 0 |
| static-derived | **0** | **0** | **0** | **0** |

`achieved_fraction` is 0.9995-1.0005 and `error_windows = 0` at rung 100 for the indexed arms.
**Rung 100 IS lag-sustained (§7.1):** `static-derived` 0/20 in 4/4 seeds; `learned@1.0` 0/20 in 2/4
seeds (42, 42-repeat) and 1-5/20 in the other 2 (seed44 = 1, seed43 = 5 — seed43 also misses the
5% achieved threshold at rung 100, 76 rps); `declared` never sustains any rung (17-18/20). This
confirms ticket 71's rung-100 finding (there static 8/8, learned 6/8): static-derived is robustly
lag-sustained at rung 100, learned is demand-gate- and noise-sensitive.

Win aggregate achieved rps (the layout win):

| arm | 42 | 43 | 44 | 42-rpt |
|---|---:|---:|---:|---:|
| declared | 74.0 | 75.0 | 74.0 | 74.0 |
| learned@1.0 | 107.0 | 107.0 | 106.0 | 107.0 |
| static-derived | 107.0 | 107.0 | 107.0 | 107.0 |

learned gain over declared (aggregate achieved): **+44.6% / +42.7% / +43.2% / +44.6%** — range
**+42.7% to +44.6%**, median **+43.9%**, far outside the measured **0.60%** throughput noise band
(declared seed-42 vs 42-repeat max relative diff; rung-100 89.45 -> 89.98 rps). Per-rung achieved
(50/100/200/400): declared 50 / 87-90 / 87-93 / 84-86; learned and static-derived hold ~100% of
offered at 50/100/200 and ~381-402 rps at 400.

### T2 (adversarial) — rung-50 lag, reorg ratio, index builds

Rung-50 lag-rejected windows (out of 40):

| arm | 42 | 43 | 44 | 42-rpt | median |
|---|---:|---:|---:|---:|---:|
| declared | 2 | 3 | 2 | 3 | 2.5 |
| learned@1.0 | 1 | 2 | 2 | 4 | 2 |
| static-derived | 0 | 2 | 0 | 3 | 1 |

learned median 2/40 vs declared 2.5/40 — **learned is at or better than declared on 3 of 4 seeds**
(42: 1<2, 43: 2<3, 44: 2=2; 42-rpt: 4>3), i.e. no >5% loss, well within the noise. **Index builds
on the 0-scan region: 0 on all 4 adversarial runs** (`P|StockMovement.index_output_bytes = 0`,
final policy `index_order_no=False` for learned) — the WP-O demand gate holds.

Adversarial reorganisation (learned/declared compaction ratio, T2 reorg band):

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 28,956,243 | 0.943x |
| 43 | 30,720,572 | 28,401,614 | 0.925x |
| 44 | 30,720,572 | 29,543,347 | 0.962x |
| 42-rpt | 30,720,572 | 28,905,853 | 0.941x |

**Median 0.942x, max 0.962x — every run <= 1x, comfortably <= 1.5x.** (Declared denominator is
deterministic 30,720,572 B, the R2 first-merge gate.) Adversarial aggregate achieved is 106-107 rps
for all three arms (learned never loses throughput).

**T2 verdict:** rung-50 lag learned within declared's band (PASS); 0 index builds on the 0-scan
region (PASS); reorg ratio median 0.942x <= 1.5x with no outlier (clean PASS).

### T3 (win) — learned/declared reorg ratio and the single-region blowup

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 17,533,306 | 0.571x |
| 43 | 30,720,572 | 17,029,067 | 0.554x |
| 44 | 30,720,572 | 28,200,076 | 0.918x |
| 42-rpt | 30,720,572 | 17,123,547 | 0.557x |

**Median 0.564x, max 0.918x (every run <= 1x, comfortably <= 3x).** static/declared median 0.956x
(0.939-0.972x). learned's win reorg is now bimodal: 3 of 4 seeds (~0.55-0.57x) defer the
`U|Customer` first merge (~11.2 MB) plus `P|Order`/`P|StockMovement`, while 1 seed (44) merges them
(0.918x). This is learner churn-control variance under this quieter box, NOT a regression: the
engine's compaction trigger is unchanged (`compaction_job(budget, COMPACT_MIN_TILES=2, …)`, the
`max_tiles` cap does not enter the merge decision — verified against `8a4ea20..ce66f3e`), and
learned aggregate throughput is unchanged (106-107 rps). The lower-reorg mode is the churn budget
deferring a marginal ~11 MB merge with no throughput penalty.

**The single-region blowup does NOT recur.** Win/learned `U|Stock` compaction is
**5.13-5.23 MB / 1 merge in all 4 seeds** (declared 5.23 MB / 1, static 5.13 MB / 1) — no
~10 MB-per-window oscillation, no 75 MB swing.

**T3 verdict:** win reorg ratio median 0.564x (<= 3x), every run <= 1x, no single-region blowup —
**PASS**.

### T4 (drift, write-spike shape) — R=1450, write x10, pre=6 segments

Steady pre-drift vs steady post-drift achieved rps (drift boundary = request 12,000; the straddle
window is excluded; pre = windows 3+ of the pre segment, post = post windows minus the last two
run-tail windows — ticket 77's method):

| run | arm | pre steady rps | post steady rps | post / offered |
|---|---:|---:|---:|---:|
| s42 | static-derived | 1452.2 | 1303.1 | 89.9% |
| s43 | static-derived | 1475.1 | 1314.3 | 90.6% |
| s44 | static-derived | 1452.4 | 1308.8 | 90.3% |
| s42-rpt | static-derived | 1450.8 | 1291.7 | 89.1% |
| s42 | learned@1.0 | (1450 offered) | 1310.7 | 90.4% |
| s43 | learned@1.0 | (1450 offered) | 1295.7 | 89.4% |
| s44 | learned@1.0 | (1450 offered) | 1313.1 | 90.6% |
| s42-rpt | learned@1.0 | (1450 offered) | 1302.4 | 89.8% |

The pre-drift (win) capacity is ~1450-1475 rps (static holds 1450 at 1450 offered); the post-drift
(write x10) steady throughput is **1291.7-1314.3 rps**, a real **-9.9% to -11.0%** drop (far
outside the same-seed repeat noise below). learned's pre is a catch-up backlog sum (windows 0-2 are
its demand-gated index burn-in), so its clean pre-drift is the offered 1450 rps it sustains after
burn-in. Whole-run achieved: learned 1136-1159 rps (burn-in drag), static 1332-1352 rps.

learned-vs-static post-drift gap: **-1.4% to +0.8%** (inside the noise band). Same-seed repeat
noise (seed 42 vs 42-repeat post-drift steady): learned 0.6%, static 0.9%. `recovery_windows = {}`
for learned in all 4 runs (no drift-triggered region re-exploration); `error_windows = 0` and
`commit_rate = 1.0` everywhere; reorg 0 B in every drift arm-run.

**T4 verdict (seed count + noise band): 3 independent seeds (42/43/44) + 1 repeat = 4 arm-runs per
arm (learned@1.0 + static-derived), 8 arm-runs total; same-seed repeat noise ~0.9% on the
post-drift quantity.** T4 is EVALUABLE (a real drop exists) and is **MEASURED NOT SUPPORTED** — the
learner does not recover and static-derived does not recover measurably slower. Both arms drop
~10% at the write-spike drift and neither returns to the 10% band of 1450 pre-drift; the learner
never triggers drift re-exploration and its post steady throughput (median ~1307 rps) is
indistinguishable from static-derived's (median ~1306 rps). Confirms ticket 77's null on the
integrated tree.

### Both judging views (summary)

- **§7.1 lag-only (primary):** rung 50 is **NOT** lag-sustained for any arm (declared 39-40/40,
  learned 4-9/40, static-derived 2/40). **Rung 100 IS lag-sustained for static-derived in 4/4 seeds
  and for learned in 2/4 seeds** (42, 42-repeat; seed44 1/20, seed43 5/20) — the positive lag-only
  sustained rung. Rung 200/400 are not lag-sustained across all seeds (the device fsync tail
  dominates at the top rungs). T4 is judged on throughput: both arms drop ~10% at the drift and
  neither recovers; learned ≈ static post-drift.
- **Both ceilings (harness / pre-registration):** `sustained = false` at every rung for every arm
  in every seed (0 of 96 rung-arm rows), because the service ceiling rejects ~16-40 windows per
  rung (the per-group `fdatasync` device floor vs a fixed 10 ms). The harness's T1 verdict is
  `NOT-MEASURABLE` and its T4 verdict is `NOT-MEASURABLE` (empty `recovery_windows`) — the
  both-ceilings/`sustained` artifact and the region-re-exploration reading, not the lag-only /
  throughput answers. The open §7.1 primary-vs-service reconciliation is unchanged and not edited.

### Slice-39 zero-gain status (unchanged)

Learned win reward-term ratios over the 4 seeds: reorg/service = **0.00111 .. 0.00320**,
index/service = **0.00014 .. 0.00025**. The reorganisation write term remains a tiny fraction of
the service term, so the lambda sweep still cannot move any candidate's argmin — the slice-39
zero-gain conclusion holds at this revision (and the lower reorg in 3/4 seeds only shrinks the
reorg term further).

## Honest verdict (pilot-grade)

**8 lock-protected harness invocations = 4 win+adversarial seeds x 3 arms + 4 drift seeds x 2 arms
= 20 arm-runs, 12.25x c51, shared box (loadavg ~4.8-6.4, fairly quiet), one flashdb process at a
time under `MemoryMax=128MiB`. NOT a 10-seed pre-registered verdict.**

1. **Rung-50 lag-rej = 0: NOT reached.** static-derived 2/40 in all 4 seeds (median 2), learned
   4-9/40 (median 5), declared 39-40/40. The residual is 2 (static) / 4-9 (learned) scattered
   windows — compaction-adjacent cold reads + device fsync tail + shared-box noise — not a named
   engine mechanism (startup warm-up windows 0-2 are still 0; `U|Stock` oscillation is still gone).
2. **Rung 100 IS lag-sustained (§7.1 lag-only) — confirmed.** static-derived 0/20 in 4/4 seeds,
   learned 0/20 in 2/4 seeds (42/44/42-rpt ≤ 1, 43 = 5). declared never sustains any rung.
3. **T1 throughput win preserved:** learned/static ~106-107 rps vs declared 74-75 rps (+42.7% to
   +44.6% aggregate achieved, median +43.9%), outside the 0.60% measured throughput noise band.
   As a sustained-load level, learned/static sustain rung 100 where declared never sustains any
   rung.
4. **T2:** rung-50 lag learned median 2/40 vs declared 2.5/40 (at/better than declared on 3/4
   seeds); 0 index builds on the 0-scan region; reorg ratio median 0.942x, max 0.962x (<= 1.5x).
5. **T3:** win reorg ratio median 0.564x, max 0.918x (every run <= 1x, <= 3x); the single-region
   blowup does NOT recur (`U|Stock` 5.13-5.23 MB / 1 merge in all 4 seeds).
6. **T4 (write-spike): EVALUABLE and measured NOT SUPPORTED** — a real ~10% capacity drop exists,
   neither arm recovers, learned ≈ static post-drift (gap ~±1%), `recovery_windows = {}` in all 4
   runs.

**What the evidence supports:** the integrated tree (tile-cap fix + write-spike drift shape + all
prior fixes) reproduces the ticket 71/77 result in one pass: the layout axis gives a reproducible
+43% achieved-throughput win, rung 100 is a clean lag-only sustained rung for static-derived
(4/4) and learned (2/4), rung 50 is still held back by a 2-9-window residual, and the write-spike
T4 is a measured null (no learner recovery, no differential advantage).

**What it does NOT support:** a lag-only sustained score at rung 50; a both-ceilings sustained
score at any rung (service ceiling device-bound, 0/96 rows); a 10-seed pre-registered verdict
(3 independent seeds + 1 repeat, shared box).

## New defects

None introduced. No new correctness, durability, or amplification defect appeared in the 20
arm-runs (0 error windows everywhere, `cgroup_peak` = the 128 MiB cap on every run, achieved
fraction <= 1.0005 at every rung, the cadence guard refused nothing). One observation to record,
not a defect: learned's win reorg is bimodal across seeds (~0.55-0.57x in 3/4, 0.918x in 1/4) and
its adversarial reorg is the mirror image vs ticket 71 (now 0.93-0.96x, no 0.6x seeds) — this is
the slice-39 churn-control budget/cooldown deferring a marginal `U|Customer` merge depending on the
box-load-dependent service-time observations, with no throughput change and learned reorg staying
<= declared (<=1x) in every seed. The engine's compaction trigger is verified unchanged
(`COMPACT_MIN_TILES = 2`, not the 1024 -> 4096 cap), so this is learner variance, not a regression.

## Gates (verbatim, final HEAD, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.03s
```
2. `cargo test --workspace --offline` — totals: **306 passed; 0 failed; 0 ignored**
(129 lib + 23 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 2 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 nvme_bench + 0 doc-tests). The integrated
tree has +2 tests over WP-S's 304: ticket 75's `max_tiles_flag_parses_and_bounds` (flashdb-load
22 -> 23) and the split `partitioned_publish_cap` (1 -> 2).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
146 passed in 4.87s
```
4. `git diff --check` — silent (exit 0).

## Residual gaps

- **Rung 50 is still not lag-sustained** (static 2/40, learned 4-9/40); the residual is scattered
  mid-run windows under shared-box load + the device fsync tail, not a named engine mechanism. The
  first positive lag-only sustained rung is **rung 100**, not rung 50.
- **learned's demand-gated window 0-2** (index OFF until the first scan-demand flip) plus box noise
  remains the systematic difference between learned and static-derived at rung 50/100 (seed43
  learned rung-100 = 5/20 and 76 rps, the noisiest arm-run of the batch).
- **learned's reorg is load-sensitive/bimodal** (win 0.55-0.92x, adversarial 0.93-0.96x), from the
  slice-39 churn budget deferring a marginal merge; always <= declared (<=1x) and no throughput
  effect, but the per-seed reorg total is not a stable constant.
- **Service ceiling remains device-bound** (both-ceilings 0/96 rows; device fsync p50 6.6 ms /
  p99 13.4 ms vs a fixed 10 ms). The open §7.1 reconciliation is untouched.
- **3 independent seeds + 1 repeat, not 10; shared box** (loadavg ~4.8-6.4, quieter than WP-S).
  All timing/count numbers are pilot-grade.
