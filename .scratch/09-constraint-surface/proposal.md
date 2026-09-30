# Constraint surface v1 — design proposal (draft for author review)

Status: draft, 2026-09-30. Author approved drafting; nothing here is implemented.
Companion: `.scratch/08-tile-pages/issues/08-tile-pages.md` (engine work),
`docs/contracts.md` (L-items this would extend).

## 1. Semantics (already settled, restated so the surface follows from it)

1. A constraint is a predicate over the state a block **would produce**:
   `post = published ∪ net_effect(block)`. Never the pre-state.
2. Tentative apply (MVCC overlay) → validate → persist → only then claim success.
3. A violation is a **terminal** error (already-exists / failed-precondition class):
   not an `else` branch, never auto-retried. Only *temporal* optimism may retry
   bounded, and it needs its own result type.
4. No history: predicates see the latest state only.

## 2. The unified mechanism: every form supplies three things

| part | question it answers |
|---|---|
| **anchor** | which keys/rows must be examined, derived from the block's ops without reading data |
| **witness** | how the predicate is evaluated over post-state: block-local cancellation first, then a query against published state for what the block does not determine |
| **fail mode** | terminal conflict, or (only when the anchor is unbounded) declared deferred/audit |

An engine that has these three per form needs no per-form special case in the write
path. Form #1 (`unique`) is already implemented and is the template.

## 3. Form catalog — the "奇奇怪怪的 form", each mapped onto the mechanism

| # | form | anchor | published-state query | intra-block effect |
|---|---|---|---|---|
| 1 | `unique(e, f)` — **implemented** | touched (e,f,value) | owners of value, minus block-touched handles | last-op-wins per (value,handle); release+reuse and swaps become legal |
| 2 | partial unique `unique(e,f) where p` | (e,f,value) **plus** the fields `p` reads | owners + the row of each candidate owner (need `p`'s fields) | block may toggle `p` (e.g. status→closed) *releasing* the value |
| 3 | composite unique `unique(e,(f1,f2))` | composite tuple | same as #1 with a composite prefix | same as #1 |
| 4 | exclusion / no-overlap (`room`, `[t1,t2)` no overlap) | a **range** per touched row, per declared index | range-overlap query on the declared index | block may move rows out of an overlap, making a pair legal |
| 5 | foreign key (child→parent exists) | referenced parent handles | existence of the parent in post-state | **the block itself may create the parent** — joint feasibility is the common case, not the exotic one |
| 6 | derived/materialized aggregate (our `PutReverse` is instance #1) | the group/aggregate key whose value changes | aggregate over the group | block-local deltas cancel; bounded by group size |
| 7 | row-local check (`a < b`, non-negative, enum) | the touched row | none (the row is materialized from the block + one read) | free: RAM only |
| 8 | unbounded/global (± "total = Σ lines", "≤ N open orders") | **unbounded** | not probe-able | none — this is the only honest home of *deferred* |

Observations that fall out:

- **#5 justifies the author's instinct the strongest.** A foreign key is *usually*
  satisfied by the block itself; validating against the pre-state would reject the
  most common legitimate pattern. Post-state validation makes it free.
- **#2 and #4 and #7 require row materialization**, not just key arithmetic: the
  anchor enumerates rows, and the predicate reads fields the block may not touch. So
  the overlay must be defined over **rows**, not only over unique keys: for a touched
  handle, `row_post = block_ops(handle) over published_row(handle)`. This is the
  generalization the current `unique`-only implementation lacks.
- **#8 is the only class where deferral is correct** (audit + violation reporting),
  and it must be an explicit declaration, never the default.
- **#4 is where cost lives**: an exclusion constraint needs an index, or it degrades
  to a scan. That is a *declared index* requirement, and it is measurable — which
  makes it a natural place for the layout-learning work rather than an assumption.

## 4. Plan selection (semantics-free, where learning plugs in)

For a given validation, the engine chooses among:
- **RAM-only** (block-local): anchors fully determined by the block (swaps, handovers, #5 with in-block parent, #7).
- **bloom probe** per net-affected key: "no published owner" answers in RAM, zero I/O (#1/#2/#3 without row materialization).
- **point probes**: N parallel page reads (random keys — the block's key span is the whole keyspace).
- **merge join**: sort the block's anchors and range-scan per tile (clustered/monotonic keys — `order_no` streams).
- **index range query** (#4, #6).

All five produce identical accept/reject decisions; only cost differs. This is exactly
the "learned layout/plan" surface that is defensible: two known-cost alternatives, a
measurable workload signal, and no semantic change.

## 5. Durability interaction

Validation results are **not** durable state: a rejected block never reaches the WAL, so
recovery never re-validates, and no dedup/constraint journal is needed — consistent with
the author's earlier decisions (no durable request-id dedup; conflict ≠ `else`).
Deferred (#8) violations do need durable reporting: a conflict-record area, which is the
one piece of *new* durable structure this proposal implies.

## 6. Open decisions for the author

- **D1 — is the deferred/audit class (#8) in v1?** Recommendation: declare the marker
  in v1 (so declarations are forward-compatible) but implement the audit runner later.
- **D2 — FK actions.** `CASCADE` implies writes the block never declared, which breaks
  "the block is the single source of truth". Recommendation: `RESTRICT`/`NO ACTION` in
  v1; express cascade as explicit client ops inside the same block.
- **D3 — may a constraint read a derived/materialized aggregate (#6)?** Recommendation:
  yes, but the derived state must itself be **declared** (as `PutReverse` already is), so
  constraints stay "row-local + declared aggregate" instead of arbitrary computation.
- **D4 — predicate vocabulary.** Minimal v1 subset (equality/range/is-null over declared
  fields) vs waiting for the expression-syntax page. Recommendation: minimal subset now;
  keep the declaration format open.
- **D5 — range forms (#4) require a declared index?** Recommendation: yes — refuse to
  declare an exclusion constraint without an index that covers it, rather than silently
  scanning (cost would be invisible and unbounded).

## 7. Consequence for the in-flight engine work (08)

- 08a/08b (page format, zero-copy reader) are prerequisites for every plan above; unchanged.
- 08c bloom should be keyed by the **net-affected anchor set**, and for partial unique
  (#2) it must be keyed by `(field, value)` only — the predicate's fields are resolved by
  row materialization, not by the filter. (Getting this wrong would silently break #2.)
- Row materialization needs the point-lookup fast path from 08b, so #2/#5/#7 land after it.
