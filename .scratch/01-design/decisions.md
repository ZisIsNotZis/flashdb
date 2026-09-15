# flashdb — raw decision log

Append-only record of what was decided, round by round, during the design brainstorm. This log holds **decisions and their provenance**; the settled design lives in `docs/` and is not duplicated here (R-DOC.3). Read `docs/design.md` first.

Participant A = the user (a data scientist, not a database engineer, by his own statement). Participant B = the assistant. Dates are 2026-09-15 unless noted.

## Round 1 — the initial pitch

- A: proposes a relational database with flash-attention-like cache-hierarchy-aware orchestration. Ideas offered: (1) JSON schema plus QBE instead of SQL, no SQL and no physical-layout guarantee, layout analyzed and background-optimized from real runtime statistics, business must state its requirement precisely with clear boundaries and no overspill fields; (2) tiled storage, neither row nor column, with learned column grouping and learned row clustering, tile and per-layer cache setup derived from the actual machine; (3) deliberate space minimization because space costs cache misses, variable-length values above a threshold stored as blobs, and the question of whether a per-row int64 pointer is needed at all; (4) all databases start from zero so the cold period is the learning opportunity, and with 1B rows a migration may be unnecessary; (5) asked whether relational semantics and transactions cause a lot of trouble; (6) the real database should guide flashdb's own development with real test cases, training and test sets, containing schema, access pattern, data amount and data generator.
- A: states he is not a database expert and asks whether optimizing for IO above all else is the right instinct, and whether techniques like deferred/merged read and write and online dynamic batching apply.
- B: mapped every idea onto named prior art (PAX, column groups, Snowflake micro-partitions and automatic clustering, C-Store compression, Umbra, database cracking, ArcaDB/NoDB, LSM-bush, Napa/Tesseract), and flagged that the pitch "nobody thought of this" is false while the combination remains defensible.
- B: challenged the premises — regime-dependent "IO first"; the flash-attention analogy breaks because its working set is provably bounded and its schedule data-independent; censored learner feedback makes off-policy evaluation the core methodological problem; "business writes lean queries" creates statistics pollution; global layout state is what really makes migration hard; and the transaction trouble is concentrated. Recommended banning interactive transactions and coupling tile boundaries to transaction boundaries.

## Round 2 — clarifications and constraints

- A: refines "optimize for IO" as regime-scoped: 10 TB with 128 GB of RAM and heavily joined lookup is the case that matters; when everything fits in memory, sub-millisecond latency is not the point because TCP is slower anyway.
- A: the flash-attention analogy should be understood as **data driven** — profiler based, the database as its own profiler: find hot spots, explain them, record and analyse hot patterns, then optimize.
- A: wants a "world model" / memory-training-style self-evaluation per deployment change plus small dynamic experiments, so a layout change can be predicted without migrating gigabytes for a trial.
- A: confirms the "same intent five ways is five patterns" problem, and concludes the QBE must make the same intent **unwritable** twice, with the database doing all the dirty work (field duplication, partitioning) and always offering the cleanest query. Single source of truth is stated as critical for both schema design and requests.
- A on key-space proximity: collect all patterns over a long run and strike a balance; a new pattern is ignored unless it **proves** its amount and necessity, and is considered more as its weight increases.
- A on transactions: no explicit transactions; a single request is atomic and takes effect immediately; prefer avoiding lock waiting, and if forced to choose, sacrifice single-request latency for aggregate throughput on contended data.
- A: single node for now, multi-node far future. Q1 research thesis. Q2 single node, highest throughput, modern Linux. Q3 no interactive transactions. Q4 first workload: a basic inventory system.
- B: gave the arithmetic that defines the thesis — 10 TB with 8 MB tiles and 128 GB of RAM means a ~1.3% resident tile set, so the metric is **dependent random reads per request**: co-located fan-out is ~1 miss (~80 µs) versus ~20 serialized misses (~1.6 ms). Concluded that row clustering by the join key matters more than column grouping for the motivating case, and that a per-tile directory paying for itself three ways (cheap reorg, parallel fan-out, relocatable tiles) should be the load-bearing primitive.

## Round 3 — IOPS, durability, minimal transactions, canonicity, drift

- A on fan-out: bounded by device reality (a maximum number of parallel IOPS; a hard disk is a different regime). Under load take the efficient approach — lossless fan-out, total work unchanged. With very few requests, extra total work is free.
- A on durability: proposes deferring it, and instead of a WAL, holding the client until the next fsync, acknowledging only after durability. Asks whether that is too extreme. States robustness requirements independently: memory safe, robust, must not crash itself.
- A on multi-update requests: multiple updates allowed only when expressible as one QBE request, i.e. logically coherent together; one request yields a list of operations; only practically possible patterns are permitted; minimize transaction burden as far as possible.
- A on canonicity: **both** a syntax property and a normalizer property. Design the QBE so it is hard to write another way, patch holes when found (possibly by disallowing a form), and fall back to the normalizer only where syntax cannot do it. Assumption stated: developers are not adversaries and follow the requirement when it is reasonable, consistent and easy to figure out.
- A on layout drift: all tile/block setups are local and may drift across the database; because migration takes time, a mixed intermediate state is itself one kind of stable state; different rows of the same table may benefit from different layouts; how heterogeneous the policy can be depends on how complex the profiler, solver and simulator can get.
- A: TPC-C is too point-lookup heavy with negligible joins and must be enhanced with heavy join cases; he could not produce good heavy-join examples himself.
- A: Q1 (derived layout authoritative on the write path, or read-only) depends on workload statistics — read heavy, write heavy or both; there must be a percentage. Q2 `/ponytail`: start quick with existing components and optimize part by part toward a fully co-designed system. Q3: Postgres as step 1, unsure whether the bar is too high, decision delegated to B.
- B: proposed the durability design that survived — do not drop the WAL, drop the **payload** from it, because immutable append-only tiles make the data path already sequential and immutability supplies atomicity; the WAL degenerates into a ~64-byte commit record foldable into the root. Flagged the real costs: no publish before data is durable, crash-safe orphan GC replacing replay, and never trusting a device's flush claim.
- B: proposed the minimal transaction model: one commit point per request, read-set validation by **tile version** (free phantom protection because immutable tiles are a validation granule), no locks hence no deadlock, retry or coalesce on conflict, idempotency keys, and a closed enumerable set of write shapes. Named the one real loss: cross-request invariants cannot be enforced.
- B: on canonicity, argued it is a property of the normalizer rather than the syntax, recommended measuring the residue as canonicalization precision/recall, and warned that the engine's own rewriter is a second writer and must produce fixed-point canonical forms.
- B: on drift, upgraded the invariant to "every intermediate state is a fully servable state", with a layout-polymorphic reader as the real engineering bill, plus warnings about churn and about scans paying for the worst layout.
- B: gave four heavy-join inventory workloads, one hypothesis each, and set the baselines: LMDB, SQLite, RocksDB as honest early comparisons and Postgres as the scoreboard rather than a step-1 goal, all at equal durability.

## Round 4 — baselines, batching, request shape, relaxation, the talking database

- A: build the harness against **both** Postgres and SQLite.
- A: wants **in-flight batching, LLM-server style** — do not fetch immediately; wait for mergeable requests, do them together, then fan the data out. Motivation: removes reliance on RAM/L1/L2/L3 residency, "we don't need to hope they still exist".
- A: strong guidance — **one incoming business request = one database request, no more**; put every logic that can go inside into it; a request is one atomic block that may contain many atomic blocks, each block restricted to permitted use cases so the least atomicity tax is paid; analysis nevertheless happens at whole-request granularity as a merged pattern; one request deriving another is allowed if necessary though a better schema should usually avoid it; compute goes into the database only when it merges requests or drastically cuts network IO, otherwise it stays service side.
- A: notes `io_uring` on modern Linux as a way to reduce syscalls.
- A: the learner's own collected data is not precious and can be stored without WAL since rollback is affordable; wants flags of the form "can afford rollback", "can accept failure", "can tolerate correctness problems within a few seconds" — i.e. ask the user to concede and optimize against the true bottom line.
- A: wants the engine to **tell the human** what is hot and what is fundamentally hard to optimize, with suggestions or checklists; regular databases do not talk, a modern one could, because only it knows the physical problem that can only be solved outside.
- B: explained derived layout and read-only versus authoritative from first principles with an inventory example, and explained why read-only is the safe v1 (worst case slowness, never wrongness).
- B: on batching, the key reframing — it replaces a **probabilistic** bet on cache residency with a **deterministic** one on co-occurrence in a scheduling window; batch the plan, not the transactions; the wait is nearly free because it coincides with the group fsync; and it needs deadline-aware batching to avoid head-of-line blocking.
- B: on the request shape, noted the request boundary is where the co-occurrence signal actually lives, so one-business-request-one-DB-request is a statistical requirement and not only a latency optimization; recommended whole-request atomicity; restricted derived follow-ups to keys computed inside the same request; and priced pushdown as bytes saved × network cost > DB CPU cost + contention cost.

## Round 5 — privacy, off-path analysis, push

- A: confirms **all analysis and optimization is off-path and scheduled**.
- A: privacy — the database does not configure the LLM, the human does; privacy level is configurable up to shielding most of the data, with LLM effectiveness decreasing but still capable of interesting patterns and suggestions based on assumptions.
- A: push may be **fire-and-forget**, no delivery guarantee needed; all suggestions and all human feedback live in the database, including what is changeable and what is not.
- B: the shield governs **egress** only — the in-database validator always reads real data, so shielded mode means hypothesis-proposed-blind then checked, with provenance recorded. Named the three-rung privacy ladder.
- B: named the human accept/reject record as a **label set** teaching the engine which relaxations this user actually grants, i.e. a second cheaper bandit alongside the layout bandit.
- B: the webhook is a wake-up nudge (cache invalidation), not a delivery channel, so no delivery guarantees are needed; the only real requirements are a monotonic cursor, a polling fallback, a bounded non-blocking queue, and shape-only payloads.

## Round 6 — blocks, not whole-request atomicity

- A: **corrects** the model — an atomic block within a request either fully applies or does not; **not** the whole request. Blocks in one request may be combined with trivial logic (`&`, `&&`, `||` and similar).
- B: proposed the semantics: one commit record per request containing N atomic blocks in one fsync, partial outcome reported as the durable prefix; `&` blocks mutually isolated and reading the pre-request snapshot; `&&` stages ordered and seeing earlier effects; `||` as fallback rather than a race; the database commits a prefix and never resumes; conditions over block results restricted to a decidable predicate set. Flagged that mixing `&&` and `||` freely creates distributivity equivalences that violate canonicity.

## Round 7 — expressiveness wins, and the syntax becomes structured

- A: accepts the distributivity hazard as real but **refuses to restrict the grammar** to avoid it, because real business logic is complex and may genuinely need those shapes; "one single request for all business is more important". A normalizer is therefore close to a must-have, client side or server side, at least one side.
- A: `&`, `&&`, `||` were an analogy only — the interface stays QBE, with **no string parsing** and a more elegant structured syntax.
- B: proposed the operator-free form — sequence is array order, parallelism is implicit within a stage (hence a physical decision), fallback is a named key on a stage — with the payoff that `(A && B) || (A && C)` becomes *unexpressible*, so the equivalence class never exists and no normalizer is needed for distributivity. If program-level fallback is ever needed, the normalizer returns and must be server-authoritative, effect-aware (factoring a write is a double-apply, i.e. corruption), idempotent, and versioned, with the version inside the pattern key.
- B: recommended freezing per-block effect annotation now (plus `harmless-if-not-applicable`), since the normalizer's soundness and the predication strategy both depend on it.

## Round 8 — prediction

- A: raises that conditional blocks "kind of invent branch prediction", and that the solver/optimizer might reason about it.
- B: reframed the general form as **access prediction** (temporal), with branch prediction as the conditional case; set the hard line "speculate reads, never writes"; noted the idle/latency mode already licenses speculation if waste is priced in queue slots; and noted it only pays when the guard needs a fresh read.

## Round 9 — what the user actually meant

- A: commonly seen patterns are **compiled into JIT-ed programs with an optimal data path**; a branch splits that program into two variants; identifying the branch in advance with high confidence lets the engine fire one and save time. The miss is computable from stats and history — probability, gain, loss — so one does not work hard to decide. The key observation: **when the non-applicable branch is a no-op, no prediction is needed at all** — `if (0.5) + then/else (1) = 1.5` in two steps becomes `then (1) + possible else (1) = 1 + 0.1·1 = 1.1`.
- B: named it **if-conversion / predication**, and argued it is worth more in a database than in a CPU because the guard is itself an IO (so the `0.5` is nearer 100 µs than a cycle). Gave the three-term decision rule (guard evaluation, variant execution, mispredict recovery), recommended the `harmless-if-not-applicable` annotation so predication needs no prediction at all, argued the compiled artifact's value is the **baked read plan** rather than the instruction count, and flagged that a reorg invalidates compiled programs, so the logical program and the physical access plan must be separated.

## Round 10 — two corrections from A

- A: **"machine code does not make an IO-bound path faster" is wrong here** — the fused code is co-designed with storage layout and does reduce IO. B conceded the artifact is co-designed and sharpened the causal chain (pattern → layout decision and code decision), insisting the two outputs be attributed and priced separately because they differ in cost and in risk. This produced the precise interface: a program **declares required physical properties**, and the storage engine satisfies them either by changing the layout permanently or by paying per execution — that fork is the layout-versus-runtime trade.
- A: **"JIT" is the wrong word** — it is static analysis over a long observation window; nothing is compiled unless it proves its importance, and the more important it is, the more of the world is traded away for it. B accepted and named it profile-guided, offline, tiered specialisation, with the JVM tier ladder, PGO and LSM tiering as precedents. B added the required completion (a shared finite budget, since promotions compete) and derived a new rule: **promotion needs weight AND stability AND proof**, with a demotion path, stopping at the knee.

## Round 11 — budget, burstiness, metrics, gray release

- A: the direct budget is latency and throughput, which stand in a ratio, and that ratio is itself time-varying (long quiet periods, very short busy periods); therefore very detailed temporal-spatial-row-column-table-wise metrics at every degree are needed. Promotion needs weight and stability **and proof it works**, with rollout like software development: **gray release**.
- B: replaced the ratio with a formulation — latency has a cliff and throughput degrades gracefully, so minimize work subject to p99 ≤ SLO per load regime, which removes the latency/throughput weight entirely. Concluded pattern weights must be **time series, not scalars**; that bursts are predictable so the engine can pre-position; that expensive background work belongs in quiet windows; and that telemetry needs the same discipline as data (sketches, tiered rollup, percentiles never means, ≈1% cache cap) — with the recursion that the telemetry store is itself a tiled tiered store. Named gray release as nearly free because the reader is layout-polymorphic, so mixed layouts are already legal; rollback O(1) but requiring a retention window; and two traps — regime-matched comparison and cache/queue contamination, so a canary result is a conservative lower bound.

## Round 12 — the angriness objective

- A: proposes a **quadratic "anger level"** — fine below the SLO, rising quadratically above it; likewise for throughput, with the SLO itself a function of concurrency; the goal might be to minimize total angriness.
- B: endorsed soft penalties over hard constraints (magnitudes, not a binary signal), and identified the **tautology trap**: if `SLO(load)` tracks observed latency then angriness is identically zero, so the anchor must be a capacity model built from measured service time (`k · service_time / (1 − utilisation)`). Payoffs: angriness becomes comparable across load, and the residual becomes diagnostic (high-load residual = scheduling or contention; all-load residual = layout or work). Insisted on clamping because latency is heavy-tailed and a superlinear penalty on a heavier-tailed distribution has an unstable expectation. Named the synthesis — angriness is the single scalar and the cost model is an angriness predictor — and the precedent: SRE error budgets and burn rates.

## Round 13 — the S-curve

- A: agrees with clamping but wants a **sigmoid, like a neuron**, and below the SLO there should still be a gradient — not fully zero, just very small.
- B: accepted as better than its own clamp (smooth *and* bounded), and specified the placement: the SLO sits **just below the inflection point**, never centred on it, since a centred sigmoid wrongly scores half the maximum penalty at the SLO. Argued the below-SLO gradient is load-bearing rather than cosmetic — it is what makes the engine keep buying **headroom** after it is compliant, and headroom is what absorbs short intense bursts. Added: report angriness as a delta from the healthy baseline; three parameters from three sources (location derived, scale normalisation, steepness learned from accept/reject on a slower timescale); and one primitive for every term (σ for penalty, 1 − σ for credit).

## Round 14 — write it down

- A: instructs the assistant to create the docs and to spawn criticizing challenger subagents (fork=false) from all kinds of aspects to find holes.
- B: wrote `docs/design.md`, `docs/objective.md`, `docs/contracts.md`, `docs/learning.md`, `docs/dev-loop.md`, `docs/glossary.md`, the spec, the ticket, this log and a README; committed them; and spawned six fresh-context adversarial reviewers, one per aspect, with findings to be dispositioned in the ticket.
