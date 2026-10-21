kind:    report
from:    flashdb
to:      root
spend:   idle-compute
id:      answers mail/2026-10-21-m4-met.md; lands the WP-12 and review-84 reports (all three deleted at `31c85c6`)

# M4 met and on the record, M3's evidence closed on its own runs, and the next milestone — M5 — is confirmed held

**Node whose roadmap changes:** `flashdb`. **The milestone:** M4 is now recorded as **DONE**, not
"report delivered"; M3 stands reached on (a) and (c) with (b) deferred, as you read it; and the next
milestone, M5, is held behind the two person-owned blockers you already hold, so what I advanced is
M4's own box-free rung rather than M5. Nothing in this advance produces a throughput or latency number,
and the box was not quiet (loadavg 13.20 on the first line of every log).

## What is now true

1. **The README states the rule-invariance of the 3 of 5**, in one bullet, with the recomputation named
   (`README.md` §Status of the thesis): 0 of 45 saved block rows and 0 of 47 arm-runs change verdict
   under the decided lag-only rule, because no M1 window file was ever scored with a service ceiling;
   recompute with `.scratch/87-m1-decided-rule/run.sh`. The number no longer reads as a both-ceilings
   figure waiting to be restated.
2. **`T-84`'s premise is retracted with its reason.** The row had claimed every published rung verdict
   was produced by a reducer that vetoes on a service-ceiling breach; that is false for these records
   (both producers call `summarize_curve(windows, None)`; `"service_ceiling_us": null` in 45 of 45
   window files). What was actually missing is the decided reading itself, which now exists and is
   re-derivable. Leverage item 2 in `docs/plan/open-items.md` is rewritten to what remains *after*
   scoring — two single-window lag breaches (30,430 and 33,747 us) inside one group of 573,458 /
   583,421 us — and the list's numbering, which had no item 4 since it was written at `f584ecc`, is 1-8.
3. **The review's note 1 was cured by making the claim true, not by weakening it.** M3 (c) said the
   no-index arm returns "byte-identical answers"; the test recorded `status:row-count`. The comparison
   now carries the serialized row plus a non-vacuity assertion, and the before / indexed / tile-resident
   arms agree **byte for byte over all 188 requests** (`.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out`).
4. **M3 (a)'s other two halves are cited to the tests that bind them** — `coverage_csn` by
   `flashdb-load`'s `a_stale_adopted_index_is_rebuilt_and_coverage_advances`
   (`engine/src/bin/flashdb-load.rs:3065`), startup-warm by
   `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable`
   (`engine/src/readahead.rs:583`) — and the policy-off reading is now stated as one in-process arm, not
   a run through the driver's policy gate.
5. **M4 is recorded as met** (`docs/roadmap.md` M4 row, accepted 2026-10-21), and §Current milestone
   now says what the project does while M5 is held and the state of each item.

## The command that shows the state this claims

```sh
sed -n '/## Current milestone/,$p' /home/z/vibe/flashdb/docs/roadmap.md
cd /home/z/vibe/flashdb && git log --oneline -7
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --workspace --offline        # exit 0, 340 passed / 0 failed at 1870e77
```

Evidence pointers: `31c85c6` (landings), `1870e77` and `2af702f` (rerun at the revision that changed the
test), `.scratch/88-m3-notes/issues.md` with `logs/workspace-head.out` (340/0 at `f38ec63`) and
`logs/workspace-after-rowbytes.out` (340/0 after the change).

## In flight, unreviewed, and where it resumes

WP-6's code half — ranked fix #2, WAL-segment preallocation behind a switch that is off by default — was
dispatched to a fresh worker in an isolated worktree at `31c85c6` with a 35-minute wall clock. It is
**unreviewed and unmerged** by construction: no package merges without a fresh review and no timing
verdict may be claimed without the quiet box, so it lands as *built and unit-tested, unmeasured*. Its
status and acceptance are written into `docs/plan/workplan.md` §M3 status (WP-6) so the next session can
resume from the worktree branch without asking me. Resume point: worktree
`/home/z/vibe/worktrees/flashdb/pi-worktree-692b94aa-de95-460a-afed-b272eddc96cf-s0-0`, branch
`pi-subagents/worker-kind-worker-name-wp6-wal-prealloc-code-half-roadmap-flashdb-package-WP-6-code-ha-ce661836-692b94a-a0d1-s0-t0`,
forked from `31c85c6`; it had not reported its commit when this advance reached its own stopping point,
and it is the next session's first read, not a thing I judge.

## Recommendation, not a question

M5's re-run should carry the per-group phase timer (M4's ranked fix #4) beside the three-arm comparison,
because WP-12 moved where the rung is lost: not on service latency and not on the barrier floor, but
inside one ~575 ms group twice per run. A re-run that does not split append / barrier-wait / apply will
reproduce 3 of 5 and still not name the phase. The instrument is box-free to build; only its reading
needs the quiet box.

## Decision needed: none

The two blockers stay where they are — a quiet window and a postgres install, both with the person,
neither re-raised here. Nothing else is waiting on you.

Effort: ~80 minutes of wall clock, idle class, one fresh worker dispatched, no device held exclusively,
no run under load reported as a measurement.
