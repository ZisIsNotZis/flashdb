# Slice 50 - family co-location: make entity-major placement expressible

**Why.** The offline measurement in `.scratch/43-placement-headroom/issues.md`
(verified in commit `3af49cf`) shows that placing one entity's derived `U|X` and
`R|X` entries in the same tile as its `P|X` row cuts dependent random reads per
request by ~35% (34.76 / 34.82% at 256 / 4096-entry tiles, stable across sizes).
That is the one measured axis with headroom, and it was **not expressible**: tiles
are monotone key-range fenced and the key is family-major (`family | entity | 0 |
rest`), so `U|X` sorts far from `P|X` and every entity's three families land in
three regions. The learner's action space could not influence placement at all.

**Deliverable.** Make family co-location expressible behind a policy selector,
without breaking any invariant.

## Decision note (key encoding)

Goal: one entity's `P|X`, `U|X`, `R|X` keys sort adjacently so a region-partitioned
publish (`RegionMode::PerRegion`, already shipped in slice 24) puts them in one
tile.

Chosen: **entity-major spelling** with an explicit first-byte marker,
`ENTITY_FIRST ('C') | entity | 0 | family | rest`. Marker `0x43` is outside
`{P(0x50), U(0x55), R(0x52)}`, so the two spellings are distinguishable from byte
0 and cannot be prefixes of each other. Key properties preserved:

- **Total order / monotone fences.** It is still a byte string; order is
  `(marker, entity, family, rest)` and tiles stay fenced ranges. Within one
  entity the family byte orders `P < R < U`.
- **Uniqueness prefix contract.** `unique_prefix_with` is a strict byte prefix of
  `unique_key_with`; `unique_prefix_of` returns `key[..len-16]` for both spellings
  (the trailing `handle(8)+~csn(8)` is 16 bytes either way); `is_unique_prefix`
  parses both. The per-tile bloom is built over exactly the strings probed.
- **Reverse-lookup contract.** `reverse_prefix_with` is a strict prefix of
  `reverse_key_with`; `decode_reverse` resolves the family through `family_of`
  and reads targets/sources from the fixed trailing 24 bytes.
- **Existing order assumptions.** `region_prefix` now also accepts `ENTITY_FIRST`:
  its result is `ENTITY_FIRST | entity | 0` for the new spelling vs `family | entity
  | 0` for the old. Under `EntityFirst` the region is `(encoding, entity)`, exactly
  the co-location unit; `tile.rs::valid_key` validates both, `region_name` renders
  `C|entity`, and the whole region-partitioning / `(cutoff, region)` machinery is
  unchanged.

Rejected alternatives:

- **Compile-time constant encoding.** Simplest diff, but one compile yields one
  encoding, so the default suite could not exercise both in a single
  `cargo test --workspace`. Rejected in favour of a runtime selector.
- **Move a family marker suffix only (`family | entity | 0 | rest` -> `entity | 0 |
  family | rest`), no marker byte.** Ambiguous with the old spelling (an entity may
  start with any byte), so old and new keys could not coexist and reads could not
  accept both. Rejected.
- **Route reads as full-region scans to avoid knowing the encoding.** Re-reads
  whole regions per probe and changes the read cost model. Rejected.
- **Persist the encoding (tile superblock / manifest).** `manifest.rs` is
  out of scope and no persistence is needed: like `region_mode` and
  `LayoutPolicy`, the encoding is a recovery input. Rejected as scope.

## Change

- `keys.rs`: add `pub const ENTITY_FIRST: u8 = b'C'` and
  `pub enum KeyEncoding { FamilyFirst (default), EntityFirst }`; add
  `*_with(enc, ..)` constructors for primary/unique/reverse keys and their scan
  prefixes. The existing `primary_key`/`unique_key`/`reverse_key`/`*_prefix`
  functions delegate to `FamilyFirst`, so every current caller is byte-identical.
  Parsing (`region_prefix`, `decode_primary`, `decode_reverse`, new `family_of`)
  accepts both spellings.
- `engine.rs`: `Engine.key_encoding` field (default `FamilyFirst`), setter
  `set_key_encoding` / getter `key_encoding`; `op_kv`/`apply_op`/`op_region` take
  the encoding; all read prefixes and commit-time unique prefixes use it.
  Recovery takes the encoding: `open_with_encoding` and
  `open_discover_with_encoding` (the existing `open`/`open_discover`/`open_with_tile`
  default to `FamilyFirst`). New inspection hook `active_tile_key_ranges()` exposes
  each active tile's `(first_key, last_key)` fence so a test can place a key in its
  tile.
- `tile.rs`: `valid_key` validates both spellings via `region_prefix` + family byte.

## Selector

Runtime, per-engine, default = today. Sequence for co-location:

```
engine.set_key_encoding(KeyEncoding::EntityFirst);
engine.set_region_mode(RegionMode::PerRegion);
```

Not persisted, matching `region_mode`/`LayoutPolicy`; a reopen of entity-first
data must call `Engine::open_discover_with_encoding(dir, KeyEncoding::EntityFirst)`
(or `open_with_encoding`), because WAL replay re-encodes ops. Reads accept both
spellings, but a read prefix must match the spelling on disk, so a default-encoded
reopen of entity-first data would not probe the right prefixes (see residual risk).

## Tests

`engine/tests/family_colocation.rs` (new, 3 tests) + `keys.rs` unit tests:

- `entity_first_puts_each_entitys_three_families_in_one_tile` - for 5 entities
  (`A`,`AB`,`ABC`,`Customer`,`Stock`), `tile_of` places `P|X`, `U|X`, `R|X` in the
  **same tile index**, asserts the tile's `(first,last)` fence range brackets all
  three keys, and that the tile's fence-derived region name is `C|<entity>`. Asserts
  the tile id and the fence ranges, not only the ordering.
- `family_first_keeps_each_family_in_its_own_tile` - default spelling + PerRegion
  emits `3 * entities` tiles and the three families land in three distinct tiles
  (today's behaviour unchanged).
- `entity_first_reads_duplicate_owner_invariant_and_reopen` - `unique_lookup`
  returns the right handle, `reverse_lookup` groups both sources under one target,
  `get` returns the doc; a second live owner for the same unique value returns
  `Outcome::Conflict` and changes nothing; `open_discover_with_encoding(dir,
  EntityFirst)` recovers the same fence ranges, regions and reads.
- `keys.rs` unit tests: `entity_first_colocates_one_entitys_three_families`,
  `entity_first_prefixes_are_strict_prefixes_of_keys`,
  `entity_first_decoders_accept_both_encodings`,
  `entity_first_region_prefix_is_encoding_scoped`.

## Gates (verbatim)

`cargo test --workspace --offline` - all 25 test binaries ok, **271 passed; 0 failed**:

```
test result: ok. 117 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
... (0 passed for the empty binaries/doc-tests)
```

`cargo test --offline -p flashdb-engine --test family_colocation`:

```
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`cargo build --workspace --offline` (after touching the three changed sources):
`Compiling flashdb-engine ... Finished dev profile` - **0 warnings**.

`git diff --check` - silent (exit 0).

## Residual risk

- **Encoding is not persisted.** A reopen of entity-first data without passing
  `EntityFirst` replays the WAL into the wrong spelling and will not serve the
  tiles correctly. This mirrors the existing non-persistence of `region_mode` and
  `LayoutPolicy`; the fix (a manifest/superblock flag) is out of this slice's
  scope. Callers must pass the encoding to the `_with_encoding` open functions.
- **Per-region `LayoutPolicy` cannot target an entity-first region.**
  `region_name` yields `C|entity`, and `is_region_spelling` only accepts `P`/`U`/`R`
  families, so a declared policy row for `C|entity` is rejected and the region
  uses the default. Fine for this slice (co-location is the point); extending the
  policy spelling is a follow-up if per-entity layout is wanted.
- **Metrics region identity changes under `EntityFirst`.** With entity-first the
  metrics region is `(encoding, entity)` (marker `C`), consistent between the
  commit path (`op_region`) and probes (`region_prefix`), but it is not the same
  `(family, entity)` row the default engine reports. Not exercised by any test;
  observational only.
- All existing tests pass unchanged, so the default spelling is not regressed.

## Exact measurement now possible (real run)

Dependent random reads per request on a **real engine run**, not the offline
model of slice 43. The layout is now selectable, so a run can compare:

- baseline: `KeyEncoding::FamilyFirst` + `RegionMode::PerRegion`
  (slice-43 modelled 94,798 reads / 4.740 per request at 256-entry tiles), vs
- co-located: `KeyEncoding::EntityFirst` + `RegionMode::PerRegion`
  (slice-43 modelled 61,848 reads / 3.092 per request, **-34.76%**).

Count = walk each request's read sequence in `request.rs` order and count a read
when its key's tile differs from the previous read's tile; tile = the active
tile whose `(first_key, last_key)` fence range (`active_tile_key_ranges`) contains
the key, or the serving memtable when uncovered. Repeat over the repository's
deterministic generator trace (seed 42, 20k requests) and report reads/request for
both encodings - now a measured engine quantity rather than a model.
