# 57 - non-blocking window boundary + cold-read optimization + entity-first co-location (wave6 WP-F)

Contract: work package F of `.scratch/52-program/issues.md`. Remove the last rung-50 lag breaches
that WP-E (`.scratch/56-overlapped-barrier/issues.md`) named as two residual mechanisms: (1) the
window-boundary barrier drain, and (2) synchronous cold point reads on the request path. For (2) do
a bounded read-path optimization (newest-first primary point read) and measure an entity-first
co-location arm. Also fix the two WP-E engine issues and report both judging views. Acceptance:
durability unchanged; rung 50 meets the lag ceiling on cold44 (8.17x) and cold51 (12.25x), or
report the exact remaining per-breach cause; the co-location arm must state its read-count and tail
delta honestly. Does not change the pre-registration or the docs (primary-vs-service ceiling
reconciliation remains an OPEN AUTHOR ITEM).

Instance/traces: `/tmp/cold44/per_region-mixed` (8.17x; 1,643,933,528 B / 192 MiB),
`/tmp/cold51/per_region-mixed` (12.25x; 1,643,925,811 B / 128 MiB), and a new entity-first arm
built from the same generator/trace (`/tmp/cold57-ef/per_region-mixed`, 1,638,234,253 B / 192 MiB
= 8.14x). Trace `adversarial.jsonl` (seed 42, sha256 bf0a5170…, 8000 lines), policy
`declared.policy.json` (default 4096/10/2), ceilings `--ceiling-us 10000 --lag-ceiling-us 10000`.
Measurement protocol: `cp -a` copies under `/tmp/cold57/`, release binary built once and copied to
`/tmp/cold57/flashdb-load` (sha256 `1a7bba8e874de671…`), run only from that copy, one process at a
time via `systemd-run --user --scope MemoryMax=<cap>M MemorySwapMax=0` (192 MiB for 8.17x/8.14x,
128 MiB for 12.25x). Payload (verbatim): `--rate-rps 50,100,200,400 --segment-requests 2000
--window-ms 1000 --metrics-json --peak-report --ceiling-us 10000 --lag-ceiling-us 10000
--region-mode per_region --policy-file declared.policy.json --group-requests 64
--checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864` (+
`--key-encoding entity_first` on the entity-first arm).

## Engine fixes (commit 8cf9988)

- `submit_group` at the in-flight cap (`MAX_PENDING_BARRIERS = 64`) previously **closed** the group
  (ack) and returned the rejected-requests marker, while the doc comment claimed it stayed OPEN for
  retry. Now cap-reached leaves the group OPEN (no ack, request stays pending) and returns a
  distinct marker the driver re-enqueues; every other failure path closes the group. Extracted
  `submit_overlapped_barrier()` so the cap check and the submit are separate.
- `Engine::end_group` now asserts `pending_barriers() == 0` via `ensure_no_pending_barrier` and
  fails closed if a barrier is still in flight, closing the path where a group could be ended while
  its covering `fdatasync` was outstanding.

## Non-blocking window-boundary drain (commit ec47e25 + 76231d2)

The WP-E driver drained **every** in-flight barrier at each 1 s window boundary (`drain_all_pending_groups`),
so a slow barrier still in flight at the boundary blocked the pacer and landed as the next window's
first-group lag (ticket 56 mechanism #2). Fixed:

- `record_group` split into `record_group_lag` (dispatch-scoped, always recorded when the group is
  dispatched) and `record_group_service` (completion-scoped, recorded when the barrier completes).
  A group that crosses a boundary now contributes its schedule-lag sample to the dispatch window and
  its service sample to the completion window, so no completion is lost.
- `drain_completed_groups` (non-blocking, `try_take_group_barrier`) replaces the blocking drain at
  every window boundary and at the top of each dispatch iteration. `drain_one_pending_group` /
  `drain_all_pending_groups` (blocking) survive only for WAL maintenance and end-of-run, where the
  engine must be drained for correctness (and never on the request path).
- Compaction is skipped for a window while `engine.pending_barriers() > 0` (the engine asserts a
  drained engine on state-mutating paths); it runs at the next drained boundary/WAL maintenance.
  This is why wave6 sees fewer, smaller merges than WP-E (see write-amp note below).
- `lag_breach_attribution` stderr line attributes each lag-rejected window's first-group breaches to
  `boundary` (only when the boundary drain itself exceeded the ceiling) or `reads`. After the
  inline drain, `boundary=0` on every run (proven; the boundary no longer contributes).

## Read-path optimization: newest-first primary point read (commit 44205d1)

`Engine::get` for a point read probes the memtable first, then the tiles **newest→oldest** and stops
at the first hit (the tiles are disjoint CSN ranges ordered by cutoff, so the first hit is the
newest version; the memtable wins if present). The old cursor scan also stopped at the first entry,
but only after opening a cursor (reading a data block) on every candidate tile that could contain
the handle prefix. The measured win is small — **-2.5% total request-path bytes** (135,363,895 →
132,012,047 B; P|Stock 15,888 → 15,070 blocks) — because the workload's stock rows are mostly
single-version (3156 decrements over 8.4 M rows), so most primary gets touch ~1 tile anyway.
Rejected: directory-read dedup (the second directory read is page-cache warm; marginal), and
per-candidate bloom pre-filtering (the candidate count is already ~1 for single-version rows).

## Entity-first co-location arm (commits 5d90b93 + build scripts)

`FLASHDB_KEY_ENCODING` env knob in `flashdb-scale` (`seed-inventory`/`checkpoint`/`tileinfo`/
`openinfo`/`verify`/…), `--key-encoding family_first|entity_first` flag in `flashdb-load`, and the
same flag threaded through `build_inventory()` + `build_cold_44.py`/`build_cold_51.py`. Entity-first
key spelling co-locates the P/U/R views of one entity in a single combined region, so the instance
has **3 write regions instead of 6** and **half the tiles**.

Exact build path (same generator/trace/dimensions as cold44, seed 42, 8 MiB cadence,
`P|StockMovement=65536:10`):

```
cd harness && source $HOME/.cargo/env
uv run python build_cold_44.py --release --workdir /tmp/cold57-ef --key-encoding entity_first
```

```
phase=seed-inventory ok=true blocks=5200 docs=10400000 customers=2000000 stocks=8400000
  csn=5200 publishes=146 rotations=145 tiles=147 region_mode=per_region
  key_encoding=EntityFirst dir=/tmp/cold57-ef/per_region-mixed elapsed_s=230.460
data_bytes=1638234253 cap_bytes=201326592 ratio=8.14x
```

The script's own post-build `openinfo` fails with `tile contains extra WAL entry` because it opens
with the default family-first spelling; opening with the correct env
(`FLASHDB_KEY_ENCODING=entity_first target/release/flashdb-scale openinfo …`) succeeds:
`active_tiles=165 serving_memtable_entries=0 tile_bytes_total=1632921715 rss_kb=39264`
(vs family-first `active_tiles=336 rss_kb=65860`). tileinfo confirms `C|Customer` tiles of
156,000 entries each (P+U+R merged). A quick smoke/verify of the entity-first arm is recorded in
the measurement table below (the 8000-request scored run itself completes rc=0, errors=0, which is
the durability/end-to-end check).

## Tests

- `submit_group_at_the_in_flight_cap_leaves_the_group_open_for_retry`
- `end_group_refuses_while_an_overlapped_barrier_is_in_flight`
- `record_group_lag_and_service_split_dispatch_and_completion_scopes`
- `get_probes_newest_first_across_tiles_and_memtable`
- `key_encoding_flag_selects_and_rejects`

## Gates (verbatim, run at HEAD `76231d2`)

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.54s
```
2. `cargo test --workspace --offline` — totals: **289 passed; 0 failed; 0 ignored** (122 lib +
22 flashdb-load + …; +5 tests from this ticket: +2 engine, +1 load lag split, +1 get, +1 key-encoding).
3. `cd harness && uv run --with pytest pytest -q` — **132 passed** (4.19s).
4. `git diff --check` — silent (exit 0).

## Before/after (rung 50, the acceptance rung)

| run | lag-rej | svc-rej | lag p99 med/max (us) | svc p99 med (us) |
|---:|---:|---:|---:|---:|
| cold44 WP-E before (blocking boundary, old get) | 5/40 | 38/40 | 3,260 / 135,165 | 15,569 |
| c51 WP-E before | 2/40 | 38/40 | 1,720 / 174,192 | 14,443 |
| cold44 wave6 run1 | 4/40 | 38/40 | 2,393 / 113,984 | 18,931 |
| cold44 wave6 run2 | 6/40 | 39/40 | 1,669 / 235,475 | 21,054 |
| c51 wave6 (12.25x) | 4/40 | 38/40 | 2,410 / 82,565 | 16,626 |
| ef wave6 (8.14x) | 15/40 | 38/40 | 4,542 / 153,009 | 20,836 |

## Before/after (whole run)

| run | late_total | lag-rej total | svc-rej total | achieved_rps | compact_bytes | merges |
|---:|---:|---:|---:|---:|---:|---:|
| cold44 WP-E before | 299 | 22 | 74 | 106 | 56,758,538 | 38 |
| c51 WP-E before | 616 | 19 | 74 | 106 | 56,758,273 | 38 |
| cold44 wave6 run1 | 221 | 14 | 74 | 106 | 20,904,807 | 26 |
| cold44 wave6 run2 | 597 | 21 | 75 | 106 | 22,704,958 | 27 |
| c51 wave6 (12.25x) | 483 | 15 | 74 | 107 | 23,310,824 | 27 |
| ef wave6 (8.14x) | 887 | 40 | 73 | 106 | 265,216,151 | 26 |

Boundary-vs-read attribution (`lag_breach_attribution`): **boundary=0 on every wave6 run** (WP-E
attributed ~4 of 11 lag-rejected windows to the boundary). All remaining lag breaches are
read-attributed.

Per-request dependent-read volume (request-path `bytes_read`):

| arm | total bytes_read | blocks/req (4k) | vs WP-E old get |
|---:|---:|---:|---:|
| cold44 WP-E old get | 135,363,895 | 4.13 | — |
| cold44 wave6 (newest-first get) | 132,012,047 | 4.03 | **-2.5%** |
| ef wave6 (entity-first) | 110,060,311 | 3.36 | **-16.6%** (vs 4.03) |

Entity-first request-path `read_us` total is also lower (18,262 ms vs 19,278 ms over the run), i.e.
fewer, not slower, reads on average.

## Verdict

**Both engine fixes and the non-blocking boundary are correct and durability-preserving** (the
append is still covered by an `fdatasync` before ack; no state-mutating path runs with a barrier in
flight). The boundary no longer contributes any lag breach (`boundary=0` on all runs), removing
ticket 56 mechanism #2 in full.

**Rung 50 still does NOT meet the lag ceiling**, so the acceptance is an honest FAIL on the primary
criterion. The remaining breaches are read-attributed (`boundary=0`, `reads=all`), but the two
read-side measures do NOT close the gap:

- **Newest-first `get()`** reduces request-path bytes by only **-2.5%** (most primary gets already
  touch ~1 tile) and does not move the rung-50 lag-rej count (4-6/40, i.e. within the shared-box
  noise band of WP-E's 5/40).
- **Entity-first co-location** does reduce reads (**4.03 → 3.36 blocks/req = -16.6%**), but it
  **regresses compaction write amplification ~12x** (compact_bytes 20-23 MB → 265 MB, same
  `--compact-bytes 67108864` and same 26 merges): merging P/U/R into one region makes each compaction
  rewrite the combined dirty set (max single actuation 8.58 MB vs 0.97 MB family-first), so the
  compaction worker saturates the device and the read **tail** worsens — ef lag-rej **15/40** and
  late_total **887**, despite fewer reads. The ticket-43 model's -34.8% tile-touch reduction is real
  for reads, but the model missed the write-amplification cost, which dominates the tail.

The exact remaining cause of the read-attributed breaches is therefore **synchronous cold point
reads blocking the single pacer, with the shared device's durability I/O (fsync + compaction)
starving them**: at 8-12x data:RAM every request is a cold read (page cache ~8% of data), the
request path does ~4 dependent block reads per request (~2.3 ms mean, tailing to 100-235 ms under
device contention), and the 10 ms lag ceiling sits below the durability barrier's own fsync p50-p99
(5-31 ms). The two runs of the same cold44 binary under identical caps disagree (lag-rej 4/40 vs
6/40, late_total 221 vs 597), which bounds the honest signal-to-noise of the shared box.

## Judging views (per the adopted §7.1)

- **Sustained-by-lag-ceiling-only:** NOT sustained at rung 50 (cold44 4-6/40, c51 4/40, ef 15/40).
  Honest FAIL. The boundary residual is gone; the remaining cause is cold reads under device
  contention, which the two read-side measures do not remove.
- **Sustained-by-both-ceilings:** NOT sustained at rung 50 (the service ceiling is breached by the
  durability barrier itself in ~38/40 windows, unchanged from the WP-E baseline and pre-existing
  tension recorded in tickets 54/55). Unchanged; judged separately, not the primary metric here.

## Residual / next steps (not implemented here)

- **Pre-registration reconciliation (open author item, NOT changed):** the primary-vs-service
  ceiling text that treats a service-ceiling-invalid window as invalidating the run conflicts with
  the adopted §7.1 (primary = lag ceiling; service reported separately). Left flagged for the author.
- **Move the request-path reads off the pacer** (async/overlapped reads, or a read thread pool) is
  the remaining dominant mechanism and is a different work package; a read-path read-count reduction
  alone (this ticket) does not remove the device-contention tail.
- **Entity-first write amplification** (265 MB vs 22 MB compact bytes) is a real regression to be
  weighed against the -16.6% read win if co-location is pursued further; it was not in the
  ticket-43 model.
