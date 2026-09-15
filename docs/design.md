# flashdb — design

Status: contract-level design draft. Synthesized from a 13-round brainstorm with the author (a data scientist, not a database engineer). The raw decision record lives in `.scratch/01-design/decisions.md`. Nothing is implemented; `flashdb` is an empty repository.

Reading order: this file (thesis, invariants, open questions), then `objective.md`, `contracts.md`, `learning.md`, `dev-loop.md`, `glossary.md`.

## Thesis

flashdb is a relational store for single-node, IO-bound workloads whose **physical layout is learned from the workload rather than declared**.

Three claims distinguish it:

1. **Layout is a materialized view over logical data**, maintained incrementally and drifting continuously. Every intermediate state is fully servable, so layout never needs a migration project — the 1 TB written last year is never migrated as a batch; it is rewritten tile by tile, in the background, forever.
2. **One scalar objective prices every decision.** Angriness (`objective.md`) is the only currency: layout choice, reorg timing, promotion tier, speculation, compilation and advice are all evaluated as predicted angriness deltas through one calibrated cost model.
3. **The database is its own profiler and its own advisor.** It learns from the trace it records, and it tells the user what it cannot fix alone.

Motivating case: ~10 TB of data, ~128 GB RAM, heavily joined lookups. In that regime the scarce resource is not bytes but the number of **dependent** random reads per request. With 8 MB tiles, ~1.3% of the tile set is cache-resident, so the design goal is to collapse each request's fan-out into as few dependent reads as possible and to make the remaining reads independent.

## Goals

- Highest throughput on a single modern Linux node; correctness and crash-atomicity never traded away for it.
- Layout, encoding and scheduling derived from the observed workload at per-tile granularity.
- One round trip per business request, with the request itself the unit of atomicity, learning and attribution.
- A relational model with no SQL: JSON schema for create/update, a QBE-like structured query surface, single-source-of-truth query canonicity.
- Continuous background adaptation with no migration project and no downtime.

## Non-goals

- Multi-node, replication, distributed commit. Deferred to the far future; the design must not preclude it, but nothing may be built for it now.
- SQL, a text query language, or string parsing of any kind.
- Interactive (multi-round-trip) transactions.
- General-purpose engine parity. The target workload is IO-bound lookup and join on a single node; other workloads are allowed to be poor.
- A physical layout guarantee of any kind, ever.

## Architecture at a glance

| Component | Responsibility | Contract |
|---|---|---|
| Front-end normalizer | Parse the structured request tree, canonicalize it, reject non-canonical forms, enforce leanness | `contracts.md` (logical model) |
| Block evaluator | Execute the stage/block program with per-block atomicity and effect-aware conditions | `contracts.md` (logical model, transaction model) |
| Planner / compiler | Turn a canonical pattern into a specialized program plus a declared set of required physical properties | `contracts.md` (layout contract) |
| Batcher / scheduler | Merge in-flight requests, issue one batched IO submission, allocate outstanding-IO slots and the commit group | `dev-loop.md` |
| Executor | Read tiles through the self-describing tile directory; pure reads may be predicated or speculated | `contracts.md` (layout contract) |
| Storage | Immutable, self-describing, relocatable tiles; one root pointer flip per commit record | `contracts.md` (layout, durability) |
| Durability | Commit record plus group fsync; orphan GC; no payload WAL | `contracts.md` (durability and failure) |
| Trace store | Shapes, counters, latency histograms, load windows — itself a tiled, tiered store | `learning.md` |
| Learner | Per-tile layout policy, funded from one shared budget, evaluated by the angriness predictor | `learning.md` |
| Advisor | Mechanical detector layer plus an LLM layer, both downstream of the trace and upstream of validation | `learning.md` |

## Invariants

Invariants are load-bearing. A change that breaks one of these is an architecture change, not an implementation detail.

- **I1 — No layout guarantee.** Physical layout may change at any time, at any granularity, without notifying users. Every intermediate state is fully servable, correct and performant-enough; no operation requires a database-wide barrier.
- **I2 — A bad layout may be slow, never wrong.** Correctness may never depend on a layout being current. Derived layouts are accelerators; the base copy is the truth.
- **I3 — No global layout state.** Every layout decision is scoped to one tile and made independently. Mixed layouts in one table are normal.
- **I4 — Analysis and optimization are off-path and scheduled.** No learning, profiling or advice work happens on a request's critical path. Expensive background work is scheduled into quiet load windows.
- **I5 — One business request = one DB request.** The request is the unit of atomicity, the unit of pattern learning, and the unit of angriness attribution.
- **I6 — Per-block atomicity, per-request commit record.** A block is all-or-nothing; a request is not. One commit record covers the request's blocks in one fsync. The database commits a durable prefix and reports it, and **never resumes a request**.
- **I7 — No locks.** Read-set validation by tile version gives serializability for the single-round-trip model. The only failure mode is retry; deadlock is impossible by construction.
- **I8 — Tiles are immutable and self-describing.** Reorg is: write a new tile, fsync, flip the root, garbage-collect the old tile. A tile header describes its own layout so any reader can consume any tile.
- **I9 — Speculate reads, never writes.** A wasted read is free on an unsaturated device; a speculative write requires undo and would reintroduce the machinery this design removes.
- **I10 — One scalar objective.** Angriness (`objective.md`) is the only currency and the cost model is its predictor. Any new mechanism must express its cost and benefit in angriness.
- **I11 — Promotions are funded, proven and deployed.** Every promotion draws from one shared finite budget, requires weight **and** stability **and** a demonstrated win, rolls out as a canary with an O(1) rollback, is recorded in the changelog, and stops at the knee.
- **I12 — The server normalizer is authoritative.** Clients may normalize as a convenience; the server's canonical form is the pattern key. Requests are trees, never text.
- **I13 — Reserved interfaces.** The following are frozen before implementation because they are cheap now and expensive to retrofit: self-describing tile header, per-tile version field, per-block effect annotation plus `harmless-if-not-applicable`, one commit point per request.

## Open questions

Honest list of what is not settled. Each is a real design fork, not a research nicety.

- **Derived layout authority.** v1 keeps derived layouts strictly read-only. The eventual design is `learning.md`'s version-checked authority; the per-tile version field is reserved for it.
- **Request grammar depth.** Whether program-level (whole-flow) fallback is ever needed. If yes, an effect-aware server normalizer becomes mandatory; if fallback stays stage-level, the distributivity equivalence class cannot be expressed at all.
- **Heterogeneity ceiling.** How complex a layout policy the reader, solver, simulator and learner can actually sustain per table.
- **Cost-model honesty.** Uncalibrated what-if prediction is optimistic in the literature. How much replay and canary calibration is needed before the predictor can be trusted for promotions is unknown and must be measured.
- **Cold-start quality.** Whether layouts learned in the first weeks of a zero-data deployment are worth keeping or should be aggressively rewritten once the steady-state trace exists.
- **Telemetry budget.** The exact share of cache and IO the trace store may consume.

## Prior art

The design occupies ground that is partly occupied. Every claim below was recalled from memory and **must be verified before it is cited as fact in any external document**.

| Area | Nearest prior art | What remains open here |
|---|---|---|
| Page-level hybrid layout | PAX (Ailamaki et al., ~2001); fractured mirrors | tile granularity at cache level rather than page level |
| Block-level adaptive clustering | Snowflake micro-partitions + automatic clustering; Databricks liquid clustering; Google Napa / Tesseract | learned policy down the cache hierarchy, per tile, one mechanism |
| Compression as an IO strategy | C-Store (~2005); BtrBlocks | per-tile encoding as a learned property, jointly with clustering |
| Variable-length data without indirection | Umbra (Neumann et al.) | per-tile directory making reorg a memcpy |
| Adaptive indexing / learning from zero | database cracking (Idreos et al., ~2007); adaptive indexing; ArcaDB / NoDB | the online reorg mechanism and drift handling |
| Adaptive LSM shape | Monkey, Dostoevsky, LSM-bush (Dayan, Idraos et al.) | the same idea applied to tiled storage, not to LSM levels |
| Offline layout search | index/table advisors (AutoAdmin, DTA) | trace-replay oracle with regret as the metric |
| Compiled query variants | HyPer / Umbra; adaptive execution of compiled queries (Menon, Leis, Neumann, ICDE 2018) | profile-guided, offline, tiered specialization of *layout + code* jointly |
| Learned physical tuning | self-driving DBMS work (CMU Peloton, ~2017) | angriness as a single scalar with a calibrated predictor |
| Advisor | `EXPLAIN`, Oracle SQL Tuning Advisor, MongoDB performance advisor | advice on logical request shape, as a first-class response field, with predicted gain |
| Operational discipline | SRE error budgets and burn rates | error budget as the objective function itself |

Nothing found so far combines: layout-as-materialized-view with continuous drift, one scalar angriness objective, per-tile learned policy under a shared budget, and canonicity-enforced requests as the learning key. That combination is the defensible contribution.
