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

## Next action

Consume read-only storage design analysis (`Designer` run `228c2b3b-6565-49d9-af91-ee06243d0f57`), refine slice boundaries, then implement with separate validation/review. No user decision yet identified.
