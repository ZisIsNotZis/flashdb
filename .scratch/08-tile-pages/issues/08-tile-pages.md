# 08 — tile pages: indexed access, cheap verification, honest scan rate

Opened 2026-09-30 from the blocked data:RAM experiment (see 07) and `probe_cost`
measurements (07/evidence/probe-cost.md, `b718fab`).

## Findings to fix (all measured, not inferred)

1. **No tile index.** `next_matching` (engine.rs:885) + `Entries` (tile.rs:200) walk
   from the tile's first entry, allocating two `Vec`s and checksumming every entry
   until a prefix hit. Measured probe cost is linear in tile size:
   29 ms @1.4 MB → 1.65 s @90.8 MB (memtable probe: ~2 µs). With `K ≤ MAX_TILES = 8`
   tiles, a point/unique probe is O(total data).
2. **Scan rate was below the device rate** — CORRECTED 2026-09-30: the parent's
   earlier "≈55 MB/s" figure was inferred from *probe* timings of the pre-08a code and
   misstated as the scan rate. Measured full-scan rates: ~324 MiB/s at `d641cca`
   (after 08a, before 08c) vs the 831 MiB/s device rate. Cause: byte-at-a-time CRC-32C
   (6.8-8.3 us per 4 KiB block), i.e. the engine is CPU-bound, not device-bound.
   Per-entry `Vec` allocation + per-entry checksum was the earlier, larger cause and
   is fixed by 08a/08b (allocation removal alone bought ~4% after 08a).
3. **Publish/compaction/rotation rewrite and re-verify whole tiles** (O(N) per event,
   ~K× write amplification). Publish re-derives the full projection; open re-verifies
   every tile against the WAL.

## Progress

- **08a DONE** (`9b5eeff`, recovered from worker `d7fdeb6c`). FDBTILE2 page-structured,
  directory-indexed format; spec `.scratch/08-tile-pages/FDBTILE2.md`. Supervisor decision
  recorded: `first_key_len`/`key_len`/`value_len` are u32 (u16 could not encode existing
  2 MiB values), `entry_count` u16, MAX_KEY/MAX_VALUE unchanged and fail-closed. Parent
  review: superblock written last, superblock/trailer cross-checked, directory bounds vs
  trailer, fence monotonicity, block-header consistency, per-block CRC on read, and a
  re-read of size/superblock/trailer on first entry read; byte-offset tests adapted with
  intent preserved. Gates: 110 Rust (+11), Python 10, `git diff --check` clean. Measured:
  tile probe 110ms→16.4ms @5.6MB, 450ms→65.1ms @22.4MB (per-entry CRC removed) — still
  linear, which 08b removes.
  Parent review findings recorded as 08b work items: (i) within-block key order is no
  longer verified anywhere (key-addressed verification replaced the positional check that
  implied it) → must be enforced while streaming; (ii) data-block offsets are not checked
  for overlap/gaps → add strictly-increasing/non-overlapping validation.
- **08b DISPATCHED** (worker `0d5cc873`): allocation-free borrowed-entry cursor, prefix
  seek via dir_search (first caller), streaming strict-order check, engine.rs scan_merged
  rewired onto prefix cursors, plus the two review findings. Acceptance targets: probe at
  a 22 MB tile ≤ 1 ms, sequential scan ≥ 400 MB/s, all 110 tests still green.
- Review batching: 08a+08b are one subsystem (tile read path) → one fresh adversarial
  reviewer after 08b lands, per the earlier batching decision.

- **08b DONE** (`d641cca`, worker `0d5cc873`). `TileCursor` (borrowed entries, no
  per-entry allocation), `prefix_cursor` seeking via `dir_search` and never reading
  blocks past the prefix range, streaming strict key-order check (restores what the old
  positional comparison implied), `open()` overlap/order rejection of data blocks,
  `scan_merged` on cursor heads, `scan_all` as a documented read-only hook. Measured:
  **probe 65.6 ms → 14–32 µs** at a 22.4 MB tile (target ≤1 ms met, no longer linear);
  scan 323→324 MiB/s (allocation removal only ~4%). 113 Rust (+3, no test weakened).
- **08c-crc DONE** (`59861dd`, worker `12ee7e0f`). Shared `engine/src/crc.rs`: `crc32c` +
  incremental `Crc32c` (raw state !0, inversion only in finish, so composition matches the
  old tile code), SSE4.2 backend behind runtime detection, portable slice-by-16 fallback,
  old byte-at-a-time table kept only as the test oracle; wal (pub re-export), manifest and
  all 11 tile call sites rewired, duplicated tables deleted. Equivalence proved by 5 tests
  (`0xE3069283`, every length 0..=1024 plus 4096/4097/65536/1 MiB, both backends called
  directly, 1-byte-chunk update vs one-shot). Worker also verified on-disk bytes
  independently: WAL/manifest/tile are sha256-identical at baseline `d641cca` and at this
  revision. Measured: CRC-only 0.44 → 8.8-9.7 GB/s; sequential tile scan 324 → 1158-1570
  MiB/s (page-cache warm, i.e. no longer CPU-bound below the 831 MiB/s device); probe
  unchanged (5.6-17.4 µs). Rust 118 (+5).
- **08d/08e/08f PENDING** the author D1–D5 decisions (09): bloom keyed by the net-affected
  anchor set, publish verifying only new pages, then the data:RAM re-run.
- **Batch review DISPATCHED** (reviewer `97c2923c`) over 08a+08b+08c as one subsystem,
  with the parent-identified attack list (prefix-cursor false negatives, order-check
  bypass, open-validation holes, CRC equivalence, scan_merged arbitration, floor-spanning
  verification).

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
- **08d — per-tile bloom over declared unique values**: probe answers "definitely
  absent" in RAM (no I/O) or "maybe" (one page read); bits/key a tunable with a
  documented RAM cost.
- **08e — publish verifies only new pages** (whole-tile digest retained at open),
  then re-measure write amplification.
- **08f — re-run the data:RAM experiment** (the 07 run) on the fixed engine.

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
whose owners are all in the block). 08d bloom applies to that set: "no published owner"
answers in RAM with zero I/O. 08a/08b are unaffected (pure implementation gaps).
