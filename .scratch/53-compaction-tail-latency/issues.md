# 53 - make load-path compaction latency-safe

Contract: ticket-49 (`.scratch/49-latency-tail-attribution/issues.md`) attributed the
dominant per-request latency tail to the load driver's per-window compaction actuation
(`compact_window` -> `Engine::maybe_compact_shape(64 MiB, CheapestPair)`), but left the
actuation's sub-cost unsplit and the per-window merge volume uncontrolled. This ticket
(a) measures what one actuation costs, and (b) changes the driver+engine so the
per-window stall is bounded below the 10 ms schedule-lag ceiling while compaction stays
ENABLED and the tiered CheapestPair policy and the strictly-increasing disjoint tile-range
invariant are preserved. The pre-registration is not changed.

Instance: `/tmp/cold44/per_region-mixed` (8.17x; 1,643,933,528 B data / 192 MiB cap,
336 active tiles, per_region). Trace `adversarial.jsonl` (seed 42, sha256 bf0a5170…),
policy `declared.policy.json` (default 4096/10/2), ceilings `--ceiling-us 10000
--lag-ceiling-us 10000`. Each run cold in `systemd-run --user --scope MemoryMax=192 MiB`.
Frozen binaries under `/tmp/cold53/` (not committed): `flashdb-load-pre` (pre-fix,
instrumented; sha256 8b12cfa6…), `flashdb-load-fix` (post-fix; sha256 ec72a53a…).

## Measured diagnosis (pre-fix instrumented actuation, /tmp/cold53/baseline.stderr, n=65)

Per-actuation phase split (us; CheapestPair, one merge per window):

| field | p50 | p95 | max | share of elapsed |
|---|---:|---:|---:|---:|
| input_bytes | 2,286,942 | 10,341,582 | 10,341,582 | — |
| output_bytes | 2,282,641 | 10,162,287 | 10,162,287 | — |
| elapsed_us (whole actuation) | 152,861 | 399,519 | 537,985 | 100% |
| stream_us (replay + reduce + tile data write) | 67,931 | 116,162 | 279,774 | 36.2% |
| finish_us (tile finalize + tile file fsync) | 38,619 | 188,748 | 262,602 | 39.4% |
| verify_us (two fail-closed passes) | 15,848 | 71,110 | 121,162 | 14.8% |
| dir_sync_us (tile dir entry) | 6,268 | 12,734 | 61,960 | 4.0% |
| manifest_us (manifest file+dir fsync) | 7,336 | 12,107 | 18,700 | 4.1% |
| unlink_us | 740 | 2,274 | 3,046 | 0.5% |

Device baseline same run: fsync p50 6,060 us / p99 18,055 us. A single durable merge has a
~24 ms floor from its four fsyncs even at the smallest input (~377 KB -> 40 ms); later
tiers grow to 10.3 MB input -> 538 ms. **Dominant sub-cost: the tile build itself**
(stream write + tile-file fsync ≈ 75%), which grows with the merged input size; the
manifest/dir fsyncs are only ~8%. The per-window merge volume is the uncontrolled knob
ticket-49 suspected.

## Decision + rejected alternatives

Fix = (b)+(d): move the build to a background worker and shrink the on-path remainder to
a lazy (no-fsync, no-unlink) flip.

- (a) time-box one merge into <10 ms slices: rejected — the durable flip alone is ~24 ms
  of fsync (four syncs at 6 ms p50), so no synchronous durable merge can fit under 10 ms.
- (b) background build: adopted (the merge is `&self`-readable and self-contained).
- (c) idle-only + min cadence: rejected — at rung 50 the ready queue gap between requests
  is ~13-17 ms, smaller than even one small merge (~40 ms), so idle-deferral would starve
  compaction and leave the policy knob inert.
- (d) many tiny merges: rejected alone — the ~24 ms fsync floor stays; adopted only as the
  lazy-flip half that removes that floor from the request path.

Durability trade (documented, not hidden): the apply flip is LAZY — the manifest page is
written but not fsynced and the retired inputs are not unlinked. The build half, however,
makes the replacement tile durable (tile file `sync_data` + parent-directory `sync_all`)
off the request path before the outcome can be applied, so a lazy manifest page that the
OS later writes back references only a durable tile: a crash can no longer leave a
CRC-valid manifest page naming a missing tile. Compaction is only a read optimisation;
the WAL remains the durability authority. A crash before the next durable publish
(checkpoint/rotation) reverts to the previous durable root, whose tiles are still on
disk, so a merge is lost but never corrupts. The next durable publish carries every
lazy page with it.

## Change

- `engine.rs`: `CompactionStats`/`CompactionBuildStats` (per-actuation phase split);
  `CompactionJob`/`CompactionOutcome` (self-contained build/apply payloads);
  `Engine::compaction_job` (run selection only), `build_compaction_job` (free function:
  bounded WAL replay + open tiles by name + stream/reduce/write/sync/verify),
  `Engine::apply_compaction_job` (lazy `publish_replacing_lazy`/`publish_lazy` + in-memory
  flip, no unlink). `stream_merged_reduction`/`verify_compacted_tile`/
  `verify_compacted_against_wal` became associated functions shared by both paths.
- `manifest.rs`: `publish_lazy`/`publish_replacing_lazy` (inactive-page write + flip, no
  file/dir sync), same A/B + strictly-increasing validation as the durable publish.
- `wal.rs`: `replay_file`/`replay_file_each` read a bounded WAL prefix via `pread`
  (`read_at`), so a worker bound to the job-creation `valid_len` never observes concurrent
  appends and never moves the shared write offset; `Wal::open_or_recover` now opens the
  segment read+write so `Wal::file_clone` yields a readable worker handle.
- `flashdb-load.rs`: one `CompactionWorker` thread when `--compact-bytes` is set; at each
  window boundary it submits one job (worker idle) and applies one finished outcome
  (lazy flip); a final drain applies the last in-flight build. Per-applied-actuation
  `compact_actuation` lines go to stderr.

## Tests

`engine/tests/compaction_job.rs`:
- `job_build_apply_matches_sync_merge_and_reopens` — job+build+apply produces the same
  reads, the same strictly-increasing disjoint cutoffs, and a reopen-identical directory
  as the synchronous merge.
- `replay_file_is_bounded_to_the_captured_valid_len` — the worker's bounded replay stops
  at the captured length (records appended later are invisible).
All existing Rust and Python tests stay green.

## Before/after (same-session; ticket-49 convention: svc p50 = median per-window p50, p95/p99 = max-over-windows; lag p50 median, p95/p99 max-over-windows; us)

Baseline = pre-fix instrumented binary (same frozen trace/instance, this session).
Ticket-49 baseline figures are quoted from ticket-49 for comparability.

| config | rung | ach rps | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | svc-rej | lag-rej | compact B |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| t49 baseline | 50 | 49.78 | 6,424 | 874,795 | 954,795 | 65 | 782,658 | 862,658 | 35 | 35 | 44,163,084 |
| t49 baseline | 100 | 97.66 | 10,841 | 827,064 | 897,064 | 2,240 | 681,335 | 751,335 | 15 | 15 | 140,110,225 |
| t49 baseline | 200 | 199.25 | 252,110 | 827,072 | 902,072 | 104,024 | 741,329 | 816,329 | 7 | 7 | 71,136,009 |
| t49 baseline | 400 | 400.00 | 332,296 | 756,881 | 821,434 | 241,872 | 695,218 | 765,218 | 4 | 4 | 23,655,742 |
| pre-fix | 50 | 49.81 | 1,747 | 453,963 | 513,963 | 62 | 416,029 | 476,029 | 37 | 37 | 55,281,100 |
| pre-fix | 100 | 99.52 | 6,012 | 505,576 | 565,576 | 69 | 465,650 | 525,650 | 16 | 16 | 159,479,070 |
| pre-fix | 200 | 198.25 | 9,062 | 509,446 | 534,446 | 2,816 | 448,267 | 473,267 | 8 | 8 | 81,298,296 |
| pre-fix | 400 | 400.00 | 20,684 | 401,384 | 458,884 | 8,266 | 341,867 | 399,367 | 4 | 4 | 32,019,332 |
| fix | 50 | 50.00 | 1,106 | 107,709 | 9,975 | 59 | 75,955 | 115,955 | 37 | 5 | 13,518,558 |
| fix | 100 | 100.00 | 1,110 | 24,353 | 52,009 | 62 | 8,769 | 32,012 | 20 | 4 | 15,403,453 |
| fix | 200 | 200.00 | 6,619 | 197,911 | 237,911 | 1,264 | 136,903 | 176,903 | 10 | 10 | 15,241,073 |
| fix | 400 | 395.86 | 13,423 | 198,807 | 223,995 | 4,285 | 148,810 | 188,810 | 5 | 5 | 11,118,016 |

Median per-window svc p99 (robust stat the 3x threshold uses), us:

| config | rung 50 | rung 100 | rung 200 | rung 400 |
|---|---:|---:|---:|---:|
| t49 baseline | 163,919 | 447,608 | 684,019 | 659,892 |
| pre-fix (same session) | 92,839 | 308,277 | 387,325 | 392,121 |
| fix pass 1 | 12,572 | 17,861 | 62,108 | 68,409 |
| fix pass 2 | 13,187 | 19,940 | 34,372 | 128,242 |

Write amp (rewrite factor = compaction output bytes / ingested bytes; ingested 979,080 B):

| config | compaction output | merges | rewrite factor |
|---|---:|---:|---:|
| pre-fix | 328,077,798 | 65 | 335.1x |
| fix | 56,758,539 | 38 | 58.0x |

## Acceptance verdict

- **Not met (best achieved, honest):** no rung is fully sustained (`lag p99 <= 10 ms` AND
  no window rejected by EITHER ceiling). Fix rung 50: lag median 114 us, 3/40 windows fully
  valid, 5 lag-ceiling rejections, 37 service-ceiling rejections. This is the SAME
  un-attributed residual ticket-49 measured for `nocompact`/`neither` (nocompact rung 50:
  svc p99 median 17,773 us, 5 lag-rej) — the residual is cold-cache read page-ins /
  allocator / OS jitter, not compaction, and is outside this ticket's scope.
- **Met:** rung 200 median per-window svc p99 62,108 us (pass 2: 34,372 us) vs ticket-49
  baseline 684,019 us = **11.0x below (pass 2: 19.9x below)**, above the required 3x. The
  fix's tail matches-or-beats `nocompact` at every rung (e.g. rung 50 svc p99 median
  12,572 vs nocompact 17,773 us), so compaction no longer adds to the tail.
- **Write amp not worse:** rewrite factor 335.1x -> 58.0x (5.8x less compaction write;
  the worker completes one merge per ~2 windows, fewer/larger merges than the baseline's
  one-per-window, which is strictly less rewrite volume).

## Residual risk / unseparated

- The apply flip is lazy (no fsync, no unlink): retired input tiles accumulate as orphans
  until a later durable publish makes the flip durable; a crash before then loses the
  merge, never data. Orphan cleanup after the next durable publish is not implemented
  (measurement scope; the run's directory keeps ~38 orphaned retired tiles).
- The worker applies one job per ~2 windows (submit and apply happen in separate boundary
  passes), so the merge cadence is half the baseline's; the tiered policy still runs and
  the disjoint invariant is preserved, but per-window merge eagerness is lower.
- The remaining service/lag tail (residual) is still not separated into read page-ins vs
  allocator/OS jitter — unchanged from ticket-49.
- Session drift: same-session pre-fix svc p99 at rung 200 is 387,325 us vs ticket-49's
  684,019 us; the 3x claim holds against both (6.2x / 11.0x).

## Gates (verbatim, run at ae1df85)

`cargo test --workspace --offline` — 274 passed; 0 failed; 0 ignored.

`cargo build --workspace --offline` — 0 warnings.

`cd harness && uv run --with pytest pytest -q` — `132 passed in 4.37s`.

`git diff --check` — silent (exit 0).

## Raw artifacts (not committed)

`/tmp/cold53/`: `flashdb-load-pre`, `flashdb-load-fix`, `adversarial.jsonl`,
`declared.policy.json`, `baseline.stdout/.stderr`, `fix.stdout/.stderr`,
`fix2.stdout/.stderr`, `baseline-instance/`, `fix-instance/`, `fix2-instance/`.

## Verifier finding (independent review, blocking)

`build_compaction_job` (`engine.rs`) writes and file-syncs the replacement tile
(`TileWriter::finish` -> `tile.rs` `sync_data`, "No parent-directory durability
claim") but never fsyncs the parent directory. `apply_compaction_job` then
installs the reference via `manifest_publish_lazy` -> `Manifest::publish_lazy` /
`publish_replacing_lazy`, which write the A/B slot with `durable=false` (no
manifest file sync, no parent-dir sync). After a crash the OS may have written
back the CRC-valid manifest page (seq N+1) while the replacement tile's directory
entry is not durable, so `Manifest::open` selects seq N+1, `open_discover` then
`Tile::open` fails `NotFound`, and the engine cannot open. Data is not silently
lost (WAL + orphan tile content survive) but recovery is not automatic, which
falsifies "a crash loses the merge never data / reopen recovers".

## Fix chosen (option a) + rationale + rejected alternative

Chosen (a): in the off-request-path build, after `writer.finish()`, also
`File::open(dir)?.sync_all()` so the replacement tile's directory entry is
durable before the outcome can be applied. The request-path apply stays lazy
(no manifest file/dir sync, no unlink); every fsync stays off the request path,
so the WP-B latency gain is not regressed.

Ordering now: tile file sync (`TileWriter::finish`) -> parent-directory sync
(build, off-path) -> lazy manifest page write (apply, on-path). Any OS writeback
of the lazy page therefore references a tile whose data and dirent are already
durable.

Rejected (b): durable manifest publish on the background worker with only the
in-memory root swap on the request path. Rejected because it would move the
manifest file/dir fsyncs onto the worker and require the worker to hold a
`&mut` engine/manifest (or a second manifest writer), which widens the change
and still leaves the in-memory swap to coordinate; (a) is strictly smaller and
keeps the exact same request-path apply already shipped.

Defense in depth added in `apply_compaction_job`: the opened replacement tile's
`cutoff()`/`digest()` are re-verified against `outcome` before the `TileRef` is
installed.

Driver fix: `flashdb-load.rs` `take_done` now propagates a disconnected worker
channel as an error instead of `Ok(None)`.

## New/updated tests

`engine/tests/compaction_job.rs`:
- `job_build_apply_matches_sync_merge_and_reopens` now builds a reference merged
  state with `maybe_compact_shape(CheapestPair)` on an identical twin engine and
  compares readings, active cutoffs, and reopen, so a wrong-pair selection fails.
- `apply_compaction_job_rejects_mismatched_digest_or_cutoff` — defense in depth.
- `lazy_apply_can_be_made_manifest_durable_and_still_reopens` — strongest
  available crash-consistency approximation: forces the lazy manifest page
  durable after apply and reopens. Precise limits: same-process, no dropping of
  not-yet-durable writes, so it cannot by itself fail if the build-side dir sync
  is removed; it proves the page can become durable and still name a matching
  tile, not real power-loss durability.
- `missing_replacement_tile_fails_reopen_after_manifest_writeback` — negative
  control showing the harness detects the `NotFound` failure mode.

## Gates (verbatim, run at the repair commit)

`cargo test --workspace --offline` — 277 passed; 0 failed; 0 ignored.

`cargo build --workspace --offline` — 0 warnings.

`cargo test --offline -p flashdb-engine --test compaction_job` — 5 passed; 0 failed.

`cd harness && uv run --with pytest pytest -q` — `132 passed in 4.66s`.

`git diff --check` — silent (exit 0).
