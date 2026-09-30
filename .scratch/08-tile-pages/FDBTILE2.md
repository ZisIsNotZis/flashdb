# FDBTILE2 tile file format

Slice 08a replaces the flat, single-record-stream `FDBTILE1` layout with a
page-structured, directory-indexed `FDBTILE2`. The public surface of
`engine/src/tile.rs` (`Tile::write`, `Tile::open`, `cutoff`, `digest`, `count`,
`entries`, `verify_projection`) is unchanged; only the on-disk representation
moves. `engine/src/wal.rs`, `manifest.rs`, and `engine.rs` are untouched.

All integers are little-endian. CRCs are CRC-32C (Castagnoli, reflected
polynomial `0x82F63B78`), i.e. `crate::wal::crc32c` / the local incremental
table, the same primitive `FDBTILE1` used. The stored value is the finalized
CRC (`!crc(!0, bytes)`); verification compares against the finalized form.

## Layout

```
+---------------------------+
| superblock (padded 4096)  |  offset 0
+---------------------------+
| data block 0              |
| data block 1              |
| ...                       |
+---------------------------+  offset dir_offset
| directory block 0         |
| directory block 1         |
| ...                       |  dir_len bytes total
+---------------------------+  size - TRAILER
| trailer (29 bytes)        |  end of file
+---------------------------+
```

### Superblock (offset 0, 80 meaningful bytes, zero-padded to 4096)

| field | size | offset |
|---|---|---|
| magic `b"FDBTILE2"` | 8 | 0 |
| cutoff | u64 | 8 |
| WAL digest | 32 | 16 |
| entry count | u64 | 48 |
| directory offset | u64 | 56 |
| directory length | u64 | 64 |
| page_size (4096) | u32 | 72 |
| superblock crc32c | u32 | 76 |

The CRC covers `superblock[0..76]`. The whole superblock occupies the first
`page_size` bytes (padding is zero), so the first data block always starts on a
4096-byte boundary at offset `page_size`. `page_size` must be exactly 4096.

### Data block

```
[block_len u32][block_crc32c u32][first_key_len u32][entry_count u16]
[first_key bytes][entries...]
```

- `block_len` is the **whole** block length in bytes, including the two u32
  prefix fields. It must fit in u32; a block whose encoded size cannot is
  rejected `InvalidData` at write time.
- `block_crc32c` covers `block[8..block_len]` (from `first_key_len` to the end).
- `first_key_len` is a u32 (decision: u32, not u16 — see below).
- `entry_count` is a u16. A 4 KiB block cannot hold more than 65535 entries, and
  an oversized entry that occupies its own block has `entry_count == 1`.
- Each entry: `[key_len u32][value_len u32][key bytes][value bytes]`.
- A block holds whole entries only. The writer appends entries until the next
  entry would push the block past the 4096-byte target, then starts a new block.
  An entry larger than the target simply gets its own (oversized) block.
- The first entry's key is duplicated into `first_key`; the reader validates
  that the header copy matches the directory fence.

### Directory

The directory is one or more CRC32C-protected blocks, contiguous from
`dir_offset` for `dir_len` bytes:

```
[block_len u32][block_crc32c u32][fence_count u16][fence records...]
```

- `block_len` is the whole directory-block length; `block_crc32c` covers
  `block[8..block_len]`; `fence_count` is a u16.
- One fence record per data block: `[first_key_len u32][block_offset u64]
  [block_len u32][first_key bytes]`.
- Directory blocks use the same 4096-byte target and are written contiguously;
  `dir_len` is their summed length. An empty tile has `dir_len == 0`.

### Trailer (last 29 bytes)

| field | size |
|---|---|
| magic `b"FDBTILE2E"` | 9 |
| directory offset | u64 |
| directory length | u64 |
| trailer crc32c | u32 |

The trailer magic is **exactly the nine bytes `b"FDBTILE2E"`** (F D B T I L E 2
E); there is no eight-byte variant and no separate length prefix. The CRC covers
`trailer[0..25]`. The trailer duplicates the directory offset/length so that a
file truncated at any point after the data region cannot be mistaken for a
complete tile.

## Validation policy

`Tile::open` fails closed (`io::ErrorKind::InvalidData`, specific message) on any
of, and **does not read data blocks eagerly**:

1. size below the minimum (`page_size + 29`);
2. superblock magic / CRC mismatches, or `page_size != 4096`;
3. trailer magic / CRC mismatches;
4. trailer directory offset/length disagreeing with the superblock;
5. directory bounds: `dir_offset >= page_size` and
   `dir_offset + dir_len == size - 29` (so the directory ends exactly at the
   trailer; a short/long region is rejected);
6. directory block framing/CRC errors, bad record lengths, or trailing bytes
   inside a directory block;
7. fence key order not strictly increasing;
8. block ranges outside the data region (`block_offset >= page_size`,
   `block_offset + block_len <= dir_offset`) or below the block header size;
9. **block header consistency** for every fence: the block header exists, its
   `block_len` equals the fence's, its `first_key_len`/first key equal the
   fence's, it declares at least one entry, and the per-block `entry_count`
   values sum to the superblock entry count.

Data-block CRC is deliberately **not** verified at open: `entries()` verifies
each block's CRC32C as it streams that block and fails `InvalidData` on a
mismatch. `entries()` also re-checks, on first read, that the file length,
superblock, and trailer still match the bytes seen at open, so a mutation after
open is caught. A corrupt data byte therefore surfaces when the block is read,
not when the tile is opened.

## Length-field width decision (2026-09-30)

The slice brief initially specified u16 for `first_key_len`, `key_len`, and
`value_len`. That cannot represent the values the engine already writes (a
2 MiB document value; a valid `U` key that embeds its value can exceed 65535
bytes), which must keep working unchanged. The parent's decision (Option A) was
to widen all three to u32 LE while keeping `MAX_KEY = 1<<24` and
`MAX_VALUE = 1<<26` enforced with `InvalidData` (no behavior regression).
`entry_count` stays u16. The writer additionally checks that a fully encoded
block length still fits the u32 `block_len` field and fails closed otherwise.
