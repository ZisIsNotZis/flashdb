# flashdb

A single-node document store that learns its own physical layout from a workload trace instead of asking the user to declare one. It is a research prototype whose central claim is under test, not a database to adopt: the comparison it is judged by, on one machine against sqlite and postgres, was lost (see [Status of the thesis](#status-of-the-thesis)).

**Not for:** production data, multi-node deployment, SQL, or any workload other than the committed inventory trace — the request adapter accepts exactly one block graph, `cust → take` (see [What is absent](#what-is-absent)).

## Try it

From a clean clone, with Rust 1.98.1. The build lines pin the crate source to the aliyun mirror and pass `--offline`, which is the hermetic form the repository's own runners use — no network, no dependence on host cargo config:

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      build --workspace --offline
./target/debug/flashdb-scale seed /tmp/flashdb-demo.db 20000 500 256
./target/debug/flashdb-scale verify /tmp/flashdb-demo.db 20000
./target/debug/flashdb-scale checkpoint /tmp/flashdb-demo.db
./target/debug/flashdb-scale tileinfo /tmp/flashdb-demo.db
```

The last four lines print, on the measured box (abridged: `...` marks omitted fields):

```
phase=seed ok=true docs=20000 csn=40 ... write_amplification=1.207 docs_s_cumulative=29385
phase=verify ok=true csn=40 docs_a=10000 docs_b=10000 probes=40 gets=40 scans=6
phase=checkpoint ok=true csn_before=40 published=true csn=40 tiles=1 serving_memtable_entries=0
phase=tileinfo tile=tile-40.tile bytes=7033309 cutoff=40 entries=40000 page_size=4096 bloom_len=50024 dir_len=72468 fences=99 ...
```

That is a WAL-backed seed, a read-back verification, a memtable→tile publish through the durable manifest, and the tile's own format report. Every number above is the command's output, not a measurement of anything else. `seed` refuses a directory that already has a WAL, so remove `/tmp/flashdb-demo.db` before re-running.

What the cargo line cannot rebuild: the crates themselves. `--offline` never fetches, so the cache must already hold them; on a cache that has never fetched them, drop `--offline` and the same pinned form resolves its dependencies from the mirror (verified with an empty `CARGO_HOME`: the workspace builds). It also builds neither the Python harness environment (`cd harness && uv run --with pytest pytest`, which needs `uv`), nor the discarded measurement instances and pre-seeded snapshots, nor the device calibration (needs `fio` and an idle NVMe).

## Run the tests

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --workspace --offline
```

Measured at revision `24c94f2`: 339 passed, 0 failed, 0 ignored, 27 test binaries, no compiler warnings. The Python harness is separate: `cd harness && env -u VIRTUAL_ENV uv run --with pytest pytest -q` -> 210 passed. Without the two `--config` flags cargo resolves against crates.io instead of the mirror; `--offline` also requires a warm cache, and an empty `CARGO_HOME` with `--offline` fails at resolution (`no matching package named serde`).

## What is in the tree

Every item names the code path or the test that pins it.

- **Storage substrate:** versioned `P`/`U`/`R` keys ordered by commit sequence number (`engine/src/keys.rs`), a CRC-framed WAL whose torn tail is cut and whose CRC-bad frame fails closed (`engine/src/wal.rs`), and per-block atomic commit.
- **Durability:** one `fdatasync` per commit group on a background worker bounded by `MAX_PENDING_BARRIERS = 64` (`engine/src/engine.rs:44`; `engine/tests/group_commit.rs`, `engine/tests/batch_commit.rs`). Uniqueness is checked per request at publish, not at validate time (`engine/tests/request_commit.rs`, `engine/tests/constraints.rs`).
- **Tiles:** the `FDBTILE3` file format — page-sized, CRC-checked data blocks (4 KiB–64 KiB per tile, 4 KiB default), a CRC-protected fence directory kept on disk, a sparse in-RAM index holding one reference per *directory block*, an optional bloom section, superblock and trailer (`engine/src/tile.rs`; `engine/tests/tile_format.rs`, `engine/tests/tile_layout.rs`, `engine/tests/tile_read.rs`).
- **Manifest:** an A/B root that chains continuation pages, so `MAX_TILES = 4096` active tiles fit; a torn page falls back to the older valid one and a malformed chain fails closed (`engine/src/manifest.rs`; `engine/tests/manifest_engine.rs`, `engine/tests/partitioned_publish_cap.rs`).
- **Maintenance calls:** `maybe_checkpoint(max_wal_bytes)` publishes the memtable, `rotate_wal(max_wal_bytes)` retires a fully published WAL onto a fresh segment named in the manifest, `maybe_compact_shape` merges bounded runs under one of three shapes with a per-region `merge_min_tiles` trigger, and `compact()` collapses the whole active list into one tile on request (`engine/src/engine.rs`, `engine/src/wal.rs`; `engine/tests/multi_tile.rs`, `engine/tests/region_compaction.rs`, `engine/tests/merge_threshold.rs`).
- **Learned axis:** a derived `order_no → handle` secondary access path for `P|StockMovement`, built off the request path from the merged memtable+tile view, consulted only as a hint with every hit cross-checked and every miss falling back to the full scan, rebuilt at every open whenever the policy asks for it (`engine/src/index.rs`, `engine/src/bin/flashdb-load.rs`; `engine/tests/order_no_index.rs`).
- **Learner surface:** per-region `page_size`, per-region `merge_min_tiles` and the `index_order_no` flag, injected as a JSON policy file and re-read per window (`engine/src/engine.rs` — `LayoutPolicy`; `harness/flashdb_harness/layout_learner.py`). The policy is not persisted: it affects only new tiles and future merges, and a reader honours each tile's stored page size.
- **Workload harness:** a deterministic inventory generator (SplitMix64, integer Zipf) with byte-frozen JSONL fixtures (`harness/flashdb_harness/generator.py`; `harness/tests/fixtures/`), and an open-loop pacer that issues requests at a declared rate and reports per-request service latency and schedule lag (`engine/src/bin/flashdb-load.rs`).

## What is absent

Each absence is a limit with a consequence, not a missing feature nobody needs yet.

- **No power-loss proof.** A complete CRC-bad frame fails closed instead of truncating, which does not distinguish an externally truncated acknowledged write from an incomplete unacknowledged tail; the external run's record carries only the declared setting (`flashdb --group-requests 64`), with no observed durability field (`.scratch/82-controlled-m1/runs/b1/rung-200/run-flashdb-declared.json`; `docs/plan/open-items.md` row `PLAN-powerloss-durability`).
- **No bounded recovery or RAM claim.** Open streams every active tile and verifies it against the retained WAL, so open time is linear in the data, and the peak during that verification is bounded by the retained WAL suffix rather than by a target (`engine/src/engine.rs` — `open_impl`; its comment records an 88 MB open-time peak on a 118 MB instance).
- **No automatic background maintenance.** `Engine` spawns one thread, the `fdatasync` worker, and the driver may add a read-ahead warner (`engine/src/readahead.rs`); neither checkpoints, rotates or compacts. Those are calls the opening process makes on its own thresholds (`flashdb-load --checkpoint-bytes --rotate-bytes --compact-bytes`), and a process that never calls them keeps one growing WAL and no tiles.
- **No general request API.** `engine/src/request.rs` pins `params` to `sku/loc/qty/email/order_no` and rejects any block graph other than `cust → take`, so a request is one fixed inventory order, not a query language; three of the four join shapes in the workload study have no execution path (`docs/plan/open-items.md` row `PLAN-request-surface`).
- **No multi-node operation, no second writer.** One process, one directory, one writer.
- **No persisted layout policy.** Reopening an instance with different flags serves existing tiles correctly only because each tile carries its own page size; region mode and the policy table are not durable (`docs/plan/open-items.md` row `PLAN-encoding-not-persisted`).

## Status of the thesis

The thesis is that a learned layout beats a declared one. Two different questions are measured, and only the first is favourable.

- **Learned vs declared, internal, pilot grade** (3 seeds plus one repeat, 12.25x data:memory instance, revision `482e3a1`): the learned arm reaches +41.3% to +50.7% achieved throughput (median +47.6%) against a ~4.0% noise band; adversarial reorganisation stays at a 0.933x median ratio and the win reorganisation at 0.767x; a write spike costs ~8-9% capacity and neither arm recovers. Not the pre-registered 10-seed matrix. Record: `.scratch/STATUS.md` §2.
- **The acceptance, external, lost** (`docs/objective.md` §0): at 200 rps `flashdb-declared` sustained 3 of 5 paired blocks while sqlite sustained 5 of 5 and postgres 5 of 5 — a highest-sustained-level ratio of 0.5x. The low-load non-inferiority gate passed with a deficit of 1.001 and the initial-policy gate passed; the 3x superiority gate is withdrawn as structurally unpassable for any pair at one offered level. Record: `.scratch/82-controlled-m1/report.md` §5 and `report.json`, at revision `51c69f3`.
- **That 3 of 5 is rule-invariant, not a provisional reading.** Re-read under the decided lag-only rule (`sustained_primary`, `docs/objective.md` §1), **0 of 45** saved block rows and **0 of 47** saved arm-runs change verdict, because no M1 window file was ever scored with a service ceiling (`"service_ceiling_us": null` in all 45) — recompute with `.scratch/87-m1-decided-rule/run.sh`. The loss therefore stands under either rule; a service ceiling switched on counterfactually flips 38 of 45 rows and cannot rank the arms, since the device's own fsync p99 (9,821 us) is inside the ceiling.

The loss is attributed to a defect rather than to the thesis: the derived `order_no` index was built from tiles only, so on that workload, whose movements were still in the memtable, it held 0 entries and every scan request paid a 5.6 MB full scan (`docs/plan/open-items.md` row `T-83`; `.scratch/83-index-probe/issues.md`). The index is built from the merged view at `8640a80`, bound by `engine/tests/order_no_index.rs`, and the defect is now **measured as work per request** rather than argued (`.scratch/84-index-defect-mechanism/issues.md`): on a rebuilt append-heavy instance of the M1 record's shape (156 tiles, 2,785 movements in the serving memtable, 188 scan requests) the index holds **2,785 entries for 2,785 rows** where the pre-fix build held **0**; **0 of 188** scan requests enter the merged scan (188 index consultations, 188 cross-checked point probes, the same 188 answers); and tile bytes per scan request fall from **616,428** to **0** while the rows are memtable-resident or **4,218** once they are tile-resident — at the record's 0.047 scan mix, **42,240 → 13,466** read bytes per request against the predicted 43,298 → 13,460 (`docs/plan/gap-analysis.md` §7 fix #1; the *after* figure hits to 0.05%, the *before* figure sits 2.4% low because the rebuilt tiles' block fill is 3,951 B against the record's 4,096 B). `engine/tests/index_defect_mechanism.rs` binds that and fails on pre-fix code with `left: 0, right: 2785`.

The 200 rps comparison is **unmeasured.** It is the one part of the claim that needs a quiet box; the window is granted on a trigger (`uptime` reading loadavg under 2 for one minute, no other agent writing in the tree) and no paired block has run yet, so this repository claims **no throughput number** for the fix: what stands is the work-per-request measurement above and the internal learner result below. It must not be re-run under foreign load — the twelve `.scratch/83-index-probe/runs/` probe files carry an `# INVALID - do not cite:` first line for exactly that reason, and a contended pair measures the load as much as the engine. Its instrument is `.scratch/89-m3b-paired-200/`: two arms on the same frozen trace, the chain that regenerates the trace and the instance from committed specs, and a gate that refuses an arm whose start loadavg is above the trigger. A borrowed machine of the same storage class is still the fallback; a laptop is not a substitute, because the 5.6 MB scan is a property of this device. The clause-by-clause state sits in `docs/plan/workplan.md` §M3 status (WP-2).

## Documentation

| File | Purpose |
|---|---|
| `docs/design.md` | Research thesis, scope and invariants. |
| `docs/engine.md` | The storage engine as implemented and as specified. |
| `docs/contracts.md` | Proposed request, transaction and layout contracts; check status/open sections. |
| `docs/objective.md` | The product bar (external comparison), the KPI ladder and the open decisions. |
| `docs/roadmap.md` | Where the work stands and the milestone list. |
| `docs/learning.md` | Offline learning and later adaptation. |
| `docs/dev-loop.md` | Workload, validation and experiment plan. |
| `docs/plan/evidence.md` | Which claim comes from which code path or measured record. |
| `docs/glossary.md` | Terminology. |
| `docs/prior-art.md` | Prior-art notes; verify citations before external use. |

Reference target: one NVMe and data:RAM ≥20:1 in a cgroup-limited experiment (for example 200 GB / 8 GB).

## Licence and problems

GNU Affero General Public License v3 — see [LICENSE.md](LICENSE.md).

Report a problem by opening an issue at <https://github.com/ZisIsNotZis/flashdb/issues>.
