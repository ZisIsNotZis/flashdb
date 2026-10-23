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
  (**18.6, Ubuntu 18.6-3build2**) each sustained 5/5 (highest-level ratio 0.5x); the postgres version
  is the one that arm read from its running server, and a claim from another version names it. The
  external-superiority claim is **not supported at this revision**, and the cause is now diagnosed
  (ticket 83): the derived `order_no` index is built from tiles only and was empty on this workload,
  so every scan request paid a 5.6 MB full scan.
  That defect is fixed and the fix is **measured, not argued** (ticket 84): on a rebuilt instance of
  this record's shape the index holds 2,785 entries for 2,785 rows against 0 before the fix, 0 of 188
  scan requests enter the merged scan, and read bytes per request fall 42,240 → 13,466 against the
  predicted 43,298 → 13,460. **The 200 rps comparison itself is `unmeasured`** (decision 7): the window
  is granted on a trigger (loadavg under 2 for one minute, no other agent writing in this tree) and no
  paired block has run, so no throughput number is claimed anywhere; the arms and the chain that
  regenerates what they need are `.scratch/89-m3b-paired-200/`.
- **Learner (pilot-grade, 3 seeds + 1 repeat, 12.25x):** T1 supported (+41.3% to +50.7%, median
  +47.6%, vs a ~4.0% noise band); T2 supported (reorg median 0.933x, 0 index builds on the 0-scan
  region); T3 supported (median 0.767x, max 0.941x); T4 evaluable, **not supported** (a real ~8-9%
  write-spike capacity drop, neither arm recovers). Not the pre-registered 10-seed matrix.
- **Device-bound residual:** rung 50 is not lag-sustained for any arm; the shared device's own fsync
  tail (77-166 ms) sits above the 10 ms lag ceiling with no build concurrent. Attribution to the
  device is recorded, not inferred.
- **Footprint:** postgres 16.40x by data directory (inside the regime), sqlite 4.04x (below the 10x
  floor, so its 5/5 is a reading at a smaller physical size). Both figures are against **postgres
  18.6 (Ubuntu 18.6-3build2)**, ticket 82; a run against another version re-takes them.
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
6. **The claim splits: mechanism now, throughput in a window.** "Is the defect fixed" is work done
   per request — counted from the engine's own counters, invariant under foreign load, and measured at
   ticket 84. "Does it now beat sqlite and postgres at 200 rps" is a rate and needs a quiet box; the
   window is granted on a trigger in decision 7, which replaces this decision's "deferred" with
   `unmeasured`. No throughput number is claimed until a block runs, and the comparison may not be
   taken early under load — the twelve invalid `.scratch/83-index-probe/runs/` probe files are the
   measured cost of doing that.

7. **The quiet window is granted on a trigger, and (b) may not be deferred a third time.** The layer
   above granted one exclusive window in root's contract (`e5fb0e1:inbox/2026-10-21-next-contract.md`,
   deleted after landing): the trigger is `uptime` reading loadavg under 2 for one minute with no other
   agent writing in this tree, and the reading the block started at is recorded in the log. The pair
   lives in `.scratch/89-m3b-paired-200/`. What the grant changes: clause (b) is `unmeasured` until a
   block runs — not "deferred", which names a state where nobody is expected to measure it — and its
   instrument (the paired block, the regeneration chain, the trigger) is written where the clause is.
   Everything else in the tree runs under load and does not wait for the window.

8. **The postgres leg runs from a container, and every result names the version it measured.**
   Root's answer of 2026-10-23 (`mail/2026-10-23-answer-postgres-present.md`) reads this box as
   carrying **postgres 16.15 (Debian 16.15-1.pgdg13+2)** in container `jolly_lamarr` on 5432 and no
   18.x, and forbids installing a system package. The M5 arm therefore comes from a docker image
   (`docker pull postgres:18` for the major version ticket 82 measured; a second container on 5433
   with the M1 record's settings block, read back from the running server), and
   `.scratch/90-m5-postgres-env/postgres-server.sh` is the one command that starts and checks it.
   No document inherits the 18.6 conclusion for a server of another version, and an environment
   fact is written with the moment it was read and the method that read it.

## Milestones

| # | Milestone (user gate) | Deliverable / vertical slice | Acceptance (decidable) | Depends |
|---|---|---|---|---|
| M1 | External baseline scoreboard (T5-T7) | One request-level harness driving flashdb (declared + learned), sqlite and postgres on the same frozen trace at matched durability in the same memory-capped regime | **DONE (ticket 82)**: T5 PASS, T7 PASS, T6 measured and withdrawn; two fairness defects removed and measured | none |
| M2 | Motivating join-heavy workload and layout-space decision | The four join-heavy shapes in the generator, frozen and hashed; a headroom measurement at >=10x; a go/no-go on L2 co-location | **DELIVERED, gate UNMET (2/4)**, proposal accepted — no general L2 (decision 3) | none |
| M3 | Access-path parity (**reached** on (a) and (c); (b) unmeasured, window granted on a trigger — decision 7) | The derived `order_no` index and the startup scan warm built from the merged memtable+tile view; a quiet-box paired 200 rps comparison of declared vs indexed | (a) an index built over a memtable-only region has one entry per row and `coverage_csn` = max covered csn; an empty build while `scan_primary` yields rows is a hard error naming both counts, bound by a test; the same for `startup_warm scan_entries` — **met**: the entries half is measured at scale (ticket 84: 2,785 entries for 2,785 rows, 0 of 188 scans entering the merged scan, `engine/tests/index_defect_mechanism.rs`), the `coverage_csn` half is bound by `engine/tests/order_no_index.rs:171` (a memtable-only region, handles committed out of order, covered csn 3 = the maximum over the rows, which "the last row's csn" would fail) and by `:221` (the maximum over *live* versions, not the tombstone's), and the startup-warm half by `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable` (`engine/src/readahead.rs:583`: 2 scan entries for a region with no tile at all) — all three green in the pinned run at `f38ec63` (`.scratch/88-m3-notes/logs/workspace-head.out`); (b) at 200 rps the indexed arm sustains at least as many paired blocks as declared, or every remaining breach is attributed to a named phase with evidence — **unmeasured** (decision 7): no paired block has run, so the clause is neither met nor falsified; the window is granted on a trigger and the instrument is `.scratch/89-m3b-paired-200/` (two arms on the same frozen trace, the chain that regenerates what they need, a loadavg gate that refuses an arm above it, and a digest-verified restore per arm); the gate polled 191 times over 16 minutes on 2026-10-21 and **not one reading was under 2.0** (min 4.92, max 20.59; `logs/paired-200-gate.out`, summary `logs/window-observation.out`), so the block is queued and no throughput number is claimed anywhere; (c) `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged — **met**: exit 0 with **340 passed / 0 failed**, rerun after the answers comparison was widened — `.scratch/88-m3-notes/logs/workspace-head.out` at `f38ec63` (loadavg 13.20 at start) and `logs/workspace-after-rowbytes.out` at `1870e77` are both 340/0; the earlier log at `ba248dd` is `.scratch/85-m3-clause-c/workspace-pinned.out`; every log carries its own loadavg on line 1 — 13.20 / 8.24 / 23.35 at start for the three, `head -1 .scratch/88-m3-notes/logs/*.out .scratch/85-m3-clause-c/workspace-pinned.out` — and no reading here is timing-derived. The no-index arm returns the same 188 answers byte for byte — status and serialized row, not a row count — as the indexed arm (`before_answers == after_answers` in `engine/tests/index_defect_mechanism.rs`; `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out`). The policy-off reading is one in-process arm through `execute_request`, not a run through the driver's policy gate | M1 |
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

**M3 — access-path parity** (ticket 83): reached on (a) and (c), **unmeasured** on (b) — the quiet
window is granted on a trigger (decision 7) and `.scratch/89-m3b-paired-200/` is its instrument; both
readings are accepted by the layer above (2026-10-21), and WP-4's review of
the evidence (PASS with notes) landed its three text notes at `31c85c6` with the runs that back them in
`.scratch/88-m3-notes/`. `docs/plan/workplan.md` §M3 status carries the clause-by-clause state and the
reopening condition. **M4** is met at `docs/plan/gap-analysis.md` — one reading, the ~573 ms group, is
recorded **unmeasured** with its instrument named, and that instrument needs the same quiet box. So
**the next milestone is M5**, and it is held, not idle: it needs a quiet window (person), and the
postgres leg's server is no longer the blocker it was — this box carries 16.15 in container
`jolly_lamarr` (read 2026-10-23, method in `.scratch/90-m5-postgres-env/issues.md`), the M5 server
starts and checks on 5433 with the M1 record's settings, and `scripts/m1-baseline-run.sh` now runs
the **postgres arm through that checked container**: the version is read back from the running server
and carried in the record (`16.15 (Debian 16.15-1.pgdg13+2)`, arm log
`.scratch/90-m5-postgres-env/logs/arm-16.15-chain-203101.out`), a setting or a required version that
differs stops the arm, and the data directory is restored under a stopped container. That run was
under loadavg 3.77, so it is a chain check and no rate is claimed; an 18.x arm follows only if the
image pull lands (`pull-postgres18.sh`), and then it names the 18.x it reads. Nothing in this tree can
produce the quiet window. What
the project does while it is held is the box-free work on M4's own ranked fix list, because that can be
done under load and it re-ranks what M5's re-run has to measure: the record-side re-scoring of the M1
blocks under the decided lag rule is **done** (WP-12: 0 of 45 rows move, so the headline is
rule-invariant), the barrier-path reading is **in flight** as WP-6's code half (a WAL-preallocation
switch, off by default, built and unit-tested; its timing verdict waits for the same quiet box), and what
is left after those is fix #4's per-group phase timer, which is the instrument the ~573 ms group has no
substitute for. Work packages, owners, acceptance and budgets are in
`docs/plan/workplan.md`; the open-item inventory with leverage ranking is `docs/plan/open-items.md`.
