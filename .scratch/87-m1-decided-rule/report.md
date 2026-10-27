# 87 — the saved M1 records read under the decided lag-only rule

**The one sentence: the README's "3 of 5" does not change.** At 200 rps `flashdb-declared` sustains
**3 of 5** paired blocks under the decided lag-only rule against **sqlite 5/5** and **postgres 5/5**,
exactly as the both-ceilings reducer reported — and it cannot change, because **no saved M1 arm-run
was ever scored with a service ceiling**: both record-producing paths call
`summarize_curve(windows, None)` (`harness/flashdb_harness/baseline_0b.py:1228` for the shared SQL
pacer, `:1869` for the flashdb arm) and all 45 window files carry `"service_ceiling_us": null`. The
"both ceilings" label on these records was therefore already the lag rule. **0 of 45 rung rows differ
between the two verdicts** — and 0 of 47 saved arm-runs once the two `probe-checkpoint` runs are read
too (§1.1).

Status 2026-10-21, WP-12 of `docs/0_ROADMAP.md`, revision `1f31c44`. No arm, rung or engine change
was run and no record was edited: every number below is re-derived from the committed window samples
in `.scratch/82-controlled-m1/runs/**` by `analysis.py`.

## 1. Per arm per rung, both readings, with their provenance

Rule read from `docs/2_GOAL.md` §1 / row `T-84`: `sustained_primary` = achieved within 5% of offered
`AND` zero error windows `AND` zero windows rejected by the **lag** ceiling `AND` not rate-exceeded;
`service_ceiling_reported_windows` is reported and never vetoes. `sustained` is the both-ceilings
reading (`cold_0b.summarize_curve`'s unchanged expression), recomputed here from the same windows and
compared with the record's stored value.

| arm | rung | blocks | `sustained` (both ceilings) | `sustained_primary` (lag rule) | `service_ceiling_reported_windows` (sum) | lag p99 us per block | verdicts differ |
|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 5 | 5/5 | 5/5 | 0 | 1038, 3894, 1055, 846, 883 | 0 |
| sqlite | 50 | 5 | 5/5 | 5/5 | 0 | 375, 577, 2544, 862, 515 | 0 |
| postgres | 50 | 5 | 5/5 | 5/5 | 0 | 1289, 1142, 385, 205, 266 | 0 |
| flashdb-declared | 100 | 5 | 5/5 | 5/5 | 0 | 857, 371, 2288, 1401, 451 | 0 |
| sqlite | 100 | 5 | 5/5 | 5/5 | 0 | 251, 308, 722, 231, 192 | 0 |
| postgres | 100 | 5 | 5/5 | 5/5 | 0 | 264, 290, 775, 171, 257 | 0 |
| flashdb-declared | 200 | 5 | **3/5** | **3/5** | 0 | 7272, **30430**, **33747**, 3088, 8389 | 0 |
| sqlite | 200 | 5 | 5/5 | 5/5 | 0 | 79, 33, 170, 164, 151 | 0 |
| postgres | 200 | 5 | 5/5 | 5/5 | 0 | 968, 274, 162, 906, 1731 | 0 |

Provenance, per row: `sustained` ← `runs/b<block>/rung-<rung>/run-<arm>.json` field
`curve[0].sustained` (recomputed from `runs/b<block>/rung-<rung>/windows-<arm>.jsonl` and
**0 of 45 rows disagree** with the stored value on `achieved_rps`, `lag_p99_us`, `p99_us` or
`sustained`); `sustained_primary` and `service_ceiling_reported_windows` ← the same windows through
`cold_0b.summarize_curve(windows, None)`, which the records did not carry (`sustained_primary` is
absent from 45 of 45 `run-*.json` files, and `cold_0b`'s `service_ceiling_rejected_windows` is `0`
in every stored `curve[0]`). The 45-row, per-block table is Appendix A; the machine-readable form is
`rows.json`.

`sustained` is byte-identical to what the records were scored with, as `T-84` claims: the expression
was not touched by commit `4916fb7` (its diff to `cold_0b.py` adds `sustained_primary` and the
constants beside the unchanged `sustained` line), and the recomputation agrees with all 45 stored
verdicts.

The 45-row, per-block table — every value with the file and field it came from — is **Appendix A**
(`rows.md`, machine-readable `rows.json`).

### 1.1 The two other saved arm-runs, so the inventory is complete rather than implied

`runs/probe-checkpoint/` holds two sqlite arm-runs (the `wal_autocheckpoint` A/B of
`.scratch/82-controlled-m1/report.md` §9.2). They are not rungs of the ladder — one arm, one offered
rate, two settings — so they are reported beside the 45 rows rather than in them:

| workdir | arm | rung | `sustained` | `sustained_primary` | `service_ceiling_reported_windows` | lag p99 us | service p99 us |
|---|---|---|---|---|---|---|---|
| `runs/probe-checkpoint/autocheckpoint-0/run-sqlite.json` | sqlite | 50 | yes | yes | 0 | 2,354 | 20,503 |
| `runs/probe-checkpoint/autocheckpoint-1000/run-sqlite.json` | sqlite | 50 | yes | yes | 0 | 1,206 | 272,442 |

Both verdicts agree with their stored `curve[0].sustained`, so **47 of 47 saved arm-runs are unchanged
by the decided rule**. `runs/snapshot/*.json` are instance snapshots and `runs/floor/*/floor.json` are
device-floor benches, not arm-runs; neither is scored. The floor benches do carry windows, but they
carry no arm and no verdict, so reading them under either rule yields nothing to compare.

## 2. The differing rows, and every rejected window

**Rows where the two verdicts differ: 0 of 45.** Every `service_ceiling_reported_windows` is 0, in
every row, because the service branch never had a ceiling to fire against (see §4).

The only rejections in the whole run are flashdb's two — **both decided by the lag ceiling**, each a
single window out of 20:

| block/rung/arm | window | reject reason | window lag p99 | window service p99 | window `group_elapsed_us` | ceiling |
|---|---|---|---|---|---|---|
| b2 / 200 / flashdb-declared | 10 of 20 | `lag_ceiling` | **30,430 us** | 569,724 us | 573,458 us | lag 10,000 us, exceeded by 20,430 us (3.04x) |
| b3 / 200 / flashdb-declared | 10 of 20 | `lag_ceiling` | **33,747 us** | 580,299 us | 583,421 us | lag 10,000 us, exceeded by 23,747 us (3.37x) |

The overshoot IS the ~575 ms group of `.scratch/82-controlled-m1/report.md` §9.5, which these two
window records localise to window 10 of 20 in both blocks. `lag_saturation` and `errors` fired nowhere.

### The service branch, sized (counterfactual, not the decided rule)

To say *how much* the rejected ceiling would have mattered had it been switched on, the same windows
were re-scored with `summarize_curve(windows, 10_000)` — `cold_0b`'s fixed service ceiling, the value
`docs/9_IMPLEMENTATION.md` row `PLAN-ceiling-reconciliation` states as "a fixed 10 ms":

| arm | rung | `sustained_primary` (decided rule) | both ceilings at a 10 ms service ceiling | service-rejected windows (sum) |
|---|---|---|---|---|
| flashdb-declared | 50 | 5/5 | **0/5** | 14 |
| sqlite | 50 | 5/5 | **1/5** | 6 |
| postgres | 50 | 5/5 | **1/5** | 4 |
| flashdb-declared | 100 | 5/5 | **1/5** | 13 |
| sqlite | 100 | 5/5 | **1/5** | 8 |
| postgres | 100 | 5/5 | **1/5** | 4 |
| flashdb-declared | 200 | 3/5 | **0/5** | 34 |
| sqlite | 200 | 5/5 | **0/5** | 21 |
| postgres | 200 | 5/5 | **0/5** | 6 |

**38 of 45 rows** flip under that ceiling, all in the same direction (`sustained` yes → no). It is not
a ranking device: at 200 rps **all three arms read 0/5**, and at 50/100 rps sqlite and postgres read
1/5 against flashdb's 0/5 and 1/5 — the ceiling is device-bound (the machine's own fsync p99 is
9,821 us) and sits below every arm's per-request service p99 (flashdb 8.7-446 ms, sqlite 9.4-75.9 ms,
postgres 6.4-51.9 ms; 40 of 45 rung-level rows are above 10,000 us and 36 of 45 above
10 ms + the 9,821 us floor). That is the defect `PLAN-ceiling-reconciliation` records for the
win/adversarial rungs, reproduced here on ticket 82's own records.

## 3. The external comparison under the decided rule

At 200 rps, paired blocks (N = 5, one seed, one frozen 10x dataset, revision `1f31c44`):

| arm | 200 rps, lag rule only | 200 rps, both ceilings as recorded | 200 rps, both ceilings at 10 ms (counterfactual) |
|---|---|---|---|
| flashdb-declared | **3 of 5** (blocks 1, 4, 5) | 3 of 5 | 0 of 5 |
| sqlite | **5 of 5** | 5 of 5 | 0 of 5 |
| postgres | **5 of 5** | 5 of 5 | 0 of 5 |

**The README's "3 of 5" is rule-invariant.** The decided rule does not move it, because the recorded
verdicts were already lag-only; the number that the rule *would* have moved is not a comparison at all
(0/5 for every arm), so no restatement of the headline is needed. What the README line `README.md:73`
should stop implying is the reasoning rather than the number: the 3-of-5 is not "the both-ceilings
number that the lag rule also gives", it is a number no ceiling but the lag ceiling ever entered, and
`flashdb-declared` loses it on two lag breaches inside one ~575 ms group per block, not on service
latency. The service p99 (403 ms median at 200 rps, 5.5-20x above the device floor) is a real defect
reported beside the comparison — under `docs/2_GOAL.md` §0's KPI ladder it is what makes this
comparison a loss on the third KPI, and it is not what rejects the throughput blocks.

## 4. Does `scripts/m1-controlled-score.py` reproduce `table.md` byte-identically? PASS

```
$ python3 scripts/m1-controlled-score.py --root .scratch/82-controlled-m1/runs \
    --out /tmp/wp12-score-report.json \
    --markdown .scratch/87-m1-decided-rule/reproduce/table.reproduced.md \
    --since 2026-10-18T00:02:18        # rc=0, T5 PASS / T6 FAIL / T7 PASS
$ diff .scratch/82-controlled-m1/runs/table.md \
       .scratch/87-m1-decided-rule/reproduce/table.reproduced.md | wc -l
0
```

Both files hash to `cda02ab0…ccad7` (1119 B). The committed `table.md` at HEAD `1f31c44` is blob
`3463319e…ddd6` and the worktree copy is unmodified (`git status` clean for that path), so the diff is
against the committed file, not a stale working copy. Log: `logs/m1-controlled-score.out`; the
reproduced table and the hash line are in `reproduce/`. `T-84`'s second claim therefore holds at HEAD.

## 5. What I could not establish

- **Whether any M1 arm-run was ever scored with a service ceiling.** Searched all 45 saved window
  files: `grep -l '"service_ceiling_us": null' .scratch/82-controlled-m1/runs/b*/rung-*/windows-*.jsonl
  | wc -l` → **45**; `grep -c '"service_ceiling_us": [0-9]' … | grep -v ':0' | wc -l` → **0**. Consistent
  with the two call sites (both `None`) and with `ceiling_us` being `null` inside every stored window
  and curve row. What I cannot establish is that no *other* code path ever wrote an M1 record under a
  ceiling: nothing in the tree does today, and the records predate no such change, but the claim rests
  on the two call sites plus the 45 files, not on a review of every historical revision.
- **Whether `sustained_primary` appears in any saved M1 record.** `grep -l sustained_primary
  .scratch/82-controlled-m1/runs/b*/rung-*/run-*.json | wc -l` → **0**: the reading below is entirely
  re-derived, which is why §1 shows it beside the recomputed `sustained` rather than trusting either.
- **Why the ~575 ms group occurs.** `grep -rl 'stall_cause\|"cause"' .scratch/82-controlled-m1/runs/`
  → **0** files: the records localise the stall to window 10 (573,458 / 583,421 us) and stop there. The
  cause needs the engine's per-region counters or new instrumentation, which this package may not run.
- **Whether 3 of 5 reproduces today, and whether it would reproduce on a quiet box.** Not attempted:
  the contract forbids a new run and `AGENTS.md` forbids the paired 200 rps comparison on this machine.
  `cat /proc/loadavg` at the time of this reading → `11.14 10.29 8.60 8/2316`, which is the state the
  comparison may not be taken in. So the 3-of-5 is a claim about the committed records at `1f31c44`
  and nothing more.
- **Whether the 96 win/adversarial rows of `PLAN-ceiling-reconciliation` behave the same way.** Out of
  scope (another `.scratch/` ticket, and those records were produced by `cold_0b`, which requires
  `--ceiling-us`); the counterfactual in §2 shows the same shape on ticket 82's records but is not
  evidence about those.

## Appendix A — all 45 rows, re-derived, with their provenance

Generated by `analysis.py` from the committed window samples; `rows.md` holds this table on its
own and `rows.json` the same rows machine-readable. `sustained` is re-derived and compared with
the record's stored `curve[0].sustained` (0 of 45 disagree); `sustained_primary` and
`service_ceiling_reported_windows` do not exist in the records and come from the same windows.

| block | rung | arm | `sustained` (both ceilings, as recorded) | `sustained_primary` (lag rule) | `service_ceiling_reported_windows` | lag p99 us | service p99 us | windows | where each value comes from |
|---|---|---|---|---|---|---|---|---|---|
| 1 | 100 | flashdb-declared | yes | yes | 0 | 857 | 446565 | 20 | `runs/b1/rung-100/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-100/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 100 | postgres | yes | yes | 0 | 264 | 32232 | 20 | `runs/b1/rung-100/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-100/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 100 | sqlite | yes | yes | 0 | 251 | 9360 | 20 | `runs/b1/rung-100/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-100/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 200 | flashdb-declared | yes | yes | 0 | 7272 | 469132 | 20 | `runs/b1/rung-200/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-200/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 200 | postgres | yes | yes | 0 | 968 | 51910 | 20 | `runs/b1/rung-200/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-200/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 200 | sqlite | yes | yes | 0 | 79 | 59013 | 20 | `runs/b1/rung-200/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-200/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 50 | flashdb-declared | yes | yes | 0 | 1038 | 88006 | 20 | `runs/b1/rung-50/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-50/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 50 | postgres | yes | yes | 0 | 1289 | 13663 | 20 | `runs/b1/rung-50/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-50/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 1 | 50 | sqlite | yes | yes | 0 | 375 | 64649 | 20 | `runs/b1/rung-50/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b1/rung-50/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 100 | flashdb-declared | yes | yes | 0 | 371 | 201985 | 20 | `runs/b2/rung-100/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-100/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 100 | postgres | yes | yes | 0 | 290 | 21583 | 20 | `runs/b2/rung-100/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-100/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 100 | sqlite | yes | yes | 0 | 308 | 65930 | 20 | `runs/b2/rung-100/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-100/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 200 | flashdb-declared | no | no | 0 | 30430 | 280654 | 20 | `runs/b2/rung-200/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-200/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 200 | postgres | yes | yes | 0 | 274 | 40135 | 20 | `runs/b2/rung-200/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-200/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 200 | sqlite | yes | yes | 0 | 33 | 51717 | 20 | `runs/b2/rung-200/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-200/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 50 | flashdb-declared | yes | yes | 0 | 3894 | 127627 | 20 | `runs/b2/rung-50/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-50/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 50 | postgres | yes | yes | 0 | 1142 | 31536 | 20 | `runs/b2/rung-50/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-50/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 2 | 50 | sqlite | yes | yes | 0 | 577 | 14883 | 20 | `runs/b2/rung-50/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b2/rung-50/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 100 | flashdb-declared | yes | yes | 0 | 2288 | 106414 | 20 | `runs/b3/rung-100/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-100/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 100 | postgres | yes | yes | 0 | 775 | 25680 | 20 | `runs/b3/rung-100/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-100/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 100 | sqlite | yes | yes | 0 | 722 | 45728 | 20 | `runs/b3/rung-100/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-100/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 200 | flashdb-declared | no | no | 0 | 33747 | 129185 | 20 | `runs/b3/rung-200/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-200/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 200 | postgres | yes | yes | 0 | 162 | 50476 | 20 | `runs/b3/rung-200/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-200/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 200 | sqlite | yes | yes | 0 | 170 | 70087 | 20 | `runs/b3/rung-200/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-200/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 50 | flashdb-declared | yes | yes | 0 | 1055 | 106744 | 20 | `runs/b3/rung-50/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-50/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 50 | postgres | yes | yes | 0 | 385 | 40158 | 20 | `runs/b3/rung-50/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-50/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 3 | 50 | sqlite | yes | yes | 0 | 2544 | 39190 | 20 | `runs/b3/rung-50/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b3/rung-50/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 100 | flashdb-declared | yes | yes | 0 | 1401 | 93214 | 20 | `runs/b4/rung-100/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-100/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 100 | postgres | yes | yes | 0 | 171 | 51237 | 20 | `runs/b4/rung-100/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-100/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 100 | sqlite | yes | yes | 0 | 231 | 75918 | 20 | `runs/b4/rung-100/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-100/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 200 | flashdb-declared | yes | yes | 0 | 3088 | 403213 | 20 | `runs/b4/rung-200/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-200/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 200 | postgres | yes | yes | 0 | 906 | 11889 | 20 | `runs/b4/rung-200/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-200/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 200 | sqlite | yes | yes | 0 | 164 | 55082 | 20 | `runs/b4/rung-200/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-200/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 50 | flashdb-declared | yes | yes | 0 | 846 | 109766 | 20 | `runs/b4/rung-50/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-50/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 50 | postgres | yes | yes | 0 | 205 | 14034 | 20 | `runs/b4/rung-50/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-50/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 4 | 50 | sqlite | yes | yes | 0 | 862 | 21768 | 20 | `runs/b4/rung-50/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b4/rung-50/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 100 | flashdb-declared | yes | yes | 0 | 451 | 8753 | 20 | `runs/b5/rung-100/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-100/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 100 | postgres | yes | yes | 0 | 257 | 6422 | 20 | `runs/b5/rung-100/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-100/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 100 | sqlite | yes | yes | 0 | 192 | 66420 | 20 | `runs/b5/rung-100/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-100/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 200 | flashdb-declared | yes | yes | 0 | 8389 | 423237 | 20 | `runs/b5/rung-200/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-200/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 200 | postgres | yes | yes | 0 | 1731 | 26228 | 20 | `runs/b5/rung-200/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-200/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 200 | sqlite | yes | yes | 0 | 151 | 66077 | 20 | `runs/b5/rung-200/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-200/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 50 | flashdb-declared | yes | yes | 0 | 883 | 117454 | 20 | `runs/b5/rung-50/run-flashdb-declared.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-50/windows-flashdb-declared.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 50 | postgres | yes | yes | 0 | 266 | 7870 | 20 | `runs/b5/rung-50/run-postgres.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-50/windows-postgres.jsonl` via `cold_0b.summarize_curve(w, None)` |
| 5 | 50 | sqlite | yes | yes | 0 | 515 | 9378 | 20 | `runs/b5/rung-50/run-sqlite.json` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`; `runs/b5/rung-50/windows-sqlite.jsonl` via `cold_0b.summarize_curve(w, None)` |
