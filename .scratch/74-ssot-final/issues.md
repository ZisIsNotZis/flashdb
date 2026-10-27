# 74 - WP-V: final SSOT refresh to the completed T1/T2/T3/T4 state

## Contract

Work package V of the program (doc-only). Single writer in `/home/z/vibe/worktrees/flashdb/wave20-ssot`
(isolated worktree, branch `wave20/ssot`, based on integrated master after the wave19/drift merge).
Refresh the current-fact SSOT to the completed T1/T2/T3/T4 state under the author's adopted §7.1
lag-only primary reading, verified against the terminal tickets, not memory. Do not touch
`.scratch/15-objective-v0/pre-registration.md`; do not strengthen any verdict beyond what the tickets
support (label pilot-grade / seed counts where the tickets do).

## What changed

- `.scratch/STATUS.md` — rewritten in place to current facts (structure and budget notes kept):
  - §1 engine: added the two post-wave6 durability/latency facts (grouped WAL append deferred onto the
    barrier worker, ticket 67; read-ahead warming incl. eager startup, tickets 57/60/69); test counts
    289 Rust / 132 Python → 304 Rust / 140 Python; evaluation instance row → the >=10x cold51
    (12.25x) decisive instance.
  - §2 instrument: kept as a current-fact trust statement (four fixed defects + verified invariants),
    removing the dated "before commit 744caaa" narrative framing.
  - §3 primary metric: replaced the stale pre-fix rung-50 failure table with the ticket-71 decisive
    T1 — rung 100 lag-sustained (static-derived 8/8, learned 6/8 seeds; declared never), rung 50 0/40
    NOT reached (residual: compaction-adjacent cold reads + device fsync tail), +42.7-52.9%
    throughput (median +48.6%, outside the 2.48% noise band).
  - §4 co-location: condensed to a current-fact note (measured, not the decisive axis).
  - §5 verdict board: T1/T2/T3/T4 statuses updated to the re-derived ticket-71/73 numbers (T2 reorg
    median 0.928x, T3 median 0.934x, T4 UNDERIVABLE), with the supporting tables.
  - §6 defects: tile-cap item corrected from "fixed on both sides" to "adopted, NOT yet implemented".
  - §7 decisions: §7.3 marked satisfied (decisive runs on the >=10x instance); §7.4 corrected to
    adopted-unimplemented; open-author items kept (§7.1 ceiling reconciliation, tile-cap fix,
    T4/residual gaps).
  - §8 environment: added the serialization locks (`/tmp/wave17-measure.lock`, `/tmp/wave19-drift.lock`).
  - §9 resume plan: replaced the stale "re-derive T1/T2/T4" plan with the post-T1-T4 next steps.
- `.scratch/52-program/issues.md` — appended the one-paragraph wave20 WP-V dispatch entry.
- `.scratch/74-ssot-final/issues.md` — this ticket.

`docs/2_GOAL.md` and `docs/8_DETAIL.md` were read and left unchanged: their objective/design facts
(angriness not-well-posed F1-F5; the §7.1 reference; engine design) are unchanged by the T1-T4
measurement. The engine-latency facts stated in STATUS §1 are not duplicated there (one home per
fact).

## Ticket sources

| fact | source |
|---|---|
| T1 rung-100 sustained (static 8/8, learned 6/8), rung-50 residual, +42.7-52.9% (median +48.6%), 2.48% noise band | `.scratch/71-eval-decisive/issues.md` |
| T2 rung-50 lag, 0 index builds, reorg median 0.928x / max 0.944x | `.scratch/71-eval-decisive/issues.md` |
| T3 win reorg median 0.934x / max 0.941x, `U|Stock` 5.13-5.23 MB / 1 merge, max actuation 11.08-11.37 MB | `.scratch/71-eval-decisive/issues.md` |
| T4 UNDERIVABLE (post-drift >= pre-drift capacity; learner region re-exploration 6 windows) | `.scratch/73-t4-drift/issues.md` |
| deferred WAL append (write-stall fix) | `.scratch/67-requestpath-write-stall/issues.md` |
| read-ahead warming + eager startup warm | `.scratch/69-startup-warm/issues.md` (and 57/60) |
| single-region merge oscillation removed (R2 payback) | `.scratch/70-merge-oscillation/issues.md` |
| demand-gated one-shot `order_no` index axis (0 builds on 0-scan) | `.scratch/66-learner-burnin-churn/issues.md` |
| `order_no` access-path axis (the T1 layout win) | `.scratch/64-learned-access-path/issues.md` |
| tile-cap defect + adopted-unimplemented decision | `.scratch/46-partitioned-publish-tile-cap/issues.md` |
| 304 Rust / 140 Python test counts | `.scratch/71-eval-decisive/issues.md`, `.scratch/73-t4-drift/issues.md` |

WP-N (ticket 65) has no directory in this worktree (its artifacts live in the unmerged wave13 worktree);
its numbers are cited through tickets 66/67 and the wave14 dispatch record, which reproduce them.

## Diff stat

```
 .scratch/52-program/issues.md    |  17 +++
 .scratch/74-ssot-final/issues.md |  75 ++++++++++
 .scratch/STATUS.md               | 307 ++++++++++++++++++++++-----------------
 3 files changed, 264 insertions(+), 135 deletions(-)
```

## Gates (verbatim)

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q` — summary.
4. `git diff --check` — silent (exit 0).
