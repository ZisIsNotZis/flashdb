Budget: 140 lines / 16,000 chars

# flashdb — design

Status: contract-level design draft, **under adversarial review**. Synthesized from a multi-round brainstorm with the author (a data scientist, not a database engineer); the raw attributed decision record lives in `.scratch/01-design/decisions.md`, and review findings and dispositions in `.scratch/01-design/issues/01-design.md`. Nothing is implemented; `flashdb` currently contains design documents, a decision log, a spec and a ticket, and no engine code. Where a review found a claim false, the claim is corrected and its consequence marked `Open` at the point of use rather than deleted.

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
- SQL, or a text query language. (**Corrected, `review-04` L-16:** an earlier wording said "or string parsing of any kind", which is false — requests and schemas arrive as JSON, constants are strings, and `$1.customer_id` is a string-encoded reference path requiring its own parser. The accurate claim is that there is no *query-language text grammar*.)
- Interactive (multi-round-trip) transactions.
- General-purpose engine parity. The target workload is IO-bound lookup and join on a single node; other workloads are allowed to be poor.
- A physical layout guarantee of any kind, ever.

## Architecture at a glance

| Component | Responsibility | Detail lives in |
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
- **I3 — No global layout state.** Every layout decision is scoped to one tile and made independently; mixed layouts in one table are normal. **The shared budget of I11 is not layout state** (`review-03` PA-10): it is a global *resource constraint* arbitrated from local statistics, not a global descriptor of how any tile is arranged. As originally worded I3 and the shared budget appeared to contradict each other, which also undercut the novelty claim.
- **I4 — Analysis and optimization are off-path and scheduled.** No learning, inference, advice generation or cost-model evaluation happens on a request's critical path. The request path only *records* counters and histograms; measurement is unavoidable and is not analysis (`review-01` D10 — the earlier wording said "no profiling", which the per-request trace contradicts). Expensive background work is scheduled into quiet load windows.
- **I5 — One business request = one DB request.** The request is the unit of pattern learning and the unit of angriness attribution. **It is not the unit of atomicity** — that is the block (I6). This conflicts with the motivating inventory workload's `decrement stock iff insert lines`; the conflict and its options are recorded in the ticket as `review-02` F4 and need an author decision.
- **I6 — Per-block atomicity, per-request commit record.** A block is all-or-nothing; a request is not. One commit record covers the request's blocks in one fsync. The database commits a durable prefix and reports it, and **never resumes a request**.
- **I7 — No locks.** Read-set validation by tile version gives serializability for the single-round-trip model. The only failure mode is retry; deadlock is impossible by construction.
- **I8 — Tiles are immutable and self-describing.** Reorg is: write a new tile, fsync, flip the root, garbage-collect the old tile. A tile header describes its own layout so any reader can consume any tile.
- **I9 — Speculate reads, never writes.** A wasted read is free on an unsaturated device; a speculative write requires undo and would reintroduce the machinery this design removes.
- **I10 — One scalar objective.** Angriness (`objective.md`) is the only currency and the cost model is its predictor. Any new mechanism must express its cost and benefit in angriness.
- **I11 — Promotions are funded, proven and deployed.** Every promotion draws from one shared finite budget, requires weight **and** stability **and** a demonstrated win, rolls out as a canary with an O(1) rollback, is recorded in the changelog, and stops at the knee.
- **I12 — The server normalizer is authoritative.** Clients may normalize as a convenience; the server's canonical form is the pattern key. Requests are trees, never text.
- **I13 — Reserved interfaces.** The list is stated **once**, in `contracts.md` under "Reserved interfaces (frozen now)". It was previously duplicated here with four items while the contract listed six, and an invariant list that disagrees with the contract it summarizes is a live source-of-truth conflict (`review-04` L-23). Freezing `harmless-if-not-applicable` before its semantics are coherent (L-01, L-21) is also the wrong thing to freeze.

## Open questions

Honest list of what is not settled. Each is a real design fork, not a research nicety.

- **Derived layout authority.** v1 keeps derived layouts strictly read-only. The eventual design is `learning.md`'s version-checked authority; the per-tile version field is reserved for it.
- **Request grammar depth.** Whether program-level (whole-flow) fallback is ever needed. If yes, an effect-aware server normalizer becomes mandatory; if fallback stays stage-level, the distributivity equivalence class cannot be expressed at all.
- **Heterogeneity ceiling.** How complex a layout policy the reader, solver, simulator and learner can actually sustain per table.
- **Cost-model honesty.** Uncalibrated what-if prediction is optimistic in the literature. How much replay and canary calibration is needed before the predictor can be trusted for promotions is unknown and must be measured.
- **Cold-start quality.** Whether layouts learned in the first weeks of a zero-data deployment are worth keeping or should be aggressively rewritten once the steady-state trace exists.
- **Telemetry budget.** The exact share of cache and IO the trace store may consume; `learning.md` gives ≈1% as a working target, which is a target and not a settled number.

The optimisation and learning-theory review (`review-01`) added the following, all of which block implementation of the learner rather than of the engine:

- **The estimand and the anchor.** Which functional of latency is penalised (mean / p95 / p99 / CVaR), whether the SLO anchor is a capability model for *that* functional, and what `k` is. Until this is settled the objective cannot be implemented (`objective.md`; `review-01` F1–F4).
- **Aggregation and scalarisation.** Which window aggregator, and whether the objective is a pure sum or a constrained program with a shadow price. The current cap contradicts I10 (`review-01` F5, D1–D3).
- **Identification for off-policy evaluation.** Estimator, overlap / ignorability / SUTVA / stationarity assumptions, and a randomisation scheme independent of the policy under test (`review-01` F9).
- **Regret and the oracle.** Normalisation, static-versus-dynamic framing, and how the oracle can be made independent of the learner's own cost model (`review-01` F10).
- **Per-tile estimation.** With ~10⁶ tiles and few pulls per arm, what pooling or shrinkage makes per-tile decisions statistically defensible (`review-01` F16).
- **Credit assignment.** Angriness belongs to a request; the decision is per tile; tiles interfere through a shared cache. Which tile is credited or blamed for a request's angriness delta (`review-01` §2.14).
- **Estimators and power.** No effect size, variance, sample size or cluster count is stated anywhere — for the sample gate, for canaries, for promotions or for traffic splits (`review-01` F8, F12).
- **The request grammar is not a specification.** Fifteen undefined or incoherent items, and the five author decisions they depend on, are enumerated with consequences in `.scratch/03-grammar-decisions/`. They cover block result types and `ifEmpty` semantics, the outcome document and the client-continuation protocol, the predicate and expression language, the equivalence relation behind canonicity, the pattern key, intra-request write-write conflicts, and request-size bounds. **No contract in this repository may be frozen until those are settled.**
- **Author decisions outstanding** — each changes design truth and is therefore not the agent's to take: (1) program-level fallback versus the stage-level restriction, which silently reversed an earlier author instruction that "one single request for all business is more important" (`review-04` L-05); (2) reject versus normalize non-canonical requests (`L-15`); (3) whether predicate-addressed writes are first-class (`L-07`); (4) whether results are ordered, and how top-N and pagination are expressed (`L-14`); (5) request-level versus per-block atomicity (`review-02` F4).

## Prior art

Positioning, the unverified-citation ledger, and the demoted novelty claim live in `prior-art.md`. In summary:

- **Every mechanism in this design is established prior work.** Continuous budgeted re-layout without downtime is shipped (Snowflake, Redshift ATO, Databricks liquid clustering). Fine-grained adaptive layout created from the query mix with the raw data as fallback is **H2O** (SIGMOD 2014) — the closest work in existence to the core mechanism. Adaptive indexing from zero is database cracking. Multiple physical layouts of one logical table with the base as fallback is fractured mirrors and C-Store's WS/RS split. Layout-as-materialized-view is the view-selection literature, which is NP-hard. Per-chunk encoding selection is BtrBlocks. One scalar objective with per-component budgets is Monkey/Dostoevsky. Joint representation and execution specialization is Data Blocks. Automatic tuning with validation and rollback is Oracle Automatic Indexing. And learned physical design synthesised from the workload is **SageDB** (CIDR 2019), which is this thesis's actual neighbourhood.
- **What survives is narrower:** a per-tile *learned* policy rather than a heuristic selector; sufficiency of one shared budget across heterogeneous decision types; and grammar-enforced canonicity making the learning key exact. All three are claims about method and measurement, and all three are falsifiable.
- The previous claim here — that nothing combines these — was **false as written** and contradicted the table directly above it.
