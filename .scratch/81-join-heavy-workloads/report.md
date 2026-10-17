# 81 — join-heavy workloads: per-region layout space vs L2 co-location (M2 WP 2.3 / 2.4)

Status: 2026-10-17, branch `wave/m2-join-heavy`. Authority: the M2 acceptance in
`.scratch/81-join-heavy-workloads/issues.md` and the workload hypotheses in `docs/dev-loop.md`. The
pre-registered contract (`.scratch/15-objective-v0/pre-registration.md`) is **not changed**, no engine
code is touched, L2 is not implemented, and no claim is made that an L2 implementation delivers any
number below. The L2 go/no-go is an author-reserved decision (`docs/roadmap.md`, "Decisions reserved to
the author").

## 0. Conclusion first

The descriptors show that none of the per-region shaping parameters can change the hop chain or the
fan-out row count of any of the four shapes; where the headroom actually is differs by shape:
placement for `star_fanout`, fan-out volume for `multi_hop`, nothing decisive for `as_of`, and
probe/tail pricing for `anti_join`. Only one of the four is cleanly an L2 case, and one is not about
layout at all:

| shape | verdict | decisive number |
|---|---|---|
| `star_fanout` | **L2 required for a structural read-count win.** Per-region parameters cannot remove a dependent hop; 64.79% of dependent parent groups are co-locatable on `sku`; the one non-co-locatable hop is a shared 64-row dimension. Per-region ceiling ≈1.1-1.2x (the 2-5-row movement fan-out), L2 ceiling ≈2x on the whole shape. | 4 read groups/request (root + 3 dependent); 1.84 co-locatable groups/request; `supplier` hop co-located share 0.0 |
| `multi_hop` | **Not determinable between the two spaces, and the dominant term is neither: it is fan-out volume.** ≥1,216 dependent read groups and 1,583 child rows per request, 5 hops, no single key co-locates the chain (best 0.5487). Page size can compress the per-row read cost by at most ≈8.2x and only if the fan-out rows are contiguous under the access path (unmeasured); L2 removes ≤ the co-located hops' groups and cannot remove the rows. | 1,215 dependent probes + root = 1,216 read groups/request; 1,583 child rows/request; `products -> lines` 0.0 and `orders -> buyers` 0.0 under the best single key |
| `as_of` | **Per-region parameters are not the lever and L2 is not justified by this shape.** 3 read groups, fan-out 1, one region/request, zero absent probes. Page size only makes single-row reads worse (1.25x / 1.95x), bloom has no absent probe to save, the trigger is throughput-neutral. L2 (sku-keyed) could collapse ≈3 reads to ≈1 on a shape that is already the cheapest of the four. | 2.88 read groups/request; absent share 0.0; 1.0 region/request |
| `anti_join` | **Not determinable, and the evidence points against L2.** 107.64 distinct dependent probes/request over 106.64 sku regions; co-location cannot remove one probe (each is a distinct sku), it would fragment the 151-row demand scan into 107 regions, and 12.45% of probes have no row to place. The plausible per-region axes are the compaction trigger (tiles per probe region) and bloom only if the absent probes are in-range (unmeasured). | 106.64 distinct supply probes/request; 12.45% absent; 106.64 regions/request |

**Overall recommendation (proposed to the author, not applied).** The M2 evidence does **not** support
moving the layout space from per-region parameters to L2 as a general answer. It supports at most a
**narrow** L2 case — the star-schema fan-out, where the related rows form a page-sized entity group
and even there the structural win is 4 → 2 reads — and it locates the multi-hop shape's cost in
**fan-out volume**, which neither a per-region parameter nor a co-location key can remove. It argues **against** L2 for the anti-join (the co-location trap the
dev-loop names). The only placement/co-location arm ever measured on this engine (within-entity family
co-location, tickets 50/57) bought **-16.6% dependent reads** for **≈12.7x compaction write amplification** and a worse
tail, so an L2 go must budget that write-amp/tail risk; it has not been measured for cross-entity L2.

**What would falsify this conclusion.**

- A cold, ≥10x engine run of `star_fanout` in which a per-region page-size or merge-trigger change
  alone moves sustained load by >15% — that would make per-region parameters sufficient without L2.
- An L2 arm on `star_fanout` that removes the co-located hops but does not move sustained load because
  the compaction write amplification or the tail regresses (exactly what the measured family-co-location
  arm did) — that would reject L2 as the actuator even where the structure favours it.
- An access-path measurement showing `multi_hop`'s 486-row line fan-out is contiguous and page/
  compaction-flat (so the volume is bandwidth-bound and only the hop groups are left), or showing it is
  scattered (so page size cannot help and only a semantics change can) — either outcome replaces the
  "not determinable" verdict with a decided one.
- A measurement showing `anti_join`'s absent probes are in-range and bloom saves ≥12% of its read
  groups — that would make bloom (a per-region parameter) the lever, not placement.
- Descriptors falsified by the data set: the numbers below come from `JoinWorld`, a modelled
  population (10k skus, 2k orders, 40 days), not a ≥10x instance. If real fan-out or co-location at
  scale differs (e.g. the 614-row hop grows with history length), the model, not just the answer,
  is wrong.

## 1. Evidence base and provenance

Everything in §2 is reproducible from the committed corpus and the committed model; nothing in this
section executed the engine.

| input | value / provenance |
|---|---|
| corpus | `harness/tests/fixtures/join_shapes_seed42.jsonl`, 100 requests (25 per shape), seed 42, 48 KB, sha256 `ecc271e43efcc4af0184dccec102920d61f5c4181e0c1351e30db59dd31a8d6a` |
| descriptor run | `harness/flashdb_harness/join_descriptors.py` → `.scratch/81-join-heavy-workloads/descriptors.json`, schema `flashdb.join-descriptors.v1` |
| ticket-29 cold headroom | `.scratch/29-workload-shapes/issues.md` §"RESULT (2026-10-02)", `harness/flashdb_harness/cold_axis_compare.py`, cap 192 MiB, 1,645,273,432 B = 8.17x, 10.3 M rows, device 4 KiB O_DIRECT read p50 99.9 us, fsync p50 ≈5.8-6.2 ms |
| measured co-location arm | `.scratch/50-family-colocation/issues.md` (selector), `.scratch/57-cold-reads-and-colocation/issues.md` §"Entity-first co-location arm" (cold arm: 4.03 → 3.36 blocks/request, -16.6%; compaction 20,904,807 → 265,216,151 B, ≈12.7x; lag-rejected windows 15/40, late_total 887) |
| device constants | `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`: 4K/16K/64K QD1 mean 89.2 / 110.9 / 174.0 us, p99 112 / 166.9 / 230.4 us; 4K QD32 ≈4.9 us/read; sequential 826.9 MiB/s; quiet device tail 1.26x mean |
| layout space | `.scratch/15-objective-v0/objective-v1.md` §3: (1) per-region merge/compaction policy and trigger, (2) page size and bloom bits per tile, (3) family partitioning (closed); the learned object is a per-region policy from a small published candidate set |
| "no L2 claim" | pre-registration §"What is NOT claimed": "Family co-location (L2) is being made expressible (ticket 50); no claim is made about it until it is measured on a real run." |

Reproduction (verified on this branch; the corpus hash and the descriptor JSON are byte-identical to
the committed ones):

```
cd harness
uv run --offline python -m flashdb_harness.generator --seed 42 --shapes join --per-shape 25 \
    --out /tmp/join_shapes_check.jsonl
sha256sum /tmp/join_shapes_check.jsonl tests/fixtures/join_shapes_seed42.jsonl
# ecc271e43efcc4af0184dccec102920d61f5c4181e0c1351e30db59dd31a8d6a  both

uv run --offline python -m flashdb_harness.join_descriptors --seed 42 --per-shape 25 \
    --out /tmp/descriptors_check.json
diff <(python3 -c "import json;print(json.dumps(json.load(open('/tmp/descriptors_check.json')),sort_keys=True))") \
     <(python3 -c "import json;print(json.dumps(json.load(open('../.scratch/81-join-heavy-workloads/descriptors.json')),sort_keys=True))")
# no output

uv run --offline --with pytest python -m pytest -q tests/test_join_shapes.py
# 8 passed
```

## 2. The measured descriptor table (every number with its method)

The values are the committed `descriptors.json` values. The methods are stored verbatim beside every
number in that file (key `shapes.<shape>.<descriptor>.method`); the method column below is the same
rule, shortened only by dropping the repeated corpus/seed wording.

### 2.1 Shape level

| shape | co-location key | fan-out width (median / max) | multi-hop depth | dependent-read share | co-location potential | best single key | absent share | regions / request |
|---|---|---|---|---|---|---|---|---|
| `star_fanout` | `sku` | 2 / 5 | 3 | 0.75 | 0.6479 | `sku` 0.6479 | 0.0533 | 1.0 |
| `multi_hop` | `order_no` | 614 / 614 | 5 | 0.8333 | 0.5487 | `order_no` 0.5487 | 0.2539 | 410.6 |
| `as_of` | `sku` | 1 / 1 | 2 | 0.5 | 1.0 | `sku` 1.0 | 0.0 | 1.0 |
| `anti_join` | `sku` | 1 / 1 | 2 | 0.5 | 1.0 | `sku` 1.0 | 0.1245 | 106.64 |

Method per column:

| descriptor | method (the counting rule; verbatim in `descriptors.json`) |
|---|---|
| fan-out width | per dependent hop, the number of child rows one parent row reaches after the block's declared `limit`, selected against `generator.JoinWorld`; reported as the median and max over requests of each request's widest dependent hop. A root read's row count is not a fan-out and is reported separately as `root_rows`. |
| multi-hop depth | number of reads on the longest `needs` path of the emitted request (root read counted), from the block graph; constant per shape. |
| dependent-read share | dependent read hops / all read hops; a hop is dependent iff its predicate binds `$block.field` from an earlier block's rows and cannot be issued before that block returns. |
| co-location potential | share of dependent parent rows whose every matched child row carries the parent's co-location-key value, over parent rows that matched at least one row; such a child would share a region with its parent under a region key materialised on that field. Parent rows with no match are excluded and reported as absent share. |
| absent share | dependent parent rows whose hop matched no row / all dependent parent rows. An absent row has no place to put anywhere, so no co-location key can make it cheap. |
| best single key | co-location potential maximised over the stated candidate keys `sku`, `order_no`, `lot_no`, `supplier_no`; below 1.0 means no single key co-locates every hop. |
| regions / request | distinct co-location-key values carried by the request's parent and child rows, averaged over the corpus: the number of regions a region keyed on that field makes one request touch. |
| root rows | rows the request's root read returns, median and max over the corpus; it bounds the parent rows the dependent hops fan out from. |

### 2.2 Per hop (all 25 requests of each shape pooled)

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

The per-hop columns use the same counting rules restricted to one block: `fan-out median/max` is the
per-parent child-row count after the block's `limit`; `rows total` is the child rows over the 25
requests; `no-match requests` counts the 25 requests whose hop matched nothing; `absent share` is the
hop's parents with no match / all the hop's parents; `key co-located share` applies the co-location
rule to that hop only.

### 2.3 Derived: read groups and child rows per request

The descriptors are counted per *hop*, and `dependent-read share` is a **hop** ratio, not a **read**
ratio. A set-binding hop (`$lines.order_no`, `$demand.sku`) becomes one dependent read per distinct
parent predicate, so the read count can exceed the hop count by the parent fan-out. This derivation
resolves each request's blocks against `JoinWorld` and deduplicates the concrete child predicates
(identical predicates are one read). It is the bridge from the descriptors to the cost model in §3.

```
cd harness && uv run --offline python - <<'EOF'
from flashdb_harness.generator import Generator, JOIN_NAMES
from flashdb_harness.join_descriptors import interpret
g = Generator(42, 100); world = g.world
pairs = list(zip([n for _ in range(25) for n in JOIN_NAMES], g.join_requests(25)))
for name in JOIN_NAMES:
    groups = rows = 0
    for n, req in pairs:
        if n != name: continue
        hops = interpret(req, world)
        groups += 1 + sum(len({tuple(sorted((k, repr(v)) for k, v in grp.where.items()))
                              for grp in h.groups}) for h in hops if h.dependent)
        rows += sum(len(h.matched) for h in hops if h.dependent)
    print(name, "read_groups/req", round(groups / 25, 2), "child_rows/req", round(rows / 25, 2))
EOF
```

| shape | read groups / request | child rows / request | root rows / request | per-hop read groups / request |
|---|---|---|---|---|
| `star_fanout` | 4.00 | 4.16 | 1.00 | line 1.0 (root), product 1.0, movements 1.0, supplier 1.0 |
| `multi_hop` | 1,216.32 | 1,582.56 | 4.44 | items 1.0 (root), products 4.44, lines 4.44, orders 410.6, returns 410.6, buyers 385.24 |
| `as_of` | 2.88 | 1.88 | 1.88 | facts 1.0 (root), versions 1.88 |
| `anti_join` | 107.64 | 132.44 | 151.28 | demand 1.0 (root scan), supply 106.64 |

The parent-row co-location counts (same basis as the descriptor, one group per parent row) are
`star_fanout` 1.84 co-located groups/request of 2.84 matched, `multi_hop` 601.32 of 1,095.84,
`as_of` 1.88 of 1.88, `anti_join` 132.44 of 132.44. **The anti-join is the proof that this ratio is not
a read count:** it has co-location potential 1.0 and its 106.64 distinct probes still each need a read,
because the parent rows repeat the same sku.

### 2.4 What the descriptor model cannot tell us

1. **It is a modelled population, not a data set.** `JoinWorld` has 10k skus, 2k orders, 40 days,
   6,000 order lines. `multi_hop`'s 614-row hop is a function of how many order lines the model holds
   per hot sku; at a ≥10x-history instance it grows. The descriptors are scale-dependent, and that
   direction is not measured.
2. **Limits clip fan-out.** `star_fanout` movements `limit 128`, `as_of` facts `limit 64`,
   `anti_join` demand `limit 512` and supply `limit 1`; `multi_hop`'s `lines`, `orders`, `buyers` hops
   have no limit, which is why its fan-out is the only unbounded one.
3. **Co-location potential excludes absent parents** and passes on tiny dimensions (the method says
   so). `star_fanout`'s 0.6479 counts the 25-row product dimension and the movement rows, and reports
   the 64-row supplier dimension as not co-located.
4. **`regions / request` is reported under one key only**, the shape's declared key. `multi_hop` has no
   single key that serves all hops (best 0.5487), so 410.6 is not the minimum over keys.
5. **No per-hop byte count, page count, block read, or access path is measured.** The descriptors count
   rows a predicate reaches; whether those rows share a page depends on the engine's key order and the
   index the read path uses, which is exactly what is missing (§5).

## 3. The model: what each layout axis can and cannot move

### 3.1 Terms and constants

For one request in the cold regime:

```
cost(req) ≈ q · G · L_dep(page)          # one page read per distinct dependent group
          + q · extra_pages · L_dep(page) # additional pages when a fan-out spans several
          + bytes / BW                    # bandwidth-bound scan volume
```

- `G` = distinct dependent read groups from §2.3 (one read per distinct child predicate, plus the
  root). This is the term a **placement** decision changes: co-locating a child with its parent removes
  a group (if the child fits the parent's page); a per-region parameter does not change `G`.
- `L_dep(page)` = 89.2 / 110.9 / 174.0 us at 4K / 16K / 64K QD1 (0a-redo means; ticket 29's
  calibration quotes 96.3 / 117.4 / 185.8, the same direction and ≈1.22x / ≈1.93x ratios).
- `extra_pages` = ceil(child rows / rows per page) − 1 when the fan-out rows are **contiguous**
  under the access path, otherwise ≈ the row count − 1 (one page per scattered probe beyond the
  first). This is the only term a **page size** changes, and it changes it only for a multi-row hop.
- `BW` = 826.9 MiB/s device / 605 MiB/s engine scan (0a-redo; ticket 29's headroom table).
- `q` = in-situ contention factor. Ticket 57 measured the flat flow at ≈4.03 dependent block reads
  per request costing ≈2.3 ms mean (≈575 us/read, ≈5.8x the 99.9 us idle p50) and tailing to
  100-235 ms under compaction/fsync contention. The idle constants therefore understate a read by
  ≈6x under load, which makes the **count** `G` more decisive than the per-read constant.
- `rows per request` for the volume term is §2.3's child rows plus the root rows.

### 3.2 What the measured headroom says about these terms

The ticket-29 cold re-run is the only cold comparison of the layout axes on this engine:

| comparison | measured effect | what it is evidence for |
|---|---|---|
| global 4K vs per-region mixed pages, equal 300 rps offered | 87.0 → 134.0 rps (+54%); `P\|Stock` bytes_read 7,789,516 → 2,519,567 (3.1x), p50 447.7 → 206.8 us | changing **which keys share a tile** (region partitioning) collapses cold read amplification. It conflates region mode with page size, so it is **not** evidence for page size alone. |
| eager vs defer compaction trigger | 135.0 vs 132.0 rps (statistically equal), compaction bytes 3,164,956,564 → 10,446,643 (≈303x less), win p99 107,540 → 208,515 us (≈1.9x worse), active tiles 10 → 306 | the trigger is a **pure trade-off**: endurance vs tail, no throughput win. |
| page size (device terms) | 4K 89.2 us, 16K 110.9, 64K 174.0 per point read (+25%, +95%) | larger pages make a **single-row probe worse**; they only pay when one page serves many rows of one hop. |
| bloom | absent probes in the flat flow are range-rejected before the bloom (0 block bytes; objective-v1 §3) | bloom pays only for an **in-range absent** probe, which the flat flow does not produce. |
| warm page-size/bloom/trigger runs (ticket 29's correction) | flat / dead / neutral, but cache-warm | those comparisons can only speak to CPU cost; they are not evidence about I/O. |

Two conclusions follow, and they are the spine of this report:

1. **The only measured >15% cold throughput effect in the layout space came from changing which keys
   share a tile, not from shaping a region's storage.** That is the same class of decision as
   co-location, so the measured evidence already points at placement for the shapes whose cost is a
   hop chain.
2. **The per-region shaping parameters are constant-factor or trade-off levers**: page size is
   bounded by `rows/page × L_dep` and only on contiguous multi-row hops (≤16/1.95 ≈ 8.2x read-seconds,
   0 if the hop is a single row or scattered); bloom is bounded by the in-range-absent share; the
   trigger is throughput-neutral by measurement. None of them can reduce the number of dependent hops
   or the fan-out row count.

### 3.3 Calibration limit (stated, not buried)

The model is **first-order and count-based**. It is calibrated at the device-constant level only
(0a-redo's QD1 means, ticket 29's cold p50, ticket 57's ≈575 us/read under contention). No join shape
has ever executed on the engine (§5), so:

- there is no request-level calibration for any of the four shapes; `G` and the child-row counts are
  structural, and the `q` factor is inferred from a different workload on the same device;
- the page-adjacency of a fan-out (`contiguous` vs `scattered`) is unmeasured and can move a term by
  ~8x;
- the model says nothing about the tail, and the tail is where this engine loses load (ticket 57);
- the pre-registered **≤25% p90 per-region calibration gate is not evaluable** for these shapes, so no
  simulated verdict may be quoted from this model, and this report does not quote one.

It can distinguish "a hop chain of 3-4 reads" from "a fan-out of 1,200 reads". It cannot be used to
claim a 10-15% sustained-load effect for any shape.

## 4. Per-shape reasoning

### 4.1 `star_fanout` — L2 required; per-region parameters cannot remove a hop

Structure: 1 root row + 3 dependent hops (`product`, `movements`, `supplier`) = **4 read groups** and
**4.16 child rows** per request. Fan-out is 1-5; `movements` is the only multi-row hop.

- **Page size**: `product` and `supplier` are single-row probes, so a larger page is a pure loss
  (1.25x at 16K, 1.95x at 64K). `movements` reaches 2-5 rows; at 2 KiB/row that is 1-3 pages at 4K
  and 1 page at 16K (≤≈2.4x on that hop if the pages are read serially, less if they are batched),
  and zero if the movement rows are not adjacent. On a 4-read request that is **≈1.1-1.2x** — far
  below the 15% bar, and it is the only hop page size can touch.
- **Bloom**: absent share 0.0533 (16% of the movement hop's parents), so the most it can save is
  0.16 of one hop's read per request (≈4% of `G`), and ticket 29's range-reject result says it will
  usually save 0. `product`/`supplier` are always present.
- **Compaction trigger**: the shape is read-only; its regions are compacted by the surrounding write
  flow. The trigger prices write amplification and tail (ticket 29), not this request's `G`. It is the
  one per-region parameter that could move the *sustain point* without changing `G` — by keeping probe
  regions few-block and improving p99 — but ticket 29's cold A/B measured it **throughput-neutral**
  (135 vs 132 rps) with a 2x p99 difference, so the measured lever is a trade-off and it does not
  distinguish this shape from any other.
- **L2**: 1.84 of the 3 dependent groups/request are co-locatable on `sku` (`line`, `product`,
  `movements` all carry the line's sku). Folding them into the parent's region removes up to 2 reads
  (4 → ≈2.2, ≈1.8x) and, if the sku's ≈4 rows fit one 16K page, up to ≈2x. The `supplier` hop cannot
  follow — a supplier is shared by every product of that supplier, and dual-copying a 64-row dimension
  into 10k sku regions is the wrong trade — so the shape's L2 ceiling is ≈2x and its **L2 floor after
  folding is 2 reads** (parent group + supplier).
- **Verdict**: the cost is set by 3 dependent hop groups; the per-region space cannot remove one. L2 is
  the required axis for a >15% move. The absolute win is small (4 → 2 reads), which is why this shape
  alone does not justify L2 — but it is the cleanest case in the set.

### 4.2 `multi_hop` — fan-out volume dominates; neither axis alone is sufficient

Structure: 5 hops, **1,216 read groups and 1,583 child rows per request**. The explosion is inside the
shape: 4.44 products per lot fan out to 486.6 order lines per request, and each line then becomes a
parent for `orders`, `returns`, and (through the order) `buyers`. The fan-out is uncapped by the
request, and `dependent-read share` 0.8333 (5/6 hops) understates it by the parent-row count.

- **Per-region page size / compaction**: the line fan-out is 486.6 rows (≈1 MB at 2 KiB/row). If those
  rows are contiguous under the access path, the read is bandwidth-bound (≈1.2 ms at 826.9 MiB/s) and
  page size changes little (ticket 26 measured the scan region JSON-decode-bound warm; cold is
  unmeasured but the 605 MiB/s engine scan includes CRC). If they are issued as independent point
  probes, batching at QD32 costs 486.6 × 4.9 us ≈ 2.4 ms; serial QD1 costs ≈43 ms. The page-size
  *bound* on the volume is 16/1.95 ≈ 8.2x read-seconds **only if the rows are contiguous**; if they
  are scattered, page size saves nothing. The compaction trigger likewise changes tiles per region
  and the tail, not the row count.
- **Bloom**: absent parent share 0.2539, concentrated in `returns` (76.4% of its parent groups). If
  those absent probes fall inside a tile's key range, bloom could remove up to ≈373 of the 1,216 read
  groups (≈31%); if the `Return` index is keyed on `return_no` and an order with no returns is
  range-rejected, it removes 0. Ticket 29's measured behaviour on the flat flow is the latter. This is
  a per-region parameter with a **possibly large** headroom, and it is undetermined.
- **L2**: best single key 0.5487; under `order_no` the `orders` and `returns` hops are co-located
  (601.32 parent groups/request) while `products -> lines` and `orders -> buyers` are 0.0. Per-hop
  placement (L2 with multiple keys, dual copies or a handle map) could fold the order/return hops
  (≈601 of 1,216 groups, ≈2x) but cannot remove the 486 line rows or the 385 buyer probes. L2 changes
  *seeks*, not *volume*.
- **Verdict**: **not determinable between the two spaces, and the dominant term is fan-out volume**,
  which neither removes. The honest rank is: (1) reduce/aggregate the volume (a semantics decision,
  outside the layout space), (2) page size/compaction price the volume if it is contiguous
  (unmeasured), (3) L2 removes up to half the hop groups. A page-size win and an L2 win are not
  additive and neither is established.

### 4.3 `as_of` — neither axis has decisive headroom; do not build L2 for it

Structure: 2 hops, **2.88 read groups** and 1.88 child rows per request, one region/request, absent
share 0.0. The root reads 1-5 supply facts; each fact probes one SCD-2 version on `(sku, day)`.

- **Page size**: the root scan returns 1-5 rows (one page already); the version probe is one row. A
  larger page is a pure loss (1.25x/1.95x). There is no multi-row fan-out to amortize.
- **Bloom**: absent share 0.0 — there is no absent probe to save. Dead by construction.
- **Compaction trigger**: one region, ≈3 reads; the trigger prices write amp and tail, and this
  read-only shape does not create the pressure.
- **L2**: co-location potential 1.0; a sku-keyed region with an 8-16K page could put the facts and
  their versions on one page, collapsing ≈3 reads to ≈1 (≈2.9x). But the shape is already the
  cheapest of the four, and at the flat flow's 4.03 blocks/request the engine is not read-bound by
  3-read requests — its lost load is the tail from 4+ cold reads plus compaction (ticket 57).
- **Verdict**: per-region parameters are **not the lever**; L2's ceiling is real (≈3x) but on a
  non-bottleneck. This is not a case for L2. If the as-of join proves time-tiling-sensitive (the
  dev-loop hypothesis), the axis is a time-bucket key/region boundary, which is a placement decision
  but not this shape's measured fan-out problem.

### 4.4 `anti_join` — not determinable; L2 cannot remove the probes and may fragment the scan

Structure: 1 root scan (151.28 demand rows/request) + **106.64 distinct supply probes/request**
(132.44 present child rows); 12.45% of the probes are absent; 106.64 regions/request.

- **Page size**: every supply probe reaches ≤1 row, so page size is a pure loss; the demand root is a
  151-row scan of one region (a few pages, bandwidth-bound), where page size is flat-to-modest cold.
- **Bloom**: bounded by the 12.45% absent share × 106.64 probes ≈ **≤13.3 reads/request (≤12% of
  `G`)**, and only if those absent probes are in-range. Ticket 29's measured flat-flow mechanism
  (absent prefix range-rejected before the bloom) gives 0. The descriptor does not record the key
  spelling, so this is the single largest undetermined item in this shape.
- **Compaction trigger**: 106.64 region consultations per request make tiles-per-region the tail
  driver; this is the plausible per-region lever, and it is a tail/endurance trade-off by measurement,
  not a throughput win.
- **L2**: co-location potential 1.0 is a trap here. A sku-keyed region would put each demand row's
  supply row beside it, saving one region visit per **repeated** sku, but it would scatter the
  151-row demand scan into 106.64 regions (one scan read group → 106.64 region visits) and it cannot
  place the 12.45% absent answers. The net could be **negative**, which is exactly the dev-loop's
  "co-location trap: you cannot cluster for absence".
- **Verdict**: **not determinable**; L2 is not the axis. The smallest deciding measurement is the
  in-range/absent split of the 12.45% plus a tiles-per-region count under eager/defer (§6, M-A).

### 4.5 Summary

| shape | dependent read groups / req | child rows / req | per-region parameter ceiling | L2 ceiling | verdict |
|---|---|---|---|---|---|
| `star_fanout` | 4.00 | 4.16 | ≈1.1-1.2x (movement hop only) | ≈1.8-2x (supplier hop remains) | **L2 required** for a >15% move |
| `multi_hop` | 1,216.32 | 1,582.56 | page size ≤8.2x on the volume, contiguous only; trigger tail trade-off | ≈2x on groups, volume remains | **not determinable; volume-dominated** |
| `as_of` | 2.88 | 1.88 | <1x (page size hurts) | ≈2.9x on a 3-read shape | **not the lever; do not build L2** |
| `anti_join` | 107.64 | 132.44 | bloom ≤12% if in-range; trigger tail trade-off | ≈0, possibly negative | **not determinable; points against L2** |

## 5. What is unverified

1. **No join shape has executed on the engine.** `engine/src/request.rs` requires the `cust -> take`
   graph and the `sku/loc/qty/email/order_no` params (`validate`, lines 129-155), and its read path
   accepts only Stock get / Customer find / StockMovement scan by `order_no` (`validate_read`,
   lines 230-263). A multi-row `$block.field` binding has no execution path — `bind` (lines 176-190)
   resolves one value per block. The anti-join's natural write half (aggregate-to-write, contract L-07)
   is also missing, so a replayed anti-join would measure only its detection half. So every per-hop
   cost in this report is structural, not observed, and the fan-out widths are counted against
   `JoinWorld`, not a real history.
2. **No per-hop page/block count, bytes read, or access path is measured.** The `contiguous` vs
   `scattered` question that carries the page-size bound (≤8.2x) is entirely unmeasured, as is which
   entity index each hop would use.
3. **The absent-probe in-range split is unmeasured** for all four shapes. It bounds bloom's headroom
   (multi_hop ≤31%, anti_join ≤12%, star ≤4%, as_of 0) and it is the difference between "a per-region
   parameter matters" and "it is dead".
4. **L2 is not implemented and has no real run.** The only measured co-location arm is *within-entity*
   family co-location (ticket 50/57): -16.6% blocks/request for ≈12.7x compaction write amplification
   and a worse tail (lag-rejected 15/40, late_total 887 vs 597). Cross-entity L2 must pay at least
   that write cost (dual copies add copies; a handle map still pays relocation), not less. Nothing
   here says L2 delivers its structural ceiling; the ceiling is a bound, and the one measured
   placement win did not convert into a sustained-load win.
5. **No calibrated simulator.** The pre-registered ≤25% p90 per-region calibration gate cannot be
   evaluated for these shapes (no join-shaped engine run, and the device/CPU term separation is still
   the recorded gap in `.scratch/14-layout-harness/gap-analysis.md`). Every number above is either a
   structural count or a device constant, not a simulated verdict.
6. **No sustained-load measurement of any join shape.** The pre-registered 15% / 5% / 3x thresholds
   are untouched and unevaluated for these shapes; this report does not clear or fail them.

## 6. The smallest measurements that would decide

The descriptors are enough to rule out a *large* per-region win on `as_of`, and to locate
`star_fanout`'s headroom in placement, but not enough to decide `multi_hop` and `anti_join`. Two
measurements, in ascending cost:

**M-A (engine-free): a page-level version of the ticket-43 dependent-read count.** Extend
`harness/flashdb_harness/placement_headroom.py`'s model — which already counts tile touches from the
committed generator trace — with (i) the engine's real key order and the index spelling each hop would
use, (ii) page granularity 4 KiB vs 64 KiB, and (iii) the per-hop fan-out rows, and report **pages
touched per request per hop**. It needs no engine run and no calibrated simulator; it decides:

- whether `star_fanout`'s movement hop and `multi_hop`'s line fan-out are contiguous (page-size lever)
  or scattered (no page-size lever);
- the tiles-per-region count for `anti_join`'s 106.64 probe regions under eager vs defer (compaction
  lever);
- whether the absent probes land in range (bloom lever) or are range-rejected.

Its calibration limit is the same as ticket 43: it is a count under a stated layout model, not a
latency; it decides structure (which axis has a count lever) and does not decide a throughput
threshold.

**M-B (engine, the only decisive throughput measurement): multi-row binding + a cold ≥10x run.** Add
multi-row `$block.field` set binding and the four shapes to `engine/src/request.rs` (the missing
capability the build record already names), then run one cold (cap 192-512 MiB, data ≥10x) offered-load
sweep per shape with the same trace under `flashdb-declared` 4K, `flashdb-declared` 64K, an eager and
a deferred trigger, and — after L2 exists — one co-located arm, with the pre-registered lag rule. This
is the only measurement that can clear or fail the 15% threshold on a join shape, and it is the
pre-registration's own gate ("only real-engine arms may decide the thresholds").

**Which is smallest for which question:** M-A decides the layout-space structure for `multi_hop`,
`anti_join` and the page-size/bloom/trigger sub-questions (and is the cheapest way to avoid building an
L2 that cannot win); M-B decides the pre-registered throughput claim and needs an engine capability
that does not exist yet. Neither is in this milestone's scope, and neither is run here.

## 7. Effect on the objective and the pre-registration

None. No threshold, arm, workload, seed count or objective statement is changed. The report proposes no
product-design change and implements no L2. The L2 go/no-go remains the author's reserved decision, and
the pre-registration's rule stands: no claim about L2 until it is measured on a real run.
