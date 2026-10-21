# 89 — M3 clause (b): the paired 200 rps block, and the four defects the verdict names

Contract: `inbox/2026-10-21-next-contract.md` (root), answering `inbox/2026-10-21-advance-review-verdict.md`.
Both are landed (`docs/roadmap.md` decision 7, `docs/plan/workplan.md` §M3 status, the four defect
repairs) and deleted; this ticket is what the contract asks for.

## What this ticket is for

The verdict says M3 is **not closed**: clause (b) — "at 200 rps the indexed arm sustains at least as
many paired blocks as declared" — has no paired block anywhere in the tree, so it is neither met nor
falsified. The contract grants the quiet window the earlier deferral said would never come, with an
explicit trigger, and forbids deferring (b) a third time: a clause that still cannot be measured is
reported **unmeasured with what would measure it**.

It also names four defects to repair at the source, each with its reproduction run at a named revision.

## The four defects, reproduced before and after

`bash .scratch/89-m3b-paired-200/defect-repros.sh <rev> present|absent` prints each observation and a
verdict against its criterion; the exit code is 0 only when all four match the expected state, so the
run is the test.

| defect | criterion | before `e5fb0e1` | after `4eb236d` | repro |
|---|---|---|---|---|
| d1 mailbox README | the header count it claims equals the lines it lists | claims four, lists five (`kind from to spend id`) | claims four, lists four | `logs/defect-repros-before-e5fb0e1.out`, `-after-4eb236d.out` |
| d2 loadavg claim | a claim about "every log" is traceable to the logs | one mail asserts "loadavg 13.20 on the first line of every log"; the logs read 13.20 / 21.43 / 23.35 and 8.24 | no document claims one reading for every log; each citation matches its log's line 1 | same |
| d3 M3 (a) citation | max-over-rows is cited to the test that binds it | `coverage_csn` = max covered csn attributed to `flashdb-load.rs:3065`, hits for `order_no_index.rs:171`: 0 | attributed to `order_no_index.rs:171` and `:221`; 3065 keeps only "a rebuild advances coverage" | same |
| d4 M4 deliverable | a per-phase ledger for the M1 **and M3** records | `gap-analysis.md` §2 ledgers M1 only | §2.1 is the M3 record's ledger, time column marked historical | same |

The tests d3 now cites were run at this revision rather than assumed:
`logs/d3-cited-tests.out` — `index_build_covers_rows_that_live_only_in_the_memtable` and
`index_holds_one_entry_per_live_row_across_versions_and_deletes`, 2 passed, exit 0.

## The quiet-window condition, as granted

Trigger: `uptime` reads **loadavg under 2 for one minute** and no other agent is writing in this tree.
The reading the block started at is recorded. Nothing else in this ticket waits for that window.

## The two arms and the one thing that changes

Both arms run the M1 record's rung-200 trace (`sha256 6472cf46…`, 4,000 requests — regenerated here and
byte-identical to the committed record) against the same snapshotted instance, one process at a time
inside `MemoryMax=128M`, with the record's own window (1,000 ms), group (64) and lag ceiling (10,000 us):

| arm | policy |
|---|---|
| declared | no `--policy-file` |
| indexed | `{"regions":{"P|StockMovement":{"index_order_no":true}}}` |

Five blocks, arm order rotated (odd declared-first, even indexed-first). Scoring is not re-implemented:
`score-pair.py` feeds the driver's dumps to `cold_0b.window_records` + `summarize_curve`, the reducer
that produced the 3-of-5, and reads the verdict on `sustained_primary` — the decided lag-only rule.
It refuses a vacuous `0 >= 0` rather than meeting the clause on it.

## Regenerable state, and what came back byte-identical

The trace and instance are not committed (`.scratch/82-controlled-m1/runs/.gitignore`); they come back
from committed specs. `prepare-instance.sh` runs the chain and restores the two `spec.json` records the
harness rewrites while regenerating:

- 10x seed trace: **sha256 `c8c6bac6…` — identical to the committed record** (1,414,978,008 B,
  10,800 blocks), so the dataset the pair runs on is the one M1 ran on.
- rung-50 trace: `859b99a1…`, identical to the committed record.
- rung-200 trace: `6472cf46…`, identical; 4,000 requests.
- pre-load instance: snapshotted under `…/runs/snapshot/flashdb-declared` with its digest record.

## State

- Regeneration finished (all four artefacts byte-identical except the instance, see below); the four
  defects reproduced before and after; the instrument exercised end to end on a smoke block and made to
  refuse on unusable data (`logs/scorer-plumbing-test-on-invalid-83.out`, exit 3 — "cannot decide").
- **The pair did not run: the window never opened.** The gate polled `/proc/loadavg` every 5 s for 16
  minutes (191 readings, 22:19:13 → 22:35:05) and **not one reading was under 2.0** — minimum 4.92,
  maximum 20.59, longest consecutive quiet run 0 of the 12 required
  (`logs/paired-200-gate.out`, summary in `logs/window-observation.out`). The gate then waits on
  nobody: it refuses every arm above the trigger, and no arm was taken. The series was stopped on the
  layer above's direction after 16 minutes; the block stays **queued**, not abandoned.

## Verdict: M3 clause (b) is `unmeasured`

Not deferred again, and not met: the clause is `unmeasured`, with the instrument that would measure it
named, built and exercised to the point the quiet box allows.

- **What is missing is the box, not the instrument.** The trigger root granted (loadavg under 2.0 for
  one minute, no other agent writing in this tree) did not occur in 191 consecutive readings; the box
  carried 4.9-20.6 the whole time.
- **What would measure it, in one command** once the trigger holds:
  `SNAP_RECORD=.scratch/89-m3b-paired-200/instance-record.json bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 55`
  — it re-checks the trigger itself, restores the instance per arm with a digest comparison against
  `instance-record.json`, runs five blocks with the arm order rotated, writes each arm's start and end
  loadavg into its own `.out`, and scores with `cold_0b.summarize_curve` on `sustained_primary`.
  Exit 0 = met, 1 = falsified, 3 = the data cannot decide it, 4 = the gate never opened, 5 = a restore
  did not verify.
- **What was exercised, and what was not.** Exercised: preflight (including that a user systemd scope
  really enforces 128 MiB), the digest-verified restore (in the refusing direction with a wrong record,
  then in the passing direction with the regenerated instance), the two arms and the rotated block
  loop, the policy reaching the engine (`engine_policy` line in `runs/smoke-p1-indexed.out`), and the
  scorer on real dumps. **Not exercised: the transition through the gate to a scored pair**, which is
  the one thing that needs the window; no throughput number is claimed anywhere in this ticket.
- **The instance is a rebuild, not the M1 bytes.** The seed and rung-200 traces come back byte-identical
  (`c8c6bac6…`, `6472cf46…`), but the rebuilt instance digests `359c33c7…` / 1,682,111,093 B against the
  M1 run's `67b3d10c…` / 1,683,012,213 B (same 158 files; `instance-record.json` vs the committed
  snapshot record). Both arms run on the same rebuild, so the pair is controlled; what it cannot do is
  compare its *level* against the M1 3-of-5, because the instance's bytes differ. The clause asks only
  for declared vs indexed in the same block, which the pair does answer.
