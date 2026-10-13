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

## Results

(To be filled after the runs.)

## Verbatim project verification

(To be filled at final HEAD.)
