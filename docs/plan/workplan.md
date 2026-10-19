# Work plan — current milestone and the ranked backlog

Owner: the main agent. Status is a field of each work package, rewritten in place. The product bar is
`../objective.md` §0; the method and gap-attribution protocol are `../dev-loop.md`; milestone order is
`../roadmap.md`; the item inventory with leverage ranking is `open-items.md`.

## Decisions in force (2026-10-19, delegated author authority)

Recorded in `../roadmap.md` §Decisions. Summary: the external bar replaces T6; §7.1 is closed; M2's
"no general L2" is accepted; baselines run un-handicapped. Any change to these needs new measured
evidence, not a preference.

## M3 — access-path parity (current)

Budget: 7 writer dispatches, 2 review cycles per package, 3 no-progress attempts per package, fresh
context for every child. One writer per worktree or file area; the main agent integrates.

| id | status | owner | scope (exact files) | acceptance and proof | depends |
|---|---|---|---|---|---|
| WP-1 | **integrated `8640a80`; review found D1-D8; superseded by WP-1b on the blocked items** | worker (fresh) | `engine/src/index.rs`, `engine/src/engine.rs` (merged primary scan with csn), `engine/src/readahead.rs` (startup warm source), `engine/src/bin/flashdb-load.rs` (eager build call site) + their tests | the index is built from the **merged** memtable+tile view: a region whose rows are all in the memtable yields one entry per row and `coverage_csn` = max covered csn, bound by a test; an empty build while the merged scan yields rows is a hard `io::Error` naming both counts; `startup_warm scan_entries` is non-zero for a memtable-only region; `cargo --config 'source.crates-io.replace-with="aliyun"' --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' test --workspace --offline` green; policy-off path unchanged | none |
| WP-2 | queued | writer (fresh) | measurement only: `scripts/` + `.scratch/83-index-probe/` (no engine edits) | quiet box (loadavg < 2 sustained before each arm run), 5 paired 200 rps blocks with arm rotation, snapshot restore verified, stderr kept; reports paired per-block sustainment declared vs indexed plus `lag_breach_attribution` for every rejected window; verdict "indexed sustains >= declared" or the named phase still responsible | WP-1 |
| WP-3 | **integrated `4916fb7`, harness 210 tests green** | worker (fresh) | `harness/flashdb_harness/baseline_0b.py` (+ its tests) | the run record carries the KPI-ladder instruments: bytes written/ingested, bytes read/request, CPU µs/request, resident bytes/data byte, recovery seconds — each with the command that read it; recomputing the M1 records yields a table whose throughput and latency columns reproduce ticket 82's report within its stated error band | none |
| WP-6 | queued | worker (fresh) | `engine/src/wal.rs` (or the segment writer) + one 200 rps paired block in `.scratch/83-index-probe/` | preallocating the WAL segment is tested as the mechanism behind the +1,432 us barrier gap vs postgres (`docs/plan/gap-analysis.md` §5.1): the block reports service p50/p99 with and without preallocation, and if the gap does not close the sync-count-per-commit measurement is taken instead; verdict names the mechanism or records it unconfirmed | WP-3, quiet box |
| WP-1b | **integrated `f1f9912`, 316 green; re-review PASS with notes** | worker (fresh) | same five files as WP-1 | the loud error is reachable in production: an empty build is refused when a signal derived from a *different* structure (`active_tile_regions` / `serving_memtable_entries`) says the region holds rows, naming visited rows, entries and the signal, bound by a test whose scan yields nothing while the engine holds rows; the tautological `debug_assert_eq!` is gone; the boundary-time scan is timed on the window record as `index_scan_us` and a failed scan is one-shot; metrics rotate after the eager build and warm so window 0's region telemetry is clean; the memory docstring matches the caller's copy; a tombstone + re-versioned handle test binds one-entry-per-live-row and `.max()` csn; counts move from 311 by exactly the new tests | WP-1, review |
| WP-8 | **integrated `cf6bf9a`, 327 green; review in flight** (carried the four WP-1c items) | worker (fresh) | `engine/src/bin/flashdb-load.rs`, `engine/src/index.rs`, `engine/src/readahead.rs` | after a memtable publish the adopted index is rebuilt off-path (or marked stale and rebuilt at the next boundary), so a region that was empty at build time is not served by the full scan for the whole run with no log line; the tile half of the scan runs on the builder thread, not the driver, with the memtable rows snapshotted inline under a stated byte bound; acceptance: a run whose region grows after the build ends with a covering index, and no inline scan exceeds the bound | WP-1b |
| WP-7 | **integrated `e5a3020`** | worker (fresh) | `README.md`, `docs/engine.md`, `docs/plan/evidence.md` | every current-state sentence resolves to a code path or a record path; no false absence and no false capability; every printed command ran; README states the negative external result, the licence and where to report a problem; `docs/plan/evidence.md` becomes the current reconciliation record (closes PLAN-readme-stale, PLAN-engine-doc-stale, PLAN-evidence-reconciliation) | none |
| WP-4 | queued | reviewer (fresh) | read-only review of WP-1..WP-3 at the integrated revision | package verdicts with defect evidence (location, criterion, reproduction); no commit | WP-1..WP-3 |

**WP-1b re-review (PASS with notes, folded into WP-8).** P1: the memtable arm of the witness counts the whole memtable, so a restored target with a non-empty memtable and no live `StockMovement` row aborted the eager build at startup (`?` on the build) rather than falling back — the "over-report is harmless" acceptance I recorded was wrong. Fix in flight: count the region's live rows from the memtable prefix. P2a: the Global-mode limit is misdescribed (`active_tile_regions()` reports the first key's region, so the witness can be wrong in either direction). P2b: a refused build re-scans at every boundary because the latch is only set on snapshot failure. P2c: no negative control for a legitimately empty region.

**WP-1b stated limit (recorded at integration, `f1f9912`).** The `RegionRows` witness derives "the region holds rows" from a tile that names the region or from a non-empty serving memtable. Under `RegionMode::Global` a tile does not name one region, so tile-resident rows are invisible to the witness and it under-reports; the merged scan keeps reads correct, and the guard simply cannot fire there. The memtable arm can over-report ("some rows are live"), which is safe for its purpose (it can only turn a would-be-silent empty build into an error) but would refuse a build on a region with no rows while another region sits in the memtable — accepted, because that state is served by the full scan either way and the refusal names both counts.

**WP-1 known risk (recorded at integration, `8640a80`).** The async build now snapshots the merged rows into a `Vec` on the driver thread at a window boundary (`movement_rows`), because the memtable is not shareable with the builder thread. For a memtable-only region that is bounded RAM work, but for a tiled 624 KiB region it streams from device on the pacer — the same stall class M3 exists to remove. Follow-up if the review confirms the exposure: keep the tile scan on the worker and snapshot only the memtable rows inline (the hybrid), with a bound on the inline bytes.

Stop conditions: a package that cannot meet acceptance reports incomplete with the exact blocker
rather than widening scope; WP-2 never runs while the box carries foreign load and queues with an
explicit resume trigger instead of reporting a contended result as a verdict.

## M4 — gap attribution (after M3)

| id | status | owner | scope | acceptance |
|---|---|---|---|---|
| WP-5 | queued | scout/analyst (fresh) | `.scratch/82-controlled-m1/runs/b1..b5`, the M3 records, `docs/engine.md` device constants; writes `docs/plan/gap-analysis.md` | every losing phase classified (a) not learned enough / (b) defect / (c) architecture with its evidence and revision; the byte-level format forensics (on-disk and in-memory bytes per row vs the format's declared minimum); a ranked fix list, each fix naming its predicted effect on the KPI ladder and its instrument |

**Rebuild cadence — superseded by the WP-8 review (2026-10-19).** The estimate below priced device time only. The review priced it on the KPI ladder and the answer changes: rebuilding the whole index every window writes ~0.19 MB (ticket-83 region) to ~2.1 MB (30 k rows) per rebuild, which is **1.4-3.6x ingest on the WP-2 snapshot and 15-40x on the 30 k-row instance** against a T3 baseline write amp of 3.92 — up to ~10x the entire existing amplification, charged to `index_output_bytes`, which is also the learner's reorg reward. A cadence floor alone cannot fix it: the floor needed to bound write amp (~10^4 csn at 200 rps) makes the index tens of seconds stale, and a scan that targets recent order numbers then misses, restoring the 6 ms full scan. **The fix is incremental maintenance (base + delta), not a longer interval**; WP-9 owns it. No 200 rps arm runs until it lands.

**Original estimate (kept as the rejected reading).** The demand arm fires at every boundary where the state moved and the movement scan consulted the index once, so under continuous scan demand a run can rebuild once per window (~2.1 MB of index tile + a directory fsync each, charged to `index_output_bytes`). Measured scale: the win workload writes ~139 movements/s at 200 rps and issues ~9.4 scans/s, so one rebuild per window costs ~6 ms of device time against up to ~56 ms of full-scan time it avoids — favourable while scans are being missed, which is exactly when the demand arm fires. **No cadence floor is added before WP-2**: the run record now carries `index_output_bytes` and the decision lines, so WP-2 decides the floor from measurement rather than from this estimate. If WP-2 shows the index rewriting more than a few percent of the instance per run or the rebuild competing with the barrier, the floor lands as `max(demand lag, N csn)` in `rebuild_trigger` and is re-measured.

## Known blockers

- **Postgres is not installed** (`psql` absent; only `libpq5` remains), although ticket 82 measured
  postgres **18.6 (Ubuntu 18.6-3build2)** on this box. M5's three-arm comparison cannot run the
  postgres leg. Resume trigger: an authorised install (system package or an unprivileged user-space
  build) — record the route here when taken, read the version and settings back from the running
  server, and never substitute a different engine while reporting the comparison as external.
- **Foreign load.** WP-2 requires a quiet box; ticket 83's first probe was invalidated by the
  author's other project. Record loadavg before and after each arm.

## Not planned (bounded backlog)

Multi-node, online SQL, general L2 co-location, the three M2 engine capabilities
(`$block.field` multi-row binding, wider validator, aggregate-to-write), and tuning a baseline into a
race. Reopening any of these needs a measured reason recorded here first.

## Completion evidence

Each work package closes with: the revision (commit hash), the command and its observed result, the
artefact path, and the reviewer verdict. The main area stays on `main`; a worktree's branch is pushed
at every safe point and merged once its review passes.
