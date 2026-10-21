# flashdb — where a session starts

This checkout is one node's project: it holds its own roadmap (`docs/roadmap.md`), its work packages
(`docs/plan/workplan.md`) and its mailbox (`mail/`). A session launched to do a package inside it
writes only the area its contract names and reports to `mail/`; the top-level docs belong to the node
that owns the project. The law is `/home/z/vibe/agent-policy/AGENTS.md`.

## Start

Read `mail/` first — a message that arrived while the node was asleep changes what you would do —
then `docs/roadmap.md` §Where we are (measured) and §Current milestone, then
`docs/plan/workplan.md` §M3 status and §Known blockers. Land what a message says in those files
before deleting it: the next session reads the files, not the conversation.

## Where a thing is

- Milestone state and the decisions in force → `docs/roadmap.md`. Work packages with owner,
  acceptance and spend → `docs/plan/workplan.md`. Item inventory with leverage ranking →
  `docs/plan/open-items.md`. Which claim rests on which code path or record →
  `docs/plan/evidence.md`.
- A measured ticket → `.scratch/<nn>-<slug>/`: `issues.md` (what was measured, and what was not),
  `run.sh` (the command that reproduces it), `logs/*.out` (the output as produced). Ticket 84 is the
  index-defect mechanism, 83 the index probe, 82 the external M1 scoreboard, 85 the M3 clause (c) run.
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

## Do not

Run the paired 200 rps comparison. It needs a quiet box, no quiet window is coming to this machine,
and a pair taken under load measures the load (`docs/plan/workplan.md` §Known blockers).
