# 08 — tile pages: indexed access, cheap verification, honest scan rate

Opened 2026-09-30 from the blocked data:RAM experiment (see 07) and `probe_cost`
measurements (07/evidence/probe-cost.md, `b718fab`).

## Findings to fix (all measured, not inferred)

1. **No tile index.** `next_matching` (engine.rs:885) + `Entries` (tile.rs:200) walk
   from the tile's first entry, allocating two `Vec`s and checksumming every entry
   until a prefix hit. Measured probe cost is linear in tile size:
   29 ms @1.4 MB → 1.65 s @90.8 MB (memtable probe: ~2 µs). With `K ≤ MAX_TILES = 8`
   tiles, a point/unique probe is O(total data).
2. **Sequential tile scan rate ≈ 55 MB/s** (90.8 MB / 1.65 s, page cache warm),
   i.e. ~15× below the 831 MiB/s measured device rate. Cause: per-entry `Vec`
   allocation + per-entry checksum, and a whole-file digest instead of per-page CRCs.
   This caps the prototype's *strongest* path (scan) below the device, so no
   throughput claim from this engine is meaningful yet.
3. **Publish/compaction/rotation rewrite and re-verify whole tiles** (O(N) per event,
   ~K× write amplification). Publish re-derives the full projection; open re-verifies
   every tile against the WAL.

## Planned slices (each independently reviewable)

- **08a — page-structured tile format (FDBTILE2)**: 4 KiB pages, per-page CRC, page
  header with entry count + first key, fence-key directory (sparse, ~1 entry/page)
  with its own CRC, fixed trailer. Immutable pages: publish appends only new pages.
  No users exist, so this is a format bump with regeneration from the trace corpus
  rather than a migration path.
- **08b — zero-copy buffered reader**: borrow entries from a page buffer instead of
  allocating per entry; per-page CRC means a probe verifies only the page it read.
  Acceptance: sequential scan ≥ 400 MB/s on cached data, probe = directory binary
  search + one page read.
- **08c — per-tile bloom over declared unique values**: probe answers "definitely
  absent" in RAM (no I/O) or "maybe" (one page read); bits/key a tunable with a
  documented RAM cost.
- **08d — publish verifies only new pages** (whole-tile digest retained at open),
  then re-measure write amplification.
- **08e — re-run the data:RAM experiment** (the 07 run) on the fixed engine.

## Constraint strategy — corrected understanding (2026-09-30, parent + author)

The author clarified the original intent, and the parent's earlier "eager probe per
op" framing was wrong. The mechanism is **projected post-state validation**: a block
is validated against the state it *would* produce (published state ∪ the block's own
net effect), never against the pre-state. That is what makes intra-block joint
feasibility legal — the author's example: A alone cannot insert, B alone cannot
insert, but A+B together are consistent (release-then-reuse, swaps, and the same
shape for expression/aggregate forms).

Ordering the author specified: tentatively write (MVCC overlay) → validate → persist
→ only then claim success.

**Already implemented for uniqueness** in `commit_block` (engine.rs:832-865): the
`touched` overlay keeps the block's net effect per unique prefix (last op wins), it is
merged over the published owners (`live.extend(overlay)`, handle-major/csn-minor
iteration so the newest version per handle wins), and the projected state must have
≤1 live owner. Validation precedes `wal.append` + `sync`, and `Committed` is returned
after the fsync. Now locked by `engine/tests/constraints.rs` (4 tests): release-then-
reuse, value swap, double-claim rejected with nothing durable (no CSN advance, no WAL
growth, no partial block), and release-then-reuse across a *published tile*. Parent
also made the validator fail **closed** on an undecodable key under the unique prefix
(previously skipped, i.e. fail-open in a constraint check; unreachable via the public
API, defense-in-depth).

What remains genuinely deferred/absent:
1. **Other constraint forms have no mechanism at all** — foreign keys, conditional
   uniqueness (unique where status='open'), exclusion/mutual-exclusion, expression and
   aggregate invariants. The generalization is a declarative surface where each form
   provides (a) how to derive its affected key set from a block's ops and (b) how to
   evaluate it over the post-state. Unique is instance #1; every listed form has the
   same shape. This is a v1 design item (links `docs/contracts.md`).
2. **Global/aggregate/audit invariants** (e.g. "sum of lines = header total", cross-
   entity aggregates) have an unbounded affected set, so they cannot be validated by
   probes at publish. They are the honest home of the "deferred" idea: an explicit
   contract class with asynchronous auditing, never the default for unique/FK.
3. **Concurrency isolation** is where MVCC/lag actually pays: with multiple concurrent
   blocks, optimistic validate-at-commit (and bounded retry only for temporal
   optimism, per the author's earlier decision) avoids turning value overlap into lock
   waits. Single-writer v0 has nothing to defer.

Consequence for this ticket: post-state validation is the semantic core and needs no
author decision. The probe set is the block's **net-affected key set**, which
intra-block cancellation can shrink or empty (a swap needs no disk probe for values
whose owners are all in the block). 08c bloom applies to that set: "no published owner"
answers in RAM with zero I/O. 08a/08b are unaffected (pure implementation gaps).
