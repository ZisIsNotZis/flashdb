# flashdb — roadmap (milestone-based)

The plan is milestone-based, not feature-based: each milestone is an ordered user gate that delivers
a vertical slice with a decidable done-criterion. The product bar and the KPI ladder are owned by
`objective.md` §0; the measurement method and the gap-attribution protocol by `dev-loop.md`; the
pre-registered falsification contract (`.scratch/15-objective-v0/pre-registration.md`) is the fixed
acceptance contract for the *learner* claim (T1-T4) only. No pre-registered learner threshold is
changed here.

## Goal and done-criterion

- Goal: prove or reject that a per-region layout policy chosen by observing data and access patterns
  sustains a higher offered load than a fixed declared policy, without losing on a no-locality
  workload, without buying the win with unbounded reorganisation and with drift recovery — **and**
  that the engine as a whole beats sqlite and postgres end-to-end on efficiency first, then sustained
  throughput, then tail latency (`objective.md` §0).
- Done: the external comparison resolves to **win** under `objective.md` §0 on one reasonably
  specified machine, the learner verdicts T1-T4 are evaluated on a >=10x instance with the
  pre-registered aggregation over 10 seeds, both are independently reviewed, and the result is
  published. A rejected thesis, or an attributed architecture gap with a published negative result,
  is a valid outcome.

## Where we are (measured)

- **External (M1, ticket 82):** T5 PASS (deficit 1.001), T7 PASS, **T6 measured and withdrawn** as
  structurally unpassable. At 200 rps flashdb-declared sustained 3/5 blocks while sqlite and postgres
  each sustained 5/5 (highest-level ratio 0.5x). The external-superiority claim is **not supported at
  this revision**, and the cause is now diagnosed (ticket 83): the derived `order_no` index is built
  from tiles only and was empty on this workload, so every scan request paid a 5.6 MB full scan.
  That defect is fixed and the fix is **measured, not argued** (ticket 84): on a rebuilt instance of
  this record's shape the index holds 2,785 entries for 2,785 rows against 0 before the fix, 0 of 188
  scan requests enter the merged scan, and read bytes per request fall 42,240 → 13,466 against the
  predicted 43,298 → 13,460. **The 200 rps comparison itself is deferred, not pending** (decisions 5-6):
  no quiet box, so no throughput number is claimed anywhere.
- **Learner (pilot-grade, 3 seeds + 1 repeat, 12.25x):** T1 supported (+41.3% to +50.7%, median
  +47.6%, vs a ~4.0% noise band); T2 supported (reorg median 0.933x, 0 index builds on the 0-scan
  region); T3 supported (median 0.767x, max 0.941x); T4 evaluable, **not supported** (a real ~8-9%
  write-spike capacity drop, neither arm recovers). Not the pre-registered 10-seed matrix.
- **Device-bound residual:** rung 50 is not lag-sustained for any arm; the shared device's own fsync
  tail (77-166 ms) sits above the 10 ms lag ceiling with no build concurrent. Attribution to the
  device is recorded, not inferred.
- **Footprint:** postgres 16.40x by data directory (inside the regime), sqlite 4.04x (below the 10x
  floor, so its 5/5 is a reading at a smaller physical size).
- **M2 (join-heavy descriptors):** delivered and merged; its gate is UNMET (2/4) because the >=10x
  and learned-vs-declared join measurements need three engine capabilities that do not exist
  (multi-row `$block.field` binding, a validator wider than `cust -> take`, aggregate-to-write).

## Decisions (2026-10-19, delegated author authority)

The author delegated every open decision before leaving. Recorded here so no later session reopens
them without new evidence:

1. **The external bar replaces T6.** "Win" is the `objective.md` §0 ladder: efficiency no worse with
   one strict gain, then sustained level at least the best baseline's, then tail latency reported. The
   3x ratio is withdrawn as structurally unpassable. `.scratch/STATUS.md` §5(b) is thereby closed;
   the learner's 15% / 5% / 10-seed thresholds stand for T1-T4 exactly as pre-registered.
2. **§7.1 is closed.** Schedule-lag p99 <= 10 ms is the throughput-feasibility rule; the service
   ceiling is reported, not used as a veto, and counts against the ladder only when a baseline holds
   the same level without breaching.
3. **M2 structural proposal accepted.** No general L2 co-location in v0. The narrow `star_fanout`
   case stays a conditional future item, reopened only if a real star-schema workload makes it worth
   the engine contract change. The three missing engine capabilities are therefore **not** planned.
4. **The baseline setup is un-handicapped.** sqlite keeps its default WAL autocheckpoint; postgres is
   tuned as a competent operator would tune it inside the same memory budget; both are driven
   open-loop. Any arm that fails this is invalid and is re-run, not reported.

## Decisions (2026-10-21, the node that now owns this roadmap)

Taken by the layer above and relayed as a contract answer; recorded here because they change what this
roadmap's own text means.

5. **M3's clause (c) is the roadmap's definition, not the workplan's paraphrase.** (c) is
   `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged, which is
   locally decidable. The efficiency KPI comparison against sqlite and postgres is **M5's** work, and
   `docs/plan/workplan.md` §M3 status carried a stale restatement of it. Recorded as assumed by the
   layer above; reversing it would move M3 from "awaiting a quiet box" to "needs postgres too", which
   this roadmap's own text does not support.
6. **The claim splits: mechanism now, throughput deferred.** "Is the defect fixed" is work done per
   request — counted from the engine's own counters, invariant under foreign load, and measured at
   ticket 84. "Does it now beat sqlite and postgres at 200 rps" is a rate, needs a quiet box, and the
   person has stated that no quiet window is coming to this machine. The deferral is written where the
   claim lives (`README.md` §Status of the thesis, the M3 row above, `docs/plan/workplan.md` §M3
   status), no throughput number is claimed, and the comparison may not be run early under load — the
   twelve invalid `.scratch/83-index-probe/runs/` probe files are the measured cost of doing that.
   Reopens on: a quiet window on this box, or a borrowed machine of the same storage class.

## Milestones

| # | Milestone (user gate) | Deliverable / vertical slice | Acceptance (decidable) | Depends |
|---|---|---|---|---|
| M1 | External baseline scoreboard (T5-T7) | One request-level harness driving flashdb (declared + learned), sqlite and postgres on the same frozen trace at matched durability in the same memory-capped regime | **DONE (ticket 82)**: T5 PASS, T7 PASS, T6 measured and withdrawn; two fairness defects removed and measured | none |
| M2 | Motivating join-heavy workload and layout-space decision | The four join-heavy shapes in the generator, frozen and hashed; a headroom measurement at >=10x; a go/no-go on L2 co-location | **DELIVERED, gate UNMET (2/4)**, proposal accepted — no general L2 (decision 3) | none |
| M3 | Access-path parity (**reached** on (a) and (c); (b) deferred for want of a quiet box, decision 6) | The derived `order_no` index and the startup scan warm built from the merged memtable+tile view; a quiet-box paired 200 rps comparison of declared vs indexed | (a) an index built over a memtable-only region has one entry per row and `coverage_csn` = max covered csn; an empty build while `scan_primary` yields rows is a hard error naming both counts, bound by a test; the same for `startup_warm scan_entries` — **met**: the entries half is measured at scale (ticket 84: 2,785 entries for 2,785 rows, 0 of 188 scans entering the merged scan, `engine/tests/index_defect_mechanism.rs`), the `coverage_csn` half is bound by `a_stale_adopted_index_is_rebuilt_and_coverage_advances` (`engine/src/bin/flashdb-load.rs:3065`: coverage equals the csn the build snapshotted and advances to the rebuild's), and the startup-warm half by `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable` (`engine/src/readahead.rs:583`: 2 scan entries for a region with no tile at all) — all three green in the pinned run at `f38ec63` (`.scratch/88-m3-notes/logs/workspace-head.out`); (b) at 200 rps the indexed arm sustains at least as many paired blocks as declared, or every remaining breach is attributed to a named phase with evidence — **deferred, not pending**: the quiet box this needs does not exist here and none is coming, so no throughput number is claimed and the pair may not be run under foreign load; it reopens on a quiet window on this box or a borrowed machine of the same storage class (WP-2); (c) `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged — **met**: exit 0 with **340 passed / 0 failed** at `f38ec63` (`.scratch/88-m3-notes/logs/workspace-head.out`, loadavg 13.20 at start; the earlier log at `ba248dd` is `.scratch/85-m3-clause-c/workspace-pinned.out`), and the no-index arm returns the same 188 answers byte for byte — status and serialized row, not a row count — as the indexed arm (`before_answers == after_answers` in `engine/tests/index_defect_mechanism.rs`; `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out`). The policy-off reading is one in-process arm through `execute_request`, not a run through the driver's policy gate | M1 |
| M4 | Gap attribution and the theoretical floor (**DONE** — accepted as met by the layer above, 2026-10-21; `docs/plan/gap-analysis.md`) | The forensic report required by `objective.md` §0 and `dev-loop.md` §Gap attribution: per-phase ledger for the M1 and M3 records, each phase compared with its theoretical floor, each gap classified (a) not learned enough / (b) defect / (c) architecture, plus the byte-level format forensics (on-disk and in-memory bytes per row) | every losing phase has a classification with its evidence and revision; the ranked fix list names each fix's predicted effect on the KPI ladder and its instrument; unclassifiable phases are recorded as unmeasured | M3 |
| M5 | Fair external re-run under the KPI ladder | The three-arm comparison re-run on a quiet box with the efficiency instruments wired: write amplification, bytes read/request, CPU µs/request, resident bytes/data byte, recovery seconds, next to the sustained level and the tail | the comparison resolves to win / not-worse / loss per `objective.md` §0 with every number conditioned; any remaining loss carries its M4 classification | M3, M4 |
| M6 | Pre-registered 10-seed verdict and publication | T1-T4 re-run at 10 independent seeds on the >=10x instance; the integrated, independently reviewed evidence package; README and docs reflect the present; the verdict published | 10 seeds, median and IQR, thresholds exactly as pre-registered; every acceptance criterion either passes or is disclosed unverified; an outsider can read the result from the artefacts | M5 |

Milestones are ordered by dependency, not by appeal. M3 -> M4 -> M5 is one chain: the access path
must be correct before a gap is attributable, and the gap must be attributed before anything is
tuned or re-measured.

## Non-goals for v0 (explicit)

- Multi-node operation, online SQL, the full 128 GiB / 8 GiB regime, and tuning any baseline into a
  race.
- General L2 co-location (decision 3); a claim about it before it is measured on a real run.
- Treating the simulator's verdict as evidence before the <=25% p90 per-region calibration gate.
- Any performance claim from an arm that fails the fair-setup rules in `objective.md` §0.

## Current milestone

**M3 — access-path parity** (ticket 83): reached on (a) and (c) and **deferred** on (b), whose quiet
box does not exist here; `docs/plan/workplan.md` §M3 status carries the clause-by-clause state and the
reopening condition. **M4**'s forensic report is delivered at `docs/plan/gap-analysis.md` and its
acceptance is met as written — one reading, the ~573 ms group, is recorded **unmeasured** with its
instrument named, and that instrument needs the same quiet box. So **the next milestone is M5**, and it
is held, not idle: it needs a quiet window (person) and the postgres server reinstalled (person, raised
as a cost report) and nothing in this tree can advance either. What the project does while it is held is
the box-free work on M4's own ranked fix list — the record-side re-scoring of the M1 blocks under the
decided lag rule and the barrier-path reading — because that can be done under load and it re-ranks what
M5's re-run has to measure. Work packages, owners, acceptance and budgets are in
`docs/plan/workplan.md`; the open-item inventory with leverage ranking is `docs/plan/open-items.md`.
