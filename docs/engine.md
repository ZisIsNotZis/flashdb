# flashdb — storage engine (v0)

Budget: 170 lines / 26,000 chars

The storage layer resolves ticket `04-storage-soundness`. Shape chosen: **a log-structured document store** — WAL + memtable + immutable tile files + background compaction. Every mechanism is standard LSM practice; the thesis lives in the key-space layout and the learned policy, not here. This document supersedes the tile/relocation/root-flip ideas recorded in earlier rounds; the decisions it encodes were made to close `review-05`.

Reference scale (v0, cgroup-limited): 200 GB data, 8 GB RAM, one NVMe. Data:RAM ≈ 25:1. **Implementation status:** the current prototype has one explicit WAL-retained checksummed tile with safe sequential positional reads and cross-tier P/U/R merge, plus a durable A/B manifest (`publish_tile`/`open_discover`) whose page carries the tile reference and a CSN/handle-watermark checkpoint; a torn manifest page falls back to the older valid page, and structural corruption fails closed. It does **not** yet implement the 16 KiB page/directory tile format, automatic checkpointing, multiple active tiles, compaction, WAL rotation, bounded RAM or recovery-time targets described below.

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
  → group commit: batch pending blocks, assign CSNs, append in CSN order, ONE fdatasync(WAL)
  → apply batch to memtable → publish CSNs → ack
```

- Exactly **one** durability barrier per commit group, and it is on a log — there is no foreground structure over tiles, so the SS-01 ordering hazard (root durable, tile not) cannot arise. Tiles are written only by background compaction.
- The memtable is applied **after** the fsync returns: a reader can never see a block whose WAL record is not yet durable.
- Crash before fsync: an **incomplete** WAL tail is cut at the last valid record; the client may retry, while a complete but unacknowledged record may recover and dedup. A complete CRC-bad frame or impossible length fails closed instead of silently truncating possibly acknowledged data. This still does not distinguish externally truncated acknowledged data from an incomplete unacknowledged tail; the current prototype is not yet evidence of power-loss durability. CSN gaps are harmless.
- `fdatasync` targets the WAL fd only — compaction barriers never delay the foreground (SS-11 resolved).
- Group commit: flush when the oldest pending block has waited **2 ms** or 128 blocks are pending, whichever first. `durable` class = its own immediate flush.

## Snapshots and validation (SS-05 resolved, honestly)

- A snapshot **is a CSN**. Read = newest version with CSN ≤ snapshot, skipping tombstones; the block's own overlay is checked first.
- **Claimed isolation: snapshot reads + first-committer-wins on matched documents.** Not serializable; range scans are not validated. This is the minimal tax, and the previous "phantom protection comes for free" claim is withdrawn rather than patched.
- **Validation set** = every document a block *matched or read by key*: check its current committed CSN == the CSN at read. Probe-condition fields are part of the read set, so a concurrent decrement can cause a timing-dependent validation mismatch; only that kind of contention may be retried on a fresh snapshot. A later probe can then fail the stock guard and conditionally backorder (SS-20). This validation path is design, not implemented in the current single-writer prototype.
- Two blocks writing the same unique value: the **unique check runs at publish**, against the memtable that contains any earlier owner. The later block gets a terminal unique-constraint error, **not** an automatic retry or `else`. Publish-time, not validate-time — without this, duplicates slip through (`review-05` follow-up found this hole in the first draft of this document).
- Blocks of one request commit independently, in dependency order. Resolution domain = committed state (including earlier blocks of the same request) + the block's own overlay.
- Unconstrained by design: scans see a snapshot; phantoms are possible and **documented, not prevented**.

## Tiles, pages, access granule (SS-03 resolved)

- Tile file ≈ **32 MiB** (autotunable), holding **16 KiB pages** (calibrated in experiment 0a-redo, `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`: 16 KB QD1 = 111 µs vs 4 KB = 89 µs vs 64 KB = 174 µs — a 25 % point-read premium over 4 KB buys 4× fewer directory entries, on an ~827 MiB/s device); a page = checksum + a run of documents contiguous in key order + per-page key bounds. This target has **2,048 pages per tile**, not 512; even at 16 bytes per directory entry that is ~32 KiB per tile. The directory cannot be packed into one small A/B root page at scale. Its immutable generation and root reference remain to be specified before a full-size manifest claim; the current tile prototype has no page directory.
- **The access granule is the page.** Calibrated by experiment 0a-redo (reduced-load window, min idle 67 %): a point read = one 16 KB page ≈ **111 µs** at QD1 (4 KB = 89 µs, 64 KB = 174 µs); a batch of 20 independent 4 KB reads at QD32 ≈ **97 µs** amortized vs **1 784 µs** serialized — an **18×** batching gain; a whole 8 MB tile read sequentially ≈ **9.9 ms**, ~100× worse than the batched page reads, which **falsifies** the original collapse-into-a-tile premise. The device tail is benign on a quiet machine (QD1 p99 = 1.26× mean): tail latency here comes from queueing and dependency chains, not device variance. Constants: `.scratch/05-engine-v0/evidence/0a-redo/analysis.md`.

**Reduced-load calibration (2026-09-28 rerun):** 16 KB QD1 ≈ **117.4 µs** (fio; an engine-level 16 KB Rust measurement remains to be made); 4 KB QD1 = **96.3 µs** in both fio and Rust `O_DIRECT read_at` in the same window; 4 KB QD32 = **202.9k / 203.7k IOPS** (fio / Rust), ≈ **4.9 µs per completed read** at saturation; fio sequential 1 MiB QD8 = **831 MiB/s**. Minimum idle during the run was 57%, so these are indicative, not clean-idle constants. An earlier Rust 4 KB QD1 sample of 149 µs under different load was incorrectly called a 16 KB cost and attributed to syscall overhead; neither inference is supported. Source: `.scratch/05-engine-v0/evidence/0a-redo/summary.json` and `nvme-bench-analysis.md`.
- A fan-out (order → its movements) = one prefix scan over `R/…/<order-handle>/` (contiguous, usually one page) + N *independent* point reads. **Dependent depth = 1 + independent fan-out.** Multi-hop relations cost one dependent read per hop, and the trace records follow-depth (SS-22).
- Kernel page cache is the block cache in v0; "cache" means it, with hit-fraction-of-requests as the metric (SS-14).

## Compaction, manifest, checkpoint, recovery (SS-02, SS-21 resolved)

- Compaction: pick inputs → merge-iterate in key order → write new whole tile files → `fdatasync` each → write new manifest page → `fdatasync` → flip. Crash before the flip: the new files are orphans, collected at recovery. Harmless by construction.
- **Version dropping horizon** = the minimum CSN among active reader snapshots. For each logical key, keep every version above that horizon **and the newest version at or below it** as the visible baseline; dropping all older versions would make the oldest snapshot read a nonexistent value. Readers must register a snapshot CSN in a registry before old versions can be discarded; the current prototype accepts arbitrary `snapshot: u64` and has no lease/expiry contract, so it must retain all versions until that boundary is resolved.
- **A tile file is unlinked only when no reader holds it open** (fd-count per file; POSIX keeps the inode alive for open fds). The engine never writes into a file a reader may hold, so the allocator-reuse hazard of SS-02 cannot arise. Unlink-deferred files sit on a trash list persisted in the manifest; recovery re-attempts.
- Manifest: one small file, **A/B pages, checksum + monotonic seq**, no rename; recovery takes the valid page with the highest seq (SS-26: two consecutive torn writes leave both invalid only if both tear — the write-always-inactive-page discipline plus per-page checksum bounds this to a double-torn-write window; accepted, documented).
- Checkpoint: when 128 MB of WAL has accumulated since the last one (or 5 minutes). Checkpoint = flush memtable to an L0 file + fsync + manifest update + WAL rotation. **Recovery budget: replay ≤ 128 MB ≈ ~1–2 s** on the reference NVMe.
- **Filesystems pinned: ext4 and XFS.** btrfs/ZFS/f2fs unsupported in v0 (CoW allocation semantics break the preallocated-WAL cost model). Tile allocation = whole files, so there is **no free-space map** on the tile side; the WAL is preallocated and rotated (SS-21).

## Write path and budget (SS-04 resolved)

- A one-field update = a ~200 B WAL record + a memtable entry. Hot-SKU decrements never touch a tile.
- Write amplification = compaction bytes written ÷ user bytes written, measured live, budgeted (default ≤ 10×, charged to the shared budget). Budget exhaustion → compaction throttles, writes slow — visible, priced, never silently violated.
- The "already sequential" justification is withdrawn; the sound argument for the WAL is small-update durability, which the workload needs.

## What the learner controls in v0

The keyspace makes the thesis concrete: **materialized prefixes are the layout.**

| Learned (offline chooser, v0) | Fixed (v0) |
|---|---|
| which `U` lookup prefixes to materialize (hot non-unique probes) | primary order = document handle |
| which `R` reverse prefixes to retain (co-location of index entries) | all schema-declared `x-ref` reverse tiles materialized at create |
| encoding per prefix (zstd level; BtrBlocks-class later) | compaction style (universal) |
| compaction trigger per prefix-region | page size 16 KB (calibrated); tile target 32 MB |

Clustering-key learning (physically re-sorting documents) is deferred to v1 — it needs dual copies or a handle map and is not required to test the thesis. v0 has no speculation, so speculative/demand trace tagging is moot (SS-23 noted for v1).

## Corrections recorded against earlier documents

- **SS-16:** there is **no per-tile version field**. Versioning is per-document CSN in the key; validation is read-set CSN comparison; "self-describing tile" keeps layout metadata only.
- **SS-24:** derived prefixes live in the same LSM and **do pay fsync**; the earlier claim that they skip it is retracted. `recomputable` now means: the prefix may be dropped and rebuilt from `P/` by scan, without violating I2.
- **SS-18:** the scheduler contract is the group-commit paragraph above; v0 has no request-rewriting batcher, no admission control beyond the in-flight block cap (1024) and shed-by-class.
- **SS-19:** `deadline` exists in the service class; v0 uses it only for shed priority and accounting, not for constraint evaluation.

## Resolution checklist

| Finding | Resolution |
|---|---|
| SS-01 | one WAL barrier on the foreground; background checkpoint protocol; orphans collected |
| SS-02 | immutable whole files + unlink-deferred by fd + snapshot horizon |
| SS-03 | page granule (16 KB, calibrated by experiment 0a), directory in manifest/LRU, bandwidth + latency priced |
| SS-04 | WAL + memtable + compaction; write-amplification budget |
| SS-05 | snapshot reads + first-committer-wins on matched docs; scans unvalidated by design |
| SS-06 | two named global structures: the keyspace layout (in the manifest) and the WAL position; tiles never relocated |
| SS-09 | no relocation exists; compaction priced as LSM merge I/O; page-cache invalidation on tile drop |
| SS-10 | dead — documents are atomic |
| SS-11 | foreground fsyncs only the WAL fd |
| SS-16 | no per-tile version; CSN in key |
| SS-19/20 | deadline field; probe fields in read set; unique checks at publish |
| SS-21 | whole-file allocation; WAL preallocated + rotated; ext4/XFS pinned; recovery ≤ ~2 s |
| SS-22 | hop bound = dependent reads per hop, recorded in trace |
| SS-24 | same LSM; recomputable = rebuildable prefix; fsync-saving claim retracted |
| SS-25/26/28 | sequential claim retracted; A/B discipline + double-torn window documented; tile size owned by this document (32 MB target, autotunable) |
