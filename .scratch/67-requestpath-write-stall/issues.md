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

Implemented at (commit to be recorded). Files:

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
- Existing overlapped tests switched to `begin_group_deferred` so they exercise the new deferred
  append path; the synchronous `end_group` path and its fault-injection test are unchanged.

## Gates (verbatim; run at the implementation commit)

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings (no warning/error
   lines).
2. `cargo test --workspace --offline` — totals: **301 passed; 0 failed; 0 ignored** (+2 tests).
3. `cd harness && uv run --with pytest pytest -q` — **138 passed in ~4.5s**.
4. `git diff --check` — silent (exit 0).

## Before/after tables

(Filled after measurement.)

## Acceptance verdict

(Filled after measurement.)

## Residual items

(Filled after measurement.)
