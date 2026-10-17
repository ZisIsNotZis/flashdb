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

## Build record (WP 2.1 + 2.2, branch `wave/m2-join-heavy`)

What exists now, and what it is evidence for:

- **Four join-heavy shapes in the deterministic generator.** They are a separate mix
  (`generator.JOIN_NAMES`, equal weights, round-robin) emitted by `Generator.join_requests()` / the CLI flag
  `--shapes join`, and they are deliberately **not** in `MIX`: the flat flow's frozen corpora and
  their hashes are the dataset every earlier slice measured on, so adding shapes must not move them.
- **A committed corpus** (`harness/tests/fixtures/join_shapes_seed42.jsonl`, 100 requests, 25 per
  shape, seed 42, 48 KB):

  ```
  sha256 ecc271e43efcc4af0184dccec102920d61f5c4181e0c1351e30db59dd31a8d6a
  ```

  Regenerate and re-hash it with:

  ```
  cd harness && uv run --offline python -m flashdb_harness.generator \
      --seed 42 --shapes join --per-shape 25 --out tests/fixtures/join_shapes_seed42.jsonl
  sha256sum tests/fixtures/join_shapes_seed42.jsonl
  ```

  The same bytes are what the descriptor run measures (the report prints the same sha256), so the
  numbers below and the fixture are one artefact, not two.
- **A committed descriptor script and its output**: `harness/flashdb_harness/join_descriptors.py`
  and `.scratch/81-join-heavy-workloads/descriptors.json` (full JSON, one `method` string per
  number). Reproduce:

  ```
  cd harness && uv run --offline python -m flashdb_harness.join_descriptors --seed 42 --per-shape 25 \
      --out ../.scratch/81-join-heavy-workloads/descriptors.json
  ```
- **Tests**: `harness/tests/test_join_shapes.py` (8 new). Every command below was run with
  `uv run --offline` (the box resolves packages from the local uv cache; without `--offline` uv
  attempts a network resolve).

### Commands and results

```
cd harness && uv run --offline --with pytest python -m pytest -q
176 passed in 18.83s                     # 168 pre-existing + 8 new; the 168 are unchanged

uv run --offline --with pytest python -m pytest -q tests/test_join_shapes.py
8 passed in 5.53s

uv run --offline python -m flashdb_harness.generator --seed 42 --requests 20 | sha256sum
929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264
cat tests/fixtures/mixed_seed42_20.jsonl | sha256sum
929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264   # unchanged
```

`test_flat_mix_corpus_hashes_are_unchanged` pins both flat corpora
(`92982259…` mixed 20, `d803afa5…` hot orders) inside the new test file, so a future edit to the
generator that silently moves the flat dataset fails here as well as in `test_generator.py`.

### Method: how each descriptor is measured

The generator emits *requests*; the fan-out a request induces is a property of the data it addresses.
That data is `generator.JoinWorld`, a deterministic modelled population built from integer
arithmetic plus the same `Rng`/`Zipf` skew as the flat flow (rows: `Product`, `Supplier`,
`ProductVersion` SCD-2, `Customer`, `Order`, `OrderLine`, `Lot`, `LotItem`, `Return`, and a
synthesised `StockMovement` supply history per `(sku, loc)`). `join_descriptors.interpret` resolves
each read block of an emitted request against that population — following `$block.field` bindings,
field by field, one parent row per binding — and the descriptors are counts over the result:

| descriptor | method (verbatim from `descriptors.json`) |
|---|---|
| fan-out width | per dependent hop, the number of child rows one parent row reaches after the block's declared `limit`; median and max over the corpus of each request's widest dependent hop. A root read's row count is not a fan-out and is reported separately as `root_rows`. |
| multi-hop depth | number of reads on the longest `needs` path of the emitted request (root read counted), counted from the block graph. |
| dependent-read share | dependent read hops / all read hops; a hop is dependent iff its predicate binds `$block.field` from an earlier block's rows and so cannot be issued before that block returns. |
| co-location potential | share of dependent parent rows whose every matched child row carries the parent's co-location-key value, over parent rows that matched at least one row. Such a child would share a region with its parent under a region key materialised on that field. Parent rows with no match are excluded and reported as `absent_parent_share`. |
| absent share | dependent parent rows whose hop matched no row / all dependent parent rows. This is the anti-join's answer, and an absent row has no place to put anywhere. |
| best single key | co-location potential maximised over the single candidate keys `sku`, `order_no`, `lot_no`, `supplier_no`; below 1.0 means no single key co-locates every hop of the shape. |
| regions / request | distinct co-location-key values carried by the request's parent and child rows: the number of regions a region keyed on that field makes one request touch. |

Model assumptions, which a real data set can falsify (also in `descriptors.json.model.notes`): one
product per sku; 64 suppliers; `1 + rank % 3` SCD-2 versions per sku with a boundary every 10 days;
50 orders/day, 1-5 lines per order; 1-8 products per lot, drawn from the same Zipfian sku
distribution as the order lines (supply follows demand); 0-2 returns per order; supply history is
`1 + (rank*37 + loc*11) % 5` movements per `(sku, loc)` and empty for a sku with
`(rank*11) % 7 == 0` (~1/7 of the catalogue — the modelled lost sale); movements are supply only.

### Descriptor table (seed 42, 25 requests per shape, corpus `ecc271e4…`)

| shape | co-location key | fan-out width (median / max) | multi-hop depth | dependent-read share | co-location potential | best single key | absent share | regions / request |
|---|---|---|---|---|---|---|---|---|
| `star_fanout` | `sku` | 2 / 5 | 3 | 0.75 | 0.6479 | `sku` 0.6479 | 0.0533 | 1.0 |
| `multi_hop` | `order_no` | 614 / 614 | 5 | 0.8333 | 0.5487 | `order_no` 0.5487 | 0.2539 | 410.6 |
| `as_of` | `sku` | 1 / 1 | 2 | 0.5 | 1.0 | `sku` 1.0 | 0.0 | 1.0 |
| `anti_join` | `sku` | 1 / 1 | 2 | 0.5 | 1.0 | `sku` 1.0 | 0.1245 | 106.64 |

Per hop (all requests of the corpus pooled; `rows total` counts child rows, `no-match requests`
counts requests whose hop matched nothing):

| shape | hop | entity | depth | parents | limit | fan-out median / max | rows total | no-match requests | absent share | key co-located share |
|---|---|---|---|---|---|---|---|---|---|---|
| `star_fanout` | `line` | OrderLine | 1 | - | - | 1 / 1 | 25 | 0 | 0.0 | - |
| `star_fanout` | `product` | Product | 2 | line | - | 1 / 1 | 25 | 0 | 0.0 | 1.0 |
| `star_fanout` | `movements` | StockMovement | 2 | line | 128 | 2 / 5 | 54 | 4 | 0.16 | 1.0 |
| `star_fanout` | `supplier` | Supplier | 3 | product | - | 1 / 1 | 25 | 0 | 0.0 | 0.0 |
| `multi_hop` | `items` | LotItem | 1 | - | - | 4 / 8 | 111 | 0 | 0.0 | - |
| `multi_hop` | `products` | Product | 2 | items | - | 1 / 1 | 111 | 0 | 0.0 | 0.0 |
| `multi_hop` | `lines` | OrderLine | 3 | products | - | 12 / 614 | 12165 | 1 | 0.2162 | 0.0 |
| `multi_hop` | `orders` | Order | 4 | lines | - | 1 / 1 | 12165 | 1 | 0.0 | 1.0 |
| `multi_hop` | `returns` | Return | 4 | lines | - | 0 / 2 | 2958 | 1 | 0.7642 | 1.0 |
| `multi_hop` | `buyers` | Customer | 5 | orders | - | 1 / 1 | 12165 | 1 | 0.0 | 0.0 |
| `as_of` | `facts` | StockMovement | 1 | - | 64 | 1 / 5 | 47 | 0 | 0.0 | - |
| `as_of` | `versions` | ProductVersion | 2 | facts | - | 1 / 1 | 47 | 0 | 0.0 | 1.0 |
| `anti_join` | `demand` | OrderLine | 1 | - | 512 | 153 / 165 | 3782 | 0 | 0.0 | - |
| `anti_join` | `supply` | StockMovement | 2 | demand | 1 | 1.0 / 1 | 3311 | 0 | 0.1245 | 1.0 |

What the table says, read straight off it (no layout claim yet — that is WP 2.3):

- `multi_hop` is the only shape whose fan-out is unbounded by construction: the product -> order-line
  hop reaches 614 child rows for one hot product (median 12 per parent row, max 614) and 12,165
  lines over 25 requests. It is also the only shape where a single key is visibly insufficient: `order_no` covers
  the lines->orders and lines->returns hops (share 1.0 each) and covers neither products->lines
  (0.0) nor orders->buyers (0.0), so the best single key reaches 0.5487 of the dependent parent rows
  and the remaining hops would need per-hop placement.
- `star_fanout` is mostly co-locatable on `sku` (0.6479): the order line, the product dimension and
  the sku's movements all carry the line's sku, so one sku-keyed region holds them; the half that is
  not co-located is the `supplier` hop (0.0 — a supplier row carries `supplier_no`, shared with every
  product of that supplier), a 64-row dimension.
- `as_of` and `anti_join` reach 1.0 co-location on `sku` for the rows that exist, and they are
  different cases rather than the same number: the as-of join is region-local (1 region/request) and
  its open question is time tiling, while the anti-join touches 106.64 sku regions per request and
  12.45% of its probes come back **empty** — there is no row to co-locate for those, so placement
  cannot make them cheaper; their cost can only be set by what a region costs to consult.
- `dependent-read share` is 0.5-0.8333 everywhere, i.e. most of every one of these requests is a read
  that cannot be issued until an earlier read returns.

### Unmeasured, and why

1. **No latency, bytes, block-read or dependent-read *cost* is measured for any shape.** Nothing here
   executes the engine, and the engine cannot execute these requests at all today:
   `engine/src/request.rs:139` pins `params` to `sku/loc/qty/email/order_no`, `:136-138` pins the
   service class to `durability` + `retry_horizon_s`, `:144-145` rejects any block graph other than
   `cust -> take`, and the read path `:230-263` accepts only `Stock` get / `Customer` find /
   `StockMovement` scan by `order_no`. So the fan-out widths above are structural, counted against
   the modelled population, not observed.
2. **Multi-row `$block.field` binding is the missing engine capability** every one of the four shapes
   depends on: `validate_read` resolves a single value (`read_text`), so `$demand.sku` /
   `$items.sku` / `$facts.day` set-bindings have no execution path. This is the first thing to build
   if these shapes are to be replayed.
3. **Aggregate-to-write is missing** for the anti-join's natural output (marking the lost skus): the
   contract lists it as missing and required (L-07), so the emitted `anti_join` request stops at the
   detection half and the absence is read from the block's `empty` status.
4. **Per-hop cost is not separable from placement** in a structural count: the same 614-row hop costs
   one page or 614 dependent reads depending on where those rows live, which is exactly the L2
   question WP 2.3 has to reason about rather than measure here.
5. **Model scale, not instance scale.** Fan-out is measured against a modelled catalogue (10k skus,
   2k orders, 6000 order lines, 40 days), not against a >=10x-cache instance; a real trace's fan-out
   grows with history length, and `multi_hop`'s 614 is a function of how many order lines the model
   holds per hot sku.
