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

## Milestones

| # | Milestone (user gate) | Deliverable / vertical slice | Acceptance (decidable) | Depends |
|---|---|---|---|---|
| M1 | External baseline scoreboard (T5-T7) | One request-level harness driving flashdb (declared + learned), sqlite and postgres on the same frozen trace at matched durability in the same memory-capped regime | **DONE (ticket 82)**: T5 PASS, T7 PASS, T6 measured and withdrawn; two fairness defects removed and measured | none |
| M2 | Motivating join-heavy workload and layout-space decision | The four join-heavy shapes in the generator, frozen and hashed; a headroom measurement at >=10x; a go/no-go on L2 co-location | **DELIVERED, gate UNMET (2/4)**, proposal accepted — no general L2 (decision 3) | none |
| M3 | Access-path parity | The derived `order_no` index and the startup scan warm built from the merged memtable+tile view; a quiet-box paired 200 rps comparison of declared vs indexed | (a) an index built over a memtable-only region has one entry per row and `coverage_csn` = max covered csn; an empty build while `scan_primary` yields rows is a hard error naming both counts, bound by a test; the same for `startup_warm scan_entries`; (b) at 200 rps the indexed arm sustains at least as many paired blocks as declared, or every remaining breach is attributed to a named phase with evidence; (c) `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged | M1 |
| M4 | Gap attribution and the theoretical floor | The forensic report required by `objective.md` §0 and `dev-loop.md` §Gap attribution: per-phase ledger for the M1 and M3 records, each phase compared with its theoretical floor, each gap classified (a) not learned enough / (b) defect / (c) architecture, plus the byte-level format forensics (on-disk and in-memory bytes per row) | every losing phase has a classification with its evidence and revision; the ranked fix list names each fix's predicted effect on the KPI ladder and its instrument; unclassifiable phases are recorded as unmeasured | M3 |
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

**M3 — access-path parity** (ticket 83). Work packages, owners, acceptance and budgets are in
`docs/plan/workplan.md`; the open-item inventory with leverage ranking is `docs/plan/open-items.md`.
