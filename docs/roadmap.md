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
| M3 | Pre-registered 10-seed verdict + §7.1 reconciliation | T1-T4 re-run at 10 independent seeds on the ≥10x instance; the rung-50 residual attributed; the §7.1 primary-vs-service-ceiling reconciliation proposed | 10 seeds, median and IQR, thresholds exactly as pre-registered; every rung-50 rejection attributed or declared device-bound with evidence; the reconciliation recommendation stated | M1, M2 |
| M4 | Final thesis verdict and publication | Integrated, independently reviewed final evidence package; README/STATUS reflect the present; the verdict published (positive or negative) | Integration proves the combined outcome; every acceptance criterion either passes or is disclosed unverified; the result is readable by an outsider from the artefacts | M3 |

Milestones are ordered by dependency, not by appeal. M1 and M2 are independent and may run in
parallel only when their file areas are disjoint (M1: harness baselines + scripts + docs; M2:
generator shapes + headroom harness).

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

- **M1 — external baseline scoreboard: delivered artefact, gate CANNOT-VERIFY.** The harness
  (`baseline_0b.py`) and the corrected scoreboard (`report.md`/`report.json`, commit `43b1297`) passed
  a fresh narrow review (PASS for the correction). The milestone gate is not met: **T6 has no
  measurement at >=10x** (the only evaluable reading is 0.50x declared / 1.00x learned against 3x,
  a FAIL), and **T5/T7 pass on throughput (deficit 1.001 at 12.546x) but the sustained half is
  not-robustly-determined** (one contended run: no arm sustained). Open items: a quiet-box repeat of
  run B, a >=10x ladder for T6 (~8 h per single seed; the pre-registration asks for 10 seeds), and
  the 10-seed aggregation. Mechanical facts: oracle PASS for all arms including the 10x flashdb
  read-back; one latency rule including the growth clause; sqlite durability observed; run B
  provenance recorded. Environment: the literal `cargo build/test --workspace --offline` fails at
  resolution on this box (lock pins crates.io, cache is mirrors.aliyun.com); the pinned-aliyun
  `--config` command builds and passes 308 tests / 168 pytest.
- **M2 — join-heavy workload descriptors: descriptors + recommendation delivered; gate UNMET (2/4); author decision needed on the ≥10x/learned-vs-declared item.** Ticket 81's `report.md` delivers the four descriptors (§2, `descriptors.json`, corpus `ecc271e4…`) and the per-shape recommendation (§0, §4): L2 co-location is required for `star_fanout` (page size alone helps only its 5-row tail, and is a net loss on the median), `multi_hop` is fan-out-volume-dominated (1,216 read groups / 1,583 child rows per request) so neither axis alone is sufficient, `as_of` is not layout-bound, and `anti_join` is not determinable and points against L2. Two of the four M2 acceptance items are met (descriptors measured, explicit recommendation); two are **unmet** — a headroom measurement at ≥10x and learned-vs-declared headroom with the latency rule — because the engine cannot execute any of the shapes (no multi-row `$block.field` binding, a validator that admits only `cust -> take`, no aggregate-to-write); the item-by-item table is `report.md` §8. Closing the gate needs that engine work plus the M-B run, or an explicit author waiver of the ≥10x item. No per-region space replaced, no pre-registered threshold changed, and the L2 go/no-go remains the author's.

## Current milestone

M2 — Motivating join-heavy workload descriptors and the layout-space recommendation. Work packages,
owners, acceptance and budget live in `.scratch/81-join-heavy-workloads/issues.md`.
