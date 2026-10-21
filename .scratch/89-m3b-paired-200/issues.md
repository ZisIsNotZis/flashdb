# 89 — M3 clause (b): the paired 200 rps block, and the four defects the verdict names

Contract: `inbox/2026-10-21-next-contract.md` (root), answering `inbox/2026-10-21-advance-review-verdict.md`.
Both landed in `docs/roadmap.md` decision 7 and `docs/plan/workplan.md` §M3 status before the files were
deleted.

## What this ticket is for

The verdict says M3 is **not closed**: clause (b) — "at 200 rps the indexed arm sustains at least as many
paired blocks as declared" — has no paired block anywhere in the tree, so it is neither met nor
falsified. The contract grants the quiet window the earlier deferral said would never come, with an
explicit trigger, and forbids deferring (b) a third time: a clause that still cannot be measured is
reported **unmeasured with what would measure it**.

It also names four defects to repair at the source, each with its reproduction run at a named revision
(`defect-repros.sh`, logs in `logs/`).

## The quiet-window condition, as granted

Trigger: `uptime` reads **loadavg under 2 for one minute** and no other agent is writing in this tree.
Record the reading the block started at. Nothing else in this ticket waits for that window.

## The two arms

Both are the M1 record's shape at rung 200, same frozen trace, one process at a time in a 128 MiB cgroup:

| arm | policy | command |
|---|---|---|
| declared | none (`--policy-file` absent) | `flashdb-load <instance> <trace> --rate-rps 200 --window-ms 1000 ...` |
| indexed | `{"regions":{"P|StockMovement":{"index_order_no":true}}}` | the same plus `--policy-file policy/indexed.policy.json` |

The pair is taken in blocks (`p1..pN`), arm order rotated, and each arm's start and end loadavg is
written into its own `.out` line 1, so a reader can see whether the block was taken on a quiet box.

## Regenerable state this needs, and where it comes from

The trace and the instance are not committed (`.scratch/82-controlled-m1/runs/.gitignore`): they come
back from the frozen seed trace plus `run-spec.json`. The chain, run in this order:

```sh
# 1. the 10x seed trace (1.41 GB) and rung-50 trace, from its committed spec.json argv
cd harness && uv run python -m flashdb_harness.baseline_0b prepare \
  --workdir ../.scratch/80-external-baselines/runs/m1-10x/rung-50 --seed 42 --requests 1000 \
  --rate 50 --customers 200000 --skus 2650000 --locs 4 --batch 64 --on-hand 1000000
# 2. the rung-200 4000-request trace, hard-linked to that seed
uv run python -m flashdb_harness.baseline_0b prepare \
  --workdir ../.scratch/82-controlled-m1/runs/b2/rung-200 --seed 42 --requests 4000 --rate 200 \
  --customers 200000 --skus 2650000 --locs 4 --theta 1.0 --on-hand 1000000 \
  --seed-trace ../.scratch/80-external-baselines/runs/m1-10x/rung-50/seed.jsonl
# 3. the pre-load flashdb instance, snapshotted
../scripts/m1-baseline-run.sh --mode instance --workdir ../.scratch/82-controlled-m1/runs/b2/rung-200 \
  --arms flashdb-declared --snapshot-dir ../.scratch/82-controlled-m1/runs/snapshot \
  --instance-root ../.scratch/82-controlled-m1/runs/live --seed-cap-mib 512 --skip-build
```

## State

- Regeneration: started at 22:08 (see `logs/prepare-seed.log`); the box read loadavg 10.60 at that
  moment, so nothing here is a timing reading.
- The pair: not yet taken.
