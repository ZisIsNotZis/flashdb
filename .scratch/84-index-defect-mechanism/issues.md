# 84 - F-M1: the ticket-83 index defect measured as WORK PER REQUEST (mechanism), throughput deferred

Contract: `mail/2026-10-21-split-the-claim.md` (root, direction). Split the claim —
"is the defect fixed" is a count of work per request and survives foreign load;
"does flashdb now beat sqlite/postgres at 200 rps" is a rate and needs a quiet box.
This package measures the first and **does not** measure the second.

Nothing here is a rate. Every reported number is a count, a byte total, or a ratio
of two counts from the same run. The 200 rps paired comparison (M3 clause (b),
WP-2) is untouched and stays deferred; the twelve `.scratch/83-index-probe/runs/`
probe files are now marked invalid in their own first line.

## The commands

```bash
bash .scratch/84-index-defect-mechanism/run.sh          # both arms, logs under logs/
```

The arms:

| arm | code | index build | log |
|---|---|---|---|
| BEFORE | pre-fix, worktree at `f584ecc` (the commit before WP-1 `8640a80`) | `build_order_no_index(dir, &tile_read_view, name)` - tiles only | `logs/pre-fix.out` |
| AFTER | HEAD `8f334d0` + mail commits, `engine/tests/index_defect_mechanism.rs` | `build_order_no_index(dir, enc, scan_primary_with_csn, RegionRows, 0, name)` - merged memtable+tile view | `logs/head.out` |
| suite | HEAD | — | `logs/workspace-tests.out` (340 passed, 0 failed) |

Both probes are byte-identical except the build call and the absence of
`Engine::order_no_index_lookups()` in the pre-fix tree; the sources are kept as
`probe-at-HEAD.rs` and `pre-fix-probe.rs` so the pair can be re-run without guessing.

## The instance

The defect needed an **append-heavy** region: movements arriving continuously and
still in the serving memtable, never in a tile. The probe rebuilds the M1 record's
shape (`.scratch/83-index-probe/issues.md` §The defect, `active_tile_regions()` =
`{P|Customer: 3, P|Stock: 153}`):

- 156 published tiles (3 customer, 153 stock), each written with its document and
  its unique-index key, so every tile's key span straddles the `P|StockMovement`
  prefix - `P|...` keys below it, `U|...` keys above it;
- **2,785 movements** appended after the last publish, all still in the serving
  memtable (`serving_memtable_entries = 5,570` = doc + unique key per movement; the
  probe asserts that no published tile names the movement region);
- **188 scan requests** (`find StockMovement where order_no`, the real request
  path `engine/src/request.rs::execute_request`), the same count as the M1 trace's
  188 of 4,000 requests.

The fix's build source is why the instance matters: on it, the pre-fix tiles-only
build sees **0 rows**, and the fixed merged build sees **2,785**.

## The numbers

From the engine's own counters (`metrics::WindowSnapshot`: pattern `scan:StockMovement`
= merged scans entered, region `P|StockMovement` `bytes_read` = tile data-block
bytes read, `probes` = point probes) plus `Engine::order_no_index_lookups()`:

| | BEFORE (pre-fix code) | AFTER, rows in memtable | AFTER, rows published to a tile |
|---|---|---|---|
| index entries / movement rows | **0 / 2,785** | **2,785 / 2,785** | 2,785 / 2,785 |
| scan requests served | 188 | 188 | 188 |
| merged scans entered (`scan:StockMovement`) | 188 | **0** | **0** |
| index consultations | (counter absent pre-fix) | 188 | 188 |
| movement point probes (`get` cross-check) | 0 | 188 | 188 |
| tile bytes read, total | 115,888,464 | **0** | 793,037 |
| **tile bytes per scan request** | **616,428** | **0** | **4,218** |
| answers | 188 ok, 0 empty | 188 ok, 0 empty | 188 ok, 0 empty |

BEFORE and AFTER-name-the-same-thing: in the pre-fix run the "after" arm is the
pre-fix build adopted, and it reports *exactly* the before arm - 188 scans entered,
616,428 B per scan - because a 0-entry index returns `None` and the read path falls
back. That is the defect stated in counters: **the fix changed nothing, and the
probe's non-emptiness assertion fails on it** (`logs/pre-fix.out`, `left: 0, right:
2785`).

## Against the prediction (43,298 -> 13,460 B/request)

`docs/6_ARCHITECTURE.md` §7 fix #1 predicts read bytes/request **43,298 ->
13,460 (-69%)**, decomposed as 638,938 B per scan -> 4,096 B (one dependent page
probe), at the M1 trace's 0.047 scans/request and its non-scan read work of
265,360,600 B / 20,000 requests = 13,268 B/request.

Measured scan term against the predicted one, and the per-request figure that
follows from the record's own mix (arithmetic: `0.047 x scan_bytes + 13,268`):

| | predicted | measured (this run) | hit? |
|---|---|---|---|
| bytes per scan, at the record's block fill | 638,938 | 616,428 | 3.5% low - my tiles' candidate blocks average 3,951 B, the record's 4,096 B; the mechanism (one block per active tile) is the one reproduced |
| bytes per scan, post-fix, row tile-resident | 4,096 | **4,218** | hit (+3.0%); 4,096 B intended, 122 B of block padding |
| bytes per scan, post-fix, row memtable-resident | 4,096 | **0** | better than predicted: the cross-check `get` hits the serving memtable, so the index path reads no tile byte at all |
| bytes per request, before | 43,298 | 42,240 | 2.4% low, from the scan term above; non-scan work is the record's |
| bytes per request, after (row tile-resident) | 13,460 | **13,466** | hit, +0.05% |
| bytes per request, after (row memtable-resident) | 13,460 | 13,268 | hit or better: the scan term is 0 |
| reduction per request | 29,838 | 28,774 | 96.4% of predicted |

**The prediction hit.** The before/after pair stated against it, with the scan term
measured here and nothing extrapolated: **616,428 -> 4,218 B per scan** (memtable
case 616,428 -> 0), and at the record's 4.7% scan mix **42,240 -> 13,466 B per
request** against the predicted 43,298 -> 13,460. The residual is the record
instance's 4,096 B-per-tile block fill versus this instance's 3,951 B.

## The path taken (why the scan is not entered)

Counted per request in the AFTER arms, from the same run:

- `order_no_index_lookups` = 188 for 188 scan requests: the index was consulted on
  every one;
- `scan:StockMovement` pattern count = **0**: `Engine::scan_primary` - the only
  caller of `scan_merged` on this path - was never entered;
- region `P|StockMovement` `probes` = 188: the request path went
  `lookup_movement_by_order_no` -> hit -> `Engine::get` (the documented
  cross-check: `engine/src/request.rs:319-334`) instead of the full scan;
- answers identical to the before arm (188 `ok`, same rows), so the index changed
  the work and not the result; the fallback still answers a miss (the pre-fix run
  and `movement_scan_falls_back_to_full_scan_when_index_absent` cover that).

## The regression test

`engine/tests/index_defect_mechanism.rs` (1 test, in `cargo test --workspace`; 340
total at `8f334d0` + this commit). It asserts, on the append-heavy instance:

1. the merged-view build produces `entries == MOVEMENTS` (2,785) - **non-empty**;
2. the merged scan is entered `0` of 188 times and the index is consulted 188 times;
3. the same answers before and after the index;
4. the full scan pays at least half a block per active tile, and the index path
   pays at most two blocks per scan.

Run against the pre-fix code it fails at (1) with `0 / 2785` - see `logs/pre-fix.out`.
The neighbouring `engine/tests/order_no_index.rs::index_build_covers_rows_that_live_only_in_the_memtable`
covers the same defect at 3 rows; this one carries the M1 instance's scale and the
counters.

## What is NOT measured (and why)

- **Throughput, achieved rate, sustained blocks, latency percentiles**: nothing.
  M3 clause (b) is five paired 200 rps blocks against declared/sqlite/postgres,
  which interleaves the arms under whatever load the box carries - this box carries
  foreign load and the person deferred the window, so the run would produce the
  same invalid artefact as the twelve files now marked invalid. Recorded as
  deferred in `docs/9_IMPLEMENTATION.md` row T-83.
- **The seeded 1.6 GB M1 instance**: its snapshot is git-ignored and absent, so the
  instance is rebuilt to the record's *shape* (tile count, append-only movement
  region, request mix), not restored. The scan term is a function of the active
  tile count and its block fill, both reproduced; the non-scan read work of the
  seeded instance is taken from the record rather than re-measured, and is named as
  the record's in every row of the table above.
- **Device bytes**: `bytes_read` counts bytes read from tile data blocks, which is
  the engine's own definition of read work (`engine/src/metrics.rs` §`bytes_read`),
  not `/proc` device counters.

## Integrity of the twelve files marked invalid

Each of `.scratch/83-index-probe/runs/p{1,2,3}-{declared,indexed}.{out,err}` gained exactly one
line at the top (`# INVALID - do not cite: ...`) and nothing else. Verified per file, all twelve:

```bash
for f in .scratch/83-index-probe/runs/p*
do cmp <(git show 23c4af1^:$f) <(tail -n +2 $f) && echo "IDENTICAL $f"
done
# 12 x IDENTICAL
```

## Residual risks

- The probe's tile block fill (3,951 B/tile) is 3.5% below the record's 4,096 B, so
  the absolute before figure is 2.4% low; the mechanism and the after figure are
  unaffected.
- `metrics_snapshot` latencies are debug-build numbers and are not reported here at
  all; only counts and byte totals are.
- The pre-fix worktree is `.worktrees/pre-fix-84` (git-ignored, declared by
  `.gitignore`). It is removed by `run.sh` left in place for re-runs; nothing in it
  is evidence, the probe source and logs are.
