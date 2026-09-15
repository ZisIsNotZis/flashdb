# flashdb — contracts

Budget: 280 lines / 21,000 chars

The four interfaces that cannot be changed cheaply after implementation. Everything else in this repository is deferrable; these are not.

The contracts are written as the design of record. Each carries an explicit **Status** line marking what is settled and what is still proposed.

## Contract 1 — Logical model

Status: settled except where marked proposed.

### Schema

Tables are created and updated through a JSON schema description. No DDL text, no parsing.

### The request tree

A request is a **structured tree**, never a string. There is no query parser, which is what makes canonicalization tractable: normalizing a typed tree is a small well-defined algorithm, whereas normalizing SQL text is unsolvable in practice.

```
request:  [ stage, stage, ... ]            // array order = sequential (the former "&&")
stage:    { blocks: [ block, ... ],        // blocks are parallel and mutually isolated ("&")
            ifEmpty: [ block, ... ] }      // optional single fallback ("||")
```

- **Sequence is array order.** No operator symbol.
- **Parallelism is implicit** within a stage, which makes it a *physical* decision like every other physical decision.
- **Fallback is a named key on a stage.** `(A && B) || (A && C)` is therefore **not expressible** — a fallback cannot contain stages — so the distributivity equivalence class that would violate canonicity never exists.

Example — "find the customer by id, else by email, then fetch their orders":

```json
[ { find: { customer: { id: "..." } } },
  { find: { customer: { email: "..." } },
    ifEmpty: [ ... ] },
  { find: { orders: { customer: "$1.customer_id", status: "open" } } } ]
```

### Blocks

Every block carries **two orthogonal annotations**, frozen into the format now because the normalizer's soundness and the scheduler's predication strategy both depend on them (`review-02` F15 — the earlier three-value table was ill-typed: it presented a third exclusive value while saying "additionally", leaving read-set validation and speculation eligibility undecidable).

| Field | Values | Meaning |
|---|---|---|
| `effect` | `read` \| `write` | `read` is safe to predicate and to speculate; `write` is never predicated and never speculated, regardless of the other field |
| `applicability` | `conditional` \| `harmless-if-not-applicable` | `harmless-if-not-applicable` means the block is a no-op when its predicate is false, so the scheduler may run it unconditionally without predicting anything |

**Snapshot rule.** A predicate guarding a `write` is evaluated against the committing transaction's own snapshot, never against a later one. A `write` is never executed speculatively under any combination of these fields (I9).

### Closed set of write shapes

Only these are permitted. Anything else is rejected at the front-end, which keeps the engine's job finite, recovery simple, and makes "is this expressible in one request" a mechanical check the API can report to the user.

`insert` · `conditional-update-by-key` · `upsert` · `delete-by-key` · `multi-row-batch` (uniform shape) · `counter-add` · `read` · `aggregate` · `join`

### Canonicity

Single source of truth: **the same intent must not be expressible two ways.** This applies to schema design and to requests alike, including which fields are requested.

- Achieved first by grammar: no nested queries, joins only implied by shared key fields, no optional or redundant parts, no defaults that change meaning, no `*` field lists, one predicate normal form.
- Where grammar cannot reach, a **server-side authoritative normalizer** covers it (I12). Clients may normalize as a convenience; the server's canonical form is the pattern key.
- The pattern key is `(normalizer_version, canonical_form)`. Without the version, upgrading the normalizer silently invalidates every pattern learned so far.
- **The acceptance grammar is versioned separately from the learning key** (`review-02` F16). A `normalizer_version` bump must invalidate only learned patterns; it must never change which request forms the server accepts. Accepted forms are guaranteed backward-compatible for at least one major version, and a rejected request returns a machine-applicable `explain_rejection` carrying the canonical form the server wanted.
- If program-level fallback is ever added (open question), the normalizer becomes **effect-aware**: factoring a block across a fallback is sound only if that block is `read`. Factoring a write is a double-apply, i.e. data corruption, not canonicalization.

### Leanness is machine-checked

Users are required to state their requirement precisely, with a clear boundary — no overspill fields. The engine does the dirty work (field duplication, partitioning, physical arrangement) and the user is always given the cleanest possible query surface.

Because the surface is canonical and lean-checkable, the engine can *compute* what is wrong with a request rather than guess, which is what makes the advisor (`learning.md`) possible at all.

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

Every tile read records `(tile, version)`. At commit the versions are re-checked; any mismatch means retry.

This is not merely cheap, it is **free in the right currency**: tiles are immutable, so a tile version is a well-defined validation granule. A range or predicate read validates by validating whole tiles, so **phantom protection comes for free** in O(number of tiles read) integer comparisons — compare with predicate locking.

Consequences:

- **Deadlock is impossible by construction.** Validation happens only at commit and never holds anything; there is no hold-and-wait. A lock manager, deadlock detector and lock timeouts are not needed.
- **The only failure mode is retry.**
- Validation at commit plus read-set coverage gives serializability for the single-round-trip model, stronger than snapshot isolation with first-committer-wins alone.

### Partial application and prefix commit

If a request dies mid-program, the durable commit record holds a **prefix** of its blocks. The database reports exactly which blocks applied.

**Open — this conflicts with the motivating workload (`review-02` F4), and needs an author decision.** The inventory correctness oracle requires `decrement stock iff insert lines` to hold atomically, which per-block atomicity does not provide: a crash or mid-request failure can leave the decrement applied and the lines missing. Two options: **(a)** make request-level atomicity the default and per-block the opt-in — nearly free, because one commit record already covers the whole request, so only the *publication* of a partial prefix costs extra; **(b)** keep per-block atomicity and define a documented client-side compensation contract, with a reference client included in the correctness oracle. Option (a) is recommended because the motivating example needs it and it inverts the default at no cost.

**The database never resumes a request.** It commits a prefix and reports it; the *client* continues via per-block idempotency keys. This is the rule that keeps the engine out of the durable-workflow business and out of needing a progress log — and a progress log would quietly reintroduce the payload WAL that Contract 3 removes.

### Idempotency

Because the client can crash after commit and before acknowledgement, every request carries a request id and every block a block id, and the engine deduplicates on `(request_id, block_id)` within a retention window.

### Conflict policy

Never block. On conflict the request retries into the next batch, or the writes are coalesced (see mergeable encodings, `learning.md`). Under contention the engine prefers aggregate throughput over single-request latency.

### What this model gives up

**Cross-request invariants cannot be enforced.** "The sum of ledger entries is zero" spanning two requests is not expressible as an invariant. Both must be placed in one request, or the constraint becomes a monitored background assertion rather than an enforced one. This is the one place the model will bite a real user and it must be stated in the API documentation, not discovered.

### Service classes

Each request carries an explicit class — never an implicit engine guess, or the engine becomes something that silently drops data.

| Field | Values |
|---|---|
| `durability` | `durable` (fsync before ack) · `batched` (ack after the group's fsync; **default**) · `lossy` (ack immediately, bounded loss window) |
| `max_staleness` | reader may read a snapshot up to N seconds old. A derived layout is served only when its coverage metadata proves it contains every row matching the predicate **and** its version matches the base snapshot. `max_staleness` governs how stale the base snapshot may be, never how stale a derived copy may be relative to that base (`review-02` F5). |
| `shedable` | may be dropped under overload, with a priority class |
| `recomputable` | never fsync; rebuild or drop on loss. Applies to engine-owned tables (trace, statistics) and to read-only derived tiles. **A `recomputable` tile must never be reachable from a durable root unless its contents are proven rebuildable before first read** — otherwise a crash can leave a durable root pointing at a tile that was never made durable, which is wrongness, not slowness (`review-02` F5). |

`recomputable` is a substantial win, not a footnote: the trace store, statistics and every derived layout stop paying fsync entirely, freeing a large share of the IO budget for foreground work.

## Contract 3 — Durability and failure

Status: design settled in outline; several assumptions must be validated by fault injection before they can be relied on.

### No payload WAL

The conventional WAL exists mainly to turn scattered in-place page writes into a sequential append, and to give a cheap durability point. Here, **tiles are already immutable and append-only, so the data path is already sequential** and a payload WAL buys nothing.

Atomicity comes from immutability instead of from a log:

```
commit = write new immutable tile(s) → fsync → atomically flip the root
```

Until the flip, the previous root remains valid and complete. There is no replay, and no intermediate state is ever observable.

The WAL degenerates into a ~64-byte **commit record**, which can be folded into the root itself.

### Physical layout of the root

- One preallocated file, overwritten in place, so no directory entries are ever created and a single `fdatasync` flushes data and root together.
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
- **Relocatable.** The tile is self-contained, so a reorg is a copy.
- **Versioned.** A per-tile version field is reserved now, for read-set validation (Contract 2) and for version-checked layout authority (`learning.md`).

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
