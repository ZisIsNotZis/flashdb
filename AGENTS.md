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
  89 the M3 clause (b) paired block and the quiet-window trigger it waits on (WP-17 made it
  harvest-based), 92 the barrier queue-wait measurement (which rejects WAL preallocation as the
  stall's cause), and 93 the indexed product arm in the M5 chain. The postgres leg is the M1
  user-space **18.6** extraction at `/var/tmp/m1-pg` (`scripts/baseline-env.sh start|check` reads the
  version and the M1 settings back); the 2026-10-23 container path is historical, and Docker has no
  images on this box (2026-10-27 read).
- Code: `engine/` — the request path in `engine/src/request.rs`, the derived index in
  `engine/src/index.rs`, the driver in `engine/src/bin/flashdb-load.rs`; `harness/` is Python;
  `scripts/` holds the M1 runs and their scoring.

## Not current

- `.scratch/STATUS.md` and `.scratch/active-work.md` carry a supersession banner and no longer own
  state; they stay as the ticket-level measurement record the roadmap cites.
- `.scratch/83-index-probe/runs/*` — all twelve files begin with `# INVALID - do not cite:`. They were
  taken under foreign load with an empty index in both arms. They are not evidence, and nothing
  unmarked may be added beside them.
- `8.8-9.7` was a shell-redirect artefact (0 B, tracked at the root since `59861dd`, nothing depending on
  it) and is deleted. A redirect that lands in the tree root is removed when it is seen, not kept: a
  tracked file with no content has no owner to ask and no reader to serve.

## The quiet window

M3 clause (b) and M5's KPI comparison are the readings that need a quiet box, and they are the ones
that may not be taken under load: a pair taken under load measures the load. Root granted the window in
its contract (`e5fb0e1:inbox/2026-10-21-next-contract.md`); the trigger is `uptime` reading **loadavg
under 2 for one minute** with no other agent writing in this tree. Record the reading at the start of
each arm and in every log; refuse the arm above the threshold. `paired-200.sh` enforces that itself —
`await_quiet` before every arm, and a lost arm is refused rather than reported. A completed pair whose
both arms started under 2.0, recorded `errors=0` and verified their restore is evidence and is **kept**
when a later arm is refused; a normal run resumes at the next pair, and the scorer will not decide the
clause below five valid pairs (`partial: N/5`, exit 3). Everything else in the tree runs under load.
