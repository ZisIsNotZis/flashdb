# 03-grammar-decisions — settle the request grammar before freezing it

Status: needs-info (five items need an author decision; nine need specification work)
Blocked by: author decisions 1–5
Ticket: this file. Decision ticket — question in, decision out.
Parent: `.scratch/01-design/` (the review that produced this list)

## Why this exists

`contracts.md` froze a request grammar that a language review (`review-04`, aspect: language design and canonicity) found to be **a sketch, not a specification**. Fourteen items are undefined or incoherent, and four of them are load-bearing for the motivating inventory workload. Freezing this grammar as-is would mean implementing semantics that were never decided.

The items were recorded inline in `contracts.md`, which pushed that file past its size budget and put a decision backlog inside a design document. Both are structural defects, so the backlog now lives here and the contract carries a pointer.

## Author decisions — five, each changing design truth

`R-DOC.1(a)` reserves these to the author. Each has a recommendation; none is taken.

| # | Question | Why it matters | Recommendation |
|---|---|---|---|
| D1 | **Program-level fallback: allow or not?** | The stage-level restriction makes two-stage conditional branches inexpressible ("if not stocked here, create a transfer request *and then* reserve incoming stock; otherwise decrement *and then* write a movement row"), each needing a second round trip — contradicting I5. It also **silently overrode the author's own round-7 instruction** that "one single request for all business is more important" than grammar purity. | **Allow it.** The server normalizer is already authoritative (I12), so the cost is one dependency-aware algorithm, not an API redesign. |
| D2 | **Reject non-canonical requests, or normalize and accept?** | Three documents say three things: the architecture table says non-canonical forms are rejected, I12 says clients may normalize as a convenience, and `contracts.md` says the server normalizer covers what the grammar cannot. The client cannot predict its own pattern key without an answer. | **Normalize always**, return the canonical form in the outcome so clients can learn it, and return `non_canonical: true` as a warning rather than an error. Reserve rejection for malformed trees. |
| D3 | **Are predicate-addressed writes first-class?** | Without `update-by-predicate` / `delete-by-predicate`, "expire reservations older than 30 minutes" or "mark all lines of order N shipped" force the client to enumerate keys it does not have — at 10 TB. | **Yes, with a bounded row cap** and a defined oversized-result behaviour. |
| D4 | **Are results ordered? How are top-N and pagination expressed?** | "The 3 most recent open orders", "top 10 SKUs by days-of-cover", and every paginated listing are currently inexpressible. A cursor also needs a stable order. | **Add `order` / `limit` / `after` to the read shapes**, and accept that a sort becomes part of the intent and therefore of the pattern key. |
| D5 | **Request-level or per-block atomicity?** | Per-block atomicity (the author's round-6 instruction) contradicts the motivating correctness oracle, which needs `decrement stock iff insert lines` to be atomic. Carried over from `review-02` F4; the same gap appears from the contract side as `review-04` L-18. | **Request-level by default**, per-block as opt-in. Nearly free: one commit record already covers the whole request, so only *publishing* a partial prefix costs extra. |

## Specification items — nine, no author decision needed

| # | Gap | What must be written |
|---|---|---|
| L-01 | `ifEmpty` has no meaning | A per-shape result type (`rows` / `count` / `affected` / `no-value`), and a definition of "empty" over it. Then decide explicitly: for a guarded write whose predicate is false, is the block *empty*, *failed*, or *applied-no-op* — and does that trigger the fallback? Without this, `decrement iff available, else backorder` is either inexpressible or silently corrupting. |
| L-02 | No outcome document; continuation unimplementable for reads | The outcome schema (fields, ordering, block addressing, error taxonomy including conflict / predicate-false / validation-mismatch / shed / not-reached), the dedup-replay response, and the retention window as a documented client contract. State plainly whether continuation is possible for read blocks at all; the engine guarantees idempotent *application* but not idempotent *observation*, because re-running a read after a prefix commit sees post-write state. |
| L-03 | No predicate or expression language | Operators (`= != < <= > >= IN BETWEEN IS [NOT] NULL`), type rules and coercion, NULL/absent semantics, collation, aggregate functions, grouping, arithmetic in update values, and the **normal form among them** (today `empty ≡ count == 0`, `field == v ≡ field in [v]`, and `in` is the only way to spell a disjunction). |
| L-04 | Canonicity is stated as "intent" | The declared, decidable equivalence relation `E`; the normalizer with termination and confluence arguments plus idempotence (`normalize ∘ normalize = normalize`); and collapse rules for the six residual equivalence classes (block order within a stage, `ifEmpty: []` vs absent, two stages vs one, join spelling vs shared-key spelling, singleton `in` vs `==`). |
| L-08 | Schema canonicity claimed, never defined | Either the canonical schema serialization (sorted fields, explicit constraints, resolved defaults, no derivable constraints) plus an equivalence test, or retract the schema half of the claim. |
| L-13 | Join semantics and result cardinality undefined | Join type (inner / left / semi / anti) if `join` stays user-expressible — otherwise delete it from the surface and express joins only through shared key fields. Also per-shape result cardinality and the meaning of a set-valued reference where a scalar is expected. `Lost sales` needs an anti-join and cannot be written today. |
| L-16 | Wire-form canonicalization missing | Number encoding (`1` vs `1.0` vs `1e0`), Unicode normalization, duplicate object keys, member order, `null` versus absent, and the sort order of parallel blocks. |
| L-20 | Intra-request write-write conflicts undefined | Two parallel blocks may both read `stock == 1`, both pass the guard and both decrement, because validation is against tile versions and not against sibling writes. Decide: reject at the front-end on key overlap (decidable for key-addressed shapes), serialize same-key writes deterministically, or merge compatible writes. **Recommended: reject**, which matches the "mechanical check the API can report" intent. |
| — | Request-size and nesting bounds | Unbounded `[stage,…]` / `[block,…]` arrays are a resource-exhaustion surface. No limit is stated anywhere. |

## Also required before this ticket closes

1. **A verb table.** The example uses `read`, the previous revision used `find`, and the shape list uses `read`. One vocabulary.
2. **A golden canonical-form corpus** and property-based generation over the grammar, so `normalize ∘ normalize = normalize` and `x E y ⇒ normalize(x) = normalize(y)` are testable rather than asserted. This is the only way "one intent one way" acquires a truth condition.
3. **A trace-residue metric** — unique canonical forms per distinct logical plan — carried over from `decisions.md`, which proposed it and which the design documents then dropped.
4. **The reference language `$n.field`**: whether it is 1-indexed from stage 1, whether it addresses an applied result *including* its fallback, and whether it may appear in predicates, update values, projections and order keys.

## Acceptance criteria

1. Every item above is either specified in `contracts.md` or explicitly withdrawn from the design.
2. `contracts.md` is back within its declared budget, with the open backlog pointing here.
3. A golden-corpus test exists that fails when a canonicalization rule is removed.
4. No item remains in an undecidable state: for each, a fresh agent can determine compliance by reading the contract.

## Not in scope

Re-running the language review. This ticket resolves the findings it produced.
