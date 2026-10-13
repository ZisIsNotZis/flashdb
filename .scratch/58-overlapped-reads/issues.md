# 58 - take request-path reads off the pacer (wave7 WP-G)

Contract: work package G of `.scratch/52-program/issues.md`. Move the request-path reads off the
pacer so a slow cold point read becomes per-request SERVICE latency (reported separately) rather
than schedule lag, while preserving every invariant: single-writer commit semantics, the arrivals
model, the durability protocol (ack only after a covering `fdatasync`), disjoint tile ranges, and
read-path correctness (newest-first get, memtable-over-tile precedence). Acceptance: durability
unchanged and invariants preserved; and rung 50 sustained by the lag ceiling (0 lag-rejected
windows) on cold44 AND c51 with no achieved-throughput or write-amp regression; OR, if unreachable,
the exact remaining per-breach cause with evidence.

## Diagnosis (checkpoint before implementation)

- Baseline (wave6 binary `17de3a3`, measured in `/tmp/cold57`, payload verbatim in ticket 57):
  cold44 rung 50 = **6/40** lag-rejected windows, lag p99 med/max **1664/235475 us**; c51 rung 50 =
  **4/40**, lag p99 med/max **2277/82565 us**. In every lag-rejected window the breach is
  `lag_breach_attribution reads=N boundary=0` (verified, e.g. window 18: one read-only group with
  `completion_us=255326` and the NEXT group dispatched `due_dispatch_us=235475`, then the pacer
  recovers). The pacer is single-threaded: a slow cold read blocks the next dispatch, converting
  read latency into schedule lag exactly as ticket 57's residual said.
- The lag p50 stays ~62 us in the breached windows, so the breaches are ISOLATED cold-read tails
  (~14-235 ms under device contention), not sustained saturation. The mean request-path read is
  ~2-3 ms (~4 dependent block reads), far below the 20 ms/request interval at rung 50.
- The service ceiling is breached in ~38-39/40 rung-50 windows by the durability barrier itself
  (pre-existing, tickets 54/55). That is the OPEN primary-vs-service reconciliation and is reported
  separately, unchanged.

## Design (chosen): read-ahead page-cache warming (a read worker)

The pacer STAYS the single writer and keeps doing the authoritative read inline (newest-first get,
memtable-over-tile precedence, exactly today's code). A dedicated background **read-ahead worker**
pre-reads the tile blocks for requests slightly ahead of the pacer's dispatched position, so by the
time the pacer serves a request its tile reads are page-cache hits (fast) and the cold page-in
happened off the pacer. Read latency therefore lands in the worker's headroom, not the pacer's
schedule lag; the pacer's own read remains correct and authoritative, so this cannot change read
semantics.

Mechanism:
- `TileHandle.tile` becomes `Arc<Tile>`; tiles are immutable after publish and `Tile` reads use
  positional `pread`, so a worker thread can share `Arc<Tile>` clones safely.
- New `readahead` module: `TileReadView` (an `Arc<Vec<Arc<Tile>>>` snapshot + key encoding) and a
  `ReadAhead` handle. The worker owns the trace lines and a shared `Arc<AtomicU64>` cursor; it keeps
  the next `READ_AHEAD_LEAD` (400) requests' tile blocks warm, parsing each trace line and running a
  TILE-ONLY version of the request's lookups (`unique_lookup` + the follow-on primary `get` for a
  found handle) purely to warm the cache, discarding results.
- The driver spawns the worker after open, updates the cursor whenever the dispatch index advances,
  and refreshes the tile snapshot at each window boundary (publish/compaction replace tiles).
- The worker's reads never mutate state and never feed a decision, so durability, ordering, disjoint
  ranges and read correctness are untouched by construction.

Why this works for the measured tail: at rung 50 the worker warms ~3.5 blocks/request (≈10% of a
second busy) while the pacer offers one request per 20 ms, so the worker stays far ahead; a single
100-235 ms worker read is absorbed by the ~8 s lead instead of becoming the next dispatch's lag.

### Rejected alternatives (recorded)

- **(a) reader thread pool / async reads returning decisions**: requires sharing the serving
  memtable (the guard for ~29% of order flows re-reads a previously decremented Stock) with the
  reader, but `Engine` is not `Sync` (`RefCell<Metrics>` plus the barrier worker's `mpsc::Receiver`),
  so a decision-returning reader needs a full read-state snapshot per group (an up-to-8 MB
  `BTreeMap` clone per request at rung 50) or an invasive lock/`Sync` refactor. Rejected as
  disproportionate and correctness-risky for this bounded package.
- **(b) single-threaded prefetch during the idle sleep**: the pacer still blocks on the prefetch, so
  a 100+ ms tail just moves the lag one request earlier; does not remove it. Rejected.
- **(c) io_uring / async file reads**: the tile read path is a synchronous `FileExt::read_at`
  cursor with CRC/directory logic; converting it to io_uring is a whole-read-path rewrite, not a
  bounded change. Rejected.
- **(d) two-stage split with the pacer handoff to a commit executor (lag measured at handoff)**:
  would make lag ≈ 0 by construction but re-owns the whole `Engine` on the executor thread and moves
  WAL maintenance, compaction, metrics snapshots and barrier drains behind a request/response
  protocol; a much larger driver rewrite with many more ordering/durability failure modes. Rejected
  in favour of the additive warming worker, which keeps the entire existing loop and read path.

## Change (planned)

1. `engine/src/engine.rs`: `TileHandle.tile` → `Arc<Tile>`; add `Engine::tile_read_view()`.
2. `engine/src/readahead.rs` (new): `TileReadView`, `ReadAhead`, the worker thread and the tile-only
   warm path.
3. `engine/src/lib.rs`: declare `mod readahead;`.
4. `engine/src/request.rs`: `request_read_lookups()` returns the unique lookups a request will
   probe, so the worker warms exactly the blocks the authoritative read touches.
5. `engine/src/bin/flashdb-load.rs`: spawn the worker, update its cursor on dispatch, refresh its
   tile snapshot at window boundaries.
6. Tests: lookups extraction + tile-read-view correctness (memtable-precedence interaction).

## Gates (verbatim, will run at the final HEAD)

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q`.
4. `git diff --check` — silent.

## Before/after table

(Filled after measurement.)

## Verdict

(Pending measurement.)
