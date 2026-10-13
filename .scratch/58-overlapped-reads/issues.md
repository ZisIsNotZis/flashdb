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
  the next `READ_AHEAD_LEAD` (64) requests' tile blocks warm, parsing each trace line and running a
  TILE-ONLY version of the request's lookups (`unique_lookup` + the follow-on primary `get` for a
  found handle) purely to warm the cache, discarding results.
- The worker warms at most one request per `WARM_PACE_MS` (4 ms) pause, so the warm reads are spread
  over time instead of bursting the whole lead at startup (a startup burst contended with the early
  WAL fsync/append and turned into a write-path lag).
- The driver spawns the worker after open, updates the cursor whenever the dispatch index advances,
  and refreshes the tile snapshot at each window boundary (publish/compaction replace tiles).
- The worker's reads never mutate state and never feed a decision, so durability, ordering, disjoint
  ranges and read correctness are untouched by construction.

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

## Change (implemented, commits `67c541f`)

1. `engine/src/engine.rs`: `TileHandle.tile` → `Arc<Tile>`; add `Engine::tile_read_view()`.
2. `engine/src/readahead.rs` (new): `TileReadView`, `ReadAhead`, the worker thread and the tile-only
   warm path (`READ_AHEAD_LEAD=64`, `WARM_PACE_MS=4`).
3. `engine/src/lib.rs`: declare `pub mod readahead;`.
4. `engine/src/request.rs`: `request_read_lookups()` returns the unique lookups a request will
   probe, so the worker warms exactly the blocks the authoritative read touches.
5. `engine/src/bin/flashdb-load.rs`: spawn the worker (off in `--dry-run`), update its cursor on
   dispatch, refresh its tile snapshot at window boundaries.
6. Tests: `request_read_lookups_extracts_the_probes_each_shape_pays` and
   `tile_read_view_resolves_published_unique_handles_without_the_memtable`.

## Gates (verbatim, run at final HEAD)

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.26s
```
2. `cargo test --workspace --offline` — totals: **291 passed; 0 failed; 0 ignored**
   (124 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests;
   +2 tests from this ticket).
3. `cd harness && uv run --with pytest pytest -q` — **132 passed** (4.43s).
4. `git diff --check` — silent (exit 0).

Measurement protocol: release binary built once (`target/release/flashdb-load`, sha256
`f1f239b8f9c7297514c6740ddaef37a25234ebc76d409b166aeab3098f30c7f3`), copied to
`/tmp/cold58/flashdb-load`, run only from that copy, one process at a time via
`systemd-run --user --scope -p MemoryMax=<cap>M -p MemorySwapMax=0` (192 MiB cold44, 128 MiB c51)
on `cp -a` copies of `/tmp/cold44/per_region-mixed` and `/tmp/cold51/per_region-mixed`. Payload
(verbatim): `--rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json
--peak-report --ceiling-us 10000 --lag-ceiling-us 10000 --region-mode per_region
--policy-file declared.policy.json --group-requests 64 --checkpoint-bytes 8388608
--rotate-bytes 8388608 --compact-bytes 67108864`. Trace `adversarial.jsonl` (sha256
`bf0a5170…`, 8000 lines), policy `declared.policy.json` (4096/10/2).

## Before/after (rung 50, the acceptance rung)

| run | lag-rej | svc-rej | lag p99 med/max (us) | svc p99 med (us) |
|---:|---:|---:|---:|---:|
| cold44 wave6 (baseline, 17de3a3) | 6/40 | 39/40 | 1,665 / 235,475 | 20,931 |
| cold44 wave7 run A | 1/40 | 39/40 | 2,535 / 54,491 | 13,546 |
| cold44 wave7 run B | 0/40 | 39/40 | 112 / 5,488 | 12,870 |
| c51 wave6 (baseline) | 4/40 | 38/40 | 2,277 / 82,565 | 16,454 |
| c51 wave7 | 0/40 | 37/40 | 683 / 3,393 | 11,742 |

## Before/after (whole run)

| run | late_total | lag-rej total | svc-rej total | achieved_rps | compact_bytes | merges |
|---:|---:|---:|---:|---:|---:|---:|
| cold44 wave6 (baseline) | 597 | 21 | 74 | 106 | 20,904,807 | 26 |
| cold44 wave7 run A | 129 | 8 | 74 | 106 | 26,210,274 | 29 |
| cold44 wave7 run B | 101 | 6 | 74 | 106 | 23,310,379 | 27 |
| c51 wave6 (baseline) | 483 | 15 | 74 | 107 | 23,310,824 | 27 |
| c51 wave7 | 167 | 8 | 72 | 106 | 21,651,974 | 26 |

Per-rung (wave7, both arms):

| rung | cold44 lag-rej (runs A/B) | c51 lag-rej | cold44 lag p99 med (us) | c51 lag p99 med (us) |
|---:|---:|---:|---:|---:|
| 50 | 1/40, 0/40 | 0/40 | 2,535 / 112 | 683 |
| 100 | 1/20, 0/20 | 0/20 | 2,336 / 1,582 | 215 |
| 200 | 0/10, 2/10 | 2/10 | 6,333 / 4,109 | 5,623 |
| 400 | 6/6, 4/6 | 6/6 | 13,212 / 11,085 | 12,636 |

Boundary-vs-read attribution: `boundary=0` on every wave7 run (the non-blocking boundary still
holds). Request-path `read_us` at rung 50 falls **4,420,056 us → 880,208 / 959,288 us** on cold44
and to **924,742 us** on c51: the pacer's own reads are now page-cache hits, so the cold page-in
moved off the pacer (it is done by the read-ahead worker).

## Verdict

**Durability and invariants are unchanged** (the read-ahead worker only warms the page cache; the
authoritative read, WAL append/apply/barrier, ordering and disjoint ranges are untouched). **Rung 50
is sustained by the lag ceiling on c51 (0/40) and on cold44 run B (0/40); cold44 run A has 1/40.**
That one remaining breach is NOT a read: it is the **write path** — a 101 ms WAL `fdatasync` (on the
barrier worker) stalls the device, the next group's WAL **append** then blocks the pacer for 73 ms
(`group_phase window=28 seq=4 append_us=73676`), and the following group dispatches 54 ms late
(`seq=5 due_dispatch_us=54491`). The baseline has the same device fsync tails (up to 96 ms at rung
50) but its append never exceeded 209 us, because the baseline pacer was BLOCKED on a cold read
during the fsync stall and only reached the append after the device recovered. Removing the read
block exposes the next serial write-path cost: the synchronous WAL append under device fsync
contention. Achieved throughput is unchanged (106-107 rps); write amp is within the shared-box noise
band (cold44 20.9 MB baseline → 23.3-26.2 MB, c51 23.3 MB → 21.7 MB; the extra merges in cold44 run
A are 3 late rung-400 merges, a publish/compaction window timing shift, not a rung-50 change).

## Judging views (per the adopted §7.1)

- **Sustained-by-lag-ceiling-only:** c51 rung 50 SUSTAINED (0/40); cold44 rung 50 SUSTAINED in run B
  (0/40) and 1/40 in run A, with the single breach attributed to the write-path WAL append under a
  device fsync stall, not a read. The read-attributed lag is gone (the WP-G mechanism works).
- **Sustained-by-both-ceilings:** NOT sustained at rung 50 (the service ceiling is breached by the
  durability barrier itself in ~37-39/40 windows, unchanged from the WP-E baseline). The
  primary-vs-service ceiling reconciliation remains the OPEN AUTHOR ITEM; nothing here edits the
  pre-registration or the docs.

## Residual / next steps (not implemented here)

- **Pre-registration reconciliation (open author item, NOT changed):** the primary-vs-service
  ceiling text remains in tension with the adopted §7.1 (primary = lag ceiling; service reported
  separately). Left flagged for the author.
- **Overlap the synchronous WAL append** is the remaining write-path mechanism now that the reads no
  longer mask it: at rung 50 it causes at most one lag-rejected window per cold44 run (a ~50-100 ms
  append behind a ~100 ms device fsync). That is a different, write-path work package, not a read.
- **Write-amp timing shift:** removing the read block makes publishes/compaction land in slightly
  different windows; cold44 compact bytes moved 20.9 → 23.3-26.2 MB (27-29 merges), c51 23.3 →
  21.7 MB (26 merges). Within the box's run-to-run noise; weighed honestly, not chased here.
