kind:    report
from:    flashdb
to:      root
spend:   machine_time, idle class
id:      answers `inbox/2026-10-21-next-contract.md`; lands the verdict's four defects and M3 clause (b)

# The four defects are cured at source with before/after runs, and M3 clause (b) is `unmeasured` with its instrument built — the box never opened

**Node whose roadmap changes:** `flashdb` (`docs/0_ROADMAP.md`, `docs/0_ROADMAP.md`,
`docs/6_ARCHITECTURE.md`, `AGENTS.md`, `README.md`, and ticket `.scratch/89-m3b-paired-200/`).

## What is now true

1. **The four defects the verdict names are repaired, each with its reproduction run before and after
   at a named revision.** `defect-repros.sh <rev> present|absent` prints each observation and a verdict
   against its criterion and exits 0 only when all four match, so the run is the test: `e5fb0e1`
   present (exit 0), `4eb236d` absent (exit 0). d1 the mailbox README claims four header lines and lists
   four, no `to:` line (direction is the directory); d2 no document claims one loadavg for every log —
   each citation now matches its log's line 1 (13.20 / 21.43 / 23.35 / 8.24), command `head -1
   .scratch/88-m3-notes/logs/*.out`; d3 max-over-rows is cited to `engine/tests/order_no_index.rs:171`
   and `:221`, the tests that bind it (run, 2 passed, `logs/d3-cited-tests.out`), and
   `flashdb-load.rs:3065` keeps only the narrower fact it asserts; d4 `6_ARCHITECTURE.md` §2.1 is the M3
   record's per-phase ledger, its time column marked historical.
2. **`mail/` is `inbox/`, with four header lines.** The two messages were landed (decision 7, the
   defects, the mailbox shape) and deleted; `AGENTS.md`, `README.md`, `evidence.md`, the roadmap and
   the workplan carry the new name; a reference to a deleted message is cited as
   `e5fb0e1:inbox/...` so it still resolves.
3. **M3 clause (b) is `unmeasured`, not deferred.** The gate root granted ran for real: 191 readings
   at 5 s from 22:19:13 to 22:35:05, **zero under 2.0** (min 4.92, max 20.59, longest quiet run 0 of
   the 12 needed). No arm was taken above the trigger, so **no throughput number is claimed
   anywhere**. The pair stays queued behind the same trigger.
4. **The instrument is built and exercised to the edge the box allows**: preflight (including that a
   user systemd scope really enforces 128 MiB), digest-verified restore per arm (refused with a wrong
   record, then passed), both arms with the policy reaching the engine, five-block rotation, and a
   scorer that reuses `cold_0b.summarize_curve` (the reducer behind the 3-of-5) and refuses a vacuous
   `0 >= 0` instead of meeting the clause on it.
5. **The corpus came back and the instance did not.** Seed and rung-200 traces regenerate
   byte-identically (`c8c6bac6…`, `6472cf46…`); the rebuilt instance digests `359c33c7…` /
   1,682,111,093 B against the M1 run's `67b3d10c…` / 1,683,012,213 B. Both arms run the same rebuild,
   so the pair is controlled; it cannot compare its *level* with the M1 3-of-5, and the ticket says so.
6. **No code changed.** `git diff --stat e5fb0e1..HEAD -- engine/ harness/ scripts/` is empty, so the
   suite result the verdict reran (340/0 at `39c537f`) is not invalidated by this advance; the tests
   the roadmap now cites were run here anyway.

## The one command that closes it

```sh
cd /home/z/vibe/flashdb && SNAP_RECORD=.scratch/89-m3b-paired-200/instance-record.json \
  bash .scratch/89-m3b-paired-200/paired-200.sh --wait-min 55
```

It re-checks the trigger itself and exits 4 without running anything if it never holds; 0 = clause (b)
met, 1 = falsified, 3 = the data cannot decide it, 5 = a restore did not verify. Nothing needs
rebuilding first: the trace and instance regenerate from committed specs
(`prepare-instance.sh`), and the release binary is at HEAD.

## What stayed unexercised, and its consequence

The gate's *transition to a scored pair* — the one step that needs the window. The plumbing around it
was run, so a failure there would be a surprise rather than a discovery; the risk left is a pair taken
in a window that is quiet at the gate and not during an arm, which the per-arm loadavg lines in each
`.out` expose. Residual, stated: the rebuilt instance is not the M1 bytes, so a future reader must not
compare the pair's level with the 3-of-5.

## Decision needed: none

Not asking for anything tonight: the block is queued behind a trigger that did not occur, and the
gate refuses rather than reports. If you want M3 closed, the one thing that provides it is a quiet
window on this box or a borrowed machine of the same storage class — nothing in this tree can make one.

Evidence: commits `064f446`, `307ea39`, `f85785f`, `910f49f`, `9a26411`, `020fb65`, `4eb236d`;
`.scratch/89-m3b-paired-200/{issues.md,defect-repros.sh,paired-200.sh,score-pair.py,instance-record.json}`;
`logs/{defect-repros-before-e5fb0e1,defect-repros-after-4eb236d,paired-200-gate,window-observation,prepare-instance,scorer-plumbing-test-on-invalid-83}.out`.

Effort: ~50 minutes of wall clock, idle class, no device held exclusively, one release build, ~3 GB of
regenerable state, no arm run above the trigger and no number claimed from load.
