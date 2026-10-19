# flashdb — storage engine (v0)

Budget: 170 lines / 26,000 chars

The storage layer resolves ticket `04-storage-soundness`. Shape chosen: **a log-structured document store** — WAL + memtable + immutable tile files + compaction, with the scheduler outside the engine. Every mechanism is standard LSM practice; the thesis lives in the key-space layout and the learned policy, not here. This document supersedes the tile/relocation/root-flip ideas recorded in earlier rounds; the decisions it encodes were made to close `review-05`.

Reference scale (v0, cgroup-limited): 200 GB data, 8 GB RAM, one NVMe. Data:RAM ≈ 25:1. **Implementation status.** The prototype publishes multiple ordered tiles (strictly increasing, non-overlapping CSN ranges) through a chained A/B manifest whose capacity is the policy constant `MAX_TILES = 4096` (`engine/src/manifest.rs`; re-exported as `MAX_ACTIVE_TILES`, `engine/src/engine.rs:469`), merges them with the memtable for latest-state reads, and compacts with `maybe_compact_shape` (three run shapes, per-region `merge_min_tiles` trigger) or `compact()` for a whole-dataset collapse that keeps only the newest version per logical key. Tiles carry the `FDBTILE3` page/directory format described below, WAL rotation exists (`rotate_wal` retires a fully published segment onto `data-<cutoff>.wal`, named in the manifest), a torn manifest page falls back to the older valid page, structural corruption fails closed, and every published tile is verified against the retained WAL at open. What is still absent is named where it is argued: no power-loss proof (§Foreground commit), no engine-internal scheduler — checkpoint, rotation and compaction are calls the opening process makes (§Compaction, manifest, checkpoint, recovery), no bounded recovery time or RAM (same section), and no sweep for tiles retired by a lazy flip (same section).

## Key space — one LSM, four prefix families

One keyspace, keys sorted byte-wise, every entry versioned by the **CSN** (commit sequence number) of the block that wrote it. A document is never split; a document handle is engine-assigned, 64-bit, never reused.

```
P/<entity>/<handle>                              → document (primary)
U/<entity>/<unique-field>/<value>                → handle   (x-unique: enforcement + lookup)
R/<entity>/<ref-field>/<target-handle>/<source>  → ø        (x-ref reverse: automatic backward)
```

- `U` **is** the uniqueness enforcement: a write checks the entry absent (or self) before publish. It is also the lookup path for unique-field probes. One structure, two roles.
- `R` **is** the automatic backward traversal: prefix scan `R/<entity>/<ref>/<target>/` returns all referencing handles, contiguously — co-location of index entries by key order is free. `reverse: "unique"` = at most one entry per target; second insert → `reverse_unique_violation`.
- An entity with no declared `x-unique` has no `U` prefix: references to it always `cannot_resolve`, and it is reachable only by scan. Consistent with the reference model.

All four families live in the same LSM, so one WAL covers everything (see Corrections: the earlier claim that derived structures skip fsync is retracted).

## Foreground commit — one barrier (SS-01 resolved)

```
block executes against snapshot + its own overlay (RAM)
  → validate: read-set versions + constraints on overlay final state + x-ref resolution
  → serialize: one WAL record = all keyspace mutations of the block (P/U/R, payloads included)
  → group commit: append in CSN order + apply to memtable immediately, ONE fdatasync(WAL) on a
    background FIFO worker → ack only after that covering fdatasync returns
```

- Exactly **one** durability barrier per commit group, and it is on a log — there is no foreground structure over tiles, so the SS-01 ordering hazard (root durable, tile not) cannot arise. Tiles enter the active list only from a checkpoint publish or a compaction, never from the commit path; the derived index writes a sidecar file outside that list.
- The memtable is applied **before** the covering `fdatasync` returns (a grouped commit appends and
  applies immediately), so a reader can observe a block whose WAL record is not yet durable. That is
  harmless: the memtable is only a cache — the **ack**, and every state-mutating
  publish/compaction/rotation, waits for the barrier, so nothing non-durable is ever made durable.
- Crash before fsync: an **incomplete** WAL tail is cut at the last valid record; the client may retry, while a complete but unacknowledged record may recover and dedup. A complete CRC-bad frame or impossible length fails closed instead of silently truncating possibly acknowledged data. This still does not distinguish externally truncated acknowledged data from an incomplete unacknowledged tail; the current prototype is not yet evidence of power-loss durability. CSN gaps are harmless.
- `fdatasync` targets the WAL fd only — compaction barriers never delay the foreground (SS-11 resolved).
- The barrier is **overlapped, not weakened**: one FIFO background worker runs `fdatasync` on a
  cloned WAL fd while the writer appends the next group, bounded by `MAX_PENDING_BARRIERS = 64` in
  flight. A failed `fdatasync` fails the group closed (no ack) and poisons the WAL until reopen.
  Group commit still owes exactly one covering barrier per group; the change is only who waits and
  when. (The measurement driver forms a group from requests already due, capped at
  `--group-requests`; it is never held open to fill.)

## Snapshots and validation (SS-05 resolved, honestly)

- **Resolved (2026-09-29): historical reads do not exist.** Every read sees the latest committed state; there is no public snapshot parameter, no reader registry, and no snapshot lease. The earlier snapshot-read isolation discussion below is retained only as design history for the future concurrent-writer work.
- Read = newest version of each logical key, skipping tombstones (a tombstone is itself a "newest state": deletion); the block's own overlay is checked first.
- **Validation set** = every document a block *matched or read by key*: check its current committed CSN == the CSN at read. Probe-condition fields are part of the read set, so a concurrent decrement can cause a timing-dependent validation mismatch; only that kind of contention may be retried on a fresh snapshot. A later probe can then fail the stock guard and conditionally backorder (SS-20). This validation path is design, not implemented in the current single-writer prototype.
- Two blocks writing the same unique value: the **unique check runs at publish**, against the memtable that contains any earlier owner. The later block gets a terminal unique-constraint error, **not** an automatic retry or `else`. Publish-time, not validate-time — without this, duplicates slip through (`review-05` follow-up found this hole in the first draft of this document).
- Blocks of one request commit independently, in dependency order. Resolution domain = committed state (including earlier blocks of the same request) + the block's own overlay.
- Unconstrained by design: scans see a snapshot; phantoms are possible and **documented, not prevented**.

## Tiles, pages, access granule (SS-03 resolved)

- Tile file: a `FDBTILE3` file — padded superblock, variable-length page-sized data blocks, an optional CRC-protected bloom section, a CRC-protected directory of fence records that **stays on disk**, and a fixed trailer (`engine/src/tile.rs`). The page size is a per-region `LayoutPolicy` value written into the tile's superblock (`MIN_PAGE_SIZE`..=`MAX_PAGE_SIZE`, 4 KiB..=64 KiB, **4 KiB default**), so a reader honours the stored value and a bad policy is slow, never wrong. RAM holds a sparse index with one entry per *directory block*, not one per data block: a lookup binary-searches it to pick one directory block, reads and verifies that block on demand, then binary-searches its fences — a wrong candidate costs one extra read and never a wrong answer. Tile size is whatever one publish or one bounded merge writes; there is no tile-size target in the code. The 16 KiB page calibrated in experiment 0a-redo (`.scratch/05-engine-v0/evidence/0a-redo/analysis.md`: 16 KB QD1 = 111 µs vs 4 KB = 89 µs vs 64 KB = 174 µs — a 25 % point-read premium over 4 KB buys 4× fewer directory entries, on an ~827 MiB/s device) is therefore a **policy choice, not the shipped default**: a 32 MiB tile of 16 KiB pages holds 2,048 pages and a ~32 KiB directory, which the in-tile directory accommodates without any separate root page. Measured on a 20,000-document seed: one 6.7 MB tile, 40,000 entries, `page_size=4096`, `dir_len=72,468`, 99 fences (`README.md` §Try it).
- **The access granule is the page.** Calibrated by experiment 0a-redo (reduced-load window, min idle 67 %): a point read = one 16 KB page ≈ **111 µs** at QD1 (4 KB = 89 µs, 64 KB = 174 µs); a batch of 20 independent 4 KB reads at QD32 ≈ **97 µs** amortized vs **1 784 µs** serialized — an **18×** batching gain; a whole 8 MB tile read sequentially ≈ **9.9 ms**, ~100× worse than the batched page reads, which **falsifies** the original collapse-into-a-tile premise. The device tail is benign on a quiet machine (QD1 p99 = 1.26× mean): tail latency here comes from queueing and dependency chains, not device variance. Constants: `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`.

**Reduced-load calibration (2026-09-28 rerun):** 16 KB QD1 ≈ **117.4 µs** (fio; an engine-level 16 KB Rust measurement remains to be made); 4 KB QD1 = **96.3 µs** in both fio and Rust `O_DIRECT read_at` in the same window; 4 KB QD32 = **202.9k / 203.7k IOPS** (fio / Rust), ≈ **4.9 µs per completed read** at saturation; fio sequential 1 MiB QD8 = **831 MiB/s**. Minimum idle during the run was 57%, so these are indicative, not clean-idle constants. An earlier Rust 4 KB QD1 sample of 149 µs under different load was incorrectly called a 16 KB cost and attributed to syscall overhead; neither inference is supported. Source: `.scratch/05-engine-v0/evidence/0a-redo/summary.json` and `nvme-bench-analysis.md`.
- A fan-out (order → its movements) = one prefix scan over `R/…/<order-handle>/` (contiguous, usually one page) + N *independent* point reads. **Dependent depth = 1 + independent fan-out.** Multi-hop relations cost one dependent read per hop, and the trace records follow-depth (SS-22).
- Kernel page cache is the block cache in v0; "cache" means it, with hit-fraction-of-requests as the metric (SS-14).

## Compaction, manifest, checkpoint, recovery (SS-02, SS-21 resolved)

- Compaction: pick inputs → merge-iterate in key order → write new whole tile files → `fdatasync` each → write the new manifest root → flip → unlink the retired inputs. Crash before the flip leaves the new files as orphans, which discovery ignores. There is **no orphan sweep**: `Engine::apply_compaction_job` flips the root lazily (`Manifest::publish_lazy`) and does not unlink its retired inputs, so they stay on disk until something else removes them, and a measured run's directory kept ~38 such retired tiles (`docs/plan/open-items.md` row `PLAN-orphan-cleanup`).
- **Version dropping (resolved 2026-09-29): unconditional.** Because no read can address historical state, compaction keeps only the newest version per logical key and may drop every superseded version with no horizon computation. The earlier "keep the baseline for the oldest registered snapshot" rule is withdrawn together with the snapshot feature it served.
- **A tile is unlinked only once the manifest no longer names it**, and POSIX keeps the inode alive for any open fd, so the engine never writes into a file a reader may hold and the allocator-reuse hazard of SS-02 cannot arise. The durable paths (`compact`, `merge_run`, `merge_region_run`) flip durably and then unlink best-effort; `apply_compaction_job` does not unlink at all (previous bullet). No trash list is persisted, which is why the orphan count can be non-zero.
- Manifest: one file, **A/B pages, checksum + monotonic seq**, no rename, plus chained continuation pages so the tile list reaches `MAX_TILES = 4096` (`CHAIN_MAX_RECORDS` per continuation page, bounded at `MAX_CHAIN_PAGES = 316`, self-checked by a compile-time assertion); recovery takes the valid page with the highest seq and a malformed chain fails closed (SS-26: two consecutive torn writes leave both invalid only if both tear — the write-always-inactive-page discipline plus per-page checksum bounds this to a double-torn-write window; accepted, documented).
- Checkpoint and rotation are **caller-driven**: `maybe_checkpoint(max_wal_bytes)` publishes everything through the current CSN as `tile-<cutoff>.tile` once the retained WAL exceeds the threshold, and `rotate_wal(max_wal_bytes)` then retires that fully published segment onto `data-<cutoff>.wal`, recorded in the manifest. There is no timer and no background scheduler in the engine — `Engine::maybe_checkpoint` states that callers invoke it explicitly; each driver passes its own threshold (`flashdb-scale`: 64 MiB checkpoint and 64 MiB rotate; `flashdb-load`: `--checkpoint-bytes`/`--rotate-bytes`, opt-in, and with neither flag one growing WAL and no tiles). The **recovery budget of replay ≤ 128 MB ≈ ~1–2 s is a design target, not an enforced bound**: open streams every published tile and verifies it against the retained WAL, so open time grows with the data and with the retained suffix, and the only recorded figure is a peak, not a time — an 88 MB open-time peak on a 118 MB, 6-tile instance (`engine/src/engine.rs`, `open_impl`).
- **Filesystems pinned: ext4 and XFS.** btrfs/ZFS/f2fs unsupported in v0 (CoW allocation semantics break the preallocated-WAL cost model). Tile allocation = whole files, so there is **no free-space map** on the tile side. The WAL is rotated; it is **not preallocated** — recovery tolerates an all-zero preallocated tail (`engine/src/wal.rs`) but nothing creates one (SS-21's preallocation half is open, `docs/plan/open-items.md`).

## Write path and budget (SS-04 resolved)

- A one-field update = a ~200 B WAL record + a memtable entry. Hot-SKU decrements never touch a tile.
- Write amplification = compaction bytes written ÷ user bytes written, measured live by the drivers (`flashdb-scale` reports `tile_bytes_compact` against `payload_bytes`). A merge call takes a byte budget and does not select a run that exceeds it, so a small budget delays reorganisation instead of slowing writes. **No throttling exists**: nothing slows or refuses foreground writes when a write-amplification budget is exceeded.
- The "already sequential" justification is withdrawn; the sound argument for the WAL is small-update durability, which the workload needs.

## What the learner controls in v0

The learner's v0 surface is the engine's per-region policy actuator, driven by an out-of-process controller (`harness/flashdb_harness/layout_learner.py`): it rewrites a JSON policy file that the driver re-reads every window (`--policy-file`, `--policy-reload`), and the policy affects only new tiles and future merges because each tile stores its own page size. Nothing else about the layout is learned, and the policy is not persisted.

| Learned (controller, v0) | Fixed (v0) |
|---|---|
| per-region page size, `{4096, 65536}` | primary order = document handle (no clustering, no re-sorting) |
| per-region compaction trigger, `merge_min_tiles` ∈ `{2, 8, 200}` | every declared `x-unique` `U` prefix and every declared `x-ref` `R` prefix is materialized — not chosen |
| `index_order_no` for `P|StockMovement`: the derived `order_no → handle` access path, demand-gated and sticky per run | no compression and no per-prefix encoding (`engine/` has no codec dependency) |
| — | page size 4096 and bloom 10 bits/key wherever the policy names nothing |
| — | compaction run shape from the driver's `FLASHDB_COMPACT_SHAPE` (`cheapest_pair`, `ratio_group`, `budget_prefix`), merge budget per call |
| — | region partition = `(family, entity)` prefix, selected by `FLASHDB_REGION_MODE`/`--region-mode`, not learned |

Clustering-key learning (physically re-sorting documents) is deferred to v1 — it needs dual copies or a handle map and is not required to test the thesis. v0 has no speculation, so speculative/demand trace tagging is moot (SS-23 noted for v1).

## Corrections recorded against earlier documents

- **SS-16:** there is **no per-tile version field**. Versioning is per-document CSN in the key; validation is read-set CSN comparison; "self-describing tile" keeps layout metadata only.
- **SS-24:** derived prefixes live in the same LSM and **do pay fsync**; the earlier claim that they skip it is retracted. `recomputable` now means: the prefix may be dropped and rebuilt from `P/` by scan, without violating I2.
- **SS-18:** the scheduler contract is the group-commit paragraph above; v0 has no request-rewriting batcher and no admission control — the only in-flight bounds are the driver's group size (`--group-requests`) and the barrier worker's `MAX_PENDING_BARRIERS`.
- **SS-19:** `deadline` exists in the service class; v0 uses it only for shed priority and accounting, not for constraint evaluation.

## Resolution checklist

| Finding | Resolution |
|---|---|
| SS-01 | one WAL barrier on the foreground; caller-driven checkpoint protocol; orphans ignored by discovery, not collected |
| SS-02 | immutable whole files + unlink once the manifest stops naming them (no trash list); snapshot horizon withdrawn |
| SS-03 | page granule is a per-region policy (4–64 KB, 4 KB default); directory kept in the tile with a sparse RAM index; bandwidth + latency priced |
| SS-04 | WAL + memtable + compaction; write-amplification measured, not throttled |
| SS-05 | first-committer-wins on matched docs; scans unvalidated by design (no historical reads, no snapshot parameter) |
| SS-06 | two named global structures: the tile list plus live WAL segment (the manifest) and the prefix spelling in the keys; tiles never relocated |
| SS-09 | no relocation exists; compaction priced as LSM merge I/O; no page-cache invalidation on tile drop is implemented |
| SS-10 | dead — documents are atomic |
| SS-11 | foreground fsyncs only the WAL fd |
| SS-16 | no per-tile version; CSN in key |
| SS-19/20 | deadline field; probe fields in read set; unique checks at publish |
| SS-21 | whole-file allocation; WAL rotated (preallocation not implemented); ext4/XFS pinned; recovery time is a design target, not enforced |
| SS-22 | hop bound = dependent reads per hop, recorded in trace |
| SS-24 | same LSM; recomputable = rebuildable prefix; fsync-saving claim retracted |
| SS-25/26/28 | sequential claim retracted; A/B discipline + double-torn window documented; tile size is whatever a publish or bounded merge writes (no target in code) |
