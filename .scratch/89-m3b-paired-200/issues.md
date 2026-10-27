# 89 — M3 clause (b): the paired 200 rps block, and the four defects the verdict names

Contract: `inbox/2026-10-21-next-contract.md` (root), answering `inbox/2026-10-21-advance-review-verdict.md`.
Both are landed (`docs/0_ROADMAP.md` decision 7, `docs/0_ROADMAP.md` §M3 status, the four defect
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
| d4 M4 deliverable | a per-phase ledger for the M1 **and M3** records | `6_ARCHITECTURE.md` §2 ledgers M1 only | §2.1 is the M3 record's ledger, time column marked historical | same |

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

## 2026-10-27 — the gate opened for one minute, and the run was refused for losing it

The trigger finally held on this box: `/proc/loadavg` read 0.82 at 15:29:08 and 0.87 at 15:30:03,
twelve consecutive readings under 2.0, so the gate opened after 55 s of polling and the pair started
inside it (p1 declared 0.88, p1 indexed 1.63). Load then climbed on its own: arms 2-5 started at 2.15,
3.10, 3.19, 3.27, 3.80, 6.34 and 6.75, and the box's 1-minute average stayed 4-7 for the rest of the
afternoon (the 5-minute average was 3.7, i.e. sustained foreign work, not the run's own 20 s blocks —
the copies are btrfs reflinks and every block reported `errors=0`).

**Those arms are refused, not reported.** `AGENTS.md` §The quiet window says "refuse the arm above the
threshold", and `paired-200.sh` did not: it gated once, before the loop, and never looked again. Nine
blocks were spent before that was noticed (the tenth, p5 indexed, was cut mid-arm by the launcher's own
five-minute cap). All of them sit in `runs/invalid-2026-10-27/`, each with `# INVALID - do not cite:` on
its first line, and the scorer's `p*-*.out` glob no longer reaches them (the 3,800-line per-group
`.err` telemetry is kept gzipped beside each `.out`: an invalid block's phase narrative has no
reader, and 6 MB of it would be paid for by every clone).

The instrument now refuses per arm: `await_quiet` runs before the run and before every block, keeps one
deadline for the whole invocation, and on expiry calls `refuse_run`, which moves the partial blocks into
`runs/invalid-<stamp>/` with the INVALID marker and deletes the restored instances. Two runs stand as its
test, both in `logs/paired-200.out`: `--wait-min 0` under loadavg 4.09 exits 4 with
`GATE UNMET (before the run)`, and `--smoke` still exercises restore + both arms + rotation with the gate
skipped (exit 0).

What the invalid blocks hint, and nothing more: per 20-window block, lag-rejected windows were 11, 11,
13, 11, 11 on the declared arm against 5, 2, 5, 5 on the indexed arm. That direction is what clause (b)
predicts, it is consistent across the four `errors=0` pairs that started under load, and it is **not
evidence** — the arms were taken above the trigger, which is the measured cost the twelve
`83-index-probe/runs/` files already record.

**Clause (b) is still `unmeasured`.** One command away, unchanged:
`SNAP_RECORD=.scratch/89-m3b-paired-200/instance-record.json bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 55`.

## WP-17 — the harvest rule: a lost trigger keeps the pairs it completed

The defect WP-17 repairs is not the gate, which is right to refuse an arm above 2.0: it is what the
gate did to evidence already taken. On 2026-10-27 the trigger held for one minute, the first pair ran
inside it (p1 declared start 0.88, p1 indexed start 1.63, both `errors=0`), the box then carried
2.15-6.75, and the old `refuse_run` moved **every** block of the invocation — the completed pair with
the refused ones — into `runs/invalid-2026-10-27/`. A rare window's one complete pair was thrown away
because a later arm was refused.

### The rule now

A pair is **complete valid evidence** only when all four hold:

1. both `pN-declared.out` and `pN-indexed.out` exist;
2. each arm's `# loadavg at start:` reading is **below 2.0** (the granted trigger);
3. each arm's window JSON / `load_summary` reports **`errors=0`**;
4. the restore was verified before the arm — the script compares the copy's tree digest and byte
   total against the snapshot record, and writes `# restore: verified sha256=… bytes=…` into the
   arm's header.

A later trigger loss moves **only the pairs that fail the rule** to `runs/invalid-<stamp>/`, each
prefixed `# INVALID - do not cite:`, and leaves every earlier valid pair in `runs/`. The manifest
`runs/pairs.json` then names, per pair, the two arms' start loadavgs, `errors`, restore state and
valid/invalid with the reason.

**Why the completed pair is evidence and the partial one is not.** A pair is one controlled
comparison: the same frozen trace and the same snapshotted instance, one arm declared and one indexed,
one process at a time. When both arms started under the trigger, the difference between them is
`--policy-file` and the reading is about the index. A half-pair has only one arm, so the comparison
does not exist; a pair with one arm started above 2.0 measures the load in that arm. Neither can be
added to a later invocation's arms and called a pair, which is why the invalid files leave the
scorer's `p*-*.out` glob instead of being annotated in place.

**One pair is still not the clause.** Keep the pre-registered unit: `score-pair.py` counts only
complete valid pairs, prints `partial: N/5` and returns 3 for N < 5, and can return 0/1 only at 5.
The vacuous `0 >= 0` refusal is unchanged.

### Resume: the next invocation continues, never overwrites

Resume is the **default**; `--resume` says so explicitly and `--fresh` refuses when any pair files
already exist (no silent overwrite). On start the script finds the highest contiguous valid pair and
sets `START_PAIR` to the next number. Stale files at or above it (a crash mid-pair, or a refusal from
an earlier invocation) are quarantined with the INVALID marker before anything runs, so a pair number
is never reused. If valid pairs are non-contiguous the script stops with exit 3 rather than score a
mixture. When the target already holds five valid pairs, a re-run runs no arm and goes straight to
scoring — it does not need a quiet window.

### Exit codes

0 met · 1 falsified · 3 cannot decide / preflight / non-contiguous series · 4 the trigger was lost
(gate unmet, or an arm's recorded start loadavg was already ≥ 2.0) · 5 a restore did not verify ·
6 an arm completed but is inadmissible (`errors>0`).
The scorer uses 0/1/3 only; `--arm-facts`, `--pair-valid` and `--manifest` are its machine-readable
modes, and all three read the same parser as the verdict.

### Commands

```bash
# the real pair; resumes by default at the next pair after the valid ones
SNAP_RECORD=.scratch/89-m3b-paired-200/instance-record.json \
  bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 55
# or say resume explicitly / refuse to touch existing pairs
bash .scratch/89-m3b-paired-200/paired-200.sh --resume --wait-min 55
bash .scratch/89-m3b-paired-200/paired-200.sh --fresh  --wait-min 55
# score what is on disk; partial: N/5 and exit 3 below five valid pairs
python3 .scratch/89-m3b-paired-200/score-pair.py .scratch/89-m3b-paired-200/runs
# (re)write the manifest
python3 .scratch/89-m3b-paired-200/score-pair.py --manifest .scratch/89-m3b-paired-200/runs

# the fixture test: no box, no real pair, no engine
bash .scratch/89-m3b-paired-200/fixtures/test-pair-harvest.sh
```

### Fixture evidence

`bash .scratch/89-m3b-paired-200/fixtures/test-pair-harvest.sh` — **ALL CHECKS PASSED**, exit 0.
It writes driver-shaped `pN-*.out` fixtures (`fixtures/fake-arm.sh`, `fixtures/make-fixtures.py`) and
exercises the real script (via `--simulate`, fake loadavg, fixture arms) and the real scorer:

- (a) after two valid pairs the fake box leaves the quiet window; the run exits **4**, keeps
  `p1`/`p2` in `runs/`, and moves exactly **one** file, `p3-declared.out`, into `invalid-<stamp>/`
  with the INVALID marker; the manifest records 2 valid pairs and names p3 invalid.
- (b) the next invocation resumes at p3 (`resuming after 2 valid pair(s); next pair is p3`), reaches
  5 valid pairs, exits 0, and p1/p2 are byte-identical before and after (sha256 checked).
- (c) two valid pairs: `partial: 2/5`, exit 3; four valid pairs: `partial: 4/5`, exit 3.
- (d) five valid pairs: exit 0 (MET in the fixture) and exit 1 in the falsified fixture — both paths
  are exercised; five valid pairs with no sustained block stay exit 3 (vacuous refusal).
- (e) a pair whose declared arm started at 2.50 is excluded and named
  (`p3-declared: start loadavg 2.50 >= 2.0`); a pair whose indexed arm reports `errors=1` is
  excluded and named (`p4-indexed: errors=1 != 0`); only 3 valid pairs remain (`partial: 3/5`,
  exit 3). A standalone `errors>0` arm exits 6 and its pair is quarantined.

### What remains untested, and what is still not claimed

- **The real-box transition through the gate to a scored pair is still unexercised.** There was no
  quiet window; no real paired run was taken, and no throughput number is claimed here.
- The fixture arms are shaped like the driver's dumps but are not the driver; the real arm's argv,
  restore path and `systemd-run` scope were not re-run in WP-17 (they are unchanged from the
  2026-10-27 smoke evidence in `logs/paired-200.out`).
- Resume has not been exercised against real arm files, only fixture files; the parser reads the
  same header fields either way.
- The completed 2026-10-27 p1 pair stays in `runs/invalid-2026-10-27/`. It is not retroactively
  promoted: the old script wrote no `# restore: verified` header, so by the file-level rule above it
  cannot be certified, and that directory is outside WP-17's write scope. The repair applies to runs
  taken from now on.
- Clause (b) remains **unmeasured**, one command away as before.
