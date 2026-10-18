# flashdb — roadmap (milestone-based)

Status: authored 2026-10-17. The plan is milestone-based, not feature-based: each milestone is an
ordered user gate that delivers a vertical slice with a decidable done-criterion. The pre-registered
falsification contract (`.scratch/15-objective-v0/pre-registration.md`) is the fixed acceptance
contract; this document only sequences the remaining work toward it and names the gates. No
pre-registered threshold is changed here.

## Goal and done-criterion

- Goal: prove or reject, at ≥10x data:memory and equal durability, that a per-region layout policy
  chosen by observing data and access patterns sustains a higher offered load than a fixed declared
  policy, without losing on a no-locality workload, without buying the win with unbounded
  reorganisation, and with drift recovery; and place the engine against external baselines.
- Done: every pre-registered verdict T1-T7 is evaluated on a ≥10x instance with the pre-registered
  aggregation, independently reviewed, and published. A rejected thesis is a valid, publishable
  result.

## Where we are (measured, pilot-grade)

- T1 supported (§7.1 lag-only): learned/static ~105-107 rps vs declared 71-75 rps, +41.3% to +50.7%,
  median +47.6%, outside the ~4.0% noise band.
- T2 supported (no-locality loss bounded; 0 index builds on the 0-scan region; reorg median 0.933x).
- T3 supported (win reorg median 0.767x, max 0.941x; no single-region blowup).
- T4 measured null (a real ~9% write-spike capacity drop, neither arm recovers, learned ≈ static).
- T5-T7 untested: no request-level baseline harness exists; postgres was absent.
- Rung-50 lag is not sustained; the both-ceilings service ceiling is device-bound; §7.1
  primary-vs-service reconciliation is open.
- The motivating join-heavy shapes (star-schema fan-out, multi-hop traceability, as-of join,
  anti-join) are now in the generator and measured structurally (ticket 81); the current win is
  per-region access-shape adaptation, not cross-entity co-location (ticket 29), and the M2 analysis
  puts the remaining headroom in placement only for the star-schema fan-out.
- Evidence is 3 independent seeds + 1 repeat, not the pre-registered 10.

## Milestones

| # | Milestone (user gate) | Deliverable / vertical slice | Acceptance (decidable) | Depends |
|---|---|---|---|---|
| M1 | External baseline scoreboard (T5-T7) | One request-level harness driving flashdb (declared + learned), sqlite and postgres on the same frozen trace at matched durability in the same memory-capped regime; a comparison table + figure; T5/T6/T7 verdicts | Each external arm executes the full trace with the inventory correctness oracle passing; equal-durability settings disclosed; the same latency rule applied; T5 within 1.5x of the slower of sqlite/postgres at 1x, T6 ≥3x the best baseline at the highest common offered level, T7 the declared arm passes T5; conditions named on every number | none |
| M2 | Motivating join-heavy workload and layout-space decision | The four join-heavy shapes from `docs/dev-loop.md` added to the generator, frozen and hashed; a headroom measurement at ≥10x; a report on whether the per-region layout space has headroom on them or whether L2 co-location is required | fan-out / multi-hop depth / dependent-read share / co-location potential measured; learned vs declared headroom per shape with the latency rule; an explicit go/no-go recommendation with evidence | none |
| M3 | Access-path parity, then a fair external comparison | The derived `order_no` index and the startup scan warm built from the merged memtable+tile view (they today build from tiles only, silently empty on a region still in the memtable); a quiet-box paired 200 rps comparison of declared vs indexed; the author's KPI ladder (efficiency > sustained throughput > tail latency) defined with its instrument and applied to sqlite/postgres | (a) an index built over a memtable-only region has one entry per row and an empty build with rows present is a hard error, bound by a test; (b) at 200 rps flashdb sustains the level in at least as many paired blocks as the best baseline, or every remaining breach is attributed to a named phase with evidence; (c) the efficiency KPI is stated with its instrument and the comparison resolves to better/not-better outside the measured error band | M1 |
| M4 | Pre-registered 10-seed verdict + §7.1 reconciliation | T1-T4 re-run at 10 independent seeds on the ≥10x instance; the rung-50 residual attributed; the §7.1 primary-vs-service-ceiling reconciliation proposed | 10 seeds, median and IQR, thresholds exactly as pre-registered; every rung-50 rejection attributed or declared device-bound with evidence; the reconciliation recommendation stated | M1, M2, M3 |
| M5 | Final thesis verdict and publication | Integrated, independently reviewed final evidence package; README/STATUS reflect the present; the verdict published (positive or negative) | Integration proves the combined outcome; every acceptance criterion either passes or is disclosed unverified; the result is readable by an outsider from the artefacts | M4 |

Milestones are ordered by dependency, not by appeal. M1 and M2 are independent and may run in
parallel only when their file areas are disjoint (M1: harness baselines + scripts + docs; M2:
generator shapes + headroom harness).

**Renumbering move (2026-10-18, author-decided).** The author made beating the mature databases the
first priority and report-writing the last, so the access-path milestone is now M3 and the former
M3/M4 shift to M4/M5. The rationale is the ticket-83 diagnosis: the M1 external verdict was measured
with flashdb's own derived access path silently empty (`.scratch/83-index-probe/issues.md`), so the
external claim is not yet judged on equal footing and re-judging it precedes any further internal
aggregation. Repaired references: the `Depends` column of M4/M5 and the `Current milestone` section.

## Non-goals for v0 (explicit)

- Multi-node operation, online SQL, the full 128 GiB / 8 GiB regime, and tuning any baseline into a
  race.
- A claim about L2 co-location before it is measured on a real run.
- Treating the simulator's verdict as evidence before the ≤25% p90 per-region calibration gate.

## Decisions reserved to the author

- The §7.1 primary-vs-service-ceiling reconciliation.
- The 10-seed / 15% / 5% / 3x thresholds as written.
- The L2 co-location go/no-go (M2 proposes; it does not apply a product-design change).
- Any external-compute or paid-service use.

## Milestone status (current)

- **M1 — external baseline scoreboard: controlled verdict delivered, T5/T7 PASS, T6 FAIR FAIL.** Ticket 82 (`51c69f3`, 5 paired blocks, 12.546x, AC + EPP performance at 3.6 GHz, snapshot restore, device floor): **T5 PASS** (deficit 1.001; non-inferiority, bounded at 1.0526 at one offered rate), **T7 PASS**, **T6 FAIL** — at 200 rps flashdb-declared sustained 3/5 blocks while sqlite and postgres each sustained 5/5; achieved ratio 1.00x vs 3x, highest-sustained-level ratio 0.5x. The **external-superiority claim is not supported**; the internal learned-vs-declared gain (T1 +41-51%) stands separately. Two method defects were removed and measured: sqlite's default WAL autocheckpoint caused a 272 ms stall (0 -> 20.5 ms) and the SQL pacers were closed-loop (now open-loop like flashdb); both had manufactured the opposite reading. **T6's pre-registered wording is structurally unpassable** (any pair at one offered level is capped at 1.0526), so the metric definition needs an author decision; the level-ratio reading also fails. Footprint: postgres 16.40x by data directory (no footprint excuse on that leg), sqlite 4.04x (below the 10x floor).
- **M2 — join-heavy workload descriptors: descriptors + recommendation delivered; gate UNMET (2/4), now with a recorded proposal.** Ticket 81's `report.md` delivers the four descriptors (§2, `descriptors.json`, corpus `ecc271e4…`) and the per-shape recommendation (§0, §4): L2 co-location is required for `star_fanout` (page size alone helps only its 5-row tail, and is a net loss on the median), `multi_hop` is fan-out-volume-dominated (1,216 read groups / 1,583 child rows per request) so neither axis alone is sufficient, `as_of` is not layout-bound, and `anti_join` points against L2. Two of the four M2 acceptance items are met (descriptors measured, explicit recommendation); two are **unmet** — a headroom measurement at ≥10x and learned-vs-declared headroom with the latency rule — because the engine cannot execute the shapes. **Proposal (simplest-reasonable principle, author may veto):** accept the structural decision — do not invest in L2 now; keep the narrow `star_fanout` case as a conditional future item; defer both the simulator (S) and the engine work (E), because S cannot close the gate (the pre-registration needs a real run) and would need E for calibration anyway, and E is not needed for the primary thesis. Revisit E only if a real star-schema workload makes the narrow case worth the engine contract change.

## Current milestone

**M3 — access-path parity, then a fair external comparison** (ticket 83). Work packages, owners and
budgets are in `.scratch/83-index-probe/issues.md` §"Work packages". The two downstream decisions stay
open and are not touched by M3: the T6 metric definition (rewrite to compare highest sustained levels,
or accept the negative external result) and the M2 structural proposal (no general L2).
