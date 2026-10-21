# 87 — what was measured, and what was not

WP-12 of `docs/plan/workplan.md`. The package reads `.scratch/82-controlled-m1/runs/**` under the rule
decided in `docs/objective.md` §1 and implemented by `T-84`, and reports. **Nothing was run that
produces load:** no arm, no rung, no 200 rps pair, no engine change, no record edited, and no file
outside this ticket written (the score script was pointed at `/tmp` for its JSON output so the
committed `runs/report.json` was not touched).

## Measured

1. **45 rows re-derived** — 5 blocks x 3 rungs x 3 arms — from each arm-run's own
   `windows-<arm>.jsonl`, through `cold_0b.summarize_curve(windows, None)` and through
   `baseline_0b.recompute_records`. `achieved_rps`, `lag_p99_us`, `p99_us` and `sustained` agree with
   the stored `curve[0]` in **45 of 45** rows (`0` differ), so the recomputed `sustained_primary`
   beside them is read off the same evidence the published verdicts were.
2. **The two verdicts differ in 0 of 45 rows**, and `service_ceiling_reported_windows` is `0` in every
   row. The only rejections in the whole run are flashdb's two `lag_ceiling` breaches at 200 rps
   (block 2 and block 3, window 10 of 20: lag p99 30,430 us and 33,747 us against 10,000 us; the
   ~575 ms group — 573,458 us and 583,421 us — is in the same window).
3. **The headline is rule-invariant**: 200 rps, `flashdb-declared` 3 of 5 paired blocks, `sqlite` 5 of
   5, `postgres` 5 of 5, under both readings. The reason is structural, not luck: no saved M1 arm-run
   was scored with a service ceiling at all (`summarize_curve(windows, None)` at
   `baseline_0b.py:1228` and `:1869`; `"service_ceiling_us": null` in 45 of 45 window files;
   `service_ceiling_rejected_windows: 0` in every stored `curve[0]`).
4. **The service branch, sized as a counterfactual** (`summarize_curve(windows, 10_000)`, not the
   decided rule): 38 of 45 rows flip, and at 200 rps every arm reads 0 of 5. That is the
   `PLAN-ceiling-reconciliation` defect on ticket 82's own records — the ceiling cannot rank arms
   because the machine's own fsync p99 (9,821 us) is under it.
5. **`T-84`'s reproduction claim holds at HEAD `1f31c44`**:
   `scripts/m1-controlled-score.py` re-emits `runs/table.md` byte-identically, `diff` = **0 lines**,
   `sha256 cda02ab0…ccad7` on both.

## Not measured (and why)

- **No new run.** Out of contract, and `AGENTS.md` forbids the paired 200 rps comparison on this box;
  `cat /proc/loadavg` at the time of this reading was `11.14 10.29 8.60`.
- **The cause of the ~575 ms group.** The records localise it to one window per block and stop; naming
  the code path needs the engine's per-region counters or new instrumentation.
- **The win/adversarial rows** of `PLAN-ceiling-reconciliation` (96 rows). Another ticket, produced by a
  harness (`cold_0b`) whose `--ceiling-us` is required; not evidence for or against them here.
- **Anything about a quiet box, another machine, or a seed other than the one frozen trace.** The
  records are one seed, one dataset, N=5, and no claim here exceeds them.

## Reproduce

`bash run.sh` (from the repository root). It writes only under this ticket.
