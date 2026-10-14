# 67 - WP-P: remove the request-path device durability stall (the last rung-50 lag blocker)

## Contract

Work package P of the wave14 program (`.scratch/52-program/issues.md`, control script
`wave14-lastblockers.js`). Remove (or precisely bound) the request-path **device write stall**
that keeps the frozen `static-derived` win layout at 3-4/40 rung-50 lag-rejected windows even with
the WP-M `order_no` index adopted. The stall is: a slow covering `fdatasync` on the overlapped
barrier worker holds the shared device, so the NEXT write group's synchronous WAL `write()`/append
blocks the single pacer, turning device latency into schedule lag. This is the last blocker between
the +40-51% layout win and a clean §7.1 lag-only SUSTAINED rung-50 verdict.

Acceptance (verbatim from the task):

- On the win workload with the index adopted, rung-50 lag-rejected windows reach **0**, OR the
  residual is proven device-bound with the device-latency distribution and the exact remaining
  count.
- Throughput and write amplification do not regress.
- Durability ordering unchanged: a request is acked only after a covering `fdatasync`; newest-first
  reads; disjoint tile ranges; no change to committed data.
- Tests: add tests that fail against the pre-change behaviour where meaningful (including that an
  ack still requires a covering `fdatasync`); keep all Rust and Python tests green.

Judging: the author's ADOPTED reading is **§7.1 lag-only primary** (report both that and
both-ceilings). Do not edit the pre-registration.

## Diagnosis (checkpoint, committed before any implementation)

### The named cause, with per-group evidence

The blocking syscall is the **synchronous WAL `write_all` (append) on the single pacer**, not the
`fdatasync` itself and not the compaction worker. The `fdatasync` is already off the pacer (WP-E,
ticket 56); what remains on the pacer is the append that feeds the NEXT group.

The evidence is the WP-G residual (ticket 58), measured on the 12.25x cold51 instance with the
`group_phase` per-group instrumentation:

> a 101 ms WAL `fdatasync` (on the barrier worker) stalls the device, the next group's WAL
> **append** then blocks the pacer for 73 ms (`group_phase window=28 seq=4 append_us=73676`), and
> the following group dispatches 54 ms late (`seq=5 due_dispatch_us=54491`).

The same cascade is confirmed as the residual mechanism in ticket 58's before/after and WP-N
(ticket 65) reproduced it as the frozen `static-derived` win arm's 3-4/40 rung-50 lag breaches
(seed 42/43/44/42-repeat = 4/3/3/3). It is present with compaction and WAL maintenance fully off
(ticket 55's `writes-only` runs), so it is the device's fsync/write tail under load, not the
compaction worker's tile I/O.

### Code path (this worktree, integrated master `04b27b2`)

- `engine/src/engine.rs::commit_block_inner` (barrier=false branch) calls
  `self.wal.append(&payload)` synchronously on the pacer thread, then applies the memtable:
  `wal.rs::Wal::append` -> `self.file.write_all(&frame)` (a device write on the pacer).
- `Engine::submit_group` only moves the `fdatasync` off the pacer (slice 56); the append still runs
  inline inside `execute_request` -> `commit_block_grouped` -> `commit_block_inner`, before
  `submit_requests_grouped` returns.
- The barrier worker (`BarrierWorker`, `WalSyncJob`) holds a cloned WAL fd and runs `sync_data`.
  When one group's `fdatasync` tails (101 ms in the ticket-58 example), the pacer is not blocked on
  the sync, but its next group's `write_all` queues behind the same device command and blocks ~74 ms.
- `wal_valid_len()` advances inline in `Wal::append`, so it currently reflects appended bytes on the
  pacer; every consumer of it (`maybe_checkpoint`/`rotate_wal` thresholds, `compaction_job` capture)
  already runs only after `ensure_no_pending_barrier` (all barriers drained), so deferred appends
  remain correct there (see invariants).

### Why this is the last blocker, quantified

WP-N (ticket 65) win rung-50 lag-rejected windows: `declared` 40/40 (full `P|StockMovement` scan on
the pacer), `learned@1.0` 25-29/40 (36-window index burn-in churn, fixed by WP-O), `static-derived`
**3-4/40** (index on from window 0, no churn — the residual is exactly this write stall + the
per-group device fsync tail). With WP-O removing the churn and this package removing the append
stall, the remaining question is only whether the device's own fsync/write tail leaves a non-zero
lag-rejected count (which is then proven device-bound, per the OR branch).

## Design (chosen): move the WAL append onto the barrier worker (append-then-sync jobs)

The pacer stops calling `Wal::append` in the grouped path. Instead it **buffers** each group's WAL
frame(s) (cheap: crc32c + a small memcpy) and the single FIFO worker does **append, then
`fdatasync`** for the whole group. The pacer enqueues and returns; the memtable apply stays inline
(so within-group guard visibility is unchanged); the ack is unchanged (the driver drains the
worker's `BarrierResult` in FIFO order and only then records completions).

- `Wal::encode_frame(payload) -> Vec<u8>`: the pure frame builder (bounds + poison check + header +
  crc + payload), shared by the synchronous `Wal::append` and the grouped buffering path.
- `Engine::group_frames: Vec<u8>`: accumulated frames for the open group, cleared by
  `begin_group`/`end_group`/`submit_group`, left intact on the in-flight-cap failure (the caller
  drains and retries the SAME group).
- `commit_block_inner` (barrier=false) buffers `self.group_frames.extend_from_slice(&frame)` instead
  of `self.wal.append(&payload)`; the synchronous single-block path (barrier=true) is untouched.
- `WalSyncJob` becomes an append+sync job (`frames: Vec<u8>` + cloned fd); the worker
  `write_all`s the frames, then `sync_data`s, and reports `append_us`, `fsync_us`, `appended_bytes`
  and `completed_at`. A failed append reports `Err` without syncing; the injected test fault still
  appends first and then fails the sync (so a complete-but-unacknowledged frame is recoverable,
  exactly the synchronous path's prefix semantics).
- `observe_barrier_result` (success) calls `Wal::record_appended(appended_bytes)` so `valid_len`
  tracks actual appended bytes; a failure poisons the WAL exactly as today.
- `BarrierCompletion` gains `append_us`; the driver's `finalize_one_pending_group` reports the
  WORKER's append time (the device-bound write) in `group_phase`, so a remaining stall is still
  attributed to `append_us` vs `fsync_us`.

### Rejected alternatives

- **(a) Ack before the covering `fdatasync` (true async group commit).** Rejected: weakens
  durability; the per-group `fdatasync` is the durability point and must return before ack.
- **(b) Defer the memtable apply until the worker's append completes.** Rejected: the pacer would
  block waiting for an append-ack on exactly the slow-device path this package removes, and the
  memtable is already allowed to be visible before durable (slice 56). Apply stays inline.
- **(c) A separate reader/executor thread or io_uring append.** Rejected as disproportionate: the
  append is a single framed `write_all`; a second channel per group buys nothing over folding the
  append into the existing FIFO barrier worker.
- **(d) `ioprio` idle on the compaction worker.** Rejected (no-op on this box's NVMe `none`
  scheduler; ticket 55) and not the cause (the stall reproduces with compaction off).
- **(e) Batch more groups per barrier / commit delay.** Rejected (ticket 54): at rung 50 writes are
  ~20 ms apart, a delay catches nothing and changes the measured group-commit protocol.

## Invariants argument (state how each is preserved)

1. **Ack only after a covering `fdatasync`.** Unchanged: the worker appends the group's frames and
   `fdatasync`s them, and the driver records completions only in `finalize_one_pending_group` after
   `wait_group_barrier`/`try_take_group_barrier` returns the worker's `Ok(BarrierResult)`. A sync
   failure returns `Err`, acks nothing, and poisons the WAL.
2. **Single-writer commit semantics.** The pacer remains the only thread that mutates `Memtable`,
   `csn`, `dedup`, `max_handle` and the metrics; the worker only performs the device append/sync on
   a cloned fd and never touches engine state. The FIFO worker preserves append/barrier order, and
   the driver drains oldest-first.
3. **Newest-first reads / memtable-over-tile.** Unchanged: reads run only on the pacer against the
   in-order memtable; the memtable apply stays inline, so a later request's guard sees earlier
   effects exactly as before. The deferred append does not feed reads.
4. **Disjoint tile ranges / no committed-data change.** Unchanged: the change touches only the WAL
   ingest framing and timing; tile/compaction/manifest logic and the on-disk WAL frame bytes are
   byte-identical (same `encode_frame`).
5. **`valid_len` correctness.** Advanced only when a barrier's append is observed successful
   (`record_appended`); every `valid_len` consumer already requires `ensure_no_pending_barrier`
   (drained), so it never reads an in-flight group's bytes. On append/sync failure the engine
   poisons and reopen recomputes `valid_len` from the file.
6. **Bounded memory.** `group_frames` is one group's frames (≤ group size), and
   `MAX_PENDING_BARRIERS = 64` still bounds the in-flight jobs/bytes.

## Change

Implemented in two commits (`98ba12e` write-stall, `16303ef` read-path follow-on). Files:

1. `engine/src/wal.rs`: `Wal::encode_frame(payload)` builds the framed bytes without
   writing (bounds + poison checks identical to `append`); `Wal::append` now calls it; new
   `Wal::record_appended(len)` advances `valid_len` when a worker append is observed successful;
   `sync_handle` doc updated to the append+barrier role.
2. `engine/src/engine.rs`:
   - `Engine.group_frames: Vec<u8>` (open group's buffered frames) and
     `Engine.group_defer_append: bool` (set by the new `Engine::begin_group_deferred`).
   - `begin_group` keeps the historical synchronous mode; `begin_group_deferred` opens the
     overlapped-append mode; `commit_block_inner` (barrier=false) buffers the frame in deferred
     mode and appends inline in synchronous mode.
   - `end_group` fails closed on a deferred-append group (never silently drops buffered frames).
   - `WalSyncJob` -> `WalAppendSyncJob { handle, frames, fail }`: the worker `write_all`s the
     group's frames then `sync_data`s; `run()` returns `(result, append_us, fsync_us,
     appended_bytes)`.
   - `BarrierResult`/`BarrierCompletion` carry `append_us` (worker-measured device write) and
     `appended_bytes`; `observe_barrier_result` advances `valid_len` via `record_appended` on
     success and poisons on failure exactly as before.
3. `engine/src/replay.rs`: `submit_requests_grouped` uses `begin_group_deferred`; doc updated.
4. `engine/src/bin/flashdb-load.rs`: `PendingGroup` drops its submit-time `append_us`; the
   `group_phase`/`window_stall` report's `append_us` is now `completion.append_us` (the worker's
   device-bound write), so a remaining stall is still attributed to append vs fsync.

### Read-path follow-on (supervisor-approved scope extension, commit `16303ef`)

The write-stall fix exposed the last rung-50 breaches as COLD INDEX-TILE page-ins on the pacer
(seed 42 static-derived windows 31/37: a 60-67 ms read -> 41-47 ms next-group lag, while a
concurrent 129 ms `fdatasync` dispatched with only 70 us lag). The WP-M `order_no` index sidecar
was absent from the read-ahead worker's warm view, so its point probe was never warmed.

- `engine/src/engine.rs::tile_read_view()` now appends the adopted `order_no_index` tile to the
  warm view; the index tile uses the same `U` key spelling, so `TileReadView::warm_lookup` covers
  it with no special-casing.
- `engine/src/index.rs`: `OrderNoIndex::tile()` shares the `Arc<Tile>`.
- `engine/src/request.rs::request_read_lookups`: a movement scan now emits the `order_no` index
  probe (`entity=StockMovement, field=order_no`) so the read-ahead worker warms it.

## Tests

- `engine/tests/group_commit.rs::deferred_append_buffers_on_the_pacer_and_lands_on_the_worker` —
  NEW, fails pre-change: after `commit_block_grouped` the WAL length and record count are still 0
  (append deferred off the pacer) while `csn()` is 1; after the barrier the frame is durable and
  recoverable.
- `engine/src/engine.rs::end_group_refuses_a_deferred_append_group` — NEW: `end_group` fails closed
  on a deferred-append group instead of dropping buffered frames.
- `engine/src/engine.rs::overlapped_barrier_sync_error_fails_closed_and_poisons_the_wal` —
  strengthened: with the injected sync fault the worker appends the complete frame and then fails
  the sync, so recovery replays handle 2 (`doc2`) while the group is never acked — the
  append-happens-but-no-ack-without-fdatasync invariant.
- `engine/src/readahead.rs::tile_read_view_includes_the_adopted_order_no_index` — NEW, fails
  pre-change: the warm view resolves the `order_no` index probe only AFTER the index is adopted
  (`Some(1)` vs `None`).
- `engine/src/request.rs::request_read_lookups_extracts_the_probes_each_shape_pays` — updated: a
  movement scan now yields the `order_no` index probe (was empty).
- Existing overlapped tests switched to `begin_group_deferred` so they exercise the new deferred
  append path; the synchronous `end_group` path and its fault-injection test are unchanged.

## Gates (verbatim; run at final HEAD `16303ef`)

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings (no warning/error
   lines).
2. `cargo test --workspace --offline` — totals: **302 passed; 0 failed; 0 ignored**
   (128 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 12 group_commit + 6 manifest_engine + 7 merge_threshold + 15 metrics +
   33 multi_tile + 7 order_flow + 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q` — **138 passed in 4.01s**.
4. `git diff --check` — silent (exit 0).

## Measurement protocol

Release binary built once per binary state (`target/release/flashdb-load`, sha256 before
`096454c4…` / after `29222e0b…`), copied to `/tmp/wave14-writestall/`, run only from that copy,
one process at a time. Instance `cp -a` of `/tmp/cold51/per_region-mixed` (12.25x), harness
`cold_0b.py --workloads win --arms declared learned static-derived --lambdas 1.0`, full ladder
50/100/200/400, `--cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608
--compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000`, seeds 42/43 + seed-42 repeat.
Box load avg 3.4-8.3 on 12 cores (shared; the parallel wave14-churn2 lane was measuring
concurrently, so every timing number is noisy — WP-Q re-measures serially).

## Before/after tables

"Before" = write-stall fix only (commit `98ba12e`); "after" = write-stall fix + index-tile warm
(commit `16303ef`). Win workload, rung 50 (the acceptance rung):

| run | arm | before lag-rej | after lag-rej |
|---|---|---:|---:|
| seed 42 | declared | 39/40 | 39/40 |
| seed 42 | learned@1.0 | 26/40 | 26/40 |
| seed 42 | static-derived | 4/40 | **2/40** |
| seed 43 | static-derived | 2/40 | **2/40** |
| seed 42 repeat | static-derived | 2/40 | **2/40** |

(`learned` rung-50 stays 25-26/40 in BOTH states: that is the learner's own 36-window index
burn-in churn, fixed by the sibling WP-O lane, not the write path. `declared` stays ~39-40/40: the
full `P|StockMovement` scan on the pacer, not the write path.)

Per-rung achieved throughput (after, the only thing the objective's throughput axis scores):

| arm | r50 | r100 | r200 | r400 |
|---|---:|---:|---:|---:|
| declared | 50 (40/40 lag-rej) | 85-92 | 85-92 | 81-89 |
| learned@1.0 | 50 | 100 | 199-200 | 351-400 |
| static-derived | 50 | 100 | 200 | 354-400 |

Write amplification is unchanged by construction (the change only moves WAL frame writes between
threads; frame bytes and compaction bytes are identical) — the three after-runs' static-derived
`reorg_bytes` are 8.0-8.9 MB, matching the before runs and WP-N.

### The write-stall is removed (per-group proof, before state seed 42)

`group_phase window=37 seq=50 … fsync_us=129566 completion_us=133033 due_dispatch_us=70` — a
129.6 ms `fdatasync` on the worker, with the NEXT group dispatched only **70 us** late. Before
this package that same tail cascaded ~54-74 ms into the next group's lag (ticket 58). Across the
three after-runs, static-derived sees **12 groups with fsync >100 ms and 23 with fsync >50 ms, and
none of them produces a lag breach** (the rung-50 breaches are windows 0-1 only, below). The
pacer's `append_us` is now ~0-44 us (worker-measured); the device write is off the dispatch path.

### Residual (after): static-derived rung-50 = 2/40, both windows 0-1 (startup warm-up)

`lag_breach_attribution` for static-derived after, all three runs: the ONLY rung-50 breaches are
`window=0` (total 16-27, `read_us` 503-757 ms) and `window=1` (total 1-2, `read_us` 26-28 ms),
every one `reads=N boundary=0`. Windows 2-40 are clean. Window 0/1 is the read-ahead worker's
cold-start warm-up: the pacer dispatches the first window's requests before the 4 ms/request warm
has filled the lead. It is a read-path warm-up transient, NOT the durability floor (no fsync >50 ms
produces any lag breach) and NOT the mid-rung index-tile stall the follow-on removed.

Device fsync distribution (per-group `fsync_us`, static-derived, the 3 after-runs, n=20,893):
p50 0 / p90 9,644 us / p95 12,436 us / p99 18,649 us / max 314,493 us; 1,889 >10 ms, 23 >50 ms,
12 >100 ms. The 10 ms lag ceiling sits BELOW the device's own fsync p90-p99, yet after this
package no fsync tail becomes schedule lag (it lands in service latency, which is the open §7.1
service-vs-lag reconciliation, unchanged).

## Acceptance verdict

**Write-path acceptance: MET.** The single pacer no longer blocks on device write latency; a
covering `fdatasync` still gates every ack (tests); throughput and write amp are unchanged;
ordering/newest-first/disjoint-tiles unchanged. The named blocker is removed with per-group proof.

**Rung-50 lag-rej = 0: NOT reached — honest residual 2/40 (static-derived), and it is proven NOT
write-path and NOT device-durability-bound.** The two remaining windows are the read-ahead
worker's startup warm-up (windows 0-1), `reads=N boundary=0`; the device fsync tail (up to 314 ms)
produces zero lag breaches. Reaching 0 would need eager startup warming of the index+scan prefix
(a read-ahead tuning change, not a write-path change), or accepting the warm-up window as a
cold-start transient.

**Judging views:** §7.1 lag-only primary — static-derived rung-50 2/40 (was 2-4/40); NOT yet
lag-sustained, but the write-path blocker is gone and the remaining 2 are warm-up. Both-ceilings —
0 rps everywhere, unchanged (the service ceiling is the device fsync floor; the open §7.1
reconciliation, not resolved here and not edited).

## Residual items

- **Startup warm-up (2/40):** windows 0-1 of the read-ahead worker's cold-start. Candidate fix:
  eager warm of the movement scan prefix + `order_no` index before the first dispatch (or a longer
  initial lead), a read-ahead tuning change out of this write-path package.
- **Learner rung-50 churn (25-26/40):** the WP-O sibling lane's scope, not the write path.
- **Service ceiling:** still device-bound (fsync p90 9.6 ms / p99 18.6 ms vs a fixed 10 ms); the
  open §7.1 primary-vs-service reconciliation is untouched and stays flagged for the author.
- **Shared-box noise:** the parallel wave14-churn2 lane was measuring concurrently; all timings are
  pilot-grade and WP-Q must re-measure the integrated result serially.
