# Status and handoff — 2026-10-05

Written for the author's return. Everything below is measured unless it says otherwise; every
number names its conditions (cold/warm, instance size, seeds), and the open decisions are listed at
the end rather than buried.

## 1. What the engine is now

A single-node, single-writer, IO-dense document store whose physical layout is per-region and
policy-driven, with a durability protocol that has been fault-injected at every boundary.

| property | measured | conditions |
|---|---|---|
| data:RAM | ~22-24:1 steady-state | 5.16 GB of data, 1 KiB docs, 64 MiB window, **seed-phase** number |
| open footprint | ~5 MB + bloom/dir, independent of data and tile count | rotated WAL; the driver is the retained suffix (open replays it at 5.15x its bytes) |
| scan | 605 MiB/s cold, 3,155 MiB/s warm at 64 KiB pages | 5 GB, single pass |
| unique probe | 0.46 us (append-shaped keys), 1.59 us (in-range absent, with bloom) | 12 active tiles |
| write throughput | 150 rps → 6,378 rps with batch commit (143 → 4,232 rps on the real request path) | one fsync per request vs one per batch; WAL bytes byte-identical |
| write amplification | 5.87 → 3.92 (merge shape) → 2.13 (cap 1024) | 5 M / 1 M docs x 1 KiB |
| tests | 250 Rust, 101 Python, zero compiler warnings | `cargo test --workspace --offline` |

Deliberately NOT claimed: bounded recovery time (open is O(suffix), not O(data), but that is not the
same as bounded), a bounded scan footprint (the request adapter streams into a bounded top-100 map,
which we verified, but the engine's own scan path was not separately bounded in this pass), or any
absolute-throughput comparison against another engine.

## 2. What experiment 0b has measured so far

Cold instance: 1.647 GB of data under a 192 MiB `MemoryMax` = **8.18x** (below the pre-registration's
10x floor, flagged in every report), 10.3 M rows, rotate cadence 8-16 KiB so the open-time suffix
replay fits, every arm's cgroup peak at the cap (cache-evicting), one seed per arm.

| threshold | verdict | evidence |
|---|---|---|
| T1 learned beats `flashdb-declared` by >15% on the win workload | **TIE / not a win** | every arm sustains the same top rung (100 rps); whole-run achieved 80 vs 81-83 rps (inside the noise band) |
| T2 no >5% loss on adversarial-uniform | **NOT-MEASURABLE (7.7% loss)** | the arm ran; the learner lost 7.7% at 400 rps, but the noise band was not measured this run and ~10-20% is what this shared box has shown, so 7.7% cannot be called a disqualifying loss |
| T3 reorganisation writes <=3x declared | **FAIL** | learner 10.7-11.1 MB vs declared 2.6 MB = **4.13-4.27x**, once compaction was actually actuated |
| T4 drift recovery within 2 windows | **NOT MEASURABLE** | the drift rate (150 rps) exceeds the capacity of its own 64%-scan mix, so every arm saturated and no pre-drift baseline exists; separately, the default parameters need 6 windows |

Findings that are *not* thesis verdicts but matter:

- **The learner produces a plausible, differentiated policy** (defer `P|Order` and `P|StockMovement`
  to merge_min_tiles=200, keep `P|Stock` at 8 with 4 KiB pages, 64 KiB for `R|Order`) that matches
  the access pattern - and still buys no throughput while costing ~4x the writes. On this workload
  the declared default is simply strong.
- **`lambda` cannot make endurance decisive, structurally**: the write term is 0.003% of the service
  term for ingest and 0.083-0.669% for reorganisation, so no lambda sweep can move a candidate's
  argmin. This is an objective-design decision (raise the weight ~100x, normalise the terms, or make
  endurance a constraint), not a calibration fix.
- **Churn control works**: per-region cooldown plus a per-window reorganisation budget cut the
  learner's reorganissation bytes from 402,984,851 to 28,191,940 (14x) under drift.
- **The learning unit is the request, not the block** (`docs/learning.md`): the co-occurrence signal
  lives at the request boundary; block-granular aggregation would split one pattern into several.

## 3. What has been falsified or closed along the way (all by measurement)

- **Per-tile ordering is not a knob** in this engine (the writer requires increasing keys; the
  directory is a monotone fence list) - so the first layout space I proposed was a no-op, and L1 was
  reframed.
- **Read-side layout axes measured flat WARM** (page size, bloom bits) - and **+54% throughput COLD**
  (per-region mixed pages vs global 4 KiB). The warm measurement could only move CPU; the whole
  "flat" era was an artefact of a 128 GB page cache, which is why the pre-registration demands
  >=10x cache.
- **Family partitioning is a dead axis** at the current cap (write amp 4.09 → 7.75); it only turns
  positive above cap ~96 and even then by +8.7%, below the 15% threshold.
- **Bloom bits are dead on these workloads**: every absent probe is rejected by the range filter
  before the bloom is consulted (new monotonic keys sort above the tile's last key).
- **Omitting a derived key family is unsafe in v0** (the doc payload is opaque to the engine, so a
  dropped `U` entry would silently break uniqueness) - only family *partitioning* is safe.
- **The tile cap is the largest single writing lever** (cap 12 → 96 cut write amplification ~59% in
  simulation; cap 12 → 1024 measured 2.637 → 2.133 with zero compactions at 1 M docs).

## 4. Measurement defects found and their state

Each of these was found because a number looked too good, and each is fixed or recorded:

1. scan-rate claim derived from probe timings (mine) - corrected.
2. device probe that truncated without writing (reported a 0 us device) - fixed, and the earlier
   baseline it invalidated is retracted.
3. warm-vs-cold conflation across all axis comparisons - corrected, and the axes were re-run cold.
4. the pacer's own timer granularity masquerading as engine saturation - fixed (burst pacing).
5. `sustained=800` with `achieved=129`: the curve builder ignored the pacer's per-window `invalid`
   flag and its lag, so a rung's first window (draining the previous rung's backlog) scored as
   sustained - fixed, with a regression test.
6. "T3 = NOT-MEASURABLE (0 vs 0)": the load path never called `maybe_compact_shape`, so the learner's
   main knob was inert - fixed by actuating compaction on the load path.
7. **ACHIEVED > OFFERED (128%)**: the per-rung attribution is still wrong (a rung's first window is
   credited with the previous rung's drain), and the runs carry no `--ceiling-us` so the latency
   constraint is not evaluated - **OPEN**, recorded, and the reason no per-rung number from this
   runner is quoted above.
8. **One unique conflict rejects the whole commit group**, and a deferred-barrier group spans
   independent requests (slice 23's `--group-requests`), so the contract's validation unit is the
   request while the implementation's is the group. Latent in every 0b run (all report `commit =
   1.000`); it bites under a hot unique key. Contract: `docs/contracts.md`; fix ticket:
   `.scratch/41-request-level-commit/issues.md`.

## 5. What the author needs to decide

1. **The pre-registration's numbers**, which a shared box and one seed per arm cannot support as
   written: the 15% / 5% thresholds, the 10-seed requirement, and the **2-window drift recovery**
   (the current parameters need 6 windows - `drift_reexplore_candidates * min_samples` - so either
   the target or the parameters must change).
2. **`lambda` semantics — resolved 2026-10-06.** Endurance is a constraint, not a price: the
   reorganisation write term is 0.083-0.669% of the service-time term (ingest 0.003%), so no lambda
   sweep can move an argmin and endurance is not the scarce resource. See `docs/objective.md`.
3. **Whether 0b's win workload should be the current inventory flow** (where the declared default is
   strong and the learner cannot win) **or the motivating join-heavy shapes** from `docs/dev-loop.md`
   (star-schema fan-out, multi-hop, as-of, anti-join), which have still never been generated - ticket
   29 shows the generator produces four roughly independent shapes, so the thesis's own motivating
   workloads are untested.
4. **Ticket 13** (open-time corruption-detection contract: eager full verification vs lazy
   verification on read), **D1-D5** (constraint surface), and whether the request grammar gets a
   create/seed shape (today dimensions can only be created through engine-level ops, not through the
   documented request surface).
5. **The external bar (author, 2026-10-06)**: not slower than SQLite/PostgreSQL at low load, far
   ahead at high load, and the initial policy not absurd. Baselines B1 (`sqlite`/`postgres`/`lmdb`/
   `rocksdb`) and thresholds T5/T6/T7 are proposed in `.scratch/15-objective-v0/pre-registration.md`
   and await sign-off.

## 6. Environment notes for anyone re-running this

- The box is shared: wall-clock varies run to run (the same 1 M-doc workload measured 6,137 / 15,060
  / 16,264 docs/s on different runs). Deterministic metrics (bytes, counts, block reads) are the
  primary evidence; timings are indicative and must state their concurrency.
- Device fsync p50 sits at 5-6.7 ms in-session (p99 to 23 ms) and must be reported alongside any
  write-side number.
- Cold runs need `MemoryMax` (it counts page cache), a rotate cadence small enough that the 5.15x
  suffix replay fits, and a fully checkpointed instance before the load.
