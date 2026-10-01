# Layout space proposal (0b) — critique of L1 and a concrete minimal first space

Status: proposal for the author/orchestrator. Written 2026-09-30. No source modified.
Scope: answers `.scratch/14-layout-harness/gap-analysis.md` "Layout space" gap and the
`docs/dev-loop.md` step-0b experiment. Grounded in `docs/{design,objective,learning,engine,dev-loop}.md`,
`.scratch/12-compaction-amplification/issues.md`, `.scratch/14-layout-harness/gap-analysis.md`,
`engine/src/keys.rs`, and the tile/publish path in `engine/src/engine.rs`
(`write_verified_tile`, `merge_run`, `stream_merged_reduction`, `TileWriter::push` in `engine/src/tile.rs`).

## Headline correction to L1

**L1's "per-tile clustering/ordering choice" is not a free knob in this engine, and as
stated it cannot express the star-schema fan-out hypothesis it is claimed to serve.**

Two facts from the code settle it:

1. `TileWriter::push` rejects any key not strictly greater than the previous one
   (`engine/src/tile.rs`), and `Tile::open`'s directory is a **monotone fence list**
   (first-key-per-block, sparse RAM index). A tile therefore has exactly one physical
   order: the byte order of the encoded key (`P|entity|0|handle|~csn`, `U|...`, `R|...`).
   You cannot permute physical pages without replacing the fence directory with a full
   key→page map — a format and read-path change well beyond a "per-tile descriptor".
2. A published tile is a **CSN-window projection of the serving memtable holding all four
   families interleaved by key** (`write_verified_tile`). So the entities in a tile are
   contiguous byte-runs (`P/Order`, `P/OrderLine`, `P/Stock`...), **not interleaved by any
   join key**.

Consequence: "build a tile around a declared unique index / reverse target / time bucket,
in that order" either (a) is a no-op for fan-out, because the key encoding already fixes
the order and the read path fences on it, or (b) silently requires cross-family
co-location, which is **L2**. Star-schema fan-out collapse = putting an order_line row in
the same page as the product/stock rows it references ⇒ a **cross-entity** reordering ⇒ L2.
**L1 cannot move the star-schema needle.** If 0b's stated hypothesis is the star-schema
fan-out row of `docs/dev-loop.md`, then L1's omission of co-location *does invalidate that
first experiment* — it would measure a no-op and could falsely reject the thesis.

## 1. What actually varies per tile, and the enumerated candidate set

The implementable, per-tile axis behind the existing writer is **composition, not order**:
*which encoded key families/prefixes a tile materializes*, plus the two already-existing
physical parameters. A tile that omits a derived family is still correct because
`docs/engine.md` defines derived prefixes as **recomputable** (rebuilt from `P/` by scan),
which is exactly invariant I2. This maps onto the *implemented* "what the learner controls
in v0" table in `docs/engine.md` (which `U` prefixes to materialize, which `R` prefixes to
retain, encoding, per-prefix compaction trigger, page size), and it is the smallest
surface that is (a) behind the writer, (b) brute-forceable, (c) able to show a win.

**Enumerated candidate set `C` (8 options), per *prefix-region* (not per tile — see the
granularity note):**

| id | sort family | materialized families | page | bloom bits/key | rationale |
|----|-------------|-----------------------|------|----------------|-----------|
| `base` | primary | P+U+R | 16 KiB | 10 | today = the `flashdb-declared` control (default) |
| `p_only` | primary | P | 16 KiB | 10 | drop all derived; pure base; anti-churn floor |
| `p_r` | primary | P+R | 16 KiB | 10 | fan-out via reverse prefixes hot |
| `p_u` | primary | P+U | 16 KiB | 10 | point lookups via unique prefixes hot |
| `p_r_u_nobloom` | primary | P+R+U | 16 KiB | 0 | isolates the bloom's cost/benefit |
| `base_p4k` | primary | P+U+R | 4 KiB | 10 | finer access granule (0a: 4K QD1 96.3 µs) |
| `base_p64k` | primary | P+U+R | 64 KiB | 10 | coarser granule (0a: 64K 174 µs) |
| `base_bloom14` | primary | P+U+R | 16 KiB | 14 | bloom saturation study |

`|C| = 8`, a small product over {families-materialized} × {page} × {bloom}; expandable to
~12 by adding encoding (zstd level) or a `time-bucket` grouping once the first axis has a
measured win. `sort_family` is retained in the descriptor for forward-compat but has one
implementable value today (`primary`); it becomes meaningful only when L2 adds a
cross-family clustering key. Brute-force / relaxed oracle: per-region best over `C`, scored
by the simulator — O(|C| × #regions), trivial at |C|=8.

**Granularity.** A derived prefix is *cumulative across CSN windows*, so materializing
`R/Order/line` for one tile but not the next would make its acceleration inconsistent
across the range. The decision therefore belongs on a **prefix-region** (a (family, entity,
field) → CSN-range) rather than a single tile. This matches `docs/engine.md`'s
"compaction trigger per prefix-region" and keeps I3 (no global layout state).

**What it CANNOT express:** cross-entity co-location / join-key clustering (**L2**),
learned page packing and quantized widths (**L3**), non-primary in-tile sort orders, and
anything needing dual copies or a handle map. Only the L2 omission is fatal, and only to a
0b *scoped to star-schema*. It does not invalidate a 0b scoped to a within-entity win.

## 2. The declaration surface

**Descriptor.** `TileLayout { sort_family: u8, families: u8 /*bitset P/U/R*/, page_size:
u16, bloom_bits_per_key: u8, encoding: u8, version: u8 }`. One descriptor per
prefix-region; a region's descriptor is applied to every tile the region produces.

**Where stored.** Not in the per-tile manifest `TileRef`: the A/B page is bounded to 4092 B
(~90 short-name tiles, `.scratch/12`), so per-tile descriptors would blow the page
immediately. Store a **`layout_id: u8` in the manifest tile record** referencing a small
**policy table** (its own A/B page, or a reserved meta key). The policy table maps
`layout_id → TileLayout`; changing a policy rewrites the table, never the tiles, and never
the tile naming/recovery invariant.

**Who decides / who applies.** Exactly one applier: the **tile builder**. `write_verified_tile`
and `write_compacted_tile` take a `&TileLayout` and (i) filter the key stream to
`families`, (ii) set `page_size`/`bloom_bits`. Publish and compaction both call a single
`layout_for(region, now) -> TileLayout`; neither hard-codes order or membership, and a
compaction output's layout is **re-chosen, not inherited** (mixed layouts are legal, I3).

**Safe default.** `layout_id = base` = today's behavior, all families, 16 KiB, 10 bloom
bits. A reader encountering an unknown/absent descriptor must fall back to today's
behavior; never fail closed on a *layout* value.

**Invariants enforced (state these exactly):**
- **I2-fallback.** A descriptor may change only *which keys a region materializes* and how
  they are physically grouped. It may never change the set of logical keys the engine is
  obligated to answer for. Any read a materialized prefix cannot answer must fall back to a
  full scan of the base `P/` prefix and still return the newest version. A wrong descriptor
  is therefore *slow, never wrong* (I2).
- **No-rewrite-on-drift.** A layout change is realized only by writing *new* tiles (a new
  CSN window or a compaction output). Correct existing tiles are never rewritten to satisfy
  a changed reader; the reader is layout-polymorphic. A rewrite is allowed only to *gain
  speed*, never required for correctness (I1: every intermediate state is servable).
- **Monotone directory preserved.** Any page-size change still yields a monotone fence list,
  so the existing `Tile::open` seek path is unchanged.

## 3. Cost model the simulator needs

Terms, with the measured statistic that validates each (all constants from 0a-redo,
`.scratch/05-engine-v0/evidence/0a-redo/`, and `.scratch/12`):

```
D        = dependent-read depth            (structural; = hop count from the trace)
F_i      = independent fan-out at a hop    (parallelizable)
cov      = fraction of a pattern's keys the materialized layout covers
L_dep    = 96.3 µs  (4 KiB QD1) / 117.4 µs (16 KiB QD1)   <- dominant term
L_bat    = 4.9 µs   (per completed read at QD32, 203k IOPS)
BW       = 831 MiB/s (fio seq) ; engine scan 605 MiB/s incl. per-block CRC
fpr      = 0.0079   (bloom false-positive rate)
q        = 1/(1-rho)  queue factor (the SLO anchor's shape; see caveat)

service(layout, pattern) =
      D            * L_dep(page_size)                 # dependent random reads
    + ceil(F_i/32) * L_bat                            # batched independent reads
    + bytes_read   / BW                               # bandwidth term
    + D * fpr      * L_dep                            # bloom-miss extra read
    + dir_seek(page_size)                             # one fence block per seek
    + (1 - cov)    * D * L_dep                        # scan fallback for omitted prefixes
latency = service * q
```

Term → validating statistic:
`L_dep` ← 0a-redo 4K/16K QD1; `L_bat` ← 0a-redo QD32 IOPS; `BW` ← fio 1 MiB QD8 (device)
vs `scanbench` 605 MiB/s (engine, includes CRC ⇒ validates the CRC term); `fpr` ← 08d-2b
probe table (0.56–1.59 µs with bloom vs 15–152 µs without, over 1/4/12 tiles) ⇒ validates
both the bloom term and `dir_seek`; `write_amp`/`tile_bytes_compact` ← 12d (3.92 at 5 GB,
budget_prefix) ⇒ validates the write side of the reorg term.

**F14 — which functional is calibrated (answer).** First version calibrates the
**p99 of per-request service latency per pattern key, at QD1 (queue-free)**, scored by a
*quantile-calibration error* (predicted vs observed p99 per pattern) with prediction
intervals. Why this is the right first choice: (i) the angriness penalty consumes a
*percentile*, so a mean-calibrated simulator systematically mispredicts the quantity the
objective charges (F14); (ii) on this device the tail is benign (QD1 p99 = 1.26× mean), so
p99 is *structurally predictable* from dependent-read depth — the calibration is learnable;
(iii) p99 is estimable at the small sample counts 0b can afford, unlike CVaR/p99.9; and
(iv) calibrating **service time at QD1** decouples the structural term (layout → dependent
reads) from the queueing anchor, which `docs/objective.md` still marks Open (F1–F4). Caveat
to pre-register: if the estimand is later chosen as mean or CVaR, this calibration target
must be redone; p99 is chosen because it is the best-supported first estimand, not because
F1 is settled.

## 4. Minimal experiment 0b that could disconfirm the thesis

- **Instance size.** Must exceed cache, or the fan-out premise is untested (`docs/dev-loop.md`).
  Use the reference 200 GB / 8 GB cgroup, or the scaled cgroup instance (`MemoryMax` ≥ 1× and
  data ≥ 10× that budget). The relaxed oracle is run *in the simulator* at this size, so it is
  not bounded by RAM.
- **Oracle.** Relaxed, per `review-02` F1: per-prefix-region best over `C` (|C|=8), reported
  as `relative_regret = (cost_learner − cost_best_candidate) / cost_best_candidate`,
  normalised by `A_baseline − A_oracle`.
- **Seeds.** Pre-register N = 10 access-distribution seeds (Zipfian popularity assignment);
  drift is *injected deterministically*, not sampled. Report median and IQR.
- **Primary metric.** Per-pattern **p99 service latency** (the F14 functional), aggregated to
  relative regret vs the relaxed oracle. Deterministic metrics (bytes, dependent-read counts,
  write_amp) are the tie-break, per the shared-machine caveat in `.scratch/12`.
- **Controls.** `flashdb-declared` = `C[base]`; `flashdb-static-derived` = offline
  argmin over `C` from the frozen trace, built once, no drift/adaptation.
- **Pre-registered falsification threshold.** *At ≥10× cache-size data, if the learner does
  not beat `flashdb-declared` by >15% on the win workload's normalised regret (median over
  10 seeds), OR fails to at least tie the adversarial-uniform workload (median regret ≤
  declared's + 5%), the learned-layout thesis is rejected for this layout space.* Drift is
  scored separately as windows-to-recover, with the reorg payback window stated.
- **Smallest workload set that can show a win or a tie.** Two workloads:
  1. a **win-capable** workload L1 can actually move — a hot-key **point-lookup / within-entity
     fan-out** workload (hot unique probes and/or an `R` reverse prefix), since L1's knob is
     prefix materialization;
  2. the **adversarial uniform-random** workload — the anti-overfitting case: the learner
     must recognise there is nothing to win and *not churn* (tie, not loss).
  A third (flat aggregate scan) is recommended but not required, to check specialization has
  not wrecked scans. **Do not use the star-schema rows of `docs/dev-loop.md` as 0b's win
  case** unless L1 is reframed/extended (see §1) — L1 cannot move them.

## 5. Three riskiest assumptions, and the cheapest test for each

1. **Per-tile "ordering" is an implementable, non-trivial knob** (the L1 premise).
   Test: build a tile in a non-default order behind `TileWriter`. It fails immediately —
   `push` demands strictly increasing keys and the directory is a monotone fence list.
   Cost: minutes (read `tile.rs`) ⇒ reframe L1 to *composition*.
2. **The candidate set `C` has enough layout headroom for a >15% win** — if the spread
   `cost(C[best]) − cost(C[worst])` on the win workload is small, no learner can win and 0b
   is unfalsifiable-by-construction. Test: score all `|C|` options in the simulator on the
   win workload at a small instance; check the spread against 15%. Cost: hours.
3. **The simulator's structural term predicts measured p99 within the acceptable error
   gate.** Test: build two tiles with different `C` layouts over the same frozen corpus, run
   the real engine, compare measured p99 against the simulator's prediction on that pair
   (also the necessary step-2 calibration target). Cost: ~1 day (needs the descriptor wiring,
   which is the enabler anyway).

Hidden cross-cutting risk: the objective's estimand is **Open (F1–F5)**, so any ">15%"
threshold is provisional and must name its functional; a change of estimand nulls the
pre-registered threshold and forces recalibration (§3).

## Bottom line

Adopt a **composition-based** L1 (per-prefix-region materialization of P/U/R + page size +
bloom bits), not an ordering-based L1. A per-tile *ordering* knob does not exist behind the
current writer, and it cannot express star-schema fan-out (that is L2). Scope 0b's
win hypothesis to a workload L1 can move (hot-probe / within-entity fan-out) plus the
adversarial-uniform anti-overfitting case; keep L2 (key-family co-location) as the explicit
second increment, gated on a 0b win.


---

## Orchestrator review (2026-09-30) — what I adopt, and one correction the code forces

**Adopted, and it is the important part:** the critique is correct. A per-tile *ordering* knob
does not exist in this engine (`TileWriter::push` requires strictly increasing keys; `Tile::open`
seeks a monotone fence list), so ordering is a no-op, and cross-entity co-location is L2. The
star-schema rows of `docs/dev-loop.md` cannot be 0b's win case for this space, and the win case
must be scoped to a workload L1 can actually move. Also adopted: |C| small and enumerated,
per-prefix-region granularity, declaration separate from tiles, one applier, I2-fallback and
no-rewrite-on-drift, the cost-model terms with their validating statistics, the F14 answer
(calibrate p99 service latency at QD1), and risk #2 as the next cheap test.

**Correction — "materialized families: P only / P+U / P+R" is NOT safe in v0.** I verified the
claim that an omitted derived prefix is rebuilt "from `P/` by scan" (proposal §1, relying on
`docs/engine.md` SS-24 and I2). That is a **v1** property: `docs/contracts.md` states derived
layouts are read-only accelerators *with a fallback to the base copy*, which requires the base
copy to contain enough to recompute the derived entry. In today's engine the document payload is
**opaque to the engine** (the adapter writes `P` and `U`/`R` entries as independent keyed ops;
nothing in `engine/` can extract a declared field from a doc value). So a tile that omits `U`
does not merely lose an accelerator - those `U` entries exist nowhere, `unique_lookup` misses
them, and the engine would silently allow a duplicate business key. That is a P1-class
correctness break, not a performance knob.

Consequences for the first space:

1. **Omission is off the table until the base copy is self-describing** (a v1 schema/extraction
   item). Family *partitioning* - keeping all entries but choosing *which tile* holds them (a
   separate companion tile per family, so the stable `R` family is not rewritten when the hot
   `P` family compacts) - preserves correctness trivially, since the read path already merges
   every active tile. That is the safe way to get the family axis, and it is a genuine
   write-amplification knob for families with different update rates.
2. **The knobs that are safe, self-describing and measurable today** are the ones the tile
   superblock already carries: **page size** (the field exists and is currently pinned to 4096;
   0a measured 96.3 us at 4 KiB vs 117.4 us at 16 KiB) and **bloom bits/key** (per tile, 0 =
   no bloom; 0.79% FPR at 10 bits measured). Both are per-tile, need no new durable structure,
   and cannot make a read wrong - only slower.
3. **Headroom must be measured before any learner is written** (risk #2). The next step is the
   simulator with the cost model in §3, scoring a candidate set over the frozen corpus, to check
   that any candidate differs from `base` by more than the provisional 15% threshold. If it does
   not, the space (not the learner) is the problem, and family partitioning or L2 must come first.
