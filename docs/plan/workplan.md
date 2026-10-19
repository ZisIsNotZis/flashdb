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
| WP-1 | **BLOCKED by review (D1); fixed by WP-1b** | worker (fresh) | `engine/src/index.rs`, `engine/src/engine.rs` (merged primary scan with csn), `engine/src/readahead.rs` (startup warm source), `engine/src/bin/flashdb-load.rs` (eager build call site) + their tests | the index is built from the **merged** memtable+tile view: a region whose rows are all in the memtable yields one entry per row and `coverage_csn` = max covered csn, bound by a test; an empty build while the merged scan yields rows is a hard `io::Error` naming both counts; `startup_warm scan_entries` is non-zero for a memtable-only region; `cargo --config 'source.crates-io.replace-with="aliyun"' --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' test --workspace --offline` green; policy-off path unchanged | none |
| WP-2 | queued | writer (fresh) | measurement only: `scripts/` + `.scratch/83-index-probe/` (no engine edits) | quiet box (loadavg < 2 sustained before each arm run), 5 paired 200 rps blocks with arm rotation, snapshot restore verified, stderr kept; reports paired per-block sustainment declared vs indexed plus `lag_breach_attribution` for every rejected window; verdict "indexed sustains >= declared" or the named phase still responsible | WP-1 |
| WP-3 | **in flight** | worker (fresh) | `harness/flashdb_harness/baseline_0b.py` (+ its tests) | the run record carries the KPI-ladder instruments: bytes written/ingested, bytes read/request, CPU µs/request, resident bytes/data byte, recovery seconds — each with the command that read it; recomputing the M1 records yields a table whose throughput and latency columns reproduce ticket 82's report within its stated error band | none |
| WP-6 | queued | worker (fresh) | `engine/src/wal.rs` (or the segment writer) + one 200 rps paired block in `.scratch/83-index-probe/` | preallocating the WAL segment is tested as the mechanism behind the +1,432 us barrier gap vs postgres (`docs/plan/gap-analysis.md` §5.1): the block reports service p50/p99 with and without preallocation, and if the gap does not close the sync-count-per-commit measurement is taken instead; verdict names the mechanism or records it unconfirmed | WP-3, quiet box |
| WP-1b | **in flight** | worker (fresh, worktree `wp1b-index-loud`) | same five files as WP-1 | the loud error is reachable in production: an empty build is refused when a signal derived from a *different* structure (`active_tile_regions` / `serving_memtable_entries`) says the region holds rows, naming visited rows, entries and the signal, bound by a test whose scan yields nothing while the engine holds rows; the tautological `debug_assert_eq!` is gone; the boundary-time scan is timed on the window record as `index_scan_us` and a failed scan is one-shot; metrics rotate after the eager build and warm so window 0's region telemetry is clean; the memory docstring matches the caller's copy; a tombstone + re-versioned handle test binds one-entry-per-live-row and `.max()` csn; counts move from 311 by exactly the new tests | WP-1, review |
| WP-8 | queued | worker (fresh) | `engine/src/bin/flashdb-load.rs`, `engine/src/index.rs`, `engine/src/readahead.rs` | after a memtable publish the adopted index is rebuilt off-path (or marked stale and rebuilt at the next boundary), so a region that was empty at build time is not served by the full scan for the whole run with no log line; the tile half of the scan runs on the builder thread, not the driver, with the memtable rows snapshotted inline under a stated byte bound; acceptance: a run whose region grows after the build ends with a covering index, and no inline scan exceeds the bound | WP-1b |
| WP-7 | **in flight** | worker (fresh, worktree `wp7-docs-reconcile`) | `README.md`, `docs/engine.md`, `docs/plan/evidence.md` | every current-state sentence resolves to a code path or a record path; no false absence and no false capability; every printed command ran; README states the negative external result, the licence and where to report a problem; `docs/plan/evidence.md` becomes the current reconciliation record (closes PLAN-readme-stale, PLAN-engine-doc-stale, PLAN-evidence-reconciliation) | none |
| WP-4 | queued | reviewer (fresh) | read-only review of WP-1..WP-3 at the integrated revision | package verdicts with defect evidence (location, criterion, reproduction); no commit | WP-1..WP-3 |

**WP-1 known risk (recorded at integration, `8640a80`).** The async build now snapshots the merged rows into a `Vec` on the driver thread at a window boundary (`movement_rows`), because the memtable is not shareable with the builder thread. For a memtable-only region that is bounded RAM work, but for a tiled 624 KiB region it streams from device on the pacer — the same stall class M3 exists to remove. Follow-up if the review confirms the exposure: keep the tile scan on the worker and snapshot only the memtable rows inline (the hybrid), with a bound on the inline bytes.

Stop conditions: a package that cannot meet acceptance reports incomplete with the exact blocker
rather than widening scope; WP-2 never runs while the box carries foreign load and queues with an
explicit resume trigger instead of reporting a contended result as a verdict.

## M4 — gap attribution (after M3)

| id | status | owner | scope | acceptance |
|---|---|---|---|---|
| WP-5 | queued | scout/analyst (fresh) | `.scratch/82-controlled-m1/runs/b1..b5`, the M3 records, `docs/engine.md` device constants; writes `docs/plan/gap-analysis.md` | every losing phase classified (a) not learned enough / (b) defect / (c) architecture with its evidence and revision; the byte-level format forensics (on-disk and in-memory bytes per row vs the format's declared minimum); a ranked fix list, each fix naming its predicted effect on the KPI ladder and its instrument |

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
