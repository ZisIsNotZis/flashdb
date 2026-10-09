# 46 - partitioned publish exhausts the tile cap: the runner's shipped cadence cannot score a per-region instance

Independent verification of the abort preserved in
`.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/` confirmed this is an
engine-level incompatibility, not a driver bug: a `RegionMode::PerRegion` publish emits one tile per
`(entity, key family)` region, while `manifest::MAX_TILES` is a fixed 1024, so a small
checkpoint/rotate cadence accumulates one tile per region per publish until a publish no longer has
one slot per region and is refused.

## Defect

A per-region publish whose window spans more regions than the manifest's remaining tile capacity
fails partway through with `InvalidInput` instead of publishing. The failure is deterministic and
reproducible at the engine level.

## Exact code paths

- `engine/src/manifest.rs:118` - `pub(crate) const MAX_TILES: usize = 1024;` (a policy constant; the
  manifest chains continuation pages, so it can be raised without an on-disk protocol change).
- `engine/src/engine.rs:1003` - `let capacity = manifest::MAX_TILES.saturating_sub(self.tiles.len());`
  is the remaining slots for the whole publish, computed once before the region loop.
- `engine/src/engine.rs:1062-1063` - inside the per-region loop, when a new region is opened:
  `if emitted.len() >= capacity { return Err(io::Error::new(io::ErrorKind::InvalidInput,
  "active tile list is full for this publish: a partitioned publish emits one tile per region and
  the remaining capacity is smaller")) }`. This is the pinned failure.
- `engine/src/engine.rs:1006` - the adjacent upfront guard, hit only when `capacity == 0` from the
  start: `"active tile list is full; compact or rotate before publishing"` (a different message than
  the pinned one).
- `engine/src/engine.rs:1144` - the pre-existing `publish_tile` guard
  `"active tile list is full; no compaction exists in this slice"`.

The per-region emit itself is the partitioned branch of `write_verified_tiles`
(`engine/src/engine.rs:1038-1090`): one `TileWriter` per region, finished when the region prefix
changes; the check at 1062-1063 runs before each new region's file is created, so the failing
publish leaves the manifest untouched (unreferenced orphan tiles only) rather than poisoning it. The
aborting publish is therefore safe to refuse - but it cannot complete.

## Observed error (preserved, verbatim)

`.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/adversarial-declared.stderr:1`:

```
flashdb-load: active tile list is full for this publish: a partitioned publish emits one tile per region and the remaining capacity is smaller
```

The run's `run.log` records `rc=1` for the `adversarial/declared` arm; the curve table in the same
directory already shows `active tiles 336` for that instance, i.e. 336 regions - close enough to the
1024 cap that three cadence publishes exhaust it.

## The test that pins it

`engine/tests/partitioned_publish_cap.rs` -
`per_region_cadence_exhausts_the_tile_cap_and_the_publish_fails`. It drives a per-region engine with
336 entities (one region each, the run's width) and a 1-byte checkpoint threshold so every cycle
publishes; the first three cadence publishes each emit 336 tiles (1008 total), and the fourth starts
with capacity 16 and is refused with the pinned text. The test's docstring states it pins **current**
behaviour, not desired behaviour. It runs in well under a second and builds no large instance.

## Cadence mismatch is long-standing, not an `a1b809f` regression

- The build/seed harness ships an **8 MiB** cadence: `harness/build_cold_44.py:45-46` and
  `harness/flashdb_harness/cold_axis_compare.py:526,528` both default `--checkpoint-bytes`/
  `--rotate-bytes` to `8 << 20`. This 8 MiB cadence was introduced with the driver's cadence flags in
  `5a8f7f0` (slice 32).
- The scored cold-run harness `harness/flashdb_harness/cold_0b.py:1122-1123` ships a **16 KiB**
  cadence (`--checkpoint-bytes`/`--rotate-bytes` default `16384`), present since the runner landed in
  `f8b19d7` (slice 35).

The two harnesses have therefore disagreed on the cadence since the 0b runner existed; the mismatch
predates `a1b809f` (slice 44) and is not a regression it introduced. `a1b809f` only surfaced it by
scoring a per-region instance under the runner's own shipped cadence.

## Candidate fixes (choice NOT made here - needs the author)

1. Raise `manifest::MAX_TILES` (`engine/src/manifest.rs:118`). The compile-time assertion at
   `engine/src/manifest.rs:134-137` bounds it by `MAX_CHAIN_PAGES * CHAIN_RECORDS_PER_PAGE`; raising
   the constant may also require raising `MAX_CHAIN_PAGES` (and the manifest byte bound) if the new
   value does not fit the existing 96-page chain. Cost: larger manifest root chains / more pages per
   publish.
2. Make the scored runner use a cadence compatible with per-region publishing - i.e. one that keeps
   the number of per-region publishes below the cap for the instance's region count (e.g. reconcile
   `cold_0b.py` with the 8 MiB build cadence, or derive the cadence from the region count).
3. Both.

The choice changes either engine capacity policy or the measurement protocol, so it is the author's
call, not a worker's.

## Consequence for measurement

A scored cold run cannot use the runner's shipped 16 KiB cadence on a per-region instance: the
cadence accumulates one tile per region per publish until the 1024-tile cap refuses a publish and the
run aborts (`rc=1`), so no throughput/latency verdict can be produced under that configuration. Any
per-region capacity curve must either use a cadence that keeps the publish count under the cap or be
produced after the cap is raised; until then, the slice-44 per-region numbers are not reproducible at
the shipped cadence.
