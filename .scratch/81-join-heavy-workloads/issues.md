# 81 - motivating join-heavy workloads: descriptors and layout-space recommendation (M2)

## Objective

Test the thesis premise that ticket 29 left open: the current generator produces four roughly
independent access shapes, and the measured win is per-region parameter adaptation, not
cross-entity co-location. Add the four join-heavy shapes named in `docs/dev-loop.md` (star-schema
fan-out, multi-hop traceability, as-of join, anti-join), measure their structural descriptors and
co-location potential, and recommend whether the layout space should stay per-region parameters or
move to L2 co-location.

Owning milestone: M2 in `docs/roadmap.md`. Ticket 29 is the originating finding.

## Scope

In scope (this slice, no engine execution, no L2 implementation):
- Extend `harness/flashdb_harness/generator.py` with four deterministic shapes:
  `star_fanout` (replenishment/days-of-cover: order lines -> product/supplier/lead time),
  `multi_hop` (recall/traceability: lot -> products -> order_lines -> orders -> customers -> returns),
  `as_of` (fact joined to a time-versioned SCD-2 dimension on `(sku, t)`),
  `anti_join` (lost sales: demanded SKUs with no supply movement).
- Measure per shape: fan-out width, multi-hop depth, dependent-read share, and co-location
  potential (how much of the cost is determined by where related rows live rather than by per-region
  parameters).
- Produce a recommendation with evidence: per-region layout space vs L2 co-location.

Out of scope: implementing L2, changing the engine, any claim about L2 before it is measured on a
real run, changing the pre-registration.

## Authoritative inputs

- `docs/dev-loop.md` (the four shapes and their hypotheses).
- `.scratch/29-workload-shapes/issues.md` (the measured headroom table and the plan).
- `harness/flashdb_harness/generator.py`, `seed.py`, `load_profile.py` (determinism and corpus rules).
- `.scratch/15-objective-v0/pre-registration.md` (the layout space and the "no L2 claim before a real
  run" rule).

## Work packages (first level)

| WP | Owner | Deliverable | Acceptance |
|---|---|---|---|
| 2.1 shapes | worker | the four shapes in the generator, deterministic, with tests and a frozen corpus sha256 | same seed -> byte-identical trace; Rng/Zipf only, no floats in the data path; tests prove determinism |
| 2.2 descriptors | worker | a measured descriptor table per shape (fan-out, multi-hop depth, dependent-read share, co-location potential) | each number names its method and its shape; no descriptor is invented where the generator does not emit it |
| 2.3 headroom | worker | an analysis of whether per-region parameters can move each shape, or the cost is co-location-bound | states the model, its calibration limit, and what would falsify the recommendation |
| 2.4 report | worker | a report with the table and the recommendation (L2 go/no-go) | conclusion first; evidence and unverified limits named; no L2 claim beyond the analysis |
| 2.5 review | reviewer (fresh) | an independent verdict on determinism, the descriptor method and the recommendation | PASS / CANNOT-VERIFY / FAIL with defect evidence |

## Budget

- Workers: 3 (2.1+2.2, 2.3+2.4), depth 1.
- Review: 1 fresh reviewer + at most 2 fix cycles.
- No-progress attempts: 2 per work package.
- Wall-clock: 3 hours.

## Stop conditions

- A shape cannot be generated deterministically -> stop and report.
- A descriptor would require engine execution the engine does not support -> record it as unmeasured
  rather than inventing a number.
- A recommendation would change the objective or claim L2 without a real run -> propose, do not apply.
