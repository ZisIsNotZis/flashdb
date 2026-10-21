# Implementation and evidence reconciliation

Current fact for what the tree implements and what the measurements showed. Every claim below names its producer — a code path, a test, or a saved run record at a stated revision. This record has no authority over the thesis, the pre-registered thresholds, the milestone list or the engine: `../objective.md` §0 owns the bar and `workplan.md` owns the order of work.

## The five layers that the old conflict collapsed

The earlier design-draft claim that no engine exists, and the README claim that the learned-layout engine was not implemented, were both read against a tree that contains a substrate. Presence of one layer says nothing about the next, so each is named with its producer.

| Layer | What it is | Producer |
|---|---|---|
| Proposal | the intended contracts, request grammar and layout language; broader than anything executable | `docs/design.md`, `docs/contracts.md`, `docs/learning.md` |
| Substrate | keyspace, WAL, memtable, tile files, manifest, compaction, recovery | `engine/src/keys.rs`, `wal.rs`, `memtable.rs`, `tile.rs`, `manifest.rs`, `engine.rs` |
| Inventory adapter | the one request shape that can execute: a `cust → take` block graph with fixed fields | `engine/src/request.rs:139-145`; `engine/tests/order_flow.rs` |
| Declared policy (actuator) | per-region `page_size`, `bloom_bits_per_key`, `merge_min_tiles`, `index_order_no`, injected as JSON and re-read per window | `engine/src/engine.rs` — `LayoutPolicy`; `engine/src/bin/flashdb-load.rs` (`--policy-file`, `--policy-reload`) |
| Learner (controller) | the out-of-process chooser over a published candidate set, priced by a unit-anchored reward | `harness/flashdb_harness/layout_learner.py` |

The learner is a controller, not engine code: it rewrites the policy file the driver re-reads. Nothing about the layout is learned inside the engine, and the policy is not persisted — it affects new tiles and future merges only, because each tile stores its own page size.

## Claim → producer

| Claim | Producer | Pinned by |
|---|---|---|
| Versioned `P`/`U`/`R` keys, CSN in the key, one keyspace for documents and derived prefixes | `engine/src/keys.rs`; `docs/engine.md` §Key space | `engine/src/keys.rs` unit tests; `engine/tests/order_flow.rs` |
| WAL with CRC framing; torn tail cut; complete CRC-bad frame fails closed | `engine/src/wal.rs` | `engine/src/wal.rs` unit tests (`torn_tail_is_ignored`, `complete_corrupt_frame_fails_closed_without_truncation`) |
| One `fdatasync` per commit group on a bounded background worker (`MAX_PENDING_BARRIERS = 64`) | `engine/src/engine.rs:44`, group-commit path | `engine/tests/group_commit.rs`, `engine/tests/batch_commit.rs` |
| Uniqueness decided per request at publish, in input order | `engine/src/engine.rs` (`commit_batch` overlay fold) | `engine/tests/request_commit.rs`, `engine/tests/constraints.rs` |
| `FDBTILE3` tile: page-sized CRC-checked data blocks (4 KiB–64 KiB, default 4 KiB), on-disk fence directory, sparse in-RAM index over directory blocks, optional bloom | `engine/src/tile.rs` | `engine/tests/tile_format.rs`, `engine/tests/tile_layout.rs`, `engine/tests/tile_read.rs` |
| Chained A/B manifest, `MAX_TILES = 4096` capacity, torn page falls back, malformed chain fails closed | `engine/src/manifest.rs` (`CHAIN_MAX_RECORDS`, `MAX_CHAIN_PAGES = 316`) | `engine/tests/manifest_engine.rs`, `engine/tests/partitioned_publish_cap.rs` |
| Caller-driven checkpoint, WAL rotation onto a segment the manifest names, compaction over three run shapes with a per-region threshold | `engine/src/engine.rs` (`maybe_checkpoint`, `rotate_wal`, `maybe_compact_shape`, `compact`); `engine/src/manifest.rs` (`SegmentRef`) | `engine/tests/multi_tile.rs`, `engine/tests/region_compaction.rs`, `engine/tests/merge_threshold.rs`, `engine/tests/batch_commit.rs:243` |
| Derived `order_no → handle` access path as a sidecar tile, hint-only, rebuilt at open, built from the merged memtable+tile view | `engine/src/index.rs`; `engine/src/bin/flashdb-load.rs` (`movement_rows`) | `engine/tests/order_no_index.rs` |
| One blocking request shape (`cust → take`), fixed `params` and ops | `engine/src/request.rs:139-145` | `engine/tests/order_flow.rs` |
| Deterministic trace generator and byte-frozen fixtures | `harness/flashdb_harness/generator.py` | `harness/tests/fixtures/*.jsonl`; `cd harness && env -u VIRTUAL_ENV uv run --with pytest pytest -q` → 210 passed at `24c94f2` |
| Device constants (page granularity, bandwidth, tail) | `bench/0a-redo.sh`, the `bench/nvme-bench/` crate | `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`, `summary.json` |
| Whole Rust workspace builds and passes | — | `cargo --config 'source.crates-io.replace-with="aliyun"' --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' test --workspace --offline` → 339 passed, 0 failed, 0 ignored, 27 binaries at `24c94f2` |

## Absent, with the consequence

- **Power-loss durability of an acknowledged write is unproven.** A complete CRC-bad frame fails closed instead of truncating, which does not distinguish an externally truncated acknowledged write; the external run's record carries only the declared setting (`flashdb --group-requests 64`), with no observed durability field (`.scratch/82-controlled-m1/runs/b1/rung-200/run-flashdb-declared.json`; `docs/engine.md` §Foreground commit; `open-items.md` row `PLAN-powerloss-durability`).
- **Recovery time is unbounded and unmeasured.** Open streams and verifies every published tile against the retained WAL, so open time grows with the data and with the retained WAL suffix; the only recorded figure is an open-time peak of 88 MB on a 118 MB, 6-tile instance (`engine/src/engine.rs`, `open_impl`). The ≤128 MB replay ≈ 1–2 s figure in `docs/engine.md` is a design target.
- **Maintenance has no scheduler.** `Engine::maybe_checkpoint` documents that callers invoke it; `rotate_wal` and the compaction calls are equally explicit. `Engine` spawns one thread, the barrier worker; the driver may add a read-ahead warner (`engine/src/readahead.rs`). A caller that never checks in keeps one growing WAL and no tiles.
- **No orphan sweep.** `Engine::apply_compaction_job` flips the manifest lazily (`Manifest::publish_lazy`) and does not unlink its retired inputs; a measured run's directory kept ~38 retired tiles (`open-items.md` row `PLAN-orphan-cleanup`).
- **The policy is not persisted,** so region mode and the merge threshold are re-declared at every open (`open-items.md` row `PLAN-encoding-not-persisted`).
- **Three of the four join shapes have no execution path** (multi-row `$block.field` binding, a validator wider than `cust → take`, aggregate-to-write), and the author decision is that they are not planned for v0 (`../roadmap.md` §Decisions; `open-items.md` row `PLAN-request-surface`).

## Measured verdicts and their conditions

- **Internal, learned vs declared, pilot grade.** Instance 1,643,925,811 B under a 128 MiB cap = 12.25x data:memory, 3 seeds (42/43/44) plus one seed-42 repeat, one flashdb process at a time under `MemoryMax=128MiB`, bound revision `482e3a1`: T1 +41.3% to +50.7% (median +47.6%) against a ~4.0% noise band; T2 reorganisation ratio median 0.933x with 0 index builds on the 0-scan region; T3 median 0.767x; T4 evaluable and **not supported** (~8–9% capacity drop, neither arm recovers). Not the pre-registered 10-seed matrix. Record: `.scratch/STATUS.md` §2.
- **External, the acceptance.** Same frozen trace through one open-loop client against sqlite and postgres at equal durability under the same memory cap, revision `51c69f3`: T5 pass (deficit 1.001), T7 pass, T6 withdrawn as structurally unpassable for any pair at one offered level; at 200 rps `flashdb-declared` sustained 3 of 5 paired blocks while sqlite sustained 5 of 5 and postgres 5 of 5, a highest-sustained-level ratio of 0.5x. Record: `.scratch/82-controlled-m1/report.md` §5, `report.json`. The verdict is **rule-invariant**: re-read under the decided lag-only rule (`sustained_primary`, `objective.md` §1), 0 of 45 rows and 0 of 47 arm-runs move, because no window file carried a service ceiling — recomputation `.scratch/87-m1-decided-rule/run.sh`.
- **Why it lost.** The derived `order_no` index was built from tiles only, so on a workload whose movements were still in the memtable it held 0 entries and each scan request paid a 5.6 MB full scan (`.scratch/83-index-probe/issues.md` §"The defect"; `open-items.md` row `T-83`). The builder now reads the merged memtable+tile view (`engine/src/index.rs`, `engine/src/bin/flashdb-load.rs` — `movement_rows`), integrated at `8640a80`. The defect is now measured as work per request rather than argued (`.scratch/84-index-defect-mechanism/issues.md`: 2,785 entries for 2,785 rows, 0 of 188 scans entered, 42,240 → 13,466 B/request against the predicted 43,298 → 13,460). Whether that closes the 200 rps throughput gap is **unmeasured** — the quiet window is granted on a trigger (`roadmap.md` decision 7) and no paired block has run, so no throughput number is claimed; `.scratch/89-m3b-paired-200/` carries the two arms, the loadavg gate and the chain that regenerates what they need.

## Statements corrected

| Old statement, and where it stood | Current fact | Source |
|---|---|---|
| "The learned-layout engine is not implemented yet" (`README.md`) | The engine, the per-region policy actuator and the harness controller exist; the thesis is under test and the external comparison is lost | `engine/src/engine.rs`, `harness/flashdb_harness/layout_learner.py`, `.scratch/82-controlled-m1/report.md` §5 |
| "no manifest, automatic checkpoint, compaction, WAL retirement" (`README.md`) | Chained `MAX_TILES = 4096` manifest, `maybe_checkpoint`, three compaction shapes, `rotate_wal` all exist; what is absent is engine-internal scheduling and an orphan sweep | `engine/src/manifest.rs`, `engine/src/engine.rs` |
| "does not yet implement the 16 KiB page/directory tile format" (`docs/engine.md`) | The page/directory format is `FDBTILE3` with a per-region page size (4 KiB–64 KiB, default 4 KiB); 16 KiB is a policy choice, not the shipped default | `engine/src/tile.rs`; measured `page_size=4096` in the README demo run |
| "no WAL rotation" (`docs/engine.md`) | `rotate_wal(max_wal_bytes)` retires a fully published segment onto `data-<cutoff>.wal`, named in the manifest | `engine/src/engine.rs:1644`; `engine/tests/batch_commit.rs:243` |
| "directory cannot be packed … the current tile prototype has no page directory" (`docs/engine.md`) | The fence directory is in the tile with a sparse index in RAM over directory blocks | `engine/src/tile.rs`; `engine/tests/tile_read.rs` |
| "unlink-deferred files sit on a trash list persisted in the manifest; recovery re-attempts" (`docs/engine.md`) | No trash list exists; durable paths unlink after the durable flip, the lazy worker path does not unlink at all | `engine/src/engine.rs` (`compact`, `merge_run`, `merge_region_run`); `open-items.md` row `PLAN-orphan-cleanup` |
| "Checkpoint: when 128 MB of WAL has accumulated (or 5 minutes)" (`docs/engine.md`) | No timer and no scheduler: `maybe_checkpoint(max_wal_bytes)` is a caller call, and each driver passes its own threshold | `engine/src/engine.rs` — `maybe_checkpoint` doc |
| "the WAL is preallocated and rotated" (`docs/engine.md`) | Rotated; not preallocated — recovery tolerates an all-zero tail but nothing creates one | `engine/src/wal.rs`; `workplan.md` WP-6 |
| "Budget exhaustion → compaction throttles, writes slow" (`docs/engine.md`) | A merge call skips a run that exceeds its byte budget while a cheaper candidate run exists, and falls back to merging the two smallest adjacent tiles when none fits (the progress guarantee), so a merge is never refused outright; no throttling exists | `engine/src/engine.rs` — `select_run` |
| "page-cache invalidation on tile drop" (`docs/engine.md` checklist) | Not implemented; no `fadvise`/`DONTNEED` call exists | `engine/src/` |
| Learner controls "which `U` prefixes to materialize / which `R` prefixes to retain / encoding per prefix" (`docs/engine.md`) | The learned axes are per-region page size, per-region `merge_min_tiles` and `index_order_no`; every declared unique and reference prefix is materialized, and no codec exists | `engine/src/engine.rs` — `LayoutPolicy`; `harness/flashdb_harness/layout_learner.py` candidate set |
| "compaction style (universal)" listed as fixed, "page size 16 KB; tile target 32 MB" (`docs/engine.md`) | Run shape is a driver choice over three shapes, page size is per-region policy, and tile size is whatever a publish or bounded merge writes | `engine/src/engine.rs` — `CompactShape`, `LayoutPolicy` |
| "no admission control beyond the in-flight block cap (1024) and shed-by-class" (`docs/engine.md`) | No admission control and no shed path exist; the bounds are the driver's group size and `MAX_PENDING_BARRIERS` | `engine/src/engine.rs` |
| `docs/roadmap.md` records learned/static pilot measurements while `README.md` called the engine unimplemented | Both are now reconciled to the same revision-bound records, with the internal and external results kept separate | `.scratch/STATUS.md` §2; `.scratch/82-controlled-m1/report.md` §5 |

## What stays unverified, and what would settle it

- **Whether the index fix closes the throughput gap.** Deferred, not pending: settled by the paired 200 rps blocks re-run at the integrated revision (`workplan.md` WP-2), which needs a quiet box — a quiet window on this box or a borrowed machine of the same storage class. No throughput number is claimed until then.
- **Whether an acknowledged write survives a power cut.** Settled by a crash or power-cut injection at the ack point, committed and bound to a revision (`open-items.md` row `PLAN-powerloss-durability`).
- **The external comparison's postgres leg on the current box.** The server that ran the measured arm is not installed now; settled by reinstalling it and reading its settings back from the running server (`open-items.md` row `PLAN-postgres-absent`).
- **Open-cost behaviour at larger data sizes.** The single recorded open figure is a peak on a 118 MB instance; settled by an open-time measurement across instance sizes.
