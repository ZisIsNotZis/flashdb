# Status and handoff

Consolidated current state, for the next session. Everything below is measured unless it says
otherwise; every number names its conditions; decisions already adopted are separated from the ones
that are still open.

## 1. The engine

Single-node, single-writer, IO-dense document store; physical layout is per-region and
policy-driven; the durability protocol is fault-injected at every boundary. Three durability/latency
facts changed in waves 2-6:

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
- **The driver's window-boundary drain is non-blocking** (ticket 57; commits `ec47e25` + `76231d2`).
  Schedule-lag samples are recorded at dispatch and service samples at barrier completion, and the
  boundary drains only already-completed barriers (`try_take_group_barrier`), so a cross-boundary
  barrier contributes only its service sample to the completion window; `lag_breach_attribution`
  reports `boundary=0` on every final-binary run. The blocking drain survives only for WAL
  maintenance and end-of-run. Also fixed in `8cf9988`: at the in-flight cap a group is left OPEN for
  retry (not closed/acked), and `end_group` refuses while a barrier is in flight.

| property | measured | conditions |
|---|---|---|
| tests | **289 Rust, 132 Python, zero warnings** (both `cargo build` and `cargo test`) | `cargo test --workspace --offline`; `uv run --with pytest pytest -q` in `harness/` |
| evaluation instances | **8.17x** cold44 (1,643,933,528 B / 192 MiB), **12.25x** cold51 (1,643,925,811 B / 128 MiB), **8.14x** entity-first (1,638,234,253 B / 192 MiB) | seed-42 `adversarial` trace, `declared.policy.json` (4096/10/2) |
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

## 2. The 0b measurement instrument is now trustworthy (four defects fixed, all verified)

Every per-rung number published before commit `744caaa` was unusable. The causes, all now fixed and
independently verified:

1. the curve builder took a **median of per-window deltas**, so a whole group's completions landed in
   one window and `achieved` exceeded `offered` (the "128%" figure);
2. the per-window `invalid`, `lag_p99_us`, `window_us` and `ceiling_us` fields **never reached
   `--metrics-json`**, so the aggregator read `invalid` as always false and the pre-registration's
   "no rejected window" criterion was **vacuous in every published run**;
3. the **trailing partial window was never flushed** (declared 7552 vs summary 8000 — 448
   completions unattributed);
4. the ceiling was applied to a **group-aggregated** quantity: the driver took one latency sample per
   group and assigned it to all members, so the "per-request" p50 was the 64-request group's elapsed
   time. Commit `965adf4` replaced this with an arrivals model: a group forms only from requests
   already due, requests are never consumed early, each request is sampled from its own due time, and
   the report separates per-request service latency from per-request schedule lag with a **named
   ceiling for each**.

Invariants verified on real output (commit `9bae6fe`): the sum of per-rung completions equals the
summary total (8000 = 8000); no rung exceeds its offered rate beyond the 2% tolerance
(`max achieved/offered = 1.0000`); all 64 invalid windows carry a `reject_reason` (service ceiling 64,
lag ceiling 63, union 64); `early_dispatches = 0` with zero tolerance; `error_windows = 0`.

## 3. Primary metric: rung 50 does not meet the lag ceiling

Final binary `76231d2` (ticket 57), `adversarial` trace (seed 42), `declared` arm, ceilings
`--ceiling-us 10000 --lag-ceiling-us 10000`, `--group-requests 64`, checkpoint/rotate 8 MiB, compact
64 MiB. Rung 50, lag-rejected windows:

| instance | data:RAM | lag-rej | svc-rej | lag p99 med/max (us) | svc p99 med (us) |
|---|---:|---:|---:|---:|---:|
| cold44 (family-first) | 8.17x | **6/40** | 39/40 | 1,669 / 235,475 | 21,054 |
| cold51 (family-first) | 12.25x | **4/40** | 38/40 | 2,410 / 82,565 | 16,626 |
| cold57-ef (entity-first) | 8.14x | 15/40 | 38/40 | 4,542 / 153,009 | 20,836 |

**No rung is sustained.** The lag ceiling is blocked by **cold point reads on the request path under
shared-device durability I/O** — not by the fsync-tail cascade (removed by the overlapped barrier)
or the window boundary (non-blocking, `boundary=0` on every run):

- Each order-flow request does its `cust`/stock guard reads synchronously inside the group dispatch,
  and a cold read blocks the single pacer and cascades into the next group's schedule lag. At 8-12x
  data:RAM every request is a cold read (page cache ≈ 8% of data); the request path reads ~**4.03
  blocks/req** (newest-first get, −2.5% from 4.13), ~2.3 ms mean tailing to 100-235 ms under device
  contention.
- The device's WAL `fdatasync` tail is **71-498 ms under load** (ticket 55); the idle 100-sample
  probe is **6.7 ms p50 / 13.4 ms p99** (ticket 54). The 10 ms lag ceiling sits below the durability
  barrier's own fsync p50-p99 (5-31 ms).
- The **service ceiling** rejects 38-39/40 windows at rung 50 and 73-74/75 windows over the whole run
  on every arm: a write group waits for its single WAL `fdatasync` (p50 6.7 ms, p99 13.4 ms on this
  device) against a fixed 10 ms service ceiling, and at low offered load group commit cannot batch
  writes (one write per group). It is **device-bound**: no engine that must `fdatasync` per write
  group on this box can satisfy a fixed 10 ms service ceiling.

Whole run (final binary, all rungs): achieved ~106-107 rps, 0 errors. Family-first compaction output
is now **20.9-23.3 MB over 26-27 merges** (cold44/c51; the off-path build + boundary-skip cadence),
down from the pre-fix ~318 MB over 64 merges; entity-first is 265 MB over 26 merges (§4).
Signal-to-noise: two runs of the same cold44 binary under identical caps disagree (lag-rej 4/40 vs
6/40, `late_total` 221 vs 597), which bounds the honest noise band of the shared box.

## 4. Co-location: the read win does not convert to a load gain

Entity-first key encoding (`--key-encoding entity_first`, commit `5d90b93`) co-locates the P/U/R
views of one entity in a single combined region (3 write regions instead of 6, half the tiles).

| arm | blocks/req (4k) | compact_bytes | merges | rung-50 lag-rej | late_total |
|---|---:|---:|---:|---:|---:|
| family-first (newest-first get) | 4.03 | 20.9 MB | 26 | 6/40 (cold44) | 597 |
| entity-first | 3.36 | 265 MB | 26 | 15/40 | 887 |

- **Request read volume −16.6%** (4.03 → 3.36 blocks/req), and request-path read time is lower
  (18,262 ms vs 19,278 ms over the run): fewer reads, not slower reads on average.
- **Compaction write amp ~12x** (compact bytes ~21 MB → 265 MB, same `--compact-bytes 67108864`, same
  26 merges): merging P/U/R into one region makes each compaction rewrite the combined dirty set (max
  single actuation 8.58 MB vs 0.97 MB family-first), and the compaction worker saturates the shared
  device.
- **Tail worse:** 15/40 lag-rejected vs 6/40, `late_total` 887 vs 597 — the write amplification
  dominates the read win.
- The offline ticket-43 model's **−34.8%** dependent-read reduction (placement 4.740 → 3.092) was a
  read-level result and **was not converted into a load gain**; the model also missed the
  write-amplification cost that now dominates the tail.

## 5. Verdict board

| threshold | status |
|---|---|
| primary metric | **0 sustained rungs** — rung 50 fails the lag ceiling (cold44 4-6/40, c51 4/40, ef 15/40); the service ceiling rejects ~74/75 windows whole-run and is device-bound (§3) |
| T1 learned beats declared by >15% on the win workload | **underived** — needs the learned arm, win workload, noise band, 10-seed median/IQR on the >=10x instance |
| T2 no >5% loss on adversarial-uniform | **underived** — same |
| T3 reorganisation writes <=3x declared | **FAIL, 4.13-4.27x** — byte sums, unaffected by the aggregator defects and unchanged by waves 2-6 |
| T4 drift recovery within 2 windows | **UNDERIVABLE** (measured, ticket 73, c51 12.25x, 3 seeds x 7 rates) — the scan-spike drift never drops throughput for the indexed arms (order_no index already on from the win mix + write share cut 40%->~11%); no recovery event exists; the learner's internal region re-exploration is still 6 windows, > the pre-registered 2 |
| T5/T6/T7 external baselines | **adopted** (§7); the request-level baseline harness does not exist yet |

Still missing for T1/T2/T4: the other arms (learned, static-derived), the win and drift workloads, a
measured noise band, and the 10-seed median/IQR (the >=10x instance now exists: cold51, 12.25x).

## 6. Defects — current state

All twelve measurement/implementation defects catalogued through wave 1 are fixed or recorded; none
is open. The load-bearing current facts:

- The validation unit is the **request** (`docs/contracts.md`, ticket 41): a repeated `block_id`
  inside one request is refused fail-closed, and a request's uniqueness verdict does not depend on
  its neighbours.
- The per-region publish tile-cap defect (the 16 KiB cadence exhausting `MAX_TILES = 1024`) is pinned
  by regression test `bab567c` and fixed on both sides (§7.4).

## 7. Decisions adopted

Adopted by the orchestrator under the author's standing autonomy grant (the author stated on
2026-10-09 that the project has one milestone, that decisions must be made from the original
philosophy rather than asked, and that work must not stop). Reviewable and reversible by the author.

1. **Keep the 10 ms bound; the primary metric is judged on the LAG ceiling.** The pre-registration
   defines its primary metric over schedule lag; the implementation now carries a lag ceiling
   (default 10,000 us) and a service-latency ceiling separately. The relative alternative (two times
   the reference-load p99) becomes concrete only once a reference-load p99 is measured. Moving the
   bound after seeing a failure would be moving the goalposts; the response to a latency failure is to
   attack the latency.
2. **T5/T6/T7 adopted** (they were proposals): at the 1x rung, within 1.5x of the slower of
   {sqlite, postgres}, 2x the fail line, 10x a design failure (T5); at the highest common offered
   level, at least 3x the best baseline's sustained throughput under the same latency rule (T6); the
   declared arm alone must pass T5 (T7). Baselines: sqlite (WAL, `synchronous` stated), postgres
   (frozen disclosed config, no tuning race), one request-level harness driving every arm, every arm
   in the same memory-constrained regime.
3. **The 10x floor is a requirement, not a qualification.** Verdicts from an 8.17x instance are
   provisional; final verdicts are re-derived on a >=10x instance (`.scratch/51-instance-10x`). An
   8.17x instance is warmer than a 10x one, so a failure there cannot be explained away by the lower
   ratio.
4. **The tile-cap defect is fixed on both sides** (`.scratch/46-partitioned-publish-tile-cap`): raise
   and make configurable the tile cap, make the run harness's default cadence compatible with
   per-region publishing, and make a scored run refuse an incompatible cadence loudly instead of
   aborting mid-run. The shipped default must be able to run the shipped engine in the documented
   regime.
5. **Endurance is a constraint, not a price** (`docs/objective.md`): the reorganisation write term is
   0.083-0.669% of the service-time term, so no weight can move an argmin; the objective is maximise
   sustained load subject to the latency ceiling and a write-amplitude budget.

**Still open for the author — primary-vs-service ceiling reconciliation (do NOT resolve here).** The
pre-registration's validity rule invalidates a window on **either** the lag ceiling **or** the
separate service ceiling (plus three-window lag growth or any request rejection), while the adopted
decision above (§7.1, 2026-10-09) judges the primary metric on the **lag** ceiling with service
latency reported separately. These conflict, and waves 3-6 left the reconciliation as a flagged open
author item. The concrete options on the table:

- (a) **Lag-only primary per §7.1** — judge the primary metric on the lag ceiling alone, report the
  service-latency distribution as the efficiency measure. Today this still fails rung 50 (lag-rej
  4-6/40 cold44, 4/40 c51).
- (b) **Keep both ceilings and treat the host/device floor as the limit** — state the durability floor
  explicitly (per-group `fdatasync`; the service ceiling is only met if the device fsync tail ≤ 10 ms,
  which it is not on this box) or make the service ceiling relative to the measured device fsync
  (e.g. `N × device_fsync_p99`).
- (c) **Move the WAL to a sub-10 ms-fsync device** and re-derive the metric there.

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
- **Reviewer hygiene:** do not use the `oracle` agent as a reviewer — it runs with forked context,
  inherits the supervisor's framing, and twice produced narration instead of a review (once with a
  fabricated sub-run id). Use a read-only worker-as-verifier that must paste command output and must
  write `CANNOT-VERIFY` rather than guess.

## 9. Resume plan

Waves 1-6 are integrated (instrumentation, compaction off-path, residual-tail attribution,
lag-stall attribution, overlapped barrier, non-blocking boundary + read optimisation + co-location).
The next steps, in dependency order:

1. **Resolve the §7 ceiling reconciliation with the author** — it decides how the primary metric is
   judged and is the gate on every further verdict.
2. **Move the request-path reads off the pacer** (async/overlapped reads or a read thread pool) — the
   remaining dominant lag blocker; a read-count reduction alone (ticket 57) did not remove the
   device-contention tail.
3. **Re-derive T1/T2/T4** on the >=10x instance (cold51) with all three arms, the win and drift
   workloads, a measured noise band, and the 10-seed median/IQR.
4. **If co-location is pursued**, weigh its +~12x compaction write-amp regression (265 MB vs 21 MB)
   against the −16.6% read win; the cost was not in the ticket-43 model.
5. **Build the request-level baseline harness** and evaluate T5/T6/T7 against sqlite/postgres/lmdb/
   rocksdb.
