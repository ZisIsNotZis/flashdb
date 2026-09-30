# FDBTILE3 tile file format

FDBTILE3 is FDBTILE2 plus a CRC-protected **bloom filter section** between the
data blocks and the directory. It is introduced by slice 08d-2a as a
*format-only* change: the bloom is written and validated by
`engine/src/tile.rs`/`engine/src/bloom.rs`, but the engine does not yet build
one, so every tile the engine writes today has an empty (`bloom_len == 0`)
section. Wiring the filter into the probe path is slice 08d-2b. The superseded
FDBTILE2 spec remains at [`FDBTILE2.md`](FDBTILE2.md); everything not listed
under "Changes from FDBTILE2" below is unchanged.

## Changes from FDBTILE2

Only two things move:

1. **Superblock.** The magic is now `b"FDBTILE3"`. Five fields are appended
   before the superblock CRC (`bloom_offset`, `bloom_len`, `bloom_crc32c`,
   `bits_per_key`, `hash_count`); the CRC now covers the enlarged header. Every
   pre-existing field keeps its offset and meaning.
2. **A bloom section** sits between the end of the data blocks and the start of
   the directory. When absent (`bloom_len == 0`) the file's data blocks,
   directory and trailer are byte-identical to FDBTILE2.

Explicitly unchanged, so their readers and writers are untouched:

- the 4096-byte padded superblock page (the first data block still starts at
  offset 4096);
- the data-block framing (u32 `block_len`, u32 CRC over `block[8..]`, u32
  `first_key_len`, u16 `entry_count`, u32 key/value lengths);
- the directory framing and fence-record layout;
- the 29-byte trailer, **including its magic `b"FDBTILE2E"`**. The trailer
  identifies the trailer structure, which did not change; keeping it
  byte-identical is the point of the constraint above. A file is identified as
  FDBTILE3 solely by the superblock magic.

## Layout

```
+---------------------------+
| superblock (padded 4096)  |  offset 0
+---------------------------+
| data block 0              |
| data block 1              |
| ...                       |
+---------------------------+  offset bloom_offset (end of last data block)
| bloom section             |  bloom_len bytes (absent when bloom_len == 0)
+---------------------------+  offset dir_offset
| directory block 0         |
| directory block 1         |
| ...                       |  dir_len bytes total
+---------------------------+  size - TRAILER
| trailer (29 bytes)        |  end of file
+---------------------------+
```

All integers are little-endian. CRCs are CRC-32C (Castagnoli, reflected
polynomial `0x82F63B78`), the finalized `!crc(!0, bytes)` form, via
`crate::crc::crc32c`.

### Superblock (offset 0, 108 meaningful bytes, zero-padded to 4096)

| field | size | offset |
|---|---|---|
| magic `b"FDBTILE3"` | 8 | 0 |
| cutoff | u64 | 8 |
| WAL digest | 32 | 16 |
| entry count | u64 | 48 |
| directory offset | u64 | 56 |
| directory length | u64 | 64 |
| page_size (4096) | u32 | 72 |
| bloom_offset | u64 | 76 |
| bloom_len | u64 | 84 |
| bloom_crc32c | u32 | 92 |
| bits_per_key | u32 | 96 |
| hash_count | u8 | 100 |
| reserved (zero) | 3 | 101 |
| superblock crc32c | u32 | 104 |

The CRC covers `superblock[0..104]`. `page_size` must be exactly 4096.

- `bloom_offset` is where the bloom section starts; `bloom_len` is its length in
  bytes. `bloom_len == 0` means "no bloom"; the writer then sets `bloom_offset`
  to `dir_offset` and `bloom_crc32c = bits_per_key = hash_count = 0`.
- `bloom_crc32c` is the CRC-32C over the whole bloom section
  (`bloom[bloom_offset..bloom_offset + bloom_len]`), i.e. it covers the bloom
  header and bit array together.
- `bits_per_key` and `hash_count` duplicate the bloom header's values so a
  reader can see the filter's shape without parsing the section; `Tile::open`
  requires them to agree with the section.

### Bloom section

A self-describing header followed by the bit array:

```
[bit_count u64][entry_count u64][bits_per_key u32][hash_count u8][reserved 3 = 0]
[bit array: ceil(bit_count / 8) bytes]
```

| field | size | offset |
|---|---|---|
| bit_count | u64 | 0 |
| entry_count | u64 | 8 |
| bits_per_key | u32 | 16 |
| hash_count | u8 | 20 |
| reserved (zero) | 3 | 21 |
| bit array | ceil(bit_count/8) | 24 |

- `bit_count` is the number of bits in the array (at least 1). The array length
  is implied by it, so the section is valid only when
  `bloom_len == 24 + ceil(bit_count / 8)`; trailing bytes are rejected.
- `entry_count` is the number of keys inserted (a hint; the filter works if it
  differs from the final entry count the same way `bits_per_key` is a sizing
  hint).
- `bits_per_key` is in `1..=64`; `hash_count` is in `1..=32`.
- A zero-entry filter has an all-zero bit array; `from_bytes` rejects a
  zero-entry filter with any bit set.

### Hash function (frozen; the persisted bytes depend on it)

One 64-bit FNV-1a hash of the key (`offset basis 0xcbf29ce484222325`, prime
`0x100000001b3`) seeds double hashing:

```
h  = fnv1a64(key)
h1 = h
h2 = rotate_left(h, 32) | 1        // forced odd
bit_i = (h1 + i * h2) mod bit_count   for i in 0..hash_count
```

Forcing `h2` odd keeps the probe sequence from collapsing for structured keys.
`hash_count` is `round(bits_per_key * ln 2)` clamped to `1..=32` (so 10
bits/key implies 7 probes). The builder clamps `bits_per_key` to `1..=64`
before deriving `hash_count`.

### Trailer (last 29 bytes)

Unchanged from FDBTILE2: magic `b"FDBTILE2E"` (9 bytes), directory offset u64,
directory length u64, CRC-32C over `trailer[0..25]`.

## False-positive-only contract

A bloom filter answers set membership approximately:

- `might_contain` returning `false` is a **proof** that the key was never
  inserted;
- returning `true` means "possibly present".

**A false negative is a correctness bug**: the caller would hide a live key.
**A false positive is only wasted work**: the caller falls back to the exact
path. Callers must treat `false` as definitive and `true` as a hint.

Consequence for use: the filter must be built over the *same byte strings* the
caller will probe. The intended wiring (08d-2b) builds it over each stored
unique-index prefix and probes with that prefix, so `false` is safe.
`Tile::might_contain_unique_prefix` returns `true` for every prefix when the
tile has no bloom, so the absent-filter case never rejects.

`BloomBuilder`/`Bloom` implement this contract in `engine/src/bloom.rs`; the
exhaustive no-false-negative test inserts a prefix-closed key set and asserts
every member reports present. The false-positive rate is *measured*, never
asserted: at 10 bits/key, 100 000 inserted keys and 100 000 absent probes, it
measured 786/100 000 = 0.786% (theoretical ~0.8%).

## Validation policy

`Tile::open` fails closed (`io::ErrorKind::InvalidData`, specific message) on
everything FDBTILE2 rejected, plus:

1. **Bloom bounds.** When `bloom_len > 0`, `bloom_offset >= page_size` and
   `bloom_offset + bloom_len <= dir_offset` (so the section can neither start
   inside the superblock page nor reach into the directory), and
   `bloom_offset >=` the end of the last data block (so it cannot overlap a data
   block). The data-block bound itself is still checked against `dir_offset`,
   exactly as in FDBTILE2.
2. **Bloom CRC.** `crc32c(section) == bloom_crc32c`.
3. **Bloom structure.** `Bloom::from_bytes` must accept the section (header
   fields in range, no trailing bytes, zero-entry filter all-zero).
4. **Cross-check.** The section's `bits_per_key`/`hash_count` must equal the
   superblock's copies.

These happen without reading any data block. `bloom_len == 0` skips all of them
and yields a tile that answers `true` for every prefix.

## Trust boundary: below-floor entries in a floor-spanning tile

Unchanged from FDBTILE2 (see that document). The bloom is a *filter*, not a
verification: it does not participate in `verify_projection`, and a below-floor
entry trusted after publication is not re-verified on reopen, bloom or not.
