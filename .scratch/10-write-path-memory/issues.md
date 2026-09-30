# 10 — write-path memory discipline (blocks the data:RAM claim)

Opened 2026-09-30 from the second scaled run (2M docs × 1 KiB, `MemoryMax=1G`,
systemd scope, page cache counted by the cgroup).

## Result of that run

The 08a-08c read-path work removed the old cliff completely:

| | before 08a-08c | after |
|---|---|---|
| seed progress | 200 k docs then stalled ~9 min | 800 k docs in **29 s** (~27.6 k docs/s) |
| outcome | never finished | **SIGKILL (exit 137)** at `rss_peak_kb=600132` |

RSS trajectory (seed progress lines): 200 k docs → 134 MB, 400 k → 573 MB,
600 k → 596 MB, 800 k → 551 MB (peak 600 MB). It **plateaus**: the working set is
not proportional to data, it is proportional to the WAL/checkpoint window (64 MB in
this run) times a large constant. With ~8-10× that window the process alone reaches
600 MB; the cgroup counts page cache too, so a 1 GB cap kills it. Data:RAM at that
point was ~250 MB data : 600 MB RAM — the opposite of the ≥20:1 objective.

## Findings to fix (measured, code-traced)

1. **The publish/verify path keeps several full materializations of the WAL window.**
   `write_verified_tile` (engine.rs:346) does `wal::replay` (all suffix payloads as
   `Vec<Vec<u8>>`), builds an `expected` `Memtable` from every record
   (`apply_op` per op), then `newest_projection(&self.mt, …)` builds *another*
   `Memtable` copy of the same content, and `Tile::write` materializes each block. At
   least three live copies of one window; per-op `Vec` keys/values in a `BTreeMap` add
   several times the payload bytes.
2. **`Memtable` overhead is unquantified.** `BTreeMap<Vec<u8>, Vec<u8>>` with
   per-entry allocations has a bytes-in-RAM per byte-of-data factor that nobody has
   measured. The 20:1 claim needs that number stated (and lowered).
3. **Rotate/checkpoint thresholds are the memory knob.** Since the working set is ∝
   window, `rotate_wal(max_wal_bytes)` and `maybe_checkpoint(max_wal_bytes)` implicitly
   set peak RAM; today they are ad-hoc call-site values (64 MB in the bench), and
   nothing enforces or documents a memory budget.
4. **Reopen verification is also O(data) in RAM.** `open_impl` builds `covered` and
   `mt` memtables while replaying the suffix and projects per tile
   (`newest_projection`) — bounded by the suffix for the WAL side, but
   `write_compacted_tile` (segment mode) additionally materializes *every tile entry*
   into one `expected` memtable before reducing it, i.e. RAM ∝ total tile bytes at
   compaction time. That is the same defect class as (1) and must be fixed in the same
   pass (stream the k-way tile merge instead of materializing it).

## Direction

- Single materialization rule: the serving memtable is the only full copy of a window;
  tile write and verification stream against it plus a bounded, key-grouped view of the
  WAL suffix (the suffix is already bounded by the rotate threshold).
- Verification should not rebuild the projection from the WAL in RAM: stream the tile
  being written (block by block) against the same source, and keep the WAL digest as the
  integrity anchor.
- Quantify and reduce `Memtable` bytes per payload byte (arena/sorted-Vec layout instead
  of `BTreeMap<Vec,Vec>` is the obvious candidate).
- Make the window size an explicit, documented memory budget, then re-run the scaled
  experiment and report data:RAM and peak RSS vs window.

## Acceptance for closing

A scaled run at ≥ 10:1 data:RAM completes under a cgroup cap with peak process RSS
reported, and the reported RSS is consistent with the stated per-window budget (with the
multiplier documented, not asserted).
