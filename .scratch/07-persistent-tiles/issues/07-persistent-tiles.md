# 07-persistent-tiles — crash-safe tile read path and checkpoint

Status: claimed
Owner: parent agent. Base revision: `77537a0`.

## Objective

Evolve the current WAL+memtable prototype toward `docs/engine.md`: immutable tile files, bounded-memory reads and scans across memtable plus tiles, a durable manifest, checkpoint/compaction and recovery without losing acknowledged blocks. Preserve per-block WAL atomicity, unique/reverse key semantics, CSN snapshots and idempotent replay. Do not claim 200 GB / 8 GB or layout-learning results until data is actually served from disk under a bounded RAM test.

## Risk boundary / dependencies

1. Define tile codec and read-through tests with WAL retained. This can establish durable tile correctness without early WAL truncation.
2. Define checkpoint publication (tile data sync before manifest activation), old-file lifetime and recovery; inject faults at each write/sync/flip boundary.
3. Only after manifest/checkpoint recovery passes, rotate/truncate WAL while retaining dedup IDs, CSN and handle high-water marks for the declared retry horizon. A partial WAL rotation must never delete the only durable copy of a published block.
4. Compaction and snapshot horizon follow; benchmark data:RAM ratio and write/read amplification after correctness gates.

## Current facts

`Engine::open` replays the entire WAL into one `Memtable`; `get`, `unique_lookup`, `reverse_lookup` and `scan_primary` read only that memtable. `Wal::open_or_recover` truncates torn tail. There is no tile codec, manifest, compaction, checkpoint, reader registry or fault-injection harness. `docs/engine.md` describes the intended protocol, not implemented behavior.

## Acceptance for first slice

One immutable persisted tile can be written and read through in a test at a snapshot, with CRC/torn-file detection and WAL retained as recovery authority. No acknowledged state can disappear across injected crashes; new writes over tile state must be visible and unique checks must consult both tiers. If this cannot be done safely as a narrow slice, document the blocker rather than pretending it is a checkpoint.

## Design checkpoint — 2026-09-28

Read-only Designer run `228c2b3b-6565-49d9-af91-ee06243d0f57` recommends A: harden WAL write/sync failure poison and define immutable tile codec → B: WAL-retained tile read-through → C: manifest/checkpoint → D: WAL rotation with durable dedup/CSN/handle metadata → E: compaction/snapshot pins → F: scale claims. First tile slice must actually evict covered versions from serving memtable after verified tile creation; on reopen replay WAL for metadata but skip only a verified covered prefix. Cross-tier P/U/R latest-visible/tombstone and uniqueness checks must be tested. A missing/corrupt tile fails closed (or explicitly rebuilds from retained WAL); no silent fallback. No WAL retirement or bounded recovery claim in B.

Two future user-owned choices are not blockers for B: arbitrary historical snapshots vs expiring registered snapshots (recommend leases before version dropping); large manifest layout at 32 MiB/16 KiB = 2,048 pages/tile, not 512 (recommend A/B roots referencing synced immutable directory generations). L-02 dedup retention remains open, so retain WAL and permanent dedup for now. Routine correction: compaction must keep the newest version at/below the oldest active snapshot, not drop all below it. Source of truth `docs/engine.md` needs this correction before compaction implementation.

## Next action

Parent hardens `wal.rs` append/sync failure poisoning in current worktree; isolated worker run `037e8d72-8a07-401d-9c45-93cebb4e5028` builds explicit WAL-retained immutable tile read-through (no manifest/rotation). WAL change: a short/failed append or failed sync poisons writer; subsequent append/sync rejects until `open_or_recover` truncates/recovers; preflight oversize/empty rejection does not poison. Unit fault injection covers partial frame and complete-but-unacked sync failure. `cargo test --workspace --offline -q` 34 unit + 7 order-flow + 5 read + 3 replay passed on current working revision, `git diff --check` passed. These injected errors model control flow, not power-loss persistence on ext4/XFS. Integrate tile slice after independent review; no user decision required yet.
