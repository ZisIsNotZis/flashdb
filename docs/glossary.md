# flashdb — glossary

Terms used in this repository's docs, explained for a reader who is not a database engineer. One line each; the docs use these freely without re-defining them.

## Storage

- **Base copy** — the single authoritative record of what a user wrote. Exactly one per row. Source of truth.
- **Tile** — the storage unit. Not a row and not a column: a self-describing, immutable block holding a subset of rows with a subset of columns, plus a header and an internal directory. Target size is on the order of megabytes, chosen against the device.
- **Derived layout** — a second, differently arranged copy of the same data, built by the engine for a query pattern. Example: one tile holding order 999's header plus its 20 lines plus the product names, so "show order 999" is one read instead of up to 41.
- **Authoritative for reads** — permission to answer a query *only* from a derived copy, without consulting the base copy. Requires the copy to be provably current; the failure mode is silent wrongness.
- **Read-only derived** — the v1 default: the derived copy is a pure accelerator with a fallback to the base copy. Worst case of any bug, lag or crash is slowness, never wrongness.
- **Materialized view** — a computed copy of data kept up to date by the writer. The correct frame for authoritative derived layouts: same node, single writer, so no conflict resolution is needed — only atomic co-update.
- **Reorg** — rewriting tiles into a different arrangement in the background. Here it is a pointer flip over immutable tiles, not a migration.
- **Fan-out** — one request needing N child records that live in N places. The number of dependent reads fan-out causes is the dominant latency term in the motivating workload.
- **Dependent read** — a read whose address is only known after a previous read completes. These serialize; independent reads can be issued together.
- **Queue depth** — how many IO requests a device can have outstanding at once. NVMe: hundreds. Hard disk: effectively one, which is why the fan-out strategy differs by device class.
- **Cache pollution** — one access evicting another's useful data. Space that data occupies is therefore a latency cost, not just a disk cost.
- **Write amplification** — one logical write becoming K physical writes, because K copies must be maintained.

## Concurrency and durability

- **WAL** — write-ahead log. The conventional durability device: write the intent sequentially and cheaply, then apply it to scattered data pages later.
- **Commit record** — the small durable artifact that makes a commit real. Here it is tiny (the data is already immutable and durable), as opposed to a payload WAL.
- **Root flip** — atomically switching which root pointer identifies the current committed state. This is what makes a multi-tile commit atomic without a log.
- **fsync / fdatasync** — force the device to make writes durable. Expensive (tens to hundreds of microseconds); amortized by batching.
- **Group commit** — many concurrent commits sharing one fsync, trading per-request latency for throughput.
- **Orphan GC** — reclaiming tiles that no committed root references, including tiles left by a crash. Crash-safe garbage collection is the real replacement for WAL replay.
- **Snapshot** — the consistent view of the data a request reads from.
- **OCC** — optimistic concurrency control. Validate at commit rather than locking up front.
- **Read-set validation** — checking at commit that everything the request read is still current. With immutable tiles it is an integer comparison per tile.
- **MVCC** — multi-version concurrency control. Readers see a snapshot while writers add versions. Deliberately minimized here.
- **First-committer-wins** — the loser of a write conflict retries instead of blocking.

## Query surface

- **QBE** — query by example. A structured, form-filling query style rather than a text language: boxes with columns and conditions. Here it is a JSON tree, never parsed text.
- **Canonical form** — one and only one way to express a given intent, so the learner sees one pattern instead of five spellings of it.
- **Pattern key** — the identity of a request shape for learning purposes. Derived from the canonical form with constants stripped.
- **Effect annotation** — marking each block as read or write. Required for the normalizer's soundness and for predication decisions.
- **Stage** — an ordered step in a request; a stage's blocks run in parallel.
- **Predication / if-conversion** — running a harmless variant unconditionally instead of evaluating a condition and branching. Cheaper when the condition itself costs an IO and the variant does nothing when inapplicable.
- **Speculation** — issuing reads for a branch before the condition is known, betting on the outcome.

## Learning and operations

- **Trace** — the recorded workload: pattern shapes, counters, bytes, misses, latency histograms, load windows. The training set.
- **Off-policy evaluation** — estimating what a *different* layout would have cost, when only the chosen layout was ever observed.
- **Angriness** — the objective: a bounded S-curve penalty for exceeding the latency SLO under the current load, plus credit terms (served fraction, deadline met) and penalty terms for unserved, wasted work, space and reorg/compile cost. One primitive covers every term — σ for penalty, 1 − σ for credit. See `objective.md` for the full term list and for its still-unresolved defects.
- **SLO** — service level objective: the latency target a request is expected to meet.
- **p99** — the 99th percentile. Used instead of the mean because latency distributions are heavy-tailed and the mean hides the failures.
- **Error budget / burn rate** — the accumulated tolerable violation, and the speed at which it is being consumed. Angriness is the budget; its derivative is the burn rate.
- **Canary / gray release** — rolling a change out to a fraction of the data, monitoring for regression, and rolling back if it loses.
- **Regret** — the gap between the layout achieved and the best layout an omniscient oracle would have chosen. The learning metric.
- **Zipfian** — a skewed access distribution where a few keys are hit constantly and most are rarely hit. Real workloads look like this; uniform synthetic benchmarks do not.
- **Sketch** — a fixed-size approximate counter or histogram (HyperLogLog for cardinality, HDR/t-digest for latency), used so telemetry stays small and never needs exactness.
