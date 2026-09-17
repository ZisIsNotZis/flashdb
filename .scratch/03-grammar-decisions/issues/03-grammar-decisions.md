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

## Resolution (2026-09-15, from the author)

**D5 — per-block atomicity, reaffirmed.** The author re-confirmed his round-6 instruction: a block is the atomicity unit, not the request, and he had already answered this. The recommendation for whole-request atomicity is **withdrawn**, and the design now honours per-block atomicity soundly. Consequences, all accepted:

- **A block is a transaction and may span tables.** "Decrement stock iff available, then insert the movement row" is one block containing several writes, applied atomically. Without a multi-write block shape, per-block atomicity makes that motivating case unexpressible (L-18) — so the shape is added rather than the atomicity model changed.
- **`else` is defined per block kind,** which resolves L-01: a read block's result is `rows` or empty, a write block's is `ok` or `failed`. `else` means "the block did not achieve its goal" — zero rows for a read, guard-false or conflict for a write. Writes never return empty, so `if_empty`-on-write is a validation error rather than an ambiguity.
- **Bindings are journalled (L-02).** Each block's WAL record carries the `$n` bindings it consumed, so dedup-replay returns recorded bindings for completed blocks instead of re-reading post-write state. Idempotent *observation* is thereby achieved, not just idempotent application. Retention becomes `retry_horizon_s`, a declared field of the service class, so it is a client contract rather than a decorative engine detail.
- **Intra-stage key overlap is rejected at the front-end** (L-20): two parallel blocks writing the same key is decidable and rejected, so the stock-oversell case cannot arise.

**D1–D4 — set to the assistant's recommendations as defaults, veto-able:** program-level `else` is allowed (D1); the server always normalizes and returns the canonical form, rejecting only malformed trees (D2); predicate-addressed writes exist with a bounded row cap (D3); `order` / `limit` / `after` exist on read shapes (D4). These are defaults taken under the author's standing instruction to keep the transaction tax minimal; any of them can be overruled.

## Addendum: reference model — v2（2026-09-15，修订，撤销同日 v1）

**撤销 v1 的“引用必须声明 key”。** 技术理由：每个 `x-unique` 为了执行唯一性检查**本来就必须**带查找结构，所以所有解析路径已经物化——“选哪条路”没有成本。关系库逼你选外键，是因为每条路径都要你声明并维护一个索引；这里没有这笔成本，模糊性是免费的。v1 的错误是把关系库的成本模型带进了一个没有这个成本的模型。

**Schema 只声明目标类型：**`"order": { "x-ref": "Order" }`——一个字都不多说。

**值由业务怎么方便怎么写：**

- 唯一字段片段：`{ "order_no": "SO-1" }` 或 `{ "trace_id": "TR-9" }`
- 组合唯一：`{ "sku": "A1", "loc": "L1" }`
- 经由其他实体：`{ "$via": { "Ticket": { "ticket_no": "TK-7" } }, "follow": "order" }`——`follow` 必须是中间实体上声明的 `x-ref` 字段；v0 单跳，多跳后话。

**解析不变量（每次提交检查）：**

1. 收集目标实体全部已声明的 `x-unique`；
2. 引用值**完整覆盖**某条 unique → 尝试解析；多条被覆盖且全部指向同一文档 → 成功；不一致 → `ambiguous_reference`；零覆盖 → `cannot_resolve`；
3. 片段里的**非唯一字段在解析后逐个校验**（不符 → `reference_condition_false`）——片段就是 probe，所以“必须是服务订单”写作 `{ "order_no": "SO-1", "type": "service" }`，不需要专门语法；
4. “逻辑上只能指向一个”因此成为可检查的提交时不变量，而不是数据巧合。

**解析域：**已提交状态 + 当前请求的待写块——同一请求先建订单、后建工单按 `order_no` 引用，必须能解析，否则引用模型在“一个请求建全套”的场景下自相矛盾。

**规范化：**写入时解析为内部句柄存储，业务给的形态是输入不是存储状态；输入形态记入 trace 作为访问路径权重。两种业务用不同路径不是污染——是该实体真实拥有两条在用的解析路径，学习器两条都养着，反正结构本来就在。

**推论：没有任何已声明 unique 的实体不可被引用**（必然 `cannot_resolve`）。想被引用，就声明一个 unique——这正好是作者“必须设定为 unique”的本意，只是落点从“引用方声明”移到了“目标方声明”。

**子类型解析：**`x-ref` 指向子类型时按继承根的唯一结构解析，命中后校验 `_type` 在目标子树内，否则按 `not_found` 处理。

**保留（与 v1 相同）：**`x-extends` 单继承、唯一性作用域 = 继承根、`_type` 引擎管理、probe 父类型命中全部子类型、数组引用逐项解析、`on_delete` 默认 restrict、`$ref`/`$defs` 是结构复用而 `x-ref` 是数据引用、引用解析本质是“最多命中一个”的 probe。

**未决：**读 `x-ref` 字段返回句柄还是自动浅填充——v0 倾向句柄，自动填充按需后置；多跳 `$via`（`follow` 数组）后话。

## Not in scope

Re-running the language review. This ticket resolves the findings it produced.
