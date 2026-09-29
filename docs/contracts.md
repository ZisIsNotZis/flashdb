# flashdb — contracts

Budget: 335 lines / 34,000 chars  <!-- budget debt: see issue 01-design, "Budget debt" -->

The four interfaces that cannot be changed cheaply after implementation. Everything else in this repository is deferrable; these are not.

The contracts are written as the design of record. Each carries an explicit **Status** line marking what is settled and what is still proposed.

## Contract 1 — Logical model

Status: settled except where marked proposed.

### Schema

Tables are created and updated through a JSON schema description. No DDL text, no parsing.

### The request tree

A request is a **named set of blocks**, not an ordered list. Names are identities and the syntax has no positional order, so "parallel blocks run in any order" is structural rather than a convention. Ordering exists only where it is declared:

```
request:  { <name>: <block>, ... }             // an object: no positional order
block:    { needs: [<name>, ...],              // declared dependencies: the only ordering
            <shape>: { ... },                  // exactly one shape per block
            else: <block> | [<block>, ...] }   // runs only on an expected condition miss
```

- **Declared order is honoured.** `needs` is sequence (`&&`), `else` is conditional fallback (`if`/`else`), not exception handling; execution order is derived from the graph rather than from position. A read with no match or a write whose explicit probe/`when` is false takes `else`. A unique constraint violation, malformed input, validation error or internal failure is an exception and does **not** take `else`. To branch on an anticipated business condition, express it as an explicit supported probe/`when` before the write; a subsequent publish-time constraint violation remains an exception. Timing contention from concurrent optimistic validation may be retried with a fresh snapshot, subject to a bounded retry policy; it is distinct from business constraint failure.
- **Undeclared is parallel.** Blocks with no `needs` path between them may run concurrently in any order. Reading a key another block writes without declaring the edge yields whichever state results — the author's "consequences are yours".
- **Same-key writes with no declared edge are rejected at the front-end**, not guessed: the engine refuses what it cannot order. Decidable for key-addressed shapes, and it is the mechanical check the API reports.
- **A block is a transaction**: every write in it applies atomically or none does, and it may span tables. A read block returns rows or empty; a write block returns `ok` or `failed`.
- **QBE, actually.** Shared variable names across blocks (`$find.sku`) are QBE's join trick — the same mechanism Query-by-Example used to join example forms — and a write is an *example of the row as it should exist*, not a command. Writes collapse to two shapes: `put` (match + row; upsert, with `when` narrowing it to present-only or absent-only) and `del`. Six shapes: `get` · `scan` · `agg` · `join` · `put` · `del`.

Example — "place an order: find the stock row (by sku, else by an alternate sku), decrement it and write the movement and the order line atomically, else write a backorder":

```json
{ "id": "ord-8812",
  "class": { "durability": "batched", "retry_horizon_s": 300 },
  "blocks": {
    "find": { "get":   { "table": "stock", "where": { "sku": "$sku", "loc": "$loc" } },
              "else":  { "get":   { "table": "product", "where": { "alt_sku": "$sku" } } } },
    "book":  { "needs": ["find"],
               "put": [ { "table": "stock", "where": { "sku": "$find.sku", "loc": "$loc" },
                          "row":  { "on_hand": "$find.on_hand - $qty" },
                          "when": { "on_hand": { "ge": "$qty" } } },
                        { "table": "stock_movement", "row": { "sku": "$find.sku", "loc": "$loc",
                                                               "delta": "-$qty" } },
                        { "table": "order_line", "row": { "order_id": "$order_id",
                                                           "sku": "$find.sku", "qty": "$qty" } } ],
               "else": [ { "table": "backorder", "row": { "sku": "$find.sku", "qty": "$qty" } } ] } } }
```

`$find.on_hand` addresses the *applied* result of `find`, including its `else`. The `when` guard belongs to the whole `book` block: if it is false, all three writes roll back and `else` runs.

### Outcome and failure detail

Every block reports `{ name, status }` with `status` ∈ `ok | empty | failed | skipped`, and a failed block carries a **structured, exhaustive reason** — no prose:

```json
{ "block": "book", "status": "failed", "reason": "when_false",
  "op": 0, "table": "stock",
  "where": { "sku": "A1", "loc": "L1" },
  "when":  { "on_hand": { "ge": "$qty" } },
  "bound": { "on_hand": 0, "$qty": 3, "$find.sku": "A1" } }
```

`reason` distinguishes `when_false` (expected, eligible for `else`), `not_found` on an explicit expected probe (eligible for `else`), `unique_violation` and other business constraint failures (terminal exception), `contention` (timing-dependent optimistic validation mismatch, eligible for bounded retry), and `validation | dependency_failed | shed | key_overlap | internal` (not implicit `else` triggers). Include `op`, `table`, `where`/`key`, the condition as written, and `bound` values. A client can distinguish retry from business failure without parsing prose. The exhaustive wire taxonomy remains to be frozen with L-02.

### Open — remaining after this redesign

**Closed here:** L-01, L-05, L-06, L-11, L-12, L-13, L-14, L-18, L-20.
**Still open:** L-04 (`E` as a normalizer, not a list), L-07 (row cap, aggregate-to-write, generated-key round-trip), L-08 (schema canonicity), L-16 (wire-form canonicalization), and the expression grammar (`$name.field`, arithmetic, `where` operators) needs one page of exact rules. **L-02 is resolved (2026-09-29):** historical reads do not exist — reads always see the latest committed state, so compaction may drop every superseded version; dedup-replay responses are not a guarantee (see Idempotency).


### Blocks

Every block carries **two orthogonal annotations**, frozen into the format now because the normalizer's soundness and the scheduler's predication strategy both depend on them (`review-02` F15 — the earlier three-value table was ill-typed: it presented a third exclusive value while saying "additionally", leaving read-set validation and speculation eligibility undecidable).

| Field | Values | Meaning |
|---|---|---|
| `effect` | `read` \| `write` | `read` is safe to predicate and to speculate; `write` is never predicated and never speculated, regardless of the other field |
| `applicability` | `conditional` \| `harmless-if-not-applicable` | `harmless-if-not-applicable` means the block is a no-op when its predicate is false, so the scheduler may run it unconditionally without predicting anything |

**Snapshot rule.** A predicate guarding a `write` is evaluated against the committing transaction's own snapshot, never against a later one. A `write` is never executed speculatively under any combination of these fields (I9).

**The annotation is server-derived, never trusted from the client (`review-05` SS-20).** `harmless-if-not-applicable` is what licenses unconditional execution, so a client-supplied value would let a buggy or hostile caller mark a non-idempotent write harmless and have it applied twice — "a double-apply, i.e. data corruption", by this document's own words. The closed shape set constrains *shape*, not safety. The server must derive the annotation from the canonical block shape and reject a client declaration that disagrees.

### Closed set of block shapes

Only these are permitted. Anything else is rejected at the front-end, which keeps the engine's job finite, recovery simple, and makes "is this expressible in one request" a mechanical check the API can report to the user. **Corrected (`review-04` L-07):** the previous list was labelled "write shapes" while three of its nine members are reads, and it omitted shapes the inventory workload needs.

| Effect | Shapes |
|---|---|
| `read` | `read-by-key` · `read-by-predicate` · `aggregate` · `join` |
| `write` | `insert` · `update-by-key` · `conditional-update-by-key` · `upsert` · `delete-by-key` · `counter-add` · `multi-row-batch` |
| **missing and required** | `update-by-predicate` and `delete-by-predicate` (without them, "expire reservations older than 30 minutes" forces the client to enumerate keys, which it cannot at 10 TB); aggregate-to-write ("day close: compute days-of-cover per SKU and write it back"); expression-valued updates (`price = price * 1.1`, `stock = min(stock, cap)`); and a generated-key round-trip so a later block can reference a row this request inserted |

`multi-row-batch` remains ambiguous: it does not say whether its rows are inserts, updates, upserts or deletes, and it overlaps the single-row shapes.

### Canonicity

Single source of truth: **the same intent must not be expressible two ways.** This applies to schema design and to requests alike, including which fields are requested.

- **Corrected (`review-04` L-04):** this is achievable only as an idempotent normal form over a **declared, decidable equivalence relation `E`**, never as "one intent one way" — intent is in the user's head and is not a property of a request. `E` must be published, with the normalizer a section of the quotient: `normalize ∘ normalize = normalize`, and `x E y ⇒ normalize(x) = normalize(y)`. It must also terminate and be confluent, or two `E`-equal requests normalize differently depending on rewrite order — the exact failure the mechanism exists to prevent (`review-04` L-10).
- **Known equivalence classes in the current grammar that `E` must collapse:** block order within a stage (blocks are declared mutually isolated); `ifEmpty: []` versus an absent `ifEmpty`; two sequential stages of independent reads versus one stage; the join spelling versus the shared-key-field spelling; and singleton `in` versus `==`.
- **Intended scope:** no nested queries, joins only implied by shared key fields, no optional or redundant parts, no defaults that change meaning, no `*` field lists, one predicate normal form. Each of these is undecidable until `E` and the predicate language (L-03) exist.
- Where grammar cannot reach, a **server-side authoritative normalizer** covers it (I12). Clients may normalize as a convenience; the server's canonical form is the pattern key.
- The pattern key is **`{grammar_version, normalizer_version, schema_version, service_class, canonical_form}`**. **Extended (`review-04` L-11):** the previous `(normalizer_version, canonical_form)` was not a stable identity. A schema change (column rename, type change, clustering key) leaves the same canonical form denoting a different pattern; `durability` (`durable`/`batched`/`lossy`) and `max_staleness` materially change latency and semantics, so aggregating them into one latency histogram makes the training signal multimodal; and the grammar and effect-annotation semantics were unversioned. These are cheap fields now and expensive to retrofit — this project's own argument for freezing interfaces.
- **Normalizer-version lifecycle is unspecified.** A bump converts silent invalidation into loud invalidation, but the accumulated trace is still unusable until relearned: there is no old→new canonical mapping, no bump policy, and during a rolling deploy two servers can emit two versions.
- **The acceptance grammar is versioned separately from the learning key** (`review-02` F16). A `normalizer_version` bump must invalidate only learned patterns; it must never change which request forms the server accepts. Accepted forms are guaranteed backward-compatible for at least one major version, and a rejected request returns a machine-applicable `explain_rejection` carrying the canonical form the server wanted.
- If program-level fallback is ever added (open question), the normalizer becomes **dependency-aware**. **Corrected (`review-04` L-09):** "is a read" is not a sufficient soundness condition — hoisting a read out of a conditional changes which snapshot it observes, so a read that sees the effect of a conditional write must not be hoisted either. The condition is *data dependency*: a block may be factored out only if it observes no effect, transitively, of any conditionally executed block. **Block-boundary rewrites are forbidden outright** (merging two `insert` blocks into one `multi-row-batch`, or folding two `counter-add` blocks into `+2`): the boundary is simultaneously the per-block atomicity unit (I6) and the `(request_id, block_id)` idempotency unit, so merging changes both the outcome document and the client's continuation logic. Canonical block boundaries and per-block idempotency are in unresolved tension.

### Leanness is machine-checked

Users are required to state their requirement precisely, with a clear boundary — no overspill fields. The engine does the dirty work (field duplication, partitioning, physical arrangement) and the user is always given the cleanest possible query surface.

**Withdrawn (`review-04` L-17):** the claim that canonicity is what makes the advisor computable is false. The advisor's rules are statistical and value-dependent — entropy, mutual information, association rules — and those come from the trace, not from canonicity. Canonicity supplies a *stable aggregation key* and nothing more. See `learning.md` for the per-rule input list, including the rules the engine cannot compute at all.

Two rules that a naive design gets wrong:

- **Canonicalize before learning.** If the same intent written five ways is five patterns, the learning signal shatters.
- **The rewriter is a second writer.** Derived layouts and internal rewrites must produce canonical forms too; the canonical form must be a fixed point under internal rewriting, or internal churn registers as new user patterns.

### Outcome document

A request returns an outcome **per block**, not a single ok/error, because atomicity is per block (Invariant I6). See Contract 2.

## Contract 2 — Transaction model

Status: settled.

### Guarantees

- **Per-block atomicity.** A block either fully applies or does not.
- **Per-request commit record.** One commit record covers the request's blocks in **one fsync**, so per-block atomicity costs one flush, not N.
- **No interactive transactions.** No begin/end, no multi-round-trip sessions.
- **No locks.** See read-set validation below.
- **Immediate effect.** A committed block takes effect immediately and is visible to subsequent requests.

### Read-set validation

A timing-dependent read-set version mismatch at commit is a candidate for bounded retry on a fresh snapshot. A uniqueness/constraint violation is a business error and must not be retried merely because it shares a legacy `conflict` label. The v0 key/CSN validation design is in `engine.md`; the tile-version scheme below is historical design context.

This is cheap in the right currency: tiles are immutable, so a tile version is a plausible validation granule. **But the previous claim that "phantom protection comes for free" is false (`review-05` SS-05), for two reasons.**

1. **Tile-set changes are invisible to per-tile versions.** A reader enumerates the tiles for key range `[1,100]` from root `R1` and reads `T1`. A writer inserts key 50 and splits `T1` into `T1a[1,49]` and `T1b[50,100]`, then flips the root. `T1` was not *mutated*, it was *replaced*, so its version is unchanged and validation **passes** — and the phantom is missed. The same hole lets a reader return rows from an obsolete snapshot after a merge. Validation must therefore be against a **root generation / snapshot epoch**, re-enumerating range membership at commit, or against the routing structure.
2. **False conflicts scale with rows per tile.** One granule per tile means any write to any row invalidates every concurrent reader of any *other* row in the same tile. At an ~8 MB tile that is ~10⁵ rows per granule. Under the Zipfian skew this design mandates, hot tiles are rewritten continuously, so "the only failure mode is retry" becomes a retry storm; a bounded retry/backoff policy and an admission rule are required.

Open questions this leaves: whether pure read-only requests validate at all (they have no commit), and what the guaranteed isolation level is per service class.

Consequences:

- **Deadlock is impossible by construction.** Validation happens only at commit and never holds anything; there is no hold-and-wait. A lock manager, deadlock detector and lock timeouts are not needed.
- **Only timing-dependent contention is retryable by default.** Invalid input, uniqueness and other business constraints fail directly; expected predicate misses may take `else`.
- Validation at commit plus read-set coverage gives serializability for the single-round-trip model, stronger than snapshot isolation with first-committer-wins alone — **only when `max_staleness = 0`** (`review-04` L-19). A request that reads a snapshot N seconds old and then validates against *current* tile versions can commit on stale reads, which is not serializable. The isolation guarantee must therefore be stated per service-class value, and `max_staleness` belongs in the pattern key.

### Partial application and prefix commit

If a request dies mid-program, the durable commit record holds a **prefix** of its blocks. The database reports exactly which blocks applied.

**Open — this conflicts with the motivating workload (`review-02` F4), and needs an author decision.** The inventory correctness oracle requires `decrement stock iff insert lines` to hold atomically, which per-block atomicity does not provide: a crash or mid-request failure can leave the decrement applied and the lines missing. Two options: **(a)** make request-level atomicity the default and per-block the opt-in — nearly free, because one commit record already covers the whole request, so only the *publication* of a partial prefix costs extra; **(b)** keep per-block atomicity and define a documented client-side compensation contract, with a reference client included in the correctness oracle. Option (a) is recommended because the motivating example needs it and it inverts the default at no cost.

**The database never resumes a request.** It commits a prefix and reports it; the *client* continues via per-block idempotency keys. This is the rule that keeps the engine out of the durable-workflow business and out of needing a progress log — and a progress log would quietly reintroduce the payload WAL that Contract 3 removes.

### Idempotency

**Resolved (author, 2026-09-29): idempotency is business uniqueness enforced by the engine, not transport-level request ids.** The engine acks a block only after its WAL record is durable; the ack is one-way and needs no reply. A client that retries an ambiguous request must write at least one engine-enforced unique field whose value is minted once when the intent is created and reused verbatim on retry (for example an order number); the retry then hits `unique_violation` and the whole block rolls back atomically, so no side effect duplicates. Blocks without such a field may re-execute on retry — that is the client's contract, not an engine defect. Blocks that mix keyed and unkeyed writes get whole-block rollback from the keyed write's uniqueness. The engine keeps no durable request-id dedup; any in-memory dedup is an internal detail that dies with the current WAL segment and is never a guarantee. `retry_horizon_s` is not used for retention.

### Conflict policy

Do not conflate business conflicts with timing contention. A unique/index constraint collision is a terminal business error (no automatic retry or `else`). Only a read-set validation mismatch attributable to concurrent publication may be retried on a new snapshot, with bounded attempts/backoff; it must not silently become an `else` branch. Coalescing is allowed only where a declared mergeable operation preserves semantics (see `learning.md`).

### What this model gives up

**Cross-request invariants cannot be enforced.** "The sum of ledger entries is zero" spanning two requests is not expressible as an invariant. **Corrected (`review-04` L-18):** the previous remedy — "both must be placed in one request" — is wrong, because a request is not all-or-nothing (I6) and a prefix can commit one block without the other. The correct rule is **both must be placed in one block**, and no shape can currently express a multi-entity atomic action in a single block. So either such a shape is added, or cross-block business atomicity is declared unenforceable and the client compensates. This is the one place the model will bite a real user and it must be stated in the API documentation, not discovered.

### Service classes

Each request carries an explicit class — never an implicit engine guess, or the engine becomes something that silently drops data.

| Field | Values |
|---|---|
| `durability` | `durable` (fsync before ack) · `batched` (ack after the group's fsync; **default**) · `lossy` (ack immediately, bounded loss window) |
| `max_staleness` | reader may read a snapshot up to N seconds old. A derived layout is served only when its coverage metadata proves it contains every row matching the predicate **and** its version matches the base snapshot. `max_staleness` governs how stale the base snapshot may be, never how stale a derived copy may be relative to that base (`review-02` F5). |
| `shedable` | may be dropped under overload, with a priority class |
| `deadline` | **added (`review-05` SS-19):** `objective.md` prices "requests that exceed a completion deadline" and `dev-loop.md` requires a workload with a hard deadline, but no request field carried one — a priced term with no interface cannot be computed, and deadline-aware batching has no input. Either this field exists or the deadline term and the batching rule are removed. |
| `recomputable` | never fsync; rebuild or drop on loss. Applies to engine-owned tables (trace, statistics) and to read-only derived tiles. **A `recomputable` tile must never be reachable from a durable root unless its contents are proven rebuildable before first read** — otherwise a crash can leave a durable root pointing at a tile that was never made durable, which is wrongness, not slowness (`review-02` F5). |

`recomputable` is a substantial win, not a footnote: the trace store, statistics and every derived layout stop paying fsync entirely, freeing a large share of the IO budget for foreground work.

## Contract 3 — Durability and failure

Status: design settled in outline; several assumptions must be validated by fault injection before they can be relied on.

### No payload WAL

The conventional WAL exists to turn scattered in-place page writes into a sequential append, to give a cheap durability point, and **to make a small update durable without rewriting a large object**. **Corrected (`review-05` SS-04, SS-25):** the arguments previously given here were wrong on both counts.

- "The data path is already sequential" is false. A preallocated file overwritten in place produces *reused holes*, which are random writes in steady state. The sound argument for no payload WAL is **immutability plus the root flip**, not sequentiality.
- "A payload WAL buys nothing" is false for the motivating workload. **There is no update path specified here, and an immutable ~8 MB tile means a one-row stock decrement rewrites the whole tile.** At 5,000 stock operations per second concentrated on one hot SKU, that is tens of GB/s of write traffic before any derived copy — which would consume the entire shared budget and saturate the device. A payload WAL exists precisely to avoid this.

A mutation granularity must therefore be chosen — per-tile delta/overlay tiles with their own read-merge and durability rules, or a smaller mutable granule — together with a **write-amplification budget in bytes per second** that the objective charges. Until then this contract's durability design has no viable write path for the workload it was written for. Tracked in `04-storage-soundness`.

Atomicity comes from immutability instead of from a log:

```
commit = write new immutable tile(s) → fsync → atomically flip the root
```

Until the flip, the previous root remains valid and complete. There is no replay, and no intermediate state is ever observable.

The WAL degenerates into a ~64-byte **commit record**, which can be folded into the root itself.

### Physical layout of the root

- **Commit ordering — corrected (`review-05` SS-01).** The previous text claimed "a single `fdatasync` flushes data and root together", while the commit recipe below says `write tile(s) → fsync → flip root`. Those are different protocols and the single-barrier version is **unsound**: `fdatasync` is a *completion* barrier, not an *ordering* barrier, so on a crash the root page can be durable while a tile it references is not. Recovery would then dereference a half-written tile.

Three viable protocols; one must be chosen and stated once:

| Protocol | Cost | Note |
|---|---|---|
| Two barriers: tiles → `fdatasync` → root → `fdatasync` | 2 flushes per commit group | simplest to reason about |
| One barrier + **self-validating commit**: per-tile content checksums recorded in the commit record, and recovery falls back to the previous root if any referenced tile fails validation | 1 flush, plus a checksum on every tile read at recovery | needs the checksum schema (missing entirely — see `04-storage-soundness`) |
| Root in a separate file with its own barrier | 2 flushes, independent ordering | separates reorg traffic from the commit barrier |

Until this is chosen, **the crash-atomicity claim in Contract 3 is unsound** and Contract 2's "one fsync" cannot be counted. A single shared file and a single fd also mean one `fdatasync` flushes *all* dirty pages, including background reorg staging, so foreground commit latency is coupled to reorg — the coupling the quiet-window policy is supposed to avoid (`SS-11`).
- Root stored as **A/B pages with a checksum and a sequence number**; a torn write is detected by checksum and the highest valid sequence wins. (The same trick as LMDB's meta pages.)

### Recovery

1. Read both root pages, discard any that fail checksum, take the highest valid sequence.
2. Discard every tile not reachable from that root (orphan GC).

There is no log replay. **Orphan GC is the real replacement for replay and is where the bugs will be.**

### Orphan GC

- Epoch- or root-version-stamp the free list.
- Never free a tile that any live root might reference.
- GC must be crash-safe and resumable: an interrupted GC leaves garbage, never loss.

### What this design costs

1. **No commit may be published before its data is durable.** With a payload WAL one can fsync a cheap sequential log and write data pages lazily; here the commit fsync must cover the tile writes. Group commit amortizes this under load, and at very low load the per-commit cost is higher. This is the price of the design and it is paid in exactly the regime where latency matters least.
2. **Retention.** Rollback is O(1) only while the previous version is still on disk, so a canary window implies a retention window, which costs space. Canary window × space is a real budget line.
3. **Assumptions must be verified.** `fsync` semantics are weaker than expected on some filesystems and some devices lie about volatile caches. The durability assumptions must be written down explicitly and backed by a fault-injection harness (simulated device losing unflushed writes, crash injection at every write point) before any of this is trusted.

### Explicitly deferred

Crash-atomicity hardening is acceptable as a later milestone, but the *interface* — one commit point, one root, immutable tiles — is frozen now. Robustness, memory safety and never crashing are not deferrable.

## Contract 4 — Layout contract

Status: settled.

### The unit of layout

**One tile.** There is no global layout state (Invariant I3). Different rows of the same table legitimately have different layouts, and the mix drifts independently across the database. A mixed state is a normal, fully supported state — which is what makes gray release free.

### Tile properties

- **Immutable.** A tile is never mutated in place.
- **Self-describing.** The header names the layout, the column group, the clustering key and the encoding, so any reader can consume any tile without external metadata.
- **Relocatable — with a caveat (`review-05` SS-06, SS-09).** Tile *payloads* are position-independent, but "the tile is self-contained" is not yet true: a parent tile storing child addresses cannot be relocated without rewriting every referrer, and a durable tile-id→file-offset map is itself global state. Nor is a reorg a `memcpy` — `glossary.md` defines it as *rewriting tiles into a different arrangement* (read → regroup → re-encode → rebuild directory → write); only pure *relocation* is a copy. What relocatability costs is unpriced: stable addresses, raw child pointers, page-cache readahead state (relocating a hot tile re-faults its pages), and cross-tile references wider than 32 bits at 10 TB.
- **Versioned.** A per-tile version field is reserved now, for read-set validation (Contract 2) and for version-checked layout authority (`learning.md`). **Ambiguity to resolve (`review-05` SS-16):** for an *immutable* tile, "version" can only mean content identity (a hash), which lives outside the tile — otherwise the version is mutated in place, contradicting immutability and re-introducing the in-place ordering problem of SS-01. If it lives outside, validation is a question about the root mapping, not the tile, and the tile is no longer fully self-describing. One of the two must give.

### Directory and offsets

Indirection is **per tile, not per row**: one directory per tile, storing tile-relative offsets.

- Tiles are kept under 4 GiB by construction, so tile-relative **32-bit offsets** suffice.
- Values above a learned, per-column threshold overflow to a compact blob area; smaller values are stored inline with a length prefix. The threshold is learned from the observed value-length distribution rather than fixed.
- This single decision buys three things at once: reorg becomes `memcpy`, fan-out can be issued as independent reads because the child addresses are in the parent tile, and tiles stay relocatable.

### Space is a latency budget

Less data means fewer cache misses, so space is minimized deliberately — but conditioned on mutability, because over-compressing a mutable region means rewriting a whole tile per update. Space costs have two components and both are charged: cache/bandwidth, and write amplification.

### Required physical properties

A compiled program **declares the physical properties it requires** — sorted by X, these columns adjacent, clustered by K, tile size N. The storage engine satisfies each requirement in one of two ways:

| Option | Cost | Benefit |
|---|---|---|
| change the layout permanently | reorg now, plus invalidation of compiled plans | every future use benefits |
| pay per execution | sort, buffer, re-read, every time | no reorg, no invalidation |

That single fork **is** the layout-versus-runtime trade, expressed as one decision point and priced in angriness. It also keeps attribution clean: the program does not alter storage, it demands properties, and the solver decides which side pays.

### Reorg

```
write new tile → fsync → atomically flip root → GC old tile
```

Properties: crash-safe (old or new root, both valid), idempotent, resumable, and transaction-local when tile boundaries align with transaction boundaries (a design goal, not an accident).

**Non-obvious cost:** a compiled program encodes a *physical* plan, so a reorg invalidates compiled programs. Mitigations: separate the logical program from a late-bound physical access plan so that a reorg invalidates only the cheap part, and count "invalidate N compiled plans" in the reorg cost, or the optimizer will trade a layout win for a compile storm.

### Derived layouts

A derived layout is a second arrangement of base data, built by the engine for an observed pattern. One inventory example, each a testable hypothesis:

| Derived layout | Pattern it serves | Effect |
|---|---|---|
| order lines co-clustered by order id | "show order N" | fan-out of ~20 dependent reads → 1 |
| stock clustered by (warehouse, item) | stock lookup and decrement | point read within one tile |
| product attribute group | catalog listing | column-group pruning |
| history as cold compressed tiles | replenishment analytics | cheaper scans, no hot-path cost |

- **v1: all derived layouts are read-only** (open question in `design.md`). A read-only derived layout is a pure accelerator with a fallback to the base copy, so the worst case of any bug, lag or crash is slowness, never wrongness (Invariant I2).
- Read-only derived layouts are what make continuous drift safe: layout never has to be *correct*, only *useful*.
- Promotion of one derived layout to authoritative is a per-layout decision taken by the objective function — never a global switch. The eventual mechanism is **version-checked authority**: serve from the derived copy when its version matches the base, fall back otherwise; a lagging copy then costs latency, not correctness. Aggregates (e.g. a running stock total) are the exception, since they cannot be version-checked cheaply; those carry a dependency version and rely on the caller's `max_staleness`.

### Read path

The executor must be **layout-polymorphic**: it dispatches on the tile header, not on a table-level descriptor. This is the real engineering bill for drift, and it is affordable precisely because tiles are self-describing.

Two warnings:

- **Do not over-specialize.** Specialization is a spectrum and scans pay for the worst layout in the table. The objective must carry a scan-cost term, or specialization for point access quietly destroys scan throughput.
- **Churn.** Layouts must have hysteresis and a payback-before-reorg rule, or tiles flip forever and the reorg cost is never amortized.

## Reserved interfaces (frozen now)

Freezing these costs nothing today and is a rewrite later:

1. Self-describing tile header.
2. Per-tile version field.
3. Per-block effect annotation, plus `harmless-if-not-applicable`.
4. One commit point per request.
5. `pattern_key = (normalizer_version, canonical_form)`.
6. Explicit per-request service class (`durability`, `max_staleness`, `shedable`, `recomputable`).
