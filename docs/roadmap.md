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
  anti-join) are not in the generator; the current win is per-region access-shape adaptation, not
  cross-entity co-location (ticket 29).
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

## Current milestone

M1 — External baseline scoreboard. Work packages, owners, acceptance and budget live in
`.scratch/80-external-baselines/issues.md`.
