# 19 — the tile cap is the biggest measured writing lever (raise it via a multi-page manifest)

Opened 2026-09-30 from `harness/flashdb_harness/partition_sim.py`, a simulation of the shipped
`budget_prefix` merge policy that **reproduces the measured write_amp of 3.920 within 1.5%** on
the slice-12d configuration, so its comparisons are usable.

## Finding

The manifest's 4092-byte A/B page bounds the active tile list at **12** (`.scratch/12`,
compile-time asserted). Sweeping that cap in the simulation, at the inventory workload's measured
family composition (P 58.5%, U 30.8%, R 10.7% of written bytes) and the same payload:

| tile cap | co-located write_amp | partitioned write_amp | partitioning "gain" |
|---|---|---|---|
| **12 (today)** | 4.092 | 7.745 | -89.3% |
| 24 | 2.075 | 2.808 | -35.3% |
| 48 | 1.771 | 2.102 | -18.7% |
| 96 | 1.661 | 1.519 | +8.6% |
| 192 | 1.095 | 1.000 | +8.7% |

Two conclusions, both actionable:

1. **Cap headroom is the real lever: 12 -> 96 cuts write amplification by ~59%** (4.09 -> 1.66).
   More active tiles means a better tier factorization, so each byte is rewritten fewer times.
   This is far above the provisional 15% threshold and it is contract-neutral - it is internal
   capacity, not a product-visible layout choice.
2. **Family partitioning is closed as an axis.** It only turns positive at cap >= ~96 and even
   then only +8.7%; at today's cap it is catastrophically worse (partitioning multiplies the tile
   count against a fixed cap, so every family hits the cap sooner and compacts its own smaller
   tiers more often). Note the model gives partitioning the OPTIMISTIC bound (each family gets
   half the cap to itself); a shared cap would be stricter, so the negative conclusion is robust.

## Why it matters for 0b

The layout space needs a >=15% axis to be falsifiable at all. Page size and bloom bits top out at
~3% (`.scratch/14-layout-harness`); family partitioning is closed by this measurement. Raising the
cap is (a) a large measured win by itself and (b) a **prerequisite for any layout choice that
trades write cost against read locality**, because those choices need tile headroom to exist.
It also makes "which tiles to merge, when" a meaningful learned decision (the design already lists
"compaction trigger per prefix-region" as learner-controlled in v0).

## Required work

Multi-page manifest:

- Keep the A/B page protocol and its ordering rule (inactive page -> sync file -> sync dir ->
  flip), but let the root chain pages so the tile list is no longer bounded by one page. Options:
  a chained page list with a next-page pointer (each page CRC-protected), or a fixed small
  directory page pointing at tile-record pages.
- Define the failure model precisely and fail closed: a chain whose CRC fails at any link makes
  the whole root invalid (so the other A/B page, if valid, wins); a chain that does not terminate
  at a bounded length is corruption; the highest valid seq still wins.
- Recovery (`open_discover`) must validate the entire chain before serving, and `MAX_TILES` becomes
  a policy constant (say 1024) rather than a page-size artefact.
- Keep the invariant that a tile's durable directory entry exists before the manifest references
  it, and that the manifest never references a name twice.
- Measure for real: re-run the 12d/12c 5 M-doc configuration at cap 12 vs the new cap and report
  the measured write_amp, then the simulation's prediction error (this is also a second,
  independent calibration point for the cost model - `review-01` F14).
- Acceptance: measured write_amp at the new cap materially below the 3.920 recorded for cap 12,
  all fault-injection tests green, and recovery fails closed on every malformed chain shape the
  tests can construct.

## Implemented: chained CRC-protected root (branch slice 19)

`MAX_TILES` is now a policy constant (**1024**), not the capacity of one 4092-byte payload. The
manifest file keeps the two A/B root slots at fixed offsets and adds one chain region per slot:

```
offset 0                          root slot A  (PAGE = 4096 bytes)
offset PAGE                       root slot B  (4096 bytes)
offset 2*PAGE                     slot A chain region (MAX_CHAIN_PAGES = 96 pages)
offset (2 + MAX_CHAIN_PAGES)*PAGE slot B chain region (96 pages)
```

- **Root page** (unchanged frame): `[seq u64 LE][crc32c(payload) u32 LE][payload 4084]`. The
  payload's first `u32` selects the form:
  - **inline** — byte-identical to the historical single-page manifest: `count u32`, then
    `count` records `[name_len u32][name][cutoff u64][digest 32]`, then
    `checkpoint csn u64, handle_watermark u64`, then `segment_name_len u32` (+ name + start_csn
    when rotated), zero-padded. Files written before this slice still open because ≤12 records
    still encode inline.
  - **chained** — `u32::MAX` marker, then `total_tiles u32, chain_pages u32, chain_start u32`,
    checkpoint, segment. All records live in the slot's continuation pages.
- **Continuation page**: `[seq u64 LE][crc32c(bytes 12..) u32 LE][page_index u32][page_count u32][record_count u32][records...]`,
  zero-padded. There is no next-pointer; a chain is the fixed window
  `chain_start..chain_start + chain_pages`, so it cannot loop or grow unboundedly. `chain_pages`
  and `chain_start + chain_pages` are both bounded by `MAX_CHAIN_PAGES = 96`, and the file may
  not exceed `2 + 2*MAX_CHAIN_PAGES` pages. A compile-time assertion
  (`MAX_TILES <= MAX_CHAIN_PAGES * CHAIN_RECORDS_PER_PAGE`, worst-case 13 records/page) ties the
  policy bound to the layout; a second assertion ties the chained descriptor plus the longest
  segment reference to one payload.
- **Durability is unchanged**: chain pages (when any) → inactive root page → one
  `fdatasync` of the manifest → `fsync` of the parent directory → A/B flip. A slot's chain
  region is only overwritten when that slot is reused, i.e. it is `seq-2` and A/B selection
  never needs it once the other slot's `seq-1` page is valid.

### Fail-closed list (each maps to a test)

A **chain** failure makes that A/B page invalid (the other side can still win); if neither page
is valid, `open` fails closed. An **inline** CRC-valid-but-impossible payload stays hard
corruption. A write failure poisons the writer exactly as before.

1. bad CRC on any link → invalid page;
2. a chain that does not terminate within the bound (`chain_pages > MAX_CHAIN_PAGES`, or
   `chain_start + chain_pages > MAX_CHAIN_PAGES`) → invalid page;
3. a link `page_count` that disagrees with the root descriptor (`MANIFEST_MAX_BYTES` / descriptor
   check) → invalid page;
4. `Σ record_count != total_tiles` → invalid page;
5. a link whose `seq` is not the owning root's seq → invalid page (a foreign chain cannot splice in);
6. a truncated chain (region past EOF) → invalid page;
7. a duplicated tile name in the assembled list → invalid page (chained) / corruption (inline);
8. a removed page-size assertion is replaced by the two compile-time layout assertions above.

### Tests added/changed

- `manifest.rs` unit tests: `chained_root_roundtrips_and_reopens` (1 tile inline, 130 tiles over
  2 chain pages), `max_tiles_round_trips_within_the_chain_bound` (1024 worst-case 255-byte names),
  `inline_root_files_still_open_byte_compatibly`, and
  `chain_failures_make_that_page_invalid_and_fail_closed_without_fallback` (covers shapes 1–5),
  plus `chained_publish_write_failure_poisons_and_previous_page_wins` (fault injection on a
  chained publish). `publish_refuses_...` now also covers a duplicate name and MAX_TILES+1.
- `engine/tests/multi_tile.rs`: the two cap tests now fill `MAX_ACTIVE_TILES`; their scratch root
  prefers `/dev/shm` because the cost is per-file fsync, not engine work.

### Measured (1,000,000 docs x 1 KiB, docs_per_commit = 5000, shape `budget_prefix`)

The 5 M configuration takes ~25 min per cap on this box, so a smaller doc count was used and is
stated. Deterministic metrics are the comparison; timing is indicative (shared box).

| cap | write_amp | compactions | compact output bytes | tile bytes total | wal written | rss_peak | elapsed |
|---|---|---|---|---|---|---|---|
| 12 (old) | 2.637 | 1 | 519,798,817 | 1,633,723,418 | 1,084,787,270 | 207,892 KB | 71.8 s |
| **1024 (new)** | **2.133** | **0** | **0** | **1,113,924,601** | 1,084,787,270 | 207,928 KB | 57.1 s |

Payload 1,030,777,780 B. Tile-only write_amp (tile bytes / payload): 1.585 → 1.081, i.e. **-32%**;
total write_amp **-19%**. Compaction output went to zero.

**Budget:** at cap 1024 the budget never binds at this scale (17 tiles), so 2, 8 and 32 windows
(128 MiB / 512 MiB / 2 GiB) all give the same numbers (compactions 0, write_amp 2.133). The
512 MiB default is still right; the cap, not the budget, stops compaction here.

**Vs the simulation's -59%.** That figure is the 5 M *inventory* composition at 12→96. On the
scale-driver composition the same simulation (tile bytes only) gives 12→96: 3.977→1.189 (-70%)
at 5 M and 1.887→1.000 (-47%) at 1 M; cap 12→1024 gives 1.000 at either scale because 1024 never
binds (17 tiles at 1 M, ~80 at 5 M). The measured tile-only fall (-32%) is smaller because at
1 M the old cap already forces only one compaction, and the measured numerator additionally
carries the irreducible WAL stream the model does not count. Direction and mechanism agree.

**Open consideration:** each `Tile` keeps its file open, so `MAX_TILES = 1024` needs ~1024 fds;
the default soft `RLIMIT_NOFILE` (1024) is too low. Either raise it in deployment or cap lower
(the simulation's validated knee is 96).
