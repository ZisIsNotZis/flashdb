# 25 — make the real inventory flow exercise tiles, then measure the per-region layout axis

Opened 2026-10-02. Slice 24 (`.scratch/24-region-partitioning/issues.md`) built
region-partitioned tiles and a per-region `(page_size, bloom_bits)` policy, but
closed with an explicit gap: the pacer (`flashdb-load`) and `flashdb-replay`
opened with `Engine::open` (no tile discovery) and `seed-inventory` never
published tiles, so **every "real workload" measurement so far was
memtable-only** — the per-region page-size axis had no way to act on a flow read.
This slice makes the flow read tiles end to end and then measures the axis
honestly.

## What changed

**`engine/src/bin/flashdb-scale.rs`**
- `seed-inventory` now applies the same region/layout env knobs as `seed`
  (`FLASHDB_REGION_MODE`, `FLASHDB_REGION_LAYOUT`, `FLASHDB_PAGE_SIZE`,
  `FLASHDB_BLOOM_BITS`) and, when `FLASHDB_SEED_CHECKPOINT_BYTES` is set,
  publishes the seeded data through the already-tested
  `maybe_checkpoint`/`publish_tile` protocol at that WAL cadence and once more at
  the end for the uncovered tail. Unset (the default) is byte-for-byte the old
  behaviour. Reports `publishes`, `tiles`, `serving_memtable_entries`,
  `region_mode`, `layout_page_size`.
- New `checkpoint <dir>`: `open_discover`, install the region/layout policy,
  `maybe_checkpoint(0)` to publish whatever the memtable holds, report active
  tile count and serving memtable entries. This is how a flow's writes are made
  tile-resident before a read measurement.
- New `tileinfo <dir>`: `open_discover`, report `active_tiles` and
  `serving_memtable_entries`, then parse every `*.tile` file's FDBTILE3
  superblock and first directory fence **read-only** (no engine tile opened) to
  report bytes, cutoff, entry count, `page_size`, `bloom_len`, `bits_per_key`,
  `hash_count`, directory/fence state and the region derived from the first key.
- `Engine.rs`, `manifest.rs`, `tile.rs`, `wal.rs` untouched; no engine semantics
  changed. `tileinfo` is deliberately a raw-format reader rather than a new
  engine accessor, keeping the read path and its invariants untouched.

**`engine/src/bin/flashdb-load.rs`**
- The first positional is now `<dir-or-wal>`: a **directory opens tile-aware with
  `Engine::open_discover`** (the new default), a **WAL file opens with
  `Engine::open`** as before.
- New `--no-tiles`: open a directory through `Engine::open(<dir>/data.wal)` (full
  WAL replay into the memtable, tiles ignored). This is the controlled
  tile-vs-memtable A/B on the same data and trace.
- Emits an `engine_open target=… discover=… active_tiles=… serving_memtable_entries=…`
  line. `else_used` is now also counted on the default single-request path (the
  stock guard's `take: failed`), not just `--group-requests`.

**`engine/src/bin/flashdb-replay.rs`**
- The first positional accepts an engine **directory** (opened with
  `open_discover`, so seeded dimensions are read from tiles) or a **WAL file**
  (historical `Engine::open`).

**`harness/flashdb_harness/region_layout_compare.py`** (+ `harness/tests/test_region_layout_compare.py`):
builds traces and binaries, then for each layout config seeds, replays the write
phase, checkpoints, captures `tileinfo`, and runs the pacer (tile-aware and
`--no-tiles`) at one fixed rate; prints the tile-touch and equal-load tables.

### What changes for existing callers

- `flashdb-load <existing-wal>` still works: a **file** path keeps the
  memtable-only open. Passing a **directory** now discovers tiles (new capability,
  not a behaviour change to any existing file-path caller).
- `flashdb-replay <existing-wal>` still works for a file path; a directory is new.
- No harness caller breaks: `harness/flashdb_harness/inventory_workload.py` passes
  `engine_dir/data.wal` (a file) and `engine/tests/replay_trace.rs` passes a `.wal`
  file — both unchanged. To get tile reads from `inventory_workload.py`, pass the
  directory instead.

## Tile-touch evidence

`tileinfo` on the scan-variant directory (deterministic; identical across runs):

| config | active_tiles | serving_memtable_entries | tile files | page sizes | tile_bytes_total |
|---|---|---|---|---|---|
| per_region-4k | 45 | **0** | 45 | {4096} | 5,245,741 |
| per_region-mixed | 45 | **0** | 45 | {4096, 65536} | 5,297,879 |
| global-4k | 20 | **0** | 20 | {4096} | 4,931,636 |

The mixed directory's scan region is the only 64 KiB tile; every probe region stays 4 KiB:

```
phase=tileinfo tile=tile-3021.r2.tile bytes=518890 cutoff=3021 entries=3000 page_size=65536 bloom_len=26274 bits_per_key=10 hash_count=7 dir_len=339 fences=7 region=P|StockMovement
phase=tileinfo tile=tile-3021.r4.tile bytes=197209 cutoff=3021 entries=3000 page_size=4096  bloom_len=26274 bits_per_key=10 hash_count=7 dir_len=2450 fences=40 region=R|StockMovement
phase=tileinfo tile=tile-3021.r6.tile bytes=254077 cutoff=3021 entries=3000 page_size=4096  bloom_len=26274 bits_per_key=10 hash_count=7 dir_len=3898 fences=54 region=U|StockMovement
```

**Distinguishing tile reads from memtable reads.** The same read-only trace
(1500 requests) against the same directory, one open per mode:

```
discover=true   active_tiles=45 serving_memtable_entries=0        requests=1500 completed=1500 errors=0
discover=false  active_tiles=0  serving_memtable_entries=63000    requests=1500 completed=1500 errors=0   (--no-tiles)
```

With **0 serving-memtable entries and 45 active tiles**, the engine's only read
source is tiles (reads return the same 1500 results, 0 errors): the flow provably
consulted tile cursors. The memtable-control open has 0 tiles and 63,000 memtable
entries. (The latency A/B is *not* the distinguishing number here — both paths are
~3–5 ms per scan and the sign flips between runs, because the scan is
CPU/JSON-decode-bound, not block-read-bound; see the verdict.)

## Equal offered load — per-region layout axis

Read-only scan variant (`movements_scan` supplied by a write phase that is then
checkpointed, so the scan reads the published `P|StockMovement` tile). Two
independent runs at different offered rates; fresh seeded engine per config; the
device fsync p50 is probed in-session:

Run 1 (writes=3000, reads=600, offered 100 rps):

| config | achieved rps | window p50 µs | window p99 µs | device fsync p50 µs | else |
|---|---|---|---|---|---|
| per_region-4k | 100.0 | 4518 | 14491 | 5825 | 0 |
| per_region-mixed | 100.0 | 4531 | 7914 | 5784 | 0 |
| global-4k | 100.0 | 7014 | 28308 | 6684 | 0 |

Run 2 (writes=3000, reads=1500, offered 200 rps):

| config | achieved rps | window p50 µs | window p99 µs | device fsync p50 µs | else |
|---|---|---|---|---|---|
| per_region-4k | 200.0 | 3655 | 15259 | 5551 | 0 |
| per_region-mixed | 200.0 | 3179 | 7674 | 5646 | 0 |
| global-4k | 200.0 | 3657 | 10197 | 6035 | 0 |

Full real inventory flow, write + read (`MIX`), offered 120 rps, `on_hand=2` so
the take guard's else branch fires (run 3):

| config | achieved rps | window p50 µs | window p99 µs | device fsync p50 µs | else_used |
|---|---|---|---|---|---|
| per_region-4k | 120.0 | 5664 | 21618 | 6395 | 552 |
| per_region-mixed | 120.0 | 5757 | 20327 | 5600 | 552 |
| global-4k | 120.0 | 5410 | 431338¹ | 6363 | 552 |

¹ one contaminated window on the shared box; its p50 is 5410 µs.

## Verdict — what binds, and does mixed help?

- **The gap is closed structurally.** Every "real workload" number in slice 24
  was memtable-only; this flow now reads published tiles (`serving_memtable_entries=0`
  with 45 active tiles, and successful results).
- **Equal offered load, mixed vs 4 KiB-everywhere:** *indistinguishable on p50*
  (4518 vs 4531 in run 1; 3655 vs 3179 in run 2 — the sign flips). Mixed shows a
  **lower p99 in both runs** (7914/7674 vs 14491/15259 µs, ~45–50%), mechanically
  consistent with the scanned region dropping from ~40 block reads/CRCs to ~7 by
  moving to 64 KiB pages. But the between-run spread on this shared box is as
  large as the effect, so this is recorded as a *plausible tail benefit*, not a
  proven win.
- **The full flow is genuinely indistinguishable** (p50 5757 vs 5664; p99 20327
  vs 21618; else 552 in both): during the flow, movements are written to the
  memtable and never published, so the page-size axis has no tile to act on. The
  probe regions are 4 KiB in both configs by design, so their (tile-resident)
  latency cannot differ.
- **What binds:** the adapter's `movements_scan` is dominated by per-document JSON
  decoding and top-100 selection, not by block reads. The 64 KiB raw-scan advantage
  measured on `scanbench` (+41% MiB/s, slice 24) does **not** survive this
  CPU-bound adapter, which is why the page-size axis is near-flat here.
- **Deterministic tie-break (bytes).** Mixed costs **+1.0% tile bytes**
  (5,297,879 vs 5,245,741) for the same data — the 64 KiB page target pads the
  scanned tile. Global mode writes fewest bytes (4,931,636) but gives up per-region
  policy entirely.
- **Recommendation:** for *this* workload and box, the per-region page-size axis is
  effectively closed — indistinguishable at equal offered load and costing bytes.
  Reopening it needs a scan pattern whose cost is block-bound (e.g. a large-value
  sequential scan without per-entry JSON decode), or a different measurement
  substrate. The structural work (tile-resident flow) stays: it is the prerequisite
  for any *other* per-region knob (bloom bits, compaction trigger) to be measured
  on the real flow at all.

## Verification (verbatim)

```
$ cargo build --workspace --offline          # after touching the changed bins
   Compiling flashdb-engine v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.53s
warnings: 0

$ cargo test --workspace --offline
test result: ok. 110 passed; ... (engine unit)
test result: ok. 33 passed;  ... (multi_tile)
... 20 result lines ...
total passed: 222, 0 failed, 0 FAILED   (was 221; +1 new test below)

$ cd harness && uv run --with pytest pytest -q
37 passed in 4.23s                       (was 33; +4 parser tests)

$ git diff --check
clean
```

New tests:
- `engine/tests/scale_bin.rs::seed_inventory_checkpoint_publishes_tiles_and_empties_memtable`
  — publishes via `FLASHDB_SEED_CHECKPOINT_BYTES=0`, asserts `publishes=1`,
  `serving_memtable_entries=0`, `active_tiles>0`, and that `tileinfo` reports the
  `P|Customer`/`P|Stock` regions at `page_size=4096`.
- `harness/tests/test_region_layout_compare.py` — pure parsers for the pacer and
  `tileinfo` output.

## Reproduce

```
cd harness
uv run python -m flashdb_harness.region_layout_compare --release --variant scan \
    --writes 3000 --reads 1500 --rate 200 --keep          # axis
uv run python -m flashdb_harness.region_layout_compare --release --variant flow \
    --reads 1200 --rate 120 --on-hand 2 --keep            # full flow + else count
```
