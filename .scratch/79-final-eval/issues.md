# 79 - final combined post-integration evaluation (setup dispatch)

## Objective

Final-revision proof of the thesis metrics on the fully integrated master (tilecap + driftshape +
rewarm repair). This record is the setup dispatch: it fixes the measured revision, the release
binaries, and the fresh cold 12.25x instance the measurement will use. The measurement itself
(arms, seeds, tables, T1/T2/T3/T4 verdicts) is recorded in this ticket by the eval worker.

## Branch / HEAD

- Branch: `wave23/final-eval`
- Worktree: `/home/z/vibe/worktrees/flashdb/wave23-final-eval`
- Base master HEAD: `48b0488af5d9db1c2252c727e9389bfb06ecdc09`

## Instance

Fresh cold copy (not reused): `cp -a /tmp/cold51/per_region-mixed /tmp/wave23-eval/per_region-mixed`.
Size: 1,643,925,811 B (12.25x vs 128 MiB cap).

## Protocol

- Measure ONLY the release binaries built in this worktree (`target/release/flashdb-load`,
  `target/release/flashdb-scale`, `target/release/flashdb-replay`), one process at a time, against
  `/tmp/wave23-eval/per_region-mixed`.
- Record `uptime`/loadavg and cgroup peak with each run; every measurement wrapped in
  `flock /tmp/wave21-measure.lock -c '<full command>'`.
- Do NOT edit `.scratch/15-objective-v0/pre-registration.md`.

## AC

- `cargo build --workspace --release --offline` completes with 0 warnings in this worktree.
- Fresh cold instance copy exists at `/tmp/wave23-eval/per_region-mixed` (1,643,925,811 B).
- This dispatch header committed on `wave23/final-eval`; dispatch entry appended to
  `.scratch/52-program/issues.md`.

# Measurement record (the final-revision proof)

Measured on `wave23/final-eval` at HEAD `da20ff3` (base master `48b0488` = tilecap + driftshape +
rewarm repair). Release binaries built in this worktree; sha256: `flashdb-load`
`e7bc1f655186942fc66ec3cbe79a87fd55f2134073f7c9b59847d7dc637e7a1f`, `flashdb-scale`
`9690dd905270d83b20a6a1acea16cd81a39197b9dae82bbf3b5ddf06aadffc19`, `flashdb-replay`
`6f0b0f9fcc21378a6d5490f949818429080fd6422d81575f3d8f8ce3e3ea8c48`. The instance is the fresh
cold copy `/tmp/wave23-eval/per_region-mixed` (1,643,925,811 B, 12.25x vs 128 MiB).

## Protocol (exact commands, one process at a time, all lock-protected)

Per seed `42`, `43`, `44`, `42r` (= seed-42 repeat), run 1 (win + adversarial; T1/T2/T3):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave23-eval/per_region-mixed --workdir /tmp/wave23-eval/<seed> \
  --report /home/z/vibe/worktrees/flashdb/wave23-final-eval/.scratch/79-final-eval/<seed>.md \
  --json   /home/z/vibe/worktrees/flashdb/wave23-final-eval/.scratch/79-final-eval/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial --arms declared learned static-derived --seed <seed>'
```

Per seed, run 2 (write-spike drift; T4):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave23-eval/per_region-mixed --workdir /tmp/wave23-eval/<seed>-drift \
  --report /home/z/vibe/worktrees/flashdb/wave23-final-eval/.scratch/79-final-eval/<seed>-drift.md \
  --json   /home/z/vibe/worktrees/flashdb/wave23-final-eval/.scratch/79-final-eval/<seed>-drift.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift --drift-shape write --drift-factor 10 \
  --drift-pre-segments 6 --drift-rate 1450 --arms declared learned static-derived --seed <seed>'
```

Win/adversarial ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
`segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10; rung 400 = 5).
Drift: `write` weight x10, `pre=6` pre-drift segments (drift boundary at request 12,000), fixed
1450 rps, 10 segments = 20,000 requests. 8 lock-protected harness invocations total (4 x run1 + 4 x
run2), one flashdb process at a time under `MemoryMax=128MiB`.

**The drift run includes `declared` (per the task's arm list).** `declared` is not a T4 subject
(it saturates ~118-121 rps vs the 1450 offer, tickets 73/77/78); it is run here so the
learner's reorganisation budget can be derived from declared's observed drift reorg (ticket 71's
churn-control convention). Consequence recorded below: declared's drift reorg is ~0, so the drift
learner's budget is 0 B/window (see T4 and the accounting note).

Box load at every boundary (shared 12-core box, 1-minute loadavg; the run was quiet throughout):

| boundary | time | loadavg 1/5/15 |
|---|---|---|
| start | 17:32:41 | 5.68 / 6.13 / 6.54 |
| seed42 win+adv start/end | 17:32:48 / 17:41:35 | 5.55 / 4.13 |
| seed42 drift start/end | 17:41:35 / 17:45:17 | 4.13 / 5.19 |
| seed43 win+adv start/end | 17:45:17 / 17:53:55 | 5.19 / 5.06 |
| seed43 drift start/end | 17:53:55 / 17:58:02 | 5.06 / 5.20 |
| seed44 win+adv start/end | 17:58:02 / 18:06:40 | 5.20 / 5.37 |
| seed44 drift start/end | 18:06:40 / 18:10:19 | 5.37 / 5.89 |
| seed42-repeat win+adv start/end | 18:10:19 / 18:19:02 | 5.89 / 3.45 |
| seed42-repeat drift start/end | 18:19:02 / 18:22:37 | 3.45 / 2.89 |

`cgroup_peak = 134217728` (= the 128 MiB cap, cache-evicting cold) on every one of the 36 arm-runs.
Raw artifacts: `.scratch/79-final-eval/{42,43,44,42r}{,-drift}.{json,md}`, `boxload.md`,
`analyze.py`.

## Judging views (both reported; neither edited)

- **§7.1 lag-only (primary).** A rung is lag-sustained iff achieved rate within 5% of offered,
  zero error windows, and zero schedule-lag-ceiling rejected windows
  (`lag_ceiling_rejected_windows == 0`). The service ceiling is reported separately and does NOT
  veto the lag-only reading.
- **Both ceilings (the harness's built-in `sustained` / T1 verdict).** A rung is `sustained` only
  if BOTH the service ceiling and the lag ceiling reject zero windows. Under §7.1 lag-only this is
  NOT the primary metric; the lag-only numbers below are re-derived from the per-rung
  `lag_ceiling_rejected_windows`/`error_windows` fields, not quoted from the harness verdict.

## Results

`error_windows = 0` on every one of the 96 win/adversarial rung-arm rows and every drift window;
`commit_rate = 1.0` everywhere; `cgroup_peak` = the 128 MiB cap on every arm-run.

### T1 (win) — rung-50, rung-100, rung-200 lag-only (the primary metric)

Rung-50 lag-rejected windows (`lag_ceiling_rejected_windows` at offered 50, out of 40):

| arm | 42 | 43 | 44 | 42r | median |
|---|---:|---:|---:|---:|---:|
| declared | 39 | 40 | 40 | 39 | 39.5 |
| learned@1.0 | 4 | 6 | 3 | 3 | 3.5 |
| static-derived | 3 | 2 | **0** | 3 | 2.5 |

**Rung 50 is NOT lag-sustained for any arm** (the 0/40 only appears once, static-derived seed 44;
the other three static seeds are 2-3/40). Achieved fraction is within 5% and `error_windows = 0`
for all arms at rung 50, so the lag ceiling alone is the only veto.

Rung-100 lag-rejected windows (out of 20):

| arm | 42 | 43 | 44 | 42r |
|---|---:|---:|---:|---:|
| declared | 17 | 18 | 16 | 17 |
| learned@1.0 | 0 | 0 | 2 | 0 |
| static-derived | **0** | **0** | **0** | **0** |

Rung-200 lag-rejected windows (out of 10):

| arm | 42 | 43 | 44 | 42r |
|---|---:|---:|---:|---:|
| declared | 17 | 16 | 17 | 17 |
| learned@1.0 | 0 | 0 | 1 | 2 |
| static-derived | **0** | **0** | **0** | **0** |

Achieved fraction is 0.9995-1.0000 and `error_windows = 0` at rungs 100 and 200 for the indexed
arms. **§7.1 lag-only: static-derived is lag-sustained at rung 100 (4/4 seeds) AND rung 200 (4/4
seeds); learned@1.0 is lag-sustained at rung 100 (3/4: 42/43/42r, seed44 = 2/20) and rung 200 (2/4:
42/43).** declared never sustains any rung (16-18/20 at rung 100, 39-40/40 at rung 50). This
advances ticket 78's result (static rung-100 4/4, learned 2/4): on the rewarm-repair tree
static-derived additionally holds rung 200, and learned holds rung 100 in 3/4 instead of 2/4.

Win aggregate achieved rps (the layout win):

| arm | 42 | 43 | 44 | 42r |
|---|---:|---:|---:|---:|
| declared | 71.0 | 74.0 | 75.0 | 71.0 |
| learned@1.0 | 107.0 | 107.0 | 106.0 | 107.0 |
| static-derived | 105.0 | 107.0 | 107.0 | 107.0 |

learned gain over declared (aggregate achieved): **+50.7% / +44.6% / +41.3% / +50.7%** — range
**+41.3% to +50.7%**, median **+47.6%**, far outside the measured **~4.0%** throughput noise band
(declared seed-42 vs 42-repeat max per-rung relative diff; rung-100 85.3 -> 82.8 = 2.9%, rung-200
80.9 -> 84.1 = 4.0%). Per-rung achieved (50/100/200/400): declared 50 / 83-90 / 81-93 / 81-86; the
indexed arms hold ~100% of offered at 50/100/200 and ~336-400 rps at 400.

### T2 (adversarial) — rung-50 lag, reorg ratio, index builds

Rung-50 lag-rejected windows (out of 40):

| arm | 42 | 43 | 44 | 42r | median |
|---|---:|---:|---:|---:|---:|
| declared | 2 | 1 | 2 | 4 | 2 |
| learned@1.0 | 0 | 1 | 2 | 3 | 1.5 |
| static-derived | 0 | 2 | 3 | 1 | 1.5 |

learned median 1.5/40 vs declared 2/40 — **at or better than declared on 4 of 4 seeds** (42: 0<2,
43: 1=1, 44: 2=2; 42r: 3<4), i.e. no >5% loss, within the noise. **Index builds on the 0-scan
region: 0 on all 4 adversarial runs** (`P|StockMovement.index_output_bytes = 0`, learned final
policy `index_order_no=False` for `P|StockMovement`) — the WP-O demand gate holds.

Adversarial reorganisation (learned/declared ratio, `summary.compact_bytes_total`):

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 28,660,225 | 0.933x |
| 43 | 30,720,572 | 28,105,596 | 0.915x |
| 44 | 30,720,572 | 30,575,644 | 0.995x |
| 42r | 30,720,572 | 28,660,225 | 0.933x |

**Median 0.933x, max 0.995x — every run <= 1x, comfortably <= 1.5x.** (declared denominator
deterministic 30,720,572 B, the R2 first-merge gate.)

**T2 verdict: PASS** (rung-50 lag learned within declared's band; 0 index builds on the 0-scan
region; reorg ratio median 0.933x, max 0.995x <= 1.5x).

### T3 (win) — learned/declared reorg ratio and the single-region blowup

| seed | declared B | learned B | ratio |
|---|---:|---:|---:|
| 42 | 30,720,572 | 28,704,315 | 0.934x |
| 43 | 30,720,572 | 28,905,853 | 0.941x |
| 44 | 30,720,572 | 16,998,771 | 0.553x |
| 42r | 30,720,572 | 18,413,976 | 0.599x |

**Median 0.767x, max 0.941x (every run <= 1x, <= 3x).** static/declared: 0.984/0.607/0.954/0.974x
(median 0.964x). learned's win reorg is bimodal: seeds 42/43 complete the ~11.37 MB `U|Customer`
merge (0.93-0.94x) while seed 44 defers `U|Customer` and seed 42r defers `P|Customer` (0.55-0.60x) — the slice-39 churn-control variance
under this quieter box (ticket 78 observed the same 0.554x in 1/4; here it is 2/4), always <=
declared and with no throughput change (learned 106-107 rps in all 4).

**The single-region blowup does NOT recur.** Win/learned `U|Stock` compaction is 5.13-5.23 MB / 1 merge
each (declared 5.23 MB / 1, static 5.13-5.23 MB / 1) — no ~10 MB-per-window oscillation, no
75 MB swing (ticket 71/78's stable result holds on the rewarm tree).

**T3 verdict: PASS** (win reorg median 0.767x, max 0.941x <= 3x; no single-region blowup).

### T4 (drift, write-spike shape) — R=1450, write x10, pre=6 segments

Steady pre-drift vs steady post-drift achieved rps (drift boundary = request 12,000; the straddle
window that contains request 12,000 is excluded; pre = windows 3+ of the pre segment; post = post
windows minus the last two run-tail windows — ticket 77/78's method):

| run | arm | pre steady rps | post steady rps | post / offered |
|---|---|---:|---:|---:|
| s42 | static-derived | 1461.5 | 1326.7 | 91.5% |
| s43 | static-derived | 1451.8 | 1300.9 | 89.7% |
| s44 | static-derived | 1445.3 | 1262.3 | 87.1% |
| s42r | static-derived | 1449.9 | 1347.8 | 93.0% |
| s42 | learned@1.0 | (1450 offered) | 1359.7 | 93.8% |
| s43 | learned@1.0 | (1450 offered) | 1283.3 | 88.5% |
| s44 | learned@1.0 | (1450 offered) | 1320.0 | 91.0% |
| s42r | learned@1.0 | (1450 offered) | 1359.9 | 93.8% |

The pre-drift (win) capacity is ~1450 rps (static holds 1445-1461 rps at 1450 offered); the
post-drift (write x10) steady throughput is **1262-1348 rps**, a real **~9.4% (static) / ~7.6%
(learned) median drop** — far outside the ~1.6% same-seed repeat noise on the post quantity
(static 42 1326.7 vs 42r 1347.8). learned's pre is a catch-up backlog sum (windows 0-2 are its
demand-gated index burn-in), so its clean pre-drift is the offered 1450 it sustains after burn-in.
Whole-run achieved: declared 118-121 rps (saturated), learned 1142-1174 rps (burn-in drag), static
1344-1379 rps.

learned-vs-static post-drift gap (median): **+2.0%** (1339.9 vs 1313.8) — inside the noise band.
`recovery_windows = {}` for learned in all 4 runs (no drift-triggered region re-exploration);
`error_windows = 0` and `commit_rate = 1.0` everywhere; drift reorg ~0 B for all arms
(`summary.compact_bytes_total` 0-266,598 B; see the accounting note — the write-heavy saturated
run does not reach a merge trigger).

**T4 verdict (seed count + noise band): 3 independent seeds (42/43/44) + 1 seed-42 repeat = 4
arm-runs per arm (learned@1.0 + static-derived + declared), 12 arm-runs total; same-seed repeat
noise ~1.6% on the post-drift quantity.** T4 is EVALUABLE (a real ~9% drop exists) and is **MEASURED
NOT SUPPORTED** — the learner does not recover and static-derived does not recover measurably
slower. Both arms drop ~8-9% at the write-spike drift and neither returns to the 1450 pre-drift
capacity; the 10% band [1305, 1595] straddles the noisy post floor (per-seed posts 1262-1360), so
the "within 10%" call is noise-straddled exactly as ticket 78 found. The robust signal is the
empty `recovery_windows` and the learned≈static post gap (+2%), not a per-window pop above 1305.
This confirms ticket 77/78's null on the rewarm-repair tree.

### Both judging views (summary)

- **§7.1 lag-only (primary):** rung 50 is **NOT** lag-sustained for any arm (declared 39-40/40,
  learned 3-6/40, static-derived 2-3/40 with one 0/40). **Rung 100 IS lag-sustained for
  static-derived in 4/4 seeds and learned in 3/4; rung 200 IS lag-sustained for static-derived in
  4/4 and learned in 2/4.** Rung 400 is not lag-sustained across all seeds (the device fsync tail
  dominates at the top rung). T4 is judged on throughput: both arms drop ~8-9% at the drift and
  neither recovers; learned ≈ static post-drift.
- **Both ceilings (harness / pre-registration):** `sustained = false` at every rung for every arm
  in every seed (0 of 96 win/adversarial rung-arm rows), because the service ceiling rejects ~17-40
  windows per rung (the per-group `fdatasync` device floor vs a fixed 10 ms). The harness's T1
  verdict is `NOT-MEASURABLE` and its T4 verdict is `NOT-MEASURABLE` (empty `recovery_windows`) —
  the both-ceilings/`sustained` artifact and the region-re-exploration reading, not the lag-only /
  throughput answers. The open §7.1 primary-vs-service reconciliation is unchanged and not edited.

## Accounting (ticket 72 convention, stated)

The reorganisation metric used for T2/T3 above is `summary.compact_bytes_total` (the driver's run
total that includes the final drained merge applied after the window loop), NOT the per-region
per-window `compaction_bytes` sum, which undercounts by that final merge. Evidence on the drift
`declared` arm: seed 42 `summary.compact_bytes_total = 266,220 B` while the per-region sum is 0 B
(the harness's `reorg_bytes=0` log line uses the undercounting sum). The learner's churn budget on
the drift run is derived from the per-region sum (0 B/window), so the drift learner is reorg-frozen
at 0 — a consequence of including `declared` in the drift run and of declared's near-zero drift
reorg, recorded honestly below.

## Honest verdict (pilot-grade)

**8 lock-protected harness invocations = 4 seeds x (win+adversarial 6 arm-runs + drift 3 arm-runs)
= 36 arm-runs, 12.25x cold instance, shared box (loadavg ~2.9-5.9, quiet), one flashdb process at
a time under `MemoryMax=128MiB`. NOT a 10-seed pre-registered verdict.**

1. **Rung-50 lag-rej = 0: NOT reached.** static-derived 2-3/40 (median 2.5, one 0/40 in seed 44),
   learned 3-6/40 (median 3.5), declared 39-40/40. The residual is scattered mid-run windows
   (compaction-adjacent cold reads + device fsync tail + shared-box noise), not a named engine
   mechanism; the rewarm-repair build-flush bound (ticket 76) did not move rung 50 to 0/40.
2. **Rung 100 IS lag-sustained (§7.1) — and rung 200 now is too (static).** static-derived 0/20 at
   rung 100 and 0/10 at rung 200 in 4/4 seeds; learned 0/20 at rung 100 in 3/4 and 0/10 at rung
   200 in 2/4. declared never sustains any rung.
3. **T1 throughput win preserved:** learned/static ~105-107 rps vs declared 71-75 rps (+41.3% to
   +50.7% aggregate achieved, median +47.6%), outside the ~4.0% measured throughput noise band. As
   a sustained-load level, learned/static sustain rung 100-200 where declared never sustains any
   rung (unbounded margin).
4. **T2:** rung-50 lag learned median 1.5/40 vs declared 2/40 (at/better on 4/4 seeds); 0 index
   builds on the 0-scan region; reorg ratio median 0.933x, max 0.995x (<= 1.5x).
5. **T3:** win reorg ratio median 0.767x, max 0.941x (every run <= 1x, <= 3x); the single-region
   blowup does NOT recur (`U|Stock` 5.13-5.23 MB / 1 merge each).
6. **T4 (write-spike): EVALUABLE and measured NOT SUPPORTED** — a real ~9% capacity drop exists,
   neither arm recovers, learned ≈ static post-drift (gap +2%), `recovery_windows = {}` in all 4
   runs.

**What the evidence supports:** the fully integrated tree (tile-cap fix + write-spike drift shape +
rewarm repair + all prior fixes) reproduces and extends the ticket 71/78 result in one pass: the
layout axis gives a reproducible +41-51% achieved-throughput win, static-derived is lag-sustained
at rung 100 AND 200 (4/4), learned at rung 100 (3/4), rung 50 is still held back by a 2-6-window
residual, and the write-spike T4 is a measured null.

**What it does NOT support:** a lag-only sustained score at rung 50; a both-ceilings sustained
score at any rung (service ceiling device-bound, 0/96 win/adversarial rows); a 10-seed
pre-registered verdict (3 independent seeds + 1 repeat, shared box).

## New defects

None introduced. No new correctness, durability, or amplification defect appeared in the 36
arm-runs (0 error windows everywhere, `cgroup_peak` = the 128 MiB cap on every run, the cadence
guard refused nothing, `commit_rate = 1.0` everywhere). Achieved fraction was <= 1.0005 at all but
one rung: seed 42-repeat win/declared rung-200 reached 1.0005 (`rate_exceeded=false`, inside the
tolerance) — not a defect. One observation, not a defect: learned's win reorg is bimodal (0.55-0.60x
in 2/4 seeds, 0.93-0.94x in 2/4) from the slice-39 churn budget deferring the ~11.37 MB
`U|Customer` merge depending on box-load-dependent service-time observations, always <= declared
and with no throughput change (ticket 78's observation, here 2/4 instead of 1/4).

## Gates (verbatim, final HEAD, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
   Compiling digest v0.10.7
   Compiling sha2 v0.10.9
   Compiling serde_derive v1.0.229
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/worktrees/flashdb/wave23-final-eval/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 11.00s
```
2. `cargo test --workspace --offline` — totals: **308 passed; 0 failed; 0 ignored**
(131 lib + 23 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 2 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests). The rewarm-repair tree
has +2 lib tests over ticket 78's 306 (ticket 76's `adopt_snapshot_rewinds_the_warm_cursor_only_when_the_tile_set_changes`
and `streaming_writer_periodic_sync_is_byte_identical`; lib 129 -> 131).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
146 passed in 6.03s
```
4. `git diff --check` — silent (exit 0).

## Residual gaps

- **Rung 50 is still not lag-sustained** (static 2-3/40, learned 3-6/40); the residual is scattered
  mid-run windows under shared-box load + the device fsync tail, not a named engine mechanism. The
  first positive lag-only sustained rung is **rung 100** (static 4/4, learned 3/4), and now also
  **rung 200 for static-derived (4/4)**.
- **The drift learner's reorg budget is 0 B/window** because `declared` was included in the drift
  run and its observed drift reorg is ~0 (declared saturates ~118 rps and the write-heavy drift
  never reaches a merge trigger). The learner still adopts the `order_no` index (demand-gated,
  `index_order_no=True` final policy) but is reorg-frozen, so the empty `recovery_windows` on the
  drift run is budget-confounded, not a clean "no drift detected". Ticket 78 (drift run WITHOUT
  declared, learner budget=None) already showed the same null cleanly, so the throughput null is
  robust; this is stated, not hidden.
- **learned's demand-gated window 0-2** (index OFF until the first scan-demand flip) plus box noise
  remains the systematic difference between learned and static-derived at rung 50/100/200.
- **Service ceiling remains device-bound** (both-ceilings 0/96 win/adversarial rows; device fsync
  p50 6.6 ms / p99 13.4 ms vs a fixed 10 ms). The open §7.1 reconciliation is untouched.
- **3 independent seeds + 1 repeat, not 10; shared box** (loadavg ~2.9-5.9, quiet). All timing/count
  numbers are pilot-grade.
