# 59 - T1/T2/T4 evaluation PILOT under the fixed engine (wave8 WP-H)

## Contract

This is a **pilot**, not the pre-registered 10-seed verdict. Its job is to get fresh, honest data
on whether the pre-registered thesis (`.scratch/15-objective-v0/pre-registration.md`) is still live
under the fixed engine, and a measured noise band, so the full 10-seed campaign can be planned. No
number here may be labelled as a 10-seed verdict; every number names its seed count and conditions.

Scope (from the work-package dispatch):

- **Instance:** `/tmp/cold51/per_region-mixed`, the only >=10x instance
  (12.25x = 1,643,925,811 B / 128 MiB). The 8.17x cold44 instance is below the pre-registration
  floor and is therefore not used.
- **Workloads:** `win` first (the T1 workload), then `adversarial` (the T2 workload) if time
  permits. Drift (T4) is out of scope of this pilot: its 2-window recovery target is unreachable
  at the published learner defaults (6 windows; ticket 58), so T4 stays underived here.
- **Arms:** `declared`, `static-derived`, `learned`. The `oracle` arm is not run (it is a
  hindsight regret reference, not a threshold). The learner runs at one lambda, `1.0`
  (~= device-cost pricing; `DEFAULT_LAMBDA_NS_PER_BYTE` = 1.148 ns/byte). The lambda sweep
  (0.1/1.0/10.0) is a dimension deferred to the full campaign; it is a policy choice, not a
  measurement.
- **Rungs:** the full harness ladder `50,100,200,400` rps in one process per arm (rung 50 is the
  acceptance rung; the higher rungs are free extra evidence the harness already produces).
- **Seeds:** 2 independent access seeds (42 and 43) per (arm, workload), via `--seed`. The learner
  tie-break seed stays at its default (`--learner-seed 0`). One run is repeated to measure the
  run-to-run noise band: `declared` on `win` at seed 42, via `--noise-repeat` (same trace, same
  seed, fresh `cp -a` instance, fresh process).
- **Judging views (both reported, per STATUS §7.1 and the open reconciliation):**
  - **§7.1 lag-only:** a rung is sustained when achieved >= 95% of offered, no error window, and
    no lag-ceiling-rejected window (`--lag-ceiling-us 10000`). Service latency is reported
    separately.
  - **Both ceilings (pre-registration):** the harness's own `sustained` flag, which additionally
    requires no service-ceiling-rejected window (`--ceiling-us 10000`).
- **Honesty gates:** if the harness cannot drive an arm (e.g. `--policy-reload` fails), report
  exactly where and stop rather than improvise. A pilot is never a verdict.

## Measurement protocol

Release binaries are built once by the harness (`cargo build --offline --release --bin
flashdb-scale --bin flashdb-load --bin flashdb-replay`). Every arm runs on a fresh `cp -a` of the
base instance, one process at a time under
`systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0` (128 MiB cap). The load
payload (verbatim, produced by `flashdb_load_command`):

```
--rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json --peak-report
--group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region
--policy-file <workdir>/<workload>-<arm>.policy.json --compact-bytes 67108864
[--policy-reload for learned] --ceiling-us 10000 --lag-ceiling-us 10000
```

## Exact commands (the run matrix)

All invocations run from `harness/` as `uv run python -m flashdb_harness.cold_0b --release ...`
with `--instance /tmp/cold51/per_region-mixed --cap-mib 128 --window-ms 1000 --group-requests 64
--checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864 --reference-rate 50
--segment-requests 2000 --ceiling-us 10000 --lag-ceiling-us 10000 --lambdas 1.0 --arms declared
learned static-derived`. The variable parts:

1. **win, seed 42 (+ noise repeat):**
   `--workloads win --seed 42 --noise-repeat --workdir /tmp/wave8-pilot/win-seed42
   --json .scratch/59-eval-pilot/win-seed42.json --report .scratch/59-eval-pilot/win-seed42.md`
2. **win, seed 43:**
   `--workloads win --seed 43 --workdir /tmp/wave8-pilot/win-seed43
   --json .scratch/59-eval-pilot/win-seed43.json --report .scratch/59-eval-pilot/win-seed43.md`
3. **adversarial, seed 42:**
   `--workloads adversarial --seed 42 --workdir /tmp/wave8-pilot/adv-seed42
   --json .scratch/59-eval-pilot/adv-seed42.json --report .scratch/59-eval-pilot/adv-seed42.md`
4. **adversarial, seed 43:**
   `--workloads adversarial --seed 43 --workdir /tmp/wave8-pilot/adv-seed43
   --json .scratch/59-eval-pilot/adv-seed43.json --report .scratch/59-eval-pilot/adv-seed43.md`

Each `--workloads win` invocation runs, in order: `declared`, `learned@1.0`, `static-derived`
(derived from the learned run's burn-in sweep at the lowest rate), and (invocation 1 only)
`declared-repeat`. Each `--workloads adversarial` invocation runs `declared`, `learned@1.0`,
`static-derived`.

## Results (2 access seeds per arm/workload; win declared seed-42 repeated)

### Environment during the runs (the box is shared, and it was loaded)

Every lag/throughput number below is measured under heavy external load that was NOT present
when WP-G measured its 0/40 baseline. Recorded at run boundaries (`.load.log` files under
`/tmp/wave8-pilot/`):

| process | share | note |
|---|---:|---|
| `llama-server` (Qwen3 27B GGUF, `-ngl 99`) | 92-95% CPU, 9.4-9.7 GB RSS | started 01:01, i.e. as the first run began; ran continuously |
| `mpv --fullscreen --loop-playlist` | ~79% CPU | looped video, ran continuously |
| `mdatp` (Microsoft Defender daemon) | ~44% CPU | continuous |
| `.tmp/34-vs2 -parallel 6` (another evaluation) | 419-438% CPU | appeared 01:21, present during win seed-43 and adv seed-42 |

Load average during the runs: **6.2 -> 15.8 -> 5.0 -> 5.8** on 12 cores. Consequence: the
service-latency p99 is inflated (e.g. adversarial rung-50 svc p99 124.7 ms here vs ~11.7 ms in
WP-G), and every timing-derived number must be read against this load. The lag ceiling, however,
is NOT uniformly destroyed: adversarial declared still sustains rung 50 at 0/40 under this load
(see below), which is what lets the win-vs-adversarial contrast be read as a workload property
rather than only as box noise.

### Primary metric, both judging views (c51, 12.25x)

`lag-only sustained` = highest rung with achieved >= 95% of offered, no error window, and
0 lag-ceiling-rejected windows (§7.1). `both-ceilings sustained` = the harness's own flag, which
also requires 0 service-ceiling-rejected windows (pre-registration validity rule). The service
ceiling rejects ~34-40/40 windows at every rung on every arm (the durability-barrier floor), so
**both-ceilings sustained = 0 rps everywhere** — the same open §7.1 reconciliation as WP-G, not
resolved here.

**win (T1 workload), 2 seeds:**

| arm | seed | rung50 lag-rej | rung50 svc-rej | rung50 lag p99 max (us) | lag-only sust (rps) | both sust (rps) |
|---|---|---:|---:|---:|---:|---:|
| declared | 42 | 39/39 | 39/39 | 378,434 | 0 | 0 |
| declared (repeat) | 42 | 39/39 | 39/39 | 376,160 | 0 | 0 |
| learned@1.0 | 42 | 34/35 | 35/35 | 1,215,344 | 0 | 0 |
| static-derived | 42 | 38/38 | 38/38 | 507,473 | 0 | 0 |
| declared | 43 | 40/40 | 40/40 | 156,039 | 0 | 0 |
| learned@1.0 | 43 | 40/40 | 40/40 | 153,577 | 0 | 0 |
| static-derived | 43 | 38/38 | 38/38 | 370,445 | 0 | 0 |

**No arm sustains rung 50 on the win workload under either reading.** This is NOT box noise: the
same binary, same cap, same ceiling, measured under the same (or higher) load, sustains rung 50 on
the adversarial workload (declared 0/40, below). The win workload differs from adversarial only in
that 30% of its requests are `movements_scan` (`find` + `order` + `limit 100`) instead of point
probes. The read-ahead worker (WP-G) warms only unique-prefix lookups; `request_read_lookups`
returns `[]` for the movement-scan shape (code-verified: `engine/src/request.rs:503-546`, test
`request_read_lookups_extracts_the_probes_each_shape_pays` asserts it is empty). So the scan's
full-entity cold reads (`P|StockMovement` read 10.9 GB over the run) still block the single pacer
and breach the lag ceiling. This is a readiness gap, not a thesis verdict.

**adversarial (T2 workload), 2 seeds:**

| arm | seed | rung50 lag-rej | rung50 svc-rej | rung50 lag p99 max (us) | lag-only sust (rps) | both sust (rps) |
|---|---|---:|---:|---:|---:|---:|
| declared | 42 | 0/40 | 40/40 | 6,062 | **50** | 0 |
| declared | 43 | 0/40 | 37/40 | 4,268 | **50** | 0 |
| learned@1.0 | 42 | 8/40 | 38/40 | 103,450 | 0 | 0 |
| learned@1.0 | 43 | 9/40 | 37/40 | 96,103 | 0 | 0 |
| static-derived | 42 | 1/40 | 39/40 | 16,631 | 0 | 0 |
| static-derived | 43 | 2/40 | 34/40 | 45,339 | 0 | 0 |

Per-rung (adversarial): declared rung100 lag-rej 3/20 (seed42) / 1/20 (seed43); rung200 6/10 / 3/10;
rung400 6/6 / 4/5. So declared's lag-only sustained level is **50 rps** (fails rung 100).

### T1 direction (learned vs declared >15% on win)

**UNDERIVABLE at this seed count.** No arm sustains any rung on the win workload (all arms 0 rps
lag-only sustained), because the win workload's movement-scan path is not warmed off the pacer. The
pre-registered T1 comparison cannot be drawn from 0-vs-0; the win workload must first be made
measurable (warm the scan shape, or make the scan cheaper). Achieved throughput at rung 50 is
near-offered for all arms (frac 0.995-1.0), but every rung-50 window is lag-rejected, so it is not
sustained. 2 seeds, 1 lambda (1.0), 1 repeat.

### T2 direction (learned must not lose >5% on adversarial-uniform)

**FAIL direction at 2 seeds** under the §7.1 lag-only reading: declared sustains rung 50 (0/40 both
seeds) while learned does NOT (8/40, 9/40 lag-rejected). Learned's lag-only sustained level is 0 vs
declared's 50 rps — a 100% loss, far outside the 5% threshold. Whole-run achieved throughput is
identical (106-107 rps for all three arms), so the loss is in lag/validity, not throughput. The
mechanism is learner churn: learned wrote **110.7 MB (seed42) / 91.1 MB (seed43)** of reorganisation
bytes vs declared **23.3 / 25.0 MB** (~4.4x / 3.6x), and that extra background compaction saturates
the shared device and causes the extra lag breaches. NOTE: the harness's own `T2` line prints
`PASS, learned loses +0.0%` — that is an artifact of the both-ceilings `sustained` flag making both
arms 0 (0/0 comparison); it is NOT a real no-loss verdict and must not be quoted as one.

### T3 direction (reorganisation <= 3x declared on win)

| run | declared reorg | learned reorg | ratio |
|---|---:|---:|---:|
| win seed42 run 1 (JSON lost; rendered report captured) | 8,024,075 B | 21,742,645 B | 2.71 |
| win seed42 run 2 | 4,302,658 B | 801,319 B | 0.19 |
| win seed43 | 11,530,478 B | 24,585,681 B | 2.13 |

Ratio spans **0.19x to 2.71x** across 3 win runs. All three samples are below the 3x threshold, but
the 14x spread (learned 0.8 -> 24.6 MB across runs of the same seed+workload) means a 2-seed T3
verdict is not stable. The learner's policy trajectory depends on per-window cost observations that
the shared box's timing noise moves, so its reorganisation output is not reproducible run-to-run.
On adversarial the learned churn is clearer and consistent: 110.7 / 91.1 MB vs declared 23.3 / 25.0 MB
(3.6-4.4x, both seeds).

### Zero-gain / write-amp problem from slice 39 — does it persist?

Yes, and it has become a **variance** problem as much as a magnitude problem. Slice 39 reported the
win-workload learned/declared reorganisation at 4.13-4.27x. Under the fixed engine here the win
ratio is 0.19-2.71x (below 3x but non-reproducible), and on adversarial-uniform the learned arm
still churns to 3.6-4.4x declared. The learner's `--learner-reorg-budget-factor 1.0` budget is
derived from declared's own reorg rate, but the budget is a per-region projection of the challenger's
added rate while the engine chooses which region compacts globally, so a churning learner can still
funnel merges into eager neighbours (the slice 37/38 caveat). The churn is the direct cause of the
T2 lag loss above.

### Seed count, run-to-run spread, and full-campaign cost

Runs completed: **13 arm-runs** = win {declared x3 (2 seeds + 1 repeat), learned x2, static x2}
+ adversarial {declared x2, learned x2, static x2}. That is 2 independent access seeds per
(arm, workload) and one repeated (arm, workload, seed) — `declared`/`win`/seed 42 — per the
dispatch floor.

Measured spread (same seed+workload, fresh process):

| quantity | win declared (seed42 x2) | adversarial declared (seed42 vs 43) |
|---|---:|---:|
| whole-run achieved rps | 50.0 vs 69.0 (38%) | 106 vs 107 (1%) |
| reorg bytes | 4.3 MB vs 5.7 MB (31%) | 23.3 MB vs 25.0 MB (7%) |
| rung-50 lag-rej | 39/39 vs 39/39 (stable, both fail) | 0/40 vs 0/40 (stable) |

Harness `noise_band_relative` (win declared repeat, max per-rung achieved diff) = **71.4%**; this is
dominated by the saturated rung-400 and overstates the acceptance-rung noise. The win workload
saturates (whole-run 50-69 rps) and is timing-sensitive; the adversarial workload keeps up
(106-107 rps) and is far more stable.

Per-arm wall time (measured, includes `cp -a` of the 1.6 GB instance + the load run):
win ~2.2-2.5 min; adversarial ~1.3-1.4 min. Release build (one-time): ~13 s.

Full 10-seed campaign cost estimate (10 seeds x {win, adversarial, drift} x
{declared, static-derived, learned@1.0}, no oracle, no lambda sweep):
~90 arm-runs x 1.3-2.5 min = **~2.5-4 hours wall**, plus the drift pre-flight ladders. With the
learned arm swept at 3 lambdas (0.1/1.0/10.0) the count rises to ~150 arm-runs = **~4-6 hours**.
Prerequisite: a quiet box. Under the external load measured here (llama-server 92-95% CPU +
parallel evaluations), the lag metric is not usable for a 10-seed verdict.

### Readiness gaps (blocking the full campaign)

1. **The win (T1) workload is not measurable yet.** Its 30% movement-scan share does cold
   full-entity reads on the pacer (not warmed by the read-ahead worker, which covers only
   unique-prefix lookups), so no arm sustains even the reference rung. Until scans are warmed (or
   made cheaper), T1 cannot be evaluated at all.
2. **The learned arm churns on adversarial-uniform** (3.6-4.4x declared reorg) and that churn loses
   the rung-50 lag ceiling (8-9/40 vs declared 0/40). This is the T2 failure the full campaign
   would confirm or refute at 10 seeds.
3. **Both-ceilings primary is 0 rps everywhere** (service ceiling = durability-barrier floor). The
   §7.1 vs pre-registration reconciliation remains the OPEN AUTHOR ITEM and is a prerequisite for
   scoring the pre-registration's primary metric.
4. **T4 (drift recovery <= 2 windows) is not run** and remains unreachable at the published learner
   defaults (6 windows), unchanged from ticket 58.
5. **T3 is not stable at 2 seeds** (win ratio 0.19-2.71x) because the learner's trajectory is
   timing-dependent on the shared box.

### Honest pilot verdict

NOT a 10-seed verdict. At 2 access seeds on the >=10x c51 instance, under heavy external box load:
(a) the win (T1) workload is currently unmeasurable — no arm sustains rung 50 because the scan path
isn't warmed; (b) the learned arm loses to declared on adversarial-uniform's lag ceiling (0 vs
50 rps lag-only sustained) while writing 3.6-4.4x declared's reorganisation bytes — the T2 failure
direction the learner was designed not to take; (c) T3 is below 3x in all three win samples but with
a 14x run-to-run spread; (d) the run-to-run throughput noise is 38-71% on win and 1% on adversarial.
The thesis is not dead and not alive: it is blocked on the win-workload scan path (readiness gap 1)
and on a quiet-box 10-seed run.

## Dispatch

- Timestamp: 2026-10-14T01:00:00+08:00 (plan commit; runs follow)
- Actor: worker (single writer, `wave8/eval`, worktree `/home/z/vibe/worktrees/flashdb/wave8-eval-pilot`)
- Base HEAD at plan: `29d7658e6751d34db3dc0ee169aac10bce669d62`
