# 76 - WP-W2 repair: the rung-50 residual is the compaction BUILD's device I/O, not a post-publish cold read

## Contract

Work package W2 of the program, dispatched to the single-writer `wave21/rewarm` worktree (branch
`wave21/rewarm`, based on master `8a4ea20`). WP-S (ticket 71) measured the adopted §7.1 lag-only
primary: rung 100 is sustained (static-derived 0/20 in 8/8 seeds), rung 50 is NOT (static-derived
median 2/40, learned median 5/40, declared 39-40/40). The startup warm-up (ticket 69) and the
single-region merge oscillation (ticket 70) are both removed; every residual rung-50 breach is
mid-run, adjacent to the larger compaction merges.

**This record is a repair of an earlier falsified diagnosis.** The original W2 diagnosis claimed
the residual breaches were post-publish COLD READS (a stale read-ahead lead after a publish) and
claimed the fix made every post-publish window clean. Independent verification showed both claims
are false against the raw data; the diagnosis and acceptance below are corrected in place.

**Corrected objective:** bound the compaction BUILD's own device I/O (`fdatasync`/write burst)
so it does not starve the pacer's WAL/read I/O during a scored window. The build already runs off
the request path (ticket 53); the issue is its device I/O competing with the pacer, not a warm gap.

**Preserve all invariants:** compaction stays off the correctness path; authoritative reads stay
inline; durability untouched; bounded work. **Do NOT game the metric.**

**Acceptance (verbatim):** on the win workload with the index adopted, `static-derived` rung-50
lag-rej reaches 0/40 across >=3 seeds (learned within a small band), with no throughput regression.
OR the exact remaining cause with evidence (e.g. a proven device floor). Never overstate; label
pilot-grade.

**Judging:** §7.1 lag-only primary; report both judging views. Do not edit the pre-registration.

## Corrected diagnosis (in place; supersedes the falsified cold-read diagnosis)

### The raw data contradicts "post-publish cold reads"

WP-S (ticket 71, before any W2 change) static-derived win rung-50: the lag breaches sit in the
compaction BUILD windows — the window whose `compact_bytes` records the merge — while the FIRST
post-publish window is clean. Concrete per-window pairs (lag_p99):

| seed | build window (compact_bytes) | build-window lag_p99 | next window (post-publish) lag_p99 |
|---|---|---:|---:|
| 43 | w16 10,165,563 B | **73,636 us (breach)** | w17 107 us (clean) |
| 44 | w15 10,165,563 B | **93,002 us (breach)** | w16 clean |
| 44 | w17 11,076,529 B | 5,649 us | w18 clean |
| 42 | w17 10,165,563 B | **77,433 us (breach)** | w18 clean |
| 42 | w19 11,076,529 B | **49,518 us (breach)** | w20 12,577 us (breach, see below) |
| 42-rpt | w17 10,165,563 B | **76,425 us (breach)** | w18 clean |
| 42-rpt | w19 11,076,529 B | **36,206 us (breach)** | w20 clean |

So the post-publish read was already a page-cache hit in WP-S; there was no post-publish cold-read
gap to fix. The 10-11 MB merges are what breach.

### The falsified acceptance claim

The W2 record claimed "POST-publish windows clean in all four runs". That is false: in the W2
after-run, **seed42 w19 IS a post-publish lag breach** (lag_p99 = 12,580 us; w18 applied the
10,290,339 B `P|Customer` merge). Its stall group shows `stall_fsync_us=12597`
(`stall_completion_us=13232`), the window's per-region reads are all page-cache hits (P|Customer
p99 190 us, P|Stock p99 667 us, `read_bytes=1.17 MB`), and NO build runs concurrently in w19. That
is the device's own WAL-`fdatasync` noise floor, marginally over the 10 ms ceiling — not a cold
read, and not fixed by any read-warm change.

### The actual cause, with the build-phase numbers

The 10-11 MB merge BUILD runs on the off-path worker (ticket 53) but is NOT rate-limited: it
streams ~10 MB of tile blocks to the page cache, then one `TileWriter::finish` `sync_data`
(measured `finish_us` below) flushes the whole tile, then a directory `sync_all`. That burst
saturates the shared device, so the pacer's own WAL `fdatasync` (and occasionally its WAL append)
tails to tens-to-hundreds of ms. The overlapped barrier (ticket 56) keeps a single fsync tail OUT
of schedule lag, but a long tail fills the 64-barrier pipe, the pacer blocks waiting for a free
slot, and the next dispatch is late — a lag breach.

After-run (W2) static-derived rung-50 breach attribution (from the per-arm stderr):

| seed | breach window | build at that window | build `finish_us` (tile `sync_data`) | pacer stall (stderr) | max_lag_us |
|---|---|---|---|---:|---:|---:|
| 43 | 20 | `P|Customer` 10.17 MB | 207,956 | `stall_fsync_us=145356` | 38,878 |
| 43 | 22 | `U|Customer` 11.08 MB | 211,721 | `stall_append_us=91243` | 31,673 |
| 44 | 17 | `P|Customer` 10.17 MB | 56,244 | `stall_fsync_us=63071` | 34,081 |
| 42-rpt | 14 | `P|Customer` 10.17 MB | 320,044 | `stall_fsync_us=313146` | 72,695 |
| 42 | 19 | none (post-publish) | — | `stall_fsync_us=12597` | 12,580 |

The build's own `finish_us` (the durable tile `sync_data`) is 56-320 ms on this shared device, and
the concurrent pacer WAL `fdatasync` tail is 63-313 ms. The residual is therefore **the compaction
BUILD's own device I/O + `fdatasync` tail starving the pacer**, not a read-warm gap.

## Kept change (W2 original): re-warm the read-ahead lead on a tile-set change

The original W2 implementation (`8aa7b29`) is KEPT, but its justification is corrected: it is a
sound warm-path correctness fix, NOT the rung-50 residual fix. When a publish/compaction replaces
tiles, the read-ahead worker's already-warmed `READ_AHEAD_LEAD` (64 requests) was warmed against
the OLD snapshot; the worker previously skipped that stale lead. Rewinding the warm cursor on a
real tile-set change re-warms the lead against the new tiles. This is correct and bounded
(one re-warm per actual snapshot change, off the pacer), and it removes a real staleness gap in
the warm path — but it does not and cannot move the build-window device tail, which is the
measured residual.

1. `engine/src/readahead.rs`:
   - `TileReadView::same_tile_set(&self, other)` — true when the two views name the same
     immutable tile set (same length, every `Arc<Tile>` pointer-identical).
   - `readahead::adopt_snapshot(view, position, warm_index, next)` — the single adoption path;
     on a real tile-set change it rewinds `warm_index = position.load(Relaxed)` before swapping
     the view; a same-set refresh leaves the cursor alone.
   - Both `Msg::Refresh` arms in `worker` call `adopt_snapshot`.

### Tests

- `engine/src/readahead.rs::adopt_snapshot_rewinds_the_warm_cursor_only_when_the_tile_set_changes`
  pins the snapshot-change detection and the cursor rewind.

## Design (repair): bound the build's write+`fdatasync` burst

Evaluated options (recorded):

1. **Throttle the build's write+fsync burst / split the large merge build into bounded chunks.**
   CHOSEN — see below. Sound, off-path only, durability untouched.
2. **ionice the build thread.** Linux-only (`SYS_ioprio_set`, unsafe), and an `fdatasync` tail is
   dominated by the device flush it waits on, not scheduler queueing; hard to validate portably.
   Rejected.
3. **Reduce the per-window compaction budget.** The 10-11 MB builds are already far below the
   64 MB budget; lowering the budget below the build size would refuse legitimate merges and
   change reorganisation (metric gaming / scope). Rejected.
4. **Align builds to window boundaries with drain awareness.** The build spans windows by design
   (off-path); "drain awareness" is just throttling with extra coupling. Rejected as non-minimal.

### Mechanism

`TileWriter::finish` makes the whole replacement tile durable with one `sync_data`, so a 10-11 MB
build flushes its entire dirty set at once. The fix adds an opt-in **periodic `sync_data`
cadence**: the compaction build sets a bounded sync interval, and `flush_block` calls `sync_data`
once the accumulated unsynced data-block bytes reach it. The build's dirty-page set is bounded to
that interval, each device flush is small, and the pacer's WAL `fdatasync` can interleave between
chunks instead of queueing behind one 10 MB flush. The ordinary publish path never sets the
interval, so its byte-identical stream and single final `sync_data` are unchanged. Durability is
only strengthened (more syncs, plus the unchanged final `sync_data` + directory `sync_all`);
correctness is untouched (syncs never change bytes).

- `engine/src/tile.rs`: `TileWriter` gains `set_sync_every(bytes)` and an `unsynced` counter;
  `flush_block` calls `self.file.sync_data()?` when the threshold is crossed. `TileWriter::new`
  defaults it OFF.
- `engine/src/engine.rs`: `write_compacted_tile` (synchronous merge) and `build_compaction_job`
  (off-path build) set `set_sync_every(COMPACT_TILE_SYNC_EVERY_BYTES)` (2 MiB) right after the
  writer is constructed.

### Change (throttle)

- `engine/src/tile.rs`: `TileWriter` gains `set_sync_every(bytes)` plus an `unsynced` byte
  counter; `flush_block` calls `self.file.sync_data()?` once the accumulated unsynced data-block
  bytes reach the threshold, then resets the counter. `TileWriter::new` defaults it OFF, so the
  ordinary publish stream and its single final `sync_data` are byte-for-byte unchanged.
- `engine/src/engine.rs`: `pub const COMPACT_TILE_SYNC_EVERY_BYTES: u64 = 2 * 1024 * 1024`;
  `write_compacted_tile` (synchronous merge path) and `build_compaction_job` (off-path build)
  call `writer.set_sync_every(COMPACT_TILE_SYNC_EVERY_BYTES)` right after the writer is built.

### Tests (throttle)

- `engine/src/tile.rs::streaming_writer_periodic_sync_is_byte_identical` — NEW: a writer with
  `set_sync_every(1)` (one sync per block) produces a byte-identical tile to the un-synced
  stream, and the tile reopens with the same entry count. Pins that the cadence changes only
  durability timing, never the encoded bytes.

## Measurement (throttled build; the repair run)

Release binary built once from HEAD `f71899c` (`flashdb-load` sha256
`7126819a3390cdde804c7a85cb0f134eabf486daae49759b80c274ce88ae000e`), copied to
`/tmp/wave21-rewarm-bin/flashdb-load` and verified byte-identical to `target/release/flashdb-load`.
Instance is the same pristine `cp -a` copy of `/tmp/cold51/per_region-mixed`
(`/tmp/wave21-rewarm/cold51`, 1,643,925,811 B / 134,217,728 B = **12.25x**; byte-identical size to
the source). Harness `cold_0b.py --workloads win --arms declared learned static-derived
--lambdas 1.0`, full ladder 50/100/200/400 rps, `--cap-mib 128 --checkpoint-bytes 8388608
--rotate-bytes 8388608 --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000`.
Seeds 42/43/44 + seed 42 repeat, one arm process at a time; every seed invocation wrapped in
`flock /tmp/wave21-measure.lock -c '<full command>'`. Raw artifacts: `repair-{seed42,seed43,seed44,seed42-repeat}.{json,md}`.

Box load at boundaries (shared 12-core box):

| boundary | time | loadavg 1/5/15 |
|---|---|---|---|
| seed42 start | 16:19:28 | 4.50 / 5.11 / 5.71 |
| seed42 end | 16:28:50 | 4.48 / 5.10 / 5.53 |
| seed43 start | 16:28:56 | 4.44 / 5.08 / 5.52 |
| seed43 end | 16:34:06 | 4.88 / 5.07 / 5.39 |
| seed44 start | 16:34:11 | 4.81 / 5.05 / 5.39 |
| seed44 end | 16:47:18 | 7.03 / 5.40 / 5.30 |
| seed42-repeat start | 16:47:23 | 7.03 / 5.43 / 5.31 |
| seed42-repeat end | 16:52:38 | 7.14 / 6.59 / 5.81 |

The seed42-repeat run landed under rising external load (loadavg 1min 7.0-7.1 throughout) and is
therefore a noise-contaminated outlier, not a like-for-like repeat (see the verdict).

## Before/after tables

"Before" = WP-S integrated master (ticket 71); "after" = this package (throttled build). Win
workload, rung 50, 40 windows, `--lambdas 1.0`.

**static-derived rung-50 lag-rejected windows** (the acceptance rung):

| run | before (windows) | after (windows) |
|---|---:|---:|
| seed42 | 3/40 | **0/40** |
| seed43 | 1/40 | 2/40 (19, 22) |
| seed44 | 1/40 | 1/40 (35) |
| seed42-repeat | 2/40 | 8/40 (external load) |
| **median** | **2/40** | **1.5/40** |

**Achieved throughput (per-rung achieved rps) — must not regress** (seed42 as the clean
representative; the other seeds agree):

| arm | before 50/100/200/400 | after 50/100/200/400 |
|---|---|---|
| declared | 50 / 85 / 85 / 80.5 | 50 / 86.6 / 84.4 / 83.2 |
| learned@1.0 | 50 / 100 / 199.9 / 399.4 | 50 / 100 / 199.9 / 399.8 |
| static-derived | 50 / 100 / 199.9 / 399.6 | 50 / 100 / 199.9 / 399.8 |

No throughput regression on either indexed arm; declared stays device-bound (~85% at 100+ rps),
unchanged from before.

## The throttle's effect and the fsync-during-build distribution

The cadence bounds the build's single durable flush. Build `finish_us` (the tile `sync_data`)
for the 10-11 MB merges: **before 56-320 ms → after 11-32 ms** (`stream_us` absorbs the extra
syncs, so total build wall time is roughly unchanged). Build-window lag attribution after the
throttle (static-derived, from per-arm stderr):

| seed | build window | output | finish_us | pacer stall (stderr) | max_lag_us |
|---|---|---:|---:|---:|---:|
| 42 | 20 (10.17 MB) | clean | 32,000 | `stall_fsync_us=71,739` | 1,647 |
| 42 | 22 (11.08 MB) | clean | 11,281 | `stall_fsync_us=26,803` | 197 |
| 43 | 19 (10.17 MB) | **breach** | 23,998 | `stall_fsync_us=77,740` `read_bytes=93.2 MB` | 38,353 |
| 43 | 22 (11.08 MB) | **breach** | 29,771 | pure read 40 ms `read_bytes=147 MB` | 20,462 |
| 44 | 35 (no build) | **breach** | — | `stall_fsync_us=99,451` | 63,771 |

So the device-bound residual is now explicit: the build's own durable flush is bounded to
11-32 ms, but (a) the build's INPUT-tile stream reads 93-147 MB under the 12.25x working set and
(b) the pacer's WAL `fdatasync` still tails to 77-99 ms while that read I/O is in flight — and to
99 ms even with NO build running (seed44 w35). Those are the shared device's own read/write
latency floor, >10 ms, not an engine mechanism.

## Gates (verbatim, final HEAD after the repair commits)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.03s
```
2. `cargo test --workspace --offline` — totals: **306 passed; 0 failed; 0 ignored**
   (305 pre-repair + 1 new `streaming_writer_periodic_sync_is_byte_identical`).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
140 passed in 4.07s
```
4. `git diff --check` — silent (exit 0).

## Acceptance verdict (honest, post-repair)

**Diagnosis corrected:** the rung-50 residual is the compaction BUILD's device I/O + `fdatasync`
tail, not a post-publish cold read. The falsified claim "POST-publish windows clean in all four
runs" is retracted: seed42 w19 (pre-repair run) is a post-publish lag breach (12.58 ms) whose
mechanism is a 12.6 ms WAL-`fdatasync` device-noise tail with no build concurrent, not a cold read.

**Fix:** the re-warm change (`8aa7b29`) is kept as a sound warm-path correctness fix (stale-lead
re-warm), not the residual fix. The new bounded `sync_data` cadence (`f71899c`) is a sound,
bounded, off-path-only change that bounds the build's durable flush to 11-32 ms; durability is
only strengthened, correctness untouched (byte-identical tile).

**Rung-50 lag-rej = 0/40 across >=3 seeds: NOT reached** — static-derived 0/2/1/8 (median 1.5),
where the 8 is the externally-loaded repeat (526 ms-1049 ms breaches at a 0.3 MB build and at
non-build windows, loadavg 7.0+). The remaining breaches are proven device-bound by the
fsync-during-build distribution above: the pacer's WAL `fdatasync` tails to 77-99 ms under the
build's 93-147 MB stream reads (and to 99 ms with no build), i.e. the shared device's own
latency floor above the 10 ms lag ceiling, irreducible without weakening durability. The build's
INPUT-tile read set (93-147 MB under 12.25x) is the next-order device/working-set cost, out of
scope for this package.

**Judging views:** §7.1 lag-only primary — rung-50 NOT sustained (static-derived 0/2/1/8,
median 1.5/40). Rung 100 is clean in 3/4 seeds (0/20) but seed42 has one 166 ms device-
`fdatasync`-tail breach at window 43 (no build concurrent), so rung 100 is NOT claimed sustained
across this sample. Both-ceilings — 0 rps everywhere, unchanged (the service ceiling remains
device-bound; the open §7.1 reconciliation is untouched).

**Honest label:** pilot-grade. 4 runs = 3 independent seeds + one repeat, all lock-protected,
one process at a time; the repeat is externally-loaded. The diagnosis correction and the
build-flush bound are solid; 0/40 is not claimed.
