# Status and handoff

Consolidated current state, for the next session. Everything below is measured unless it says
otherwise; every number names its conditions; decisions already adopted are separated from the ones
that are still open. T1/T2/T3 are re-derived and T4 is measured underivable on the >=10x instance
(§3, §5); the verdicts are pilot-grade and name their seed counts, never a 10-seed claim.

## 1. The engine

Single-node, single-writer, IO-dense document store; physical layout is per-region and
policy-driven; the durability protocol is fault-injected at every boundary. Load-bearing
durability/latency facts (history in the named tickets):

- **Load-path compaction is off the request path** (ticket 53; commits `a59f703` + `ae1df85`, repair
  `f4539e4`). One `CompactionWorker` thread builds the replacement tile in the background
  (`build_compaction_job`: bounded WAL replay + stream/reduce/write/sync/verify), and the request
  path applies only a lazy flip (`apply_compaction_job`: inactive manifest page write, no fsync, no
  unlink). The build half makes the replacement tile durable (tile `sync_data` + parent-directory
  `sync_all`) off the request path before it can be applied, so a lazy manifest page the OS later
  writes back references only a durable tile: a crash loses the merge, never data.
- **The WAL group barrier is overlapped and durability-preserving** (ticket 56; commits `083da49` +
  `413176c`). The pacer appends+applies group N+1 while a single FIFO background worker runs group
  N's `fdatasync` on a cloned WAL fd. A group is acked only after its covering `fdatasync` returns; a
  failed `fdatasync` fails the group closed (no ack) and poisons the WAL until reopen. At most
  `MAX_PENDING_BARRIERS = 64` barriers are in flight, and every state-mutating path asserts no
  barrier is in flight, so a rotated/compacted snapshot is always a drained prefix.
- **The grouped WAL append is deferred onto the barrier worker** (ticket 67; commits `98ba12e` +
  `16303ef`). The pacer buffers each group's frames (`encode_frame`, crc + memcpy) and the FIFO
  worker does append-then-`fdatasync` on the cloned fd; the memtable apply stays inline. A covering
  `fdatasync` still gates every ack, and the pacer no longer blocks on device write latency (a
  129.6 ms `fdatasync` tail dispatches the next group ~70 us late; no fsync >50 ms becomes schedule
  lag). `wal_valid_len` advances only on an observed successful append.
- **The driver's window-boundary drain is non-blocking** (ticket 57; commits `ec47e25` + `76231d2`).
  Schedule-lag samples are recorded at dispatch and service samples at barrier completion, and the
  boundary drains only already-completed barriers (`try_take_group_barrier`), so a cross-boundary
  barrier contributes only its service sample to the completion window; `lag_breach_attribution`
  reports `boundary=0` on every final-binary run. The blocking drain survives only for WAL
  maintenance and end-of-run. Also fixed in `8cf9988`: at the in-flight cap a group is left OPEN for
  retry (not closed/acked), and `end_group` refuses while a barrier is in flight.
- **Read-ahead warming, incl. eager startup** (tickets 57/60/69). A read-ahead worker warms point
  lookups, full-entity scan prefixes, and the adopted `order_no` index tile (ticket 67's follow-on)
  off the pacer. When the frozen policy wants the index at open, the driver builds+adopts it eagerly
  and runs a bounded synchronous `warm_startup` pass (movement scan prefix + index-tile prefix,
  ~6.9 MB one-time) BEFORE the timed windows, then spawns the worker — the startup warm-up breaches
  (windows 0-2) are removed (`startup_warm scan_entries=30000 index_entries=30000`, windows 0-2
  `total=0` on every static-derived run).

| property | measured | conditions |
|---|---|---|
| tests | **304 Rust, 140 Python, zero warnings** (both `cargo build` and `cargo test`) | `cargo test --workspace --offline`; `uv run --with pytest pytest -q` in `harness/` |
| evaluation instance | **12.25x** cold51 (1,643,925,811 B / 128 MiB, 30,000 seeded StockMovement docs) | the >=10x instance for the decisive T1-T4 runs (`per_region-mixed`, seed-42 `adversarial` trace) |
| data:RAM (seed phase) | ~22-24:1 steady state | 5.16 GB, 1 KiB docs, 64 MiB window |
| open footprint | ~5 MB + bloom/dir, independent of data and tile count | rotated WAL; the driver is the retained suffix (open replays it at 5.15x its bytes) |
| scan | 605 MiB/s cold, 3,155 MiB/s warm at 64 KiB pages | 5 GB single pass |
| unique probe | 0.46 us append-shaped, 1.59 us in-range absent | 12 active tiles |
| write throughput | 143 -> 6,378 rps with batch commit; 184 -> 4,232 rps on the real request path | one fsync per request vs per batch; WAL bytes byte-identical |
| write amplification | 5.87 -> 3.92 (merge shape) -> 2.13 (cap 1024) | 5 M / 1 M docs x 1 KiB |

NOT claimed: bounded recovery time (open is O(suffix), not O(data)); a bounded engine scan footprint
(the request adapter streams into a bounded top-100 map — verified — but the engine's own scan path
was not separately bounded); any absolute comparison against another engine yet (the T5-T7 baseline
harness does not exist); power-loss durability (the crash-consistency tests are same-process
approximations, not real power-loss evidence).

## 2. The 0b measurement instrument

The per-rung measurement instrument is trustworthy. Four defects were fixed and independently
verified (commit `744caaa` marks the boundary before which every per-rung number was unusable):

1. the curve builder took a **median of per-window deltas**, so a whole group's completions landed in
   one window and `achieved` exceeded `offered`;
2. the per-window `invalid`, `lag_p99_us`, `window_us` and `ceiling_us` fields **never reached
   `--metrics-json`**, so the pre-registration's "no rejected window" criterion was vacuous;
3. the **trailing partial window was never flushed** (448 completions unattributed);
4. the ceiling was applied to a **group-aggregated** quantity. Commit `965adf4` replaced this with an
   arrivals model: a group forms only from requests already due, each request is sampled from its own
   due time, and the report separates per-request service latency from per-request schedule lag with
   a **named ceiling for each**.

Invariants verified on real output (commit `9bae6fe`): the sum of per-rung completions equals the
summary total; no rung exceeds its offered rate beyond the 2% tolerance (`max achieved/offered =
1.0000`); all invalid windows carry a `reject_reason`; `early_dispatches = 0` with zero tolerance;
`error_windows = 0`.

## 3. Primary metric (adopted §7.1 lag-only): rung 100 is sustained; rung 50 is not

Decisive measurement (ticket 71): 8 runs = 7 independent seeds (42-48) + one seed-42 repeat, on the
>=10x instance cold51 (12.25x), one flashdb process at a time under `MemoryMax=128MiB` and
`flock /tmp/wave17-measure.lock`, shared 12-core box (loadavg 3.1-12.1; the first 4 seeds were
noisier). **Pilot-grade, not the pre-registered 10-seed matrix.**

Rung-50 lag-rejected windows (out of 40), win workload (`lag_ceiling_rejected_windows`):

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-rpt | median |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 39 | 40 | 39 | 40 | 40 | 40 | 40 | 39 | 40 |
| learned@1.0 | 4 | 6 | 5 | 5 | 5 | 5 | 4 | 4 | 5 |
| static-derived | 3 | 1 | 1 | 2 | 2 | 3 | 2 | 2 | 2 |

**Rung 50 is NOT lag-sustained for any arm (0/40 not reached).** The WP-R1 startup warm-up
(windows 0-2) and the WP-R2 single-region merge oscillation are both removed; the residual 1-3
(static) / 4-6 (learned) windows are scattered mid-run shared-box noise plus the device fsync tail —
compaction-adjacent cold reads on the pacer and device `fdatasync` tails — not a named engine
mechanism. learned's extra 2-4 windows over static are its demand-gated index-OFF observation window
(the WP-O design, ticket 66).

Rung-100 lag-rejected windows (out of 20), win workload:

| arm | 42 | 43 | 44 | 45 | 46 | 47 | 48 | 42-rpt |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| declared | 17 | 17 | 17 | 17 | 17 | 17 | 16 | 17 |
| learned@1.0 | 2 | 0 | 0 | 0 | 1 | 0 | 0 | 0 |
| static-derived | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |

**Rung 100 IS lag-sustained (§7.1):** `static-derived` 0/20 in 8/8 seeds, `learned@1.0` 0/20 in 6/8
seeds (seed42 = 2/20, seed46 = 1/20); `declared` never sustains any rung (rung 50 already 39-40/40,
rung 100 16-17/20). `achieved_fraction` is 0.9995-1.0005 and `error_windows = 0` at rung 100 for both
indexed arms. Rung 200 and 400 are not lag-sustained across all seeds (the device fsync tail
dominates at the top rung). This is the first positive lag-only sustained rung in any multi-seed
sample.

Throughput (aggregate achieved rps, win): declared 70-75, learned/static 106-107 → learned gain over
declared **+42.7% to +52.9%, median +48.6%**, outside the measured **2.48%** throughput noise band
(declared seed-42 vs 42-repeat max relative diff). Per-rung achieved (50/100/200/400): declared
50 / 82-87 / 78-91 / 80-94; the indexed arms hold ~100% of offered at 50/100/200 and ~370-400 rps at
400.

## 4. Co-location (entity-first) is measured, not the decisive axis

Entity-first key encoding (`--key-encoding entity_first`, ticket 57) co-locates the P/U/R views of
one entity in a single combined region. Measured once (pre-R1/R2 engine): request read volume
−16.6% (4.03 → 3.36 blocks/req) but compaction write amp ~12x (≈21 → 265 MB, same 26 merges) and a
worse tail; the offline ticket-43 model's −34.8% dependent-read reduction did not convert into a
load gain. It is not the axis the decisive T1-T4 runs use: those run family-first (newest-first get)
with the learned `order_no` index. History and per-run numbers: ticket 57.

## 5. Verdict board (pilot-grade; seed counts stated)

| threshold | status |
|---|---|
| primary metric (§7.1 lag-only) | rung 100 sustained — static-derived 8/8, learned 6/8 seeds; rung 50 NOT reached (declared 39-40/40, learned 4-6/40, static 1-3/40); declared never sustains any rung |
| T1 learned beats declared by >15% on the win workload | sustained-load: learned sustains rung 100 in 6/8 seeds vs declared never sustained (unbounded margin); achieved throughput: +42.7-52.9% (median +48.6%), outside the 2.48% noise band |
| T2 no >5% loss on adversarial-uniform | learned at/better than declared (rung-50 lag median 1/40 vs 2.5/40); 0 index builds on the 0-scan region (all 8 runs); reorg median **0.928x**, max 0.944x (<=1.5x) |
| T3 reorganisation writes <=3x declared | win reorg median **0.934x**, max 0.941x (<=3x); static/declared median 0.939x; no single-region blowup |
| T4 drift recovery within 2 windows | **UNDERIVABLE** (ticket 73): no post-drift throughput drop exists to recover from; the learner's internal region re-exploration is still 6 windows > 2 |
| T5/T6/T7 external baselines | adopted (§7.2); the request-level baseline harness does not exist yet |

T2 (adversarial, ticket 71): rung-50 lag learned median 1/40 vs declared 2.5/40 (static median
2/40) — learned within, in fact at/better than, declared's band on 7 of 8 seeds. Index builds on the
0-scan region: **0 on all 8 runs** (`index_output_bytes = 0`, final policy `index_order_no=False` —
the demand gate holds). Learned/declared reorganisation ratio per seed 0.925/0.932/0.601/0.944/
0.900/0.933/0.611/0.941 → median 0.928x, max 0.944x (every run <=1x).

T3 (win reorg, ticket 71): learned/declared ratio per seed 0.934/0.941/0.918/0.918/0.934/0.918/
0.934/0.941 → median 0.934x, max 0.941x. The declared denominator is deterministic (30,720,572 B in
all 8 seeds, the R2 first-merge gate). **The single-region blowup does not recur:** win/learned
`U|Stock` is 5.13-5.23 MB / 1 merge in all 8 seeds (pre-fix it swung 75.2 MB / 8 vs 0 MB / 0 on the
same trace), and the max single-window actuation across all 8 runs is 11.08-11.37 MB, one cheapest
contiguous pair.

T4 (drift, ticket 73): **UNDERIVABLE, pilot-grade and not faked.** 3 seeds (42/43/44) × 7 rates
(300-1500 rps) = 9 drift arm-runs on c51. The pre-registered `scan`-weight x10 drift never drops
throughput for the indexed arms: the `order_no` index is already demanded by the win mix's 30% scan
share, and the drift cuts the write share 40% → ~11% (the fsync-bound bottleneck), so the post-drift
mix is strictly no harder than the pre-drift mix (static-derived pre 1485 → post 1500 rps at 1500
offered; no drop at any tested rate). No offered rate exists below the pre-drift capacity that still
saturates the post-drift mix, so the "recover to within 10% of pre-drift throughput within 2
windows" event never occurs. The harness's internal region-level re-exploration signal fires on an
unrelated region (`U|Customer`) and reports 6 windows, which still exceeds the pre-registered
2-window target (`min_samples × drift_reexplore_candidates = 6` with the published defaults) — that
is the region-re-exploration reading, not the throughput T4 the pre-registration wrote.

## 6. Defects — current state

All twelve measurement/implementation defects catalogued through wave 6 are fixed or recorded; the
two items flagged below remain. Load-bearing current facts:

- The validation unit is the **request** (`docs/contracts.md`, ticket 41): a repeated `block_id`
  inside one request is refused fail-closed, and a request's uniqueness verdict does not depend on
  its neighbours.
- The per-region publish tile-cap defect (the 16 KiB cadence exhausting the 1024-tile cap) is
  fixed and re-pinned: `manifest::MAX_TILES` is raised to 4096 (with `MAX_CHAIN_PAGES` 96 -> 316)
  and made a per-engine configurable policy cap (`Engine::set_max_tiles`), the scored runner ships
  the build harness's 8 MiB cadence, and a per-region run refuses an incompatible cadence loudly
  up front. A genuine over-cap publish still fails closed. Regression pins in
  `engine/tests/partitioned_publish_cap.rs` (ticket 46 decision, implemented in ticket 75).

## 7. Decisions adopted

Adopted by the orchestrator under the author's standing autonomy grant (the author stated on
2026-10-09 that the project has one milestone, that decisions must be made from the original
philosophy rather than asked, and that work must not stop). Reviewable and reversible by the author.

1. **Keep the 10 ms bound; the primary metric is judged on the LAG ceiling.** The pre-registration
   defines its primary metric over schedule lag; the implementation carries a lag ceiling (default
   10,000 us) and a service-latency ceiling separately. Moving the bound after seeing a failure would
   be moving the goalposts; the response to a latency failure is to attack the latency.
2. **T5/T6/T7 adopted** (they were proposals): at the 1x rung, within 1.5x of the slower of
   {sqlite, postgres}, 2x the fail line, 10x a design failure (T5); at the highest common offered
   level, at least 3x the best baseline's sustained throughput under the same latency rule (T6); the
   declared arm alone must pass T5 (T7). Baselines: sqlite (WAL, `synchronous` stated), postgres
   (frozen disclosed config, no tuning race), one request-level harness driving every arm, every arm
   in the same memory-constrained regime.
3. **The 10x floor is a requirement, not a qualification.** The decisive T1-T4 runs satisfy it: they
   run on the >=10x instance cold51 (12.25x). An 8.17x instance is warmer than a 10x one, so a
   failure there cannot be explained away by the lower ratio.
4. **The tile-cap fix is implemented** (`.scratch/75-tile-cap-fix/issues.md`, branch
   `wave21/tilecap`): the tile cap is raised (1024 -> 4096, `MAX_CHAIN_PAGES` 96 -> 316) and made
   configurable per engine (`Engine::set_max_tiles`, `--max-tiles`), the scored runner's default
   checkpoint/rotate cadence is the build harness's 8 MiB, and a per-region scored run refuses an
   incompatible cadence loudly up front instead of aborting mid-run. The shipped default must be
   able to run the shipped engine in the documented regime, and a genuine over-cap publish still
   fails closed.
5. **Endurance is a constraint, not a price** (`docs/objective.md`): the reorganisation write term is
   0.083-0.669% of the service-time term, so no weight can move an argmin; the objective is maximise
   sustained load subject to the latency ceiling and a write-amplitude budget.

**Still open for the author (none of these is resolved here):**

- **Primary-vs-service ceiling reconciliation (do NOT resolve here).** The pre-registration's
  validity rule invalidates a window on **either** the lag ceiling **or** the separate service
  ceiling, while the adopted §7.1 judges the primary metric on the **lag** ceiling with service
  latency reported separately. They conflict: under both-ceilings the sustained rate is 0 everywhere
  (the per-group `fdatasync` floor — device fsync p50 6.6 ms / p99 13.4 ms vs a fixed 10 ms — keeps
  the service ceiling rejecting ~16-40 windows per rung on every arm). The concrete options on the
  table:
  - (a) **Lag-only primary per §7.1** — judge the primary on the lag ceiling alone, report service
    latency separately. Today rung 50 still fails (static 1-3/40, learned 4-6/40); rung 100 is
    lag-sustained for the indexed arms (§3).
  - (b) **Keep both ceilings and treat the host/device floor as the limit** — state the durability
    floor explicitly, or make the service ceiling relative to the measured device fsync.
  - (c) **Move the WAL to a sub-10 ms-fsync device** and re-derive the metric there.
- **T4 / residual gaps** (ticket 73): the harness drift shape is hardcoded to `scan x10` (no CLI
  knob), so a write-spike or key-popularity-spike drift is not reachable and might still produce a
  recovery event; the drift static-derived control is derived over the full pre+post run, not a
  strictly pre-drift-frozen control; the rung-50 residual (compaction-adjacent cold reads + device
  fsync tail) and learned's demand-gated window 0-2 (index OFF until the first scan-demand flip) are
  the remaining systematic lag items.

Other items still open for the author (none of these blocks the plan in §9): the 15% / 5% / 3x
numbers and the 10-seed requirement as written for a shared box; the 2-window drift target against
the 6 windows the current parameters need; ticket 13 (open-time corruption detection); D1-D5
(constraint surface); whether the request grammar gets a create/seed shape.

## 8. Environment notes for re-running

- The box is shared: the same 1 M-doc workload has measured 6,137 / 15,060 / 16,264 docs/s on
  different runs. Deterministic metrics (bytes, counts, block reads) are the primary evidence;
  timings must state their concurrency.
- Device fsync is **6.7 ms p50 / 13.4 ms p99** (100-sample idle probe), and under load the per-group
  WAL `fdatasync` tail reaches **71-498 ms**; any write-side number must be read against this floor.
- Cold runs need `MemoryMax` (it counts page cache), a rotate cadence large enough for the per-region
  publish to stay inside the tile cap, and a fully checkpointed instance before the load.
- Timing-sensitive decisive runs are serialized: win/adversarial under
  `flock /tmp/wave17-measure.lock` (ticket 71), drift under `flock /tmp/wave19-drift.lock`
  (ticket 73); one flashdb process at a time.
- **Reviewer hygiene:** do not use the `oracle` agent as a reviewer — it runs with forked context,
  inherits the supervisor's framing, and twice produced narration instead of a review (once with a
  fabricated sub-run id). Use a read-only worker-as-verifier that must paste command output and must
  write `CANNOT-VERIFY` rather than guess.

## 9. Resume plan

T1/T2/T3 are re-derived (pilot-grade, §3/§5) and T4 is measured underivable with the current
harness (ticket 73). The next steps, in dependency order:

1. **Resolve the §7.1 ceiling reconciliation with the author** — it decides how the primary metric is
   judged for any both-ceilings claim and is unchanged; the lag-only rung-100 result already stands,
   but a pre-registration-consistent both-ceilings verdict cannot be stated while the service
   ceiling is device-bound.
2. **Implement the adopted tile-cap fix** (§7.4) — DONE in `wave21/tilecap`, ticket 75: cap
   raised/configured, scored runner cadence reconciled to 8 MiB, incompatible cadence refused
   loudly up front.
3. **Close the T4 evaluability gap** — add a non-scan drift shape (write-spike / key-popularity
   spike) to the harness and re-test, or state T4 as underivable-with-current-harness to the author.
4. **Resolve the rung-50 residual** (compaction-adjacent cold reads + device fsync tail) if a clean
   rung-50 lag-only sustained verdict is required — a compaction re-warm / device-durability axis,
   distinct from the removed startup warm-up.
5. **Build the request-level baseline harness** and evaluate T5/T6/T7 against sqlite/postgres/lmdb/
   rocksdb.
