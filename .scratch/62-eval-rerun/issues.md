# 62 - WP-K: re-run T1/T2 after the WP-I scan-readahead fix and the WP-J learner-churn fix

## Contract

This is a **pilot-grade re-measurement**, not the pre-registered 10-seed verdict. Its job is to
check whether the two blocking findings of the WP-H pilot (`.scratch/59-eval-pilot/issues.md`) are
resolved by the wave9 fixes, on the integrated engine (WP-I scan-warm in the engine, WP-J learner
churn control in the harness). No number here is labelled as a 10-seed verdict; every number names
its seed count, its workload, and its conditions.

The pilot's two blocking findings this package re-measures:

1. **T1 (win) was UNDERIVABLE** — no arm sustained rung 50 because the win mix's ~30%
   `movements_scan` share did cold full-entity reads synchronously on the pacer.
   WP-I (`.scratch/60-scan-readahead/issues.md`) extended the read-ahead warm set to the scan shape
   and found the scan was CPU-bound, not page-in-bound (98.9% cache-hit before AND after), so the
   scan-warm did NOT move the win lag ceiling (38/38 vs 39/40 lag-rej). The exact remaining cause
   was recorded: the scan's inline CPU cost (full-entity cursor walk + per-document JSON parse +
   `order_no` filter on the single pacer). This re-measure checks whether the win workload is now
   lag-sustained for ANY arm on the integrated engine.
2. **T2 (adversarial) was FAIL direction** — learned churned (3.6-4.4x declared reorg, 8-9/40
   lag-rej vs declared 0/40). WP-J (`.scratch/61-learner-churn/issues.md`) made the learner's cost
   signal per-request and median-scored and raised the switch/drift margins to 50%/200%. Its
   pilot result: learned adversarial rung-50 lag-rej fell to 0-1/40 (matching declared), and the
   reorg ratio fell to 0.19-2.40x declared (one late-rung 2.40x outlier from device-saturation
   drift). This re-measure checks the ratio and lag-sustainability on the integrated engine with
   more seeds.

### Scope

- **Instance:** `/tmp/cold51/per_region-mixed`, the only >=10x instance (12.25x =
  1,643,925,811 B / 128 MiB). Fresh `cp -a` per arm via the harness.
- **Arms:** `declared`, `learned@1.0`, `static-derived` (derived from the learned burn-in sweep).
  `static-derived` is included because it costs ~1 extra arm-run per (workload, seed) — ~50% more
  wall-clock, not a doubling — and it is the T4-context reference. The `oracle` arm is not run
  (hindsight regret reference, not a threshold).
- **Workloads:** `win` (T1) and `adversarial` (T2). Drift (T4) is out of scope (unchanged:
  the 2-window target is unreachable at published defaults, ticket 58).
- **Rungs:** the full harness ladder 50/100/200/400 rps (rate multiples 1/2/4/8 of the 50 rps
  reference).
- **Seeds:** 3 independent access seeds (42, 43, 44) per (arm, workload), plus seed 42 repeated in
  full as the noise band. More if wall-clock allows.
- **Judging views (both reported):**
  - **§7.1 lag-only:** a rung is sustained when achieved >= 95% of offered, no error window, and
    no lag-ceiling-rejected window. Service latency is reported separately. Computed by hand from
    the committed JSON (`achieved_fraction`, `error_windows`,
    `lag_ceiling_rejected_windows`, `rate_exceeded`), NOT from the curve's `sustained` flag.
  - **Both ceilings (pre-registration):** the harness's own `sustained` flag, which additionally
    requires no service-ceiling-rejected window. This is expected to be 0 rps everywhere (the
    per-group `fdatasync` durability floor) and is flagged as the open §7.1 reconciliation.
- **Honesty gates:** if the harness cannot drive an arm, report exactly where and stop rather than
  improvise. A pilot is never a verdict.

## Measurement protocol

Release binaries are built once by the harness (`cargo build --offline --release --bin
flashdb-scale --bin flashdb-load --bin flashdb-replay`). Every arm runs on a fresh `cp -a` of the
base instance, one process at a time under `systemd-run --user --scope -p MemoryMax=134217728 -p
MemorySwapMax=0` (128 MiB cap), enforced by the harness. The load payload (verbatim, produced by
`flashdb_load_command`):

```
--rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json --peak-report
--group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region
--policy-file <workdir>/<workload>-<arm>.policy.json --compact-bytes 67108864
[--policy-reload for learned] --ceiling-us 10000 --lag-ceiling-us 10000
```

The learner runs at one lambda, `1.0` (`--lambdas 1.0`). Learner tie-break seed stays at its
default (`--learner-seed 0`). Churn-control parameters stay at their post-WP-J defaults
(`--learner-cooldown-windows 3`, `--learner-reorg-budget-factor 1.0`; switch margin 50%, drift
margin 200% live inside `LayoutLearner`).

## Exact commands (the run matrix)

All invocations run from `harness/` as `uv run python -m flashdb_harness.cold_0b --release ...`
with `--instance /tmp/cold51/per_region-mixed --cap-mib 128 --window-ms 1000 --group-requests 64
--checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864 --reference-rate 50
--segment-requests 2000 --ceiling-us 10000 --lag-ceiling-us 10000 --lambdas 1.0 --arms declared
learned static-derived --workloads win adversarial`. The variable parts:

1. **seed 42:** `--seed 42 --workdir /tmp/wave10-eval/seed42
   --json .scratch/62-eval-rerun/seed42.json --report .scratch/62-eval-rerun/seed42.md`
2. **seed 43:** `--seed 43 --workdir /tmp/wave10-eval/seed43
   --json .scratch/62-eval-rerun/seed43.json --report .scratch/62-eval-rerun/seed43.md`
3. **seed 44:** `--seed 44 --workdir /tmp/wave10-eval/seed44
   --json .scratch/62-eval-rerun/seed44.json --report .scratch/62-eval-rerun/seed44.md`
4. **seed 42 repeat (noise band):** `--seed 42 --workdir /tmp/wave10-eval/seed42-repeat
   --json .scratch/62-eval-rerun/seed42-repeat.json --report .scratch/62-eval-rerun/seed42-repeat.md`

Each invocation runs, in order: win {declared, learned@1.0, static-derived}, then adversarial
{declared, learned@1.0, static-derived} (6 arm-runs per invocation; 24 arm-runs total). The repeat
is a full re-run of seed 42 (same trace seed, fresh `cp -a` instance, fresh process) so the noise
band covers both workloads and both arms. The harness's `--noise-repeat` flag is NOT used (it only
repeats win/declared); the noise band is computed from the committed JSONs.

Load is recorded at every run boundary: `/proc/loadavg` plus the top CPU consumers, before and
after each invocation, into `.scratch/62-eval-rerun/load.log`.

## Base and environment at plan time

- Base HEAD: `66e43c133f33ba9d6f5317a690af33ee2a9b58a5` (integrated master = wave8/eval + wave9/scanwarm + wave9/churn).
- Both WP-I (engine scan-warm) and WP-J (harness learner churn control) are ancestors of this HEAD.
- Box at plan time (recorded 2026-10-14T02:55+08:00): 12 cores, load average 5.02 / 5.42 / 5.52;
  `llama-server` ~27% CPU / 7.3 GB RSS, `mpv` ~100% CPU, `wdavdaemon` (mdatp) ~181% CPU,
  `evalplus` ~91% CPU. The box is shared and noisy; every timing-derived number must be read
  against this load.
- Disk: 211 GB free on `/tmp` (same volume).

## Results (4 runs = 3 independent access seeds 42/43/44 + seed 42 repeated, 12.25x c51)

Runs completed: **24 arm-runs** = 4 invocations × 6 arm-runs
(win {declared, learned@1.0, static-derived} + adversarial {declared, learned@1.0, static-derived}).
Raw artifacts: `seed42.json`, `seed43.json`, `seed44.json`, `seed42-repeat.json` (plus rendered `.md`
and per-invocation `.stdout.log`), committed here. Load recorded at every run boundary in
`load.log`.

### Environment during the runs (shared, noisy box)

Load average over the four invocations: **4.59 -> 8.82 -> 5.49 -> 7.17 -> 6.77** on 12 cores
(snapshots at `.scratch/62-eval-rerun/load.log`). Present throughout: `llama-server` (95-371% CPU,
7.8-16.4 GB RSS), `mpv` (~83% CPU), `wdavdaemon`/mdatp (41-58% CPU), `ghostty` (~30% CPU). A
`chrome-headless` batch appeared during seed44. Every timing-derived number below must be read
against this load; the lag ceiling is not uniformly destroyed by it (adversarial static-derived
still holds 0/40 at rung 50 on all four runs), which is what lets the win-vs-adversarial contrast
be read as a workload property rather than box noise alone.

### Primary metric, both judging views

`lag-only sustained` (STATUS §7.1) = a rung with achieved >= 95% of offered, no error window, and
0 lag-ceiling-rejected windows, computed by hand from the committed JSON. `both-ceilings
sustained` = the harness's own `sustained` flag (additionally 0 service-ceiling-rejected windows).
The service ceiling rejects ~34-40/40 windows at every rung on every arm (the per-group `fdatasync`
durability floor, STATUS §3), so **both-ceilings sustained = 0 rps everywhere** — the same open
§7.1 reconciliation as WP-G/WP-H, not resolved here.

**win (T1 workload), rung-50 lag-rejections:**

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 40/40 | 40/40 | 40/40 | 39/39 |
| learned@1.0 | 40/40 | 40/40 | 40/40 | 39/39 |
| static-derived | 39/40 | 40/40 | 40/40 | 39/39 |

**No arm lag-sustains rung 50 on win.** Achieved throughput at rung 50 is near-offered for every
arm (frac 0.997-1.000), but every rung-50 window is lag-rejected. Higher rungs also fail
(declared frac 0.76-0.91 at rung 100, ~0.18-0.22 at rung 400). This is the WP-I exact remaining
cause, unchanged: the win mix's ~30% `movements_scan` share runs a full-entity
`P|StockMovement` primary scan (10.86 GB logical reads per run) whose inline CPU cost (cursor walk
+ per-document JSON parse + `order_no` filter on the single pacer) saturates the pacer. The scan
warm (WP-I) warms pages, not CPU, so it does not close the gap.

**adversarial (T2 workload), rung-50 lag-rejections:**

| arm | seed42 | seed43 | seed44 | seed42-repeat |
|---|---:|---:|---:|---:|
| declared | 1/40 | 0/40 | 1/40 | 1/40 |
| learned@1.0 | 1/40 | 0/40 | 1/40 | 0/40 |
| static-derived | 0/40 | 0/40 | 0/40 | 0/40 |

learned **matches declared** at rung 50 on every run (0-1/40), versus the pre-fix pilot's 8-9/40.
The rung-50 lag churn the pre-registration disqualifies is gone. The residual 1/40 on seeds 42/44 is
box noise: declared itself shows it, and static-derived holds 0/40 on the same runs.

### T1 verdict (learned beats declared by >15% on win)

**STILL UNDERIVABLE.** No arm sustains any rung on win under lag-only (all arms 0 rps), so the T1
comparison cannot be drawn from 0-vs-0. The scan gap WP-I found is NOT closed. 3 independent seeds
+ 1 repeat, 1 lambda (1.0). This is a readiness gap, not a thesis verdict: the win workload must
first be made measurable (an `order_no` index or an overlapped scan — both recorded by WP-I as
out of scope for a read-ahead package).

### T2 verdict (learned must not lose >5% on adversarial-uniform)

**PARTIAL at 3 seeds + 1 repeat.** Two sub-questions:

1. **Rung-50 lag: FIXED.** learned rung-50 lag-rej 0-1/40 = declared 0-1/40 on all 4 runs (the
   pilot's 8-9/40 vs 0/40 disqualifying gap is gone). Under lag-only, learned's rung-50
   sustainability now tracks declared's exactly (both fail the same 1/40 noise windows on seeds
   42/44, both pass on seed 43).
2. **Reorganisation band: NOT robustly met.** learned/declared reorg ratio on adversarial:

   | seed42 | seed43 | seed44 | seed42-repeat | median |
   |---:|---:|---:|---:|---:|
   | 2.88x | 0.13x | 0.24x | 2.09x | 1.16x |

   Two of four runs (both seed 42) exceed the 1.5x band (2.09x, 2.88x). The churn is a **single
   region**: `P|Customer` oscillates its `merge_min_tiles` and writes 51-62 MB (5-6 merges) versus
   declared's ~25-29 MB. On seed 42 the episode is late-run (52 MB at rung 100 + 15.7 MB at rung
   200), and it leaks into higher-rung lag: learned rung-100 lag-rej 9/20 vs declared 1/20. So
   rung 50 is clean but the WP-J residual (device-saturation drift re-committing a different
   `P|Customer` threshold) is still live and still measurable.

The harness's own `T2` line prints `PASS, learned loses +0.0%` — that is the both-ceilings 0/0
artifact (both arms `sustained` = 0 because the service ceiling rejects every rung), NOT a real
no-loss verdict. It must not be quoted as one.

### Slice-39 zero-gain problem — persists (numbers)

win learned/declared reorg ratio:

| seed42 | seed43 | seed44 | seed42-repeat | median |
|---:|---:|---:|---:|---:|
| 0.28x | 0.38x | 8.37x | 0.38x | 0.38x |

Three of four runs put learned **below** declared (0.28-0.38x — a mild reorganisation efficiency
win), but that gives **no lag/throughput gain** (both arms still fail rung 50, see T1). One run
(seed44) churns: `P|Customer` writes 72.5 MB (7 merges) — 63.6 MB of it **during rung 50** plus
10.5 MB at rung 400 — for an 8.37x declared ratio, a **T3 FAIL** (`learned reorg 74,143,790 B vs
declared 8,859,979 B`). So the slice-39 finding has changed shape: it is no longer a systematic
4.13-4.27x, it is now a mostly-below-declared baseline with a **rare but severe churn blowup** that
can land in the acceptance rung itself.

### New defect introduced by the fixes?

No new defect class. WP-I's scan-warm is inert on adversarial (no regression, matches WP-I's
record) and necessary-but-insufficient on win. WP-J's churn is reduced in frequency but not
eliminated: the same `P|Customer` threshold oscillation persists, and the win seed44 case shows it
can fire at rung 50 (the acceptance rung) rather than only late-run. The 200% drift margin makes
the episodes rarer but does not remove the underlying single-region oscillation.

### Wall-clock cost per seed, and the full-campaign estimate

Per seed (6 arm-runs = 3 arms x 2 workloads): seed42 10m01s, seed43 9m36s, seed44 10m10s,
seed42-repeat 10m17s — **~10 min per seed**, i.e. ~1.6 min per arm-run (win ~2 min, adversarial
~1.3 min, each including the `cp -a` of the 1.6 GB instance). Release build is one-time (~13 s).

Full 10-seed pre-registered campaign: 10 seeds x {win, adversarial, drift} x
{declared, learned, static-derived} = 90 arm-runs ≈ **~2.5 hours**, plus the drift pre-flight
ladder; with a lambda sweep (0.1/1.0/10.0) it rises to ~150 arm-runs ≈ **~4-6 hours**. The
binding prerequisite is unchanged from the pilot: a quiet box (the lag metric is not usable under
llama-server + mpv + parallel evaluations for a 10-seed verdict).

### Honest verdict

NOT a 10-seed verdict. At 3 independent access seeds + 1 repeat on the 12.25x c51 instance, under
shared-box load: (a) **T1 is still underivable** — the win scan gap is not closed (no arm sustains
rung 50, 39-40/40 lag-rej); (b) **T2's disqualifying rung-50 lag churn is fixed** (learned 0-1/40
= declared), but **the reorganisation churn is not robustly fixed** — learned exceeds declared's
band (2.09x, 2.88x) on both seed-42 runs via a single-region `P|Customer` oscillation, and one win
run (seed44) churns to 8.37x during rung 50 (T3 FAIL). The learner still does not beat declared on
win, and its reorg cost is still not reliably within declared's band on adversarial.

## Verbatim project verification (final HEAD, run after the matrix)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 10.37s
```
2. `cargo test --workspace --offline` — totals: **293 passed; 0 failed; 0 ignored**
(126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 11 group_commit + 6 manifest_engine + 7 merge_threshold + 14 metrics + 33 multi_tile + 7 order_flow
+ 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction + 6 region_partition +
3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout + 12 tile_read +
0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
134 passed in 5.23s
```
4. `git diff --check` — silent (exit 0).
