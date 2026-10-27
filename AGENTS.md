# flashdb — where a session starts

This checkout is one node's project: it holds its own roadmap (`docs/0_ROADMAP.md`, which also holds
the work packages and the decisions in force) and its mailbox (`inbox/`). A session launched to do a package inside it
writes only the area its contract names and reports to `inbox/`; the top-level docs belong to the node
that owns the project. The law is `/home/z/vibe/agent-policy/AGENTS.md`.

## Start

Read `inbox/` first — a message that arrived while the node was asleep changes what you would do —
then `docs/0_ROADMAP.md` §Where we are (measured), §Current milestone, §M3 status and §Known blockers. Land what a message says in those files
before deleting it: the next session reads the files, not the conversation.

## Where a thing is

- Milestone state, the decisions in force and the work packages with owner, acceptance and spend →
  `docs/0_ROADMAP.md`. The item inventory with leverage ranking, and which claim rests on which code
  path or record → `docs/9_IMPLEMENTATION.md`.
- Where a new fact belongs: the level map and the rejected alternatives are `docs/0_ROADMAP.md` §How
  the doc base is filed; the rule it enforces is `/home/z/vibe/AGENTS.md` §Docs.
- A measured ticket → `.scratch/<nn>-<slug>/`: `issues.md` (what was measured, and what was not),
  `run.sh` (the command that reproduces it), `logs/*.out` (the output as produced). Ticket 84 is the
  index-defect mechanism, 83 the index probe, 82 the external M1 scoreboard, 85 the M3 clause (c) run,
89 the M3 clause (b) paired block and the quiet-window trigger it waits on (90 is the M5 postgres server:
`postgres-server.sh` starts and checks it, `scripts/m1-baseline-run.sh` runs the postgres arm through
it at the version it reads back, `pull-postgres18.sh` is the repeatable 18.x attempt, and `issues.md`
carries the environment read with its moment).
- Code: `engine/` — the request path in `engine/src/request.rs`, the derived index in
  `engine/src/index.rs`, the driver in `engine/src/bin/flashdb-load.rs`; `harness/` is Python;
  `scripts/` holds the M1 runs and their scoring.

## Not current

- `.scratch/STATUS.md` and `.scratch/active-work.md` carry a supersession banner and no longer own
  state; they stay as the ticket-level measurement record the roadmap cites.
- `.scratch/83-index-probe/runs/*` — all twelve files begin with `# INVALID - do not cite:`. They were
  taken under foreign load with an empty index in both arms. They are not evidence, and nothing
  unmarked may be added beside them.
- `8.8-9.7` (0 B, tracked at the root since `59861dd`) is a shell-redirect artefact: no content, no
  owner, kept rather than deleted because deleting to tidy is not allowed. Nothing depends on it.

## The quiet window

M3 clause (b) is the only reading here that needs a quiet box, and it is the only one that may not be
taken under load: a pair taken under load measures the load. Root granted the window in its contract
(`e5fb0e1:inbox/2026-10-21-next-contract.md`); the trigger is `uptime` reading **loadavg under 2 for one
minute** with no other agent writing in this tree. Record the reading at the start of each arm and
in every log; refuse the arm above the threshold. `paired-200.sh` now enforces that itself — `await_quiet`
before every arm, `refuse_run` moving a lost run's blocks out of the scorer's glob — because gating once
per run let nine blocks be spent above 2.0 on 2026-10-27. Everything else in the tree runs under load.
