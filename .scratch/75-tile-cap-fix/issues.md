# 75 - implement the adopted tile-cap fix (STATUS SS7.4)

## Contract

Work package W1 in the `wave21/tilecap` worktree (branch `wave21/tilecap`, based on
`master 8a4ea20`). Single writer. Implement exactly the adopted §7.4 decision recorded in
`.scratch/46-partitioned-publish-tile-cap/issues.md` and `.scratch/STATUS.md` §7.4:

1. raise and make configurable the active-tile cap;
2. make the scored run harness's default checkpoint/rotate cadence compatible with
   per-region publishing;
3. make a scored run refuse an incompatible cadence loudly up front instead of aborting
   mid-run.

Preserve correctness: no silent tile loss; publish still fails closed if the cap is genuinely
exceeded. Update the regression test `engine/tests/partitioned_publish_cap.rs` to pin the fixed
behaviour (shipped default cadence completes; an over-cap publish still fails closed). Do NOT
edit the pre-registration; no threshold change.

Acceptance (verbatim from the dispatch):

- the shipped default cadence runs a per-region scored workload to completion without exhausting
  the cap;
- the cap is raised/configured;
- an incompatible cadence is refused loudly up front;
- a genuine over-cap publish still fails closed;
- all tests green.

Gates (verbatim):

- `cargo build --workspace --offline` (0 warnings)
- `cargo test --workspace --offline` (totals)
- `cd harness && uv run --with pytest pytest -q`
- `git diff --check` (silent)

## Design

### 1. Raise + configure the cap (engine)

`engine/src/manifest.rs`:

- Raise the policy/structural ceiling `MAX_TILES` 1024 -> 4096. The manifest already chains
  continuation pages, so this is a bound change, not an on-disk protocol change.
- Raise `MAX_CHAIN_PAGES` 96 -> 316 so the compile-time self-consistency assertion still holds:
  `MAX_TILES (4096) <= MAX_CHAIN_PAGES (316) * CHAIN_RECORDS_PER_PAGE (13) = 4108`.
  `MANIFEST_MAX_BYTES` follows automatically.
- `MAX_TILES` stays the decode-time corruption bound (a CRC-valid page claiming more is
  corruption). It is also the new default policy cap.

`engine/src/engine.rs`:

- Add `Engine.max_tiles: usize`, default `manifest::MAX_TILES`, initialised in every constructor.
- Add `Engine::set_max_tiles(max_tiles)` (clamped to `1..=manifest::MAX_TILES`) and
  `Engine::max_tiles()`.
- Use `self.max_tiles` in the two publish guards (`write_verified_tiles` capacity and
  `publish_tile`), replacing the hard-coded `manifest::MAX_TILES`. `MAX_ACTIVE_TILES` stays the
  public ceiling/default constant.
- `write_verified_tiles` (per-region branch): count the distinct regions in the publish window up
  front and refuse the whole publish BEFORE writing any region tile file when
  `distinct_regions > capacity`. Same pinned error text. This removes the mid-loop orphan-file
  partial publish while keeping the exact fail-closed boundary (a publish of exactly `capacity`
  regions still succeeds).

### 2. Configurable cap in the driver

`engine/src/bin/flashdb-load.rs`:

- Add `--max-tiles N` (positive, `<= MAX_ACTIVE_TILES`), applied via `Engine::set_max_tiles`
  after open. Default: the engine default.

### 3. Scored runner cadence + loud refusal

`harness/flashdb_harness/cold_0b.py`:

- Change the scored runner's default `--checkpoint-bytes`/`--rotate-bytes` from 16384 to
  `8 << 20` (the build harness's known-compatible cadence).
- Add `--max-tiles` (default 4096, the engine's `MAX_ACTIVE_TILES`) and pass it through
  `flashdb_load_command`.
- Add an up-front validation before any arm runs, using `flashdb-scale tileinfo` on the base
  instance to read the region count and active tile count, and the trace's write count:
  - refuse loudly if `region_count > max_tiles` (one full publish cannot fit);
  - refuse loudly if `checkpoint_bytes != rotate_bytes` (the paired maintenance protocol needs
    equal cadence; the shipped default is equal);
  - refuse loudly if `starting_tiles + ceil(write_count * MAX_WAL_BYTES_PER_WRITE / cadence)
    * region_count > max_tiles`, where `MAX_WAL_BYTES_PER_WRITE = 4096` is a documented
    conservative ceiling for the generator's fixed ~1.3 KiB order_flow WAL frame. This is a
    sufficient (sound) condition for guaranteed completion: if it passes, the run cannot exhaust
    the cap; if it fails, the run is refused instead of aborted mid-run.

The guard is a guard, not a metric: it never changes a scored threshold.

### 4. Tests

- `engine/tests/partitioned_publish_cap.rs`: replace the current-behaviour pin with
  - `per_region_publish_over_the_cap_fails_closed` (12 full-region publishes fit under the
    4096 default; the 13th is refused with the pinned text, leaves no partial/orphan publish);
  - `shipped_default_cadence_keeps_a_per_region_run_under_the_cap` (with the 8 MiB cadence a
    per-region workload stays below the publish threshold, then one final checkpoint emits all
    336 regions in a single publish that fits).
- `engine/tests/multi_tile.rs`: the two `MAX_ACTIVE_TILES` fill tests now use a small configured
  cap (16) via `set_max_tiles`, keeping them fast and exercising the configurable bound.
- `harness/tests/test_cold_0b.py`: pin the shipped default cadence (8 MiB), the `--max-tiles`
  plumbing, and the cadence guard's refuse/allow decisions.

## Change

Files (target):

- `engine/src/manifest.rs`
- `engine/src/engine.rs`
- `engine/src/bin/flashdb-load.rs`
- `engine/tests/partitioned_publish_cap.rs`
- `engine/tests/multi_tile.rs`
- `harness/flashdb_harness/cold_0b.py`
- `harness/tests/test_cold_0b.py`
- `.scratch/STATUS.md` (SSOT: §6 / §7.4 / §9 reflect the implemented fix)

## Gates

(see Contract; verbatim results recorded in the acceptance verdict below)

## Acceptance verdict

(to be filled after the gates run)

## Residual

(to be filled after the gates run)
