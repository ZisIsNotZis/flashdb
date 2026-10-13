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

## Results (filled after the runs)

_(per-arm/per-rung tables and the two judging views, T1-T4 directions, noise band, and the full
campaign cost estimate are appended below as the runs complete.)_

## Dispatch

- Timestamp: 2026-10-14T01:00:00+08:00 (plan commit; runs follow)
- Actor: worker (single writer, `wave8/eval`, worktree `/home/z/vibe/worktrees/flashdb/wave8-eval-pilot`)
- Base HEAD at plan: `29d7658e6751d34db3dc0ee169aac10bce669d62`
