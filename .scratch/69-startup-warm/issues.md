# 69 - WP-R: eagerly warm the win scan + its index at run start

## Contract

Work package R1 of the program. The only remaining rung-50 lag-ceiling breaches for the frozen
`static-derived` win layout are the read-ahead worker's STARTUP warm-up: at windows 0-2 the scan
prefix and the adopted `order_no` index tile are not yet warm, so the first scans take cold page-ins
on the pacer (`lag_breach_attribution reads=N boundary=0`). WP-Q (ticket 68) measured static-derived
win rung-50 = 3/2/2/3 out of 40, all in windows 0-2; learned 13/4/3/3 with the extra seed42 blowup
from a separate merge-oscillation (sibling lane R2).

**Objective:** eliminate the startup warm-up breaches with the smallest sound change: warm the
relevant scan prefix (and the adopted index tile) EAGERLY at engine open / run start, before the
first window, rather than waiting for the background worker's first cadence; or make the driver
perform a bounded synchronous warm-up pass before the timed windows begin.

**Preserve all invariants:** the warm path is read-only and off the correctness path; authoritative
reads stay inline; durability untouched; the warm work is bounded (no second full scan on the
request path). **Do NOT game the metric** (no extending/skipping the timed window, no pre-issuing
requests).

**Acceptance (verbatim):** on the win workload with the index adopted, `static-derived` rung-50
lag-rej reaches 0/40 (across >=3 seeds), and learned is within a small band of static-derived (its
extra breaches being the separate R2 oscillation); throughput must not regress. OR the exact
remaining cause with evidence. Never overstate; label pilot-grade.

Judging: the author's ADOPTED reading is §7.1 lag-only primary; report both that and both-ceilings.
Do not edit the pre-registration.

## Diagnosis (checkpoint, committed before implementation)

### Code path (this worktree, HEAD `e78d036`)

- `engine/src/readahead.rs::TileReadView` warms point lookups (`warm_lookup`) and full-entity scan
  prefixes (`warm_scan_prefix`, memoized per snapshot) off the pacer. `Engine::tile_read_view`
  (engine.rs) appends the adopted `order_no` index tile to the warm view (ticket 67's WP-P follow-on),
  and `request_read_lookups` emits the `order_no` index probe for a movement scan (request.rs).
- `engine/src/bin/flashdb-load.rs::run` startup order (HEAD):
  1. open engine + install policy (`engine.set_layout_policy` at open);
  2. spawn compaction worker;
  3. **spawn read-ahead worker with `engine.tile_read_view()` — view WITHOUT the index**;
  4. spawn the async `order_no` index build (if `order_no_index_wanted`);
  5. `t0 = Instant::now()`; run loop starts.

### The named cause, with the two distinct defects

The index build/adopt and the worker's index view both happen lazily, one window boundary too late:

1. **The index is adopted only at the FIRST window boundary.** `manage_order_no_index` runs inside
   `if args.metrics_json` at each window boundary, and the async build (spawned at open) is adopted
   there. So during window 0 the authoritative movement scan runs WITHOUT the index: the full
   30,000-doc `P|StockMovement` scan on the pacer (~38-43 ms/scan CPU + cold page-ins). WP-P measured
   window 0 `read_us` 503-757 ms — that is the full scan, not a single cold probe.
2. **The read-ahead worker's view lacks the index until the boundary AFTER adoption.** `ra.refresh`
   runs at the window boundary BEFORE `manage_order_no_index`, so after the window-0 adoption the
   worker still warms against the no-index view through all of window 1. Window 1's movement scans
   then cold-page-in the index tile on the pacer (WP-P measured window 1 `read_us` 26-28 ms).

Together they produce static-derived windows 0-1 (and the harness's observed 0-2). The device fsync
tail is NOT involved (WP-P proved no fsync >50 ms becomes a lag breach). The mechanism is
read-warm-up, exactly the scope of this package.

### Why the existing worker cadence is not enough

The worker is spawned just before `t0` with a stale (no-index) view and paces warm at 4 ms/request;
the first dispatch is ~20 ms later. Even a fast worker cannot fix defect 1 (the index is simply not
adopted during window 0) or defect 2 (its view excludes the index until the next boundary refresh).
Eager adoption + eager warm at open removes both, without touching the worker's steady-state cadence.

## Design (chosen)

1. **Eagerly build + adopt the index at open when the frozen policy wants it** (`order_no_index_wanted`
   at open). A bounded, synchronous, one-time build/fsync/adopt BEFORE `t0`, so window 0 already
   serves the index. The same build already runs async at open today; this only moves its completion
   ahead of the first dispatch. `record_order_no_index_build` still runs, so the build bytes land in
   the first window's metrics exactly as before (first snapshot is at window 0 boundary).
2. **Bounded synchronous warm-up pass before the timed windows** (`TileReadView::warm_startup`):
   walk the movement `P|StockMovement` primary prefix (the scan fallback read set) AND, when the
   index is adopted, the whole `U|StockMovement|order_no` index-tile prefix. ~4.78 MB + ~2.14 MB,
   one-time, read-only, results discarded. This makes the very first scan/index probe a page-cache
   hit independent of worker pacing.
3. **Spawn the read-ahead worker AFTER the eager adopt** so its initial view already includes the
   index tile, and it keeps the point-probe lead warm from window 0.

The steady-state `manage_order_no_index` (async build/adopt for the learned arm's later toggles) is
unchanged. The learned arm starts index-OFF (demand gate, ticket 66), so its own window 0-1 full
scan is the learner's designed observation window and is NOT warmed away here (out of scope; recorded
below).

### Rejected alternatives

- **(a) Lengthen the worker's initial lead / reduce the warm pace.** Does not fix defect 1 (the
  index is absent during window 0 regardless of pace) or defect 2 (stale view until the boundary
  refresh). Rejected as not addressing the cause.
- **(b) Refresh the worker's view immediately after `manage_order_no_index` at the boundary.** Fixes
  defect 2 but leaves defect 1 (window 0 full scan). Rejected as partial.
- **(c) Block the run loop's first boundary on the async build.** Still leaves window 0 without the
  index and turns the build into a boundary stall (lag risk). Rejected.
- **(d) Pre-issue / skip / extend the first timed window.** Explicitly forbidden (metric gaming).
- **(e) Warm per request on the request path.** Exactly the forbidden second full scan on the request
  path; the eager pass is a single bounded startup read off the request path. Rejected.

### Invariants argument (state how each is preserved)

1. **Correctness (index never on the correctness path).** Unchanged: the eager adopt installs the
   same `OrderNoIndex` via `adopt_order_no_index`; every hit is cross-checked and every miss/mismatch
   falls back to the authoritative scan. The warm pass discards its results.
2. **Durability.** Unchanged: the build writes via `TileWriter::finish` (sync_data) and the driver
   `sync_all`s the parent directory before adopting, exactly as `manage_order_no_index` does. No
   request ack depends on the index.
3. **Authoritative reads stay inline.** Unchanged: the warm pass only reads tile blocks into the
   page cache; the authoritative scan/probe still runs inline on the pacer.
4. **No second full scan on the request path.** The warm pass is a single bounded startup read
   (~6.9 MB total) BEFORE the timed windows; the request path is byte-for-byte unchanged.
5. **Bounded work.** The eager build is one 30k-entry sort + one tile write (the same build that
   already runs once per adoption); the warm pass is two bounded prefix walks. Both are one-time at
   open, off the timed windows.
6. **No metric gaming.** The timed windows are not extended/skipped; no request is pre-issued. The
   warm-up happens before `t0`/`window_start`, so it is simply off the timed clock (the same rule
   that already puts device probing and WAL replay outside the timed windows).

## Change

(TODO after implementation; commits and files.)

## Tests

(TODO after implementation.)

## Measurement

(TODO; harness `cold_0b.py --workloads win --arms declared learned static-derived`, seeds >=3 + a
repeat, on `cp -a` copies of `/tmp/cold51/per_region-mixed`, one process at a time.)

## Gates (verbatim)

(TODO after implementation.)

## Before/after tables

(TODO after measurement.)

## Acceptance verdict

(TODO.)

## Residual items

- learned's own demand-gated window 0-1 (index OFF until the first scan-demand flip) is the WP-O
  learner design and is NOT a read-warm-up defect; it is out of scope here (the R2 lane owns the
  separate merge oscillation).
- Service ceiling remains device-bound (both-ceilings 0 everywhere); the open §7.1 reconciliation is
  untouched.
