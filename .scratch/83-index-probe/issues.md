# 83 - why the movement scan is slow: the derived index is built from tiles only, and this workload's movements are in the memtable

Diagnosis only. No engine change yet. Nothing here changes the pre-registration.

## Trigger

The M1 controlled verdict (ticket 82) reports flashdb 3/5 sustained at 200 rps while sqlite and
postgres are 5/5, and the report never explains what flashdb is doing during the rejected windows.
The author asked for a root cause rather than a reading.

## What the recorded data already shows (no new runs)

- `runs/b2/rung-200/run-flashdb-declared.json`: the failing window has `service_p99_us = 569,724`
  (one group), `lag_p99_us = 30,430`, `max_group_size = 8`, `mean_group_size = 1.01`, and no
  compaction bytes; its region delta is dominated by `P|StockMovement`, `bytes_read = 5,750,442` over
  `count = 9` reads, `mean_ns = 5,902,888` (5.9 ms/read).
- The same shape repeats in `runs/b3/rung-200/run-flashdb-declared.json` (570 ms service, 5.75 MB /
  9 reads). Blocks 1, 4, 5 have no rejected window but the same per-window scan cost.
- The trace mix at every rung (`runs/b*/rung-200/trace.jsonl`, 4000 requests): 2785 order-flow
  writes, 610 `get Stock`, 417 `find Customer`, **188 `find StockMovement where order_no`**. So 4.7%
  of requests scan the movement region, at ~6 ms each.

## What the baselines do with the same 4.7%

`harness/flashdb_harness/baseline_0b.py:431` creates
`CREATE INDEX stock_movement_order ON stock_movement (order_no, movement_no)`, which is exactly the
scan predicate, so sqlite and postgres answer it with an index range scan. flashdb's catalog indexes
`StockMovement` by `movement_no` (unique) and `stock` (reverse) only; `order_no` is served by the
derived secondary path (`engine/src/index.rs`, WP-M).

## The defect

`build_order_no_index` iterates `TileReadView::scan_primary`, which reads **tiles only**
(`engine/src/readahead.rs:75-97`). On the seeded instance the movement region has **no tiles at all**:
`active_tile_regions()` on `runs/snapshot/flashdb-declared` is `{P|Customer: 3, P|Stock: 153}` and the
2785 movements are still in the serving memtable (`serving_memtable_entries = 19,495` after serving).

Measured on the restored snapshot with the policy that requests the index
(`{"regions":{"P|StockMovement":{"index_order_no":true}}}`):

| probe | result |
|---|---|
| `Engine::scan_primary(b"StockMovement")` (merges memtable + tiles) | **2785 rows** |
| `build_order_no_index(...)` (tiles only) | **0 entries**, 4125-byte tile, `coverage_csn = 0` |
| `startup_warm` on the same open | `scan_entries=0 index_entries=0` |

So the eager build at `engine/src/bin/flashdb-load.rs:1256` writes an **empty sidecar** and the read
path silently falls back to the 5.6 MB full scan on every scan request. No error, no log line: the
feature is dead exactly when the region is append-heavy and still in the memtable, which is the case
this workload presents.

The same tiles-only view explains `startup_warm scan_entries=0`: the WP-R warm-up cannot warm a region
that has no tiles either.

## The 200 rps mechanism this produces

The pacer dispatches a group, then executes it inline; a scan group costs ~6 ms of a single-threaded
dispatch path. At 200 rps a group lands in 1.01 requests, so a scan group blocks the next dispatch by
~6 ms, and when that lands across a window boundary the next window's lag p99 breaches the 10 ms
ceiling. Two such alignments out of 20 windows per block is what "3/5 sustained" is.

## A/B run under foreign load (invalid, recorded for honesty)

**Each of the twelve files in `runs/` now says so in its own first line** (added 2026-10-21 by
F-M1: a `# INVALID - do not cite:` marker line; the recorded data below it is untouched, and a
consumer must skip line 1 - `tail -n +2`).

`probe.sh` ran 3 paired 200 rps blocks on the frozen snapshot: declared vs the same engine with the
index policy. The box was carrying the author's other project (Remotion/Chromium renders): loadavg
1.9 -> 9.9 during the probe. Results were inconsistent (pair 1: 14 vs 2 rejected windows; pair 3: 15
vs 17), so **no conclusion is drawn from them**. They also could not have shown the index's effect
for the reason above: the index was empty in the indexed arm too.

## What a fix has to do

1. Build the derived index from the **merged** view (memtable + tiles), i.e. from the same source
   `Engine::scan_primary` uses, and record the covered CSN.
2. Rebuild/adopt off-path when the memtable publishes, so coverage does not silently go stale.
3. Make an empty build loud: `entries == 0` with rows present is a defect, not a normal result.
4. Then re-measure: 200 rps, declared vs indexed, on a quiet box, with `--peak-report` and stderr
   captured so `lag_breach_attribution` names the phase.

## Limits of this diagnosis

The per-scan cost is measured from the recorded region telemetry (9 scans, 5.75 MB, 5.9 ms mean), not
from a per-request trace inside the engine; the phase attribution that would confirm "the scan is the
stall" is available at run time (`lag_breach_attribution reads=N read_us=...`) but ticket 82 did not
keep the arm stderr.

## Work packages (M3, advance phase, author authorised "auto advance" 2026-10-18)

Budget for the milestone: 4 worker dispatches, 2 review cycles per package, 3 no-progress attempts
per package, fresh context for every child. One writer per branch/worktree; the main agent integrates.

| id | owner | scope | acceptance | depends |
|---|---|---|---|---|
| WP-1 | worker (fresh) | `engine/src/index.rs`, `engine/src/readahead.rs` (warm path), `engine/src/engine.rs` (build entry), `engine/src/bin/flashdb-load.rs` (eager build + adopt), its tests | the index covers a region whose rows are all in the memtable (one entry per row, `coverage_csn` = max covered csn); an empty build while `scan_primary` yields rows is a hard error naming both counts; the same for the startup warn's `scan_entries`; `cargo test --workspace` green with the project's pinned aliyun `--config` flags; a policy-off run is unchanged | none |
| WP-2 | worker (fresh) | `.scratch/83-index-probe/` measurement only, no engine edits | on a quiet box (loadavg < 2 sustained before each arm-run), 5 paired 200 rps blocks with arm rotation, snapshot restore verified, stderr kept; reports paired per-block sustainment of declared vs indexed plus `lag_breach_attribution` for every rejected window; verdict is "indexed sustains >= declared" or the named phase still responsible | WP-1 |
| WP-3 | worker (fresh) | `docs/` + `.scratch/83-index-probe/report.md` | the KPI ladder (efficiency > sustained throughput > tail latency) stated with its instrument and error band; the external comparison recomputed from the M1 records plus WP-2 and resolved to better / not-better per KPI; every number carries its condition | WP-2 |
| WP-4 | reviewer (fresh) | read-only review of WP-1..WP-3 at the integrated revision | package verdicts with defect evidence; no commit | WP-1..WP-3 |

Stop conditions: a work package that cannot meet acceptance reports incomplete with the exact
blocker rather than widening its scope; WP-2 does not run while the box carries foreign load and
queues with an explicit resume trigger instead of reporting a contended result as a verdict.
