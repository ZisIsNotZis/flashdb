# 04-storage-soundness — the engine is unsound in four specific ways

Status: needs-info (four items need a design decision, several need specification)
Blocked by: resolutions 1–4 below
Ticket: this file. Decision ticket — question in, decision out.
Parent: `.scratch/01-design/` (the review that produced this list)

## Why this exists

A storage-systems review (`review-05`, aspect: storage systems and hardware) is the first to attack the **engine** rather than the claims around it. It found the durability model unsound in four concrete, independently fatal ways, plus fourteen classes of undefined mechanism. Reviews 01–04 attacked the case *for* the design; this attacks the design.

The false claims it identified have been corrected in place (`contracts.md`, `design.md`, `objective.md`, `learning.md`). What remains is the work, and it is not cosmetic: **two of these four make silent wrongness possible, which breaks I2 — "a bad layout may be slow, never wrong."**

## The four soundness defects

| # | Defect | Why it is unsound | Decision required |
|---|---|---|---|
| **SS-01** | **`fdatasync` is a completion barrier, not an ordering barrier.** The contract claimed "a single `fdatasync` flushes data and root together" while the commit recipe said `write tiles → fsync → flip root`. | On a crash the root page can be durable while a tile it references is not, so recovery dereferences a half-written tile. As written, crash-atomicity does not hold. | Pick one: two barriers; or one barrier plus per-tile content checksums recorded in the commit record with fallback to the previous root, which needs a checksum schema that does not exist; or the root in a separate file with its own barrier. State the fsync count in Contract 2 once. |
| **SS-02** | **Space reuse versus in-flight readers.** "Never free a tile any live root might reference" protects roots, not readers of old roots. | A reader loads root `R_k`, is descheduled arbitrarily long, the writer commits `R_{k+1}`, orphan GC frees `T` and the allocator reuses its space for `T′`. The reader then reads a *valid* tile with a *valid* checksum and returns wrong rows, silently. | Reader epochs / hazard pointers with an RCU-style grace period, or never reuse space until every reader that could have loaded an older root has drained, or reads pinned through a refcounted root. Then a recovery and startup budget for a 10 TB store. |
| **SS-04** | **There is no update path.** Tiles are immutable and ~8 MB, so a one-row stock decrement rewrites the whole tile. | At 5,000 stock ops/s on one hot SKU that is tens of GB/s before any derived copy — the entire shared budget, on the workload the design was written for. The claim "a payload WAL buys nothing" was false: a payload WAL exists to make a 100-byte update durable without rewriting a megabyte. | Choose a mutation granularity — per-tile delta/overlay tiles with read-merge and durability rules, or a smaller mutable granule — and add a **write-amplification budget in bytes per second** that the objective charges. |
| **SS-05** | **"Phantom protection comes for free" was false.** Tile-set changes are invisible to per-tile versions. | A writer inserts key 50 and splits `T1` into `T1a`/`T1b`. `T1` was not mutated, it was *replaced*, so its version is unchanged, validation passes, and the phantom is missed. Merges leak obsolete rows the same way. | Validate against a **root generation / snapshot epoch** and re-enumerate range membership at commit (or validate the routing structure), not against per-tile versions. Also: whether read-only requests validate at all, and the isolation level per service class. |

## The global structures that I3 denies exist

**SS-06.** I3 says "no global layout state", but every key-range read needs a **routing map** from key range to tile, and reorg changes it. A parent tile holding child addresses cannot be relocated without rewriting every referrer, so either addresses are logical (`tile_id`, offset) with a durable id→offset map — which is global state — or the tile is not self-contained. Restate the invariant as: **tile payloads are position-independent; the tile→offset map and the routing map are the only global structures, and the root flip publishes them.** And distinguish *relocation* (a copy) from *rearrangement* (`glossary.md`'s definition, which is read → regroup → re-encode → rebuild directory → write).

**SS-09.** Relocatability is not free and none of its cost is charged: stable addresses, raw child pointers, page-cache readahead state (relocating a hot tile re-faults its pages), and cross-tile references wider than 32 bits at 10 TB. Replace "reorg is a `memcpy`" with an itemized cost table: read bytes, re-encode CPU, allocate, write bytes, barriers, routing update, retention space, page-cache invalidation, read-set invalidation, GC.

**SS-10.** Per-tile independent column groups with per-tile independent clustering keys **cannot be composited into a row** without a stable row/key identity present in *every* tile. `T1={a,b}` clustered by `a` and `T2={c,d}` clustered by `c` requires one lookup in each, in different orders — two dependent reads. State the cross-tile row-identity invariant (every tile carries the full clustering key, or a stable row id) and accept the space duplication, or drop the claim for base tiles.

## Missing mechanisms

None of the following exists anywhere in the repository, and each is load-bearing:

1. **Device model as an input.** No probe, no descriptor (class, queue depth, read/write IOPS and bandwidth, sector size, power-loss protection, rotational/FTL), no consumer, no `device_class` field. The HDD policy appears nowhere, and the scheduler row pointed at a document that does not describe it. **Also: no placeholder number in `objective.md` may be read without naming the device class.**
2. **Access granule.** Device block size, alignment, sub-tile directory layout, readahead policy, and whether a point read touches a page or the whole tile. The entire latency model depends on this.
3. **Whose cache.** Kernel page cache or engine buffer pool — the 1.3% figure, cache pollution and the telemetry cap all presuppose an owner and an eviction policy that is never named.
4. **Placement and free-space allocation.** Who assigns a tile's file offset, how holes are chosen, fragmentation, whether the allocator is durable, and how file offsets relate to device physical locality. Without this the sorted-sweep HDD policy is unimplementable above a filesystem.
5. **Recovery and startup.** Free-space map reconstruction, reader-epoch handling, and a time budget for a 10 TB store. SS-21: rebuilding the free-space map from the root is a walk of up to ~1.25 M tiles, unstated and unpriced; keeping it in-file makes it a durable structure with its own ordering requirements.
6. **Checksums and corruption.** Per-tile content checksum, torn-write detection for data (not only the root), scrubbing, bad-block handling.
7. **The scheduler contract (SS-18).** Mergeability predicate, maximum wait, per-request slot reservation, admission control, group-commit sizing, head-of-line rules, and how validation interleaves with the root flip.
8. **Exact tile header schema.** Fields, size, alignment, compatibility, and the decoder registry — plus the obligation created by "any reader can consume any tile" together with perpetual drift: **carrying every layout ever learned, forever.** "No migration project" has that as its price and it is never stated.
9. **Durability test matrix.** Which filesystems (ext4, XFS, btrfs, ZFS, f2fs), which device classes, with or without power-loss protection, and the fault-injection tooling. SS-21 also notes the scheme presumes in-place overwrite semantics that btrfs/ZFS/f2fs do not provide, so the supported set must be pinned and the unsupported set named.
10. **Read/write asymmetry and endurance.** One `service_time` and one `utilization` cannot express separate read and write capacity, and there is no wear term — despite a design that rewrites the same hot bytes forever.
11. **A sized shared budget.** "One shared finite budget (cache, write amplification, reorg bandwidth, background IO)" is declared and never quantified.
12. **Retry and livelock control** (SS-05): retry bounds, backoff, and the admission rule that keeps a hot-tile validation loop from consuming the device.
13. **Multi-volume story.** One preallocated file on one device is assumed; nothing says what happens when the store outgrows the preallocation.
14. **Recomputable storage location (SS-24).** If trace and derived tiles share the base file, foreground `fdatasync` flushes their dirty pages too, so the claimed fsync saving does not exist; if they live elsewhere, the commit barrier does not cover them and the root can reference a half-written accelerator after a crash. State where they live, whether they are checksummed, and what recovery does with a root referencing a non-durable accelerator.

## Smaller but real

- **SS-12 / SS-13.** The objective has almost no gradient where storage decisions are made (saturated comparisons) and is **invariant to uniform per-request slowdowns**, because the SLO anchor floats with measured `service_time` — so the commit fsync and decode costs this design adds are exactly what it cannot see. Needs an absolute floor or a deploy-time reference capability.
- **SS-15.** "Less space means fewer misses" holds only under uniform random access, which this design's own Zipfian rule rejects; shrinking can also split a co-located fan-out. Restate with the failing conditions and define the space unit.
- **SS-17.** The canary cannot supply I11's "demonstrated win" (it is declared a conservative lower bound), and the simulator is admitted uncalibrated — so I11's proof requirement is satisfiable by neither instrument. Say which mechanism supplies the proof, or weaken I11.
- **SS-08.** No construction enforces the 4 GiB tile ceiling; signedness of "32-bit" is unspecified; an in-tile blob area *is* indirection, contradicting the prior-art row.
- **SS-26.** A/B root pages tolerate one torn write; two consecutive torn writes leave both invalid. State the always-write-the-non-current-page discipline and the checksum coverage.
- **SS-28.** Tile size has three owners (`glossary.md` "chosen against the device", `design.md` "8 MB", the layout contract "tile size N"). Pick one.

## Acceptance criteria

1. SS-01, SS-02, SS-04 and SS-05 each have a stated resolution, and `contracts.md` contains exactly one commit protocol, one validation granule, and one mutation granularity.
2. The global structures are named, and I3 is restated to permit them.
3. Every item in "Missing mechanisms" either exists in a document or is explicitly declared out of scope with its consequence stated.
4. A durability test matrix names the supported filesystems and device classes, and a fault-injection harness exists before any throughput number is claimed.
5. No claim in `contracts.md` rests on in-place overwrite semantics without naming the filesystem assumption.

## Resolution (partial, 2026-09-15, from the author)

**SS-04 resolved: keep the WAL, memtable and background compaction.** The author delegated this decision with one constraint — minimise the "transaction tax" — and the WAL is the low-tax choice, not the high-tax one: it makes a 100-byte update durable without rewriting an 8 MB tile, whereas the immutable-tile-only alternative pays tens of GB/s of write amplification on a hot SKU. Consequences: a payload WAL stays; writes go to a memtable and are journalled per block with their `$n` bindings; background compaction rewrites tiles into the learned layout; and the write-amplification budget is charged in bytes per second to the shared budget.

**Isolation is reduced to the minimum real workloads use,** per the author's constraint: block-level atomicity, optimistic validation of the keys a block touches, conflicts retried per block. **Range reads are not serializable by default** — they see a snapshot; serializable range semantics are opt-in via re-enumeration at commit, which also removes the retry-storm risk SS-05 identified for hot tiles.

**Device: NVMe-only for v1.** SATA SSD, RAID and rotational media are explicitly unsupported; the device descriptor and the sorted-sweep policy are deferred with them.

**Scale: v0 targets the ratio, not the absolutes.** 10 TB / 128 GB (78:1) is a far target; v0 runs cgroup-limited at the same or a stated ratio, and the ratio itself becomes an experimental variable the harness can sweep.

Still open: SS-01 (commit ordering protocol), SS-02 (reader epochs), SS-05 (snapshot generation), and the missing-mechanism list.

## Final resolution (2026-09-15)

The storage layer is now specified in `docs/engine.md`: a log-structured document store (WAL + memtable + immutable tile files + background compaction), one keyspace with four prefix families (primary / x-unique lookup / x-ref reverse), document-CSN versioning, 64 KB page access granule, single-barrier foreground commit, unlink-deferred file lifetime with a snapshot horizon, and ext4/XFS pinned.

Findings resolved: SS-01, SS-02, SS-03, SS-04, SS-05, SS-06, SS-09, SS-10 (dissolved by the document model), SS-11, SS-16, SS-19, SS-20 (publish-time unique checks — a hole found in the first draft of the resolution itself), SS-21, SS-22, SS-24, SS-25, SS-26, SS-28.

Corrections made against this repository's earlier text: "phantom protection comes for free" (withdrawn — snapshot reads + first-committer-wins on matched documents, scans unvalidated by design); "a payload WAL buys nothing" and "the data path is already sequential" (withdrawn); "derived structures skip fsync" (retracted — same LSM); "reorg is a memcpy" and tile relocation (dropped — whole immutable files, no relocation exists); per-tile version field (dropped — CSN in key).

Deferred to v1, with reasons recorded in `docs/engine.md`: clustering-key learning, speculation, canary, multi-hop `$via`, general constraint DSL.

## Not in scope

Re-running the storage review. This ticket resolves the findings it produced.
