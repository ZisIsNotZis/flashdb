//! Explicit, WAL-retained immutable tile prototype. No manifest, checkpoint, or WAL retirement.
//! The file is validated before it can displace any serving memtable entries. Reads use
//! owned, checksum-verified block buffers; this is not a bounded-recovery claim.
//!
//! On-disk layout is FDBTILE3 (see `.scratch/08-tile-pages/FDBTILE3.md`):
//! FDBTILE2 plus a CRC-protected bloom section between the data blocks and the
//! directory. A padded superblock sits at offset 0, then variable-length 4 KiB
//! data blocks, an optional bloom section, a CRC-protected directory of fence
//! records, and a fixed trailer. `Tile::open` validates the superblock, trailer,
//! directory structure, block headers and (when present) the bloom's bounds, CRC
//! and structure but deliberately does not read data blocks; `entries()` streams
//! blocks in key order, verifying each block's CRC on the way.
//!
//! The directory stays on disk as the authoritative index. RAM holds only a
//! sparse index with one [`DirBlockRef`] per *directory block* (its file offset,
//! length, cumulative first data-block index and first fence key), not one
//! entry per data block. A lookup binary-searches that sparse index to pick the
//! directory block, reads and CRC-verifies that one block on demand, parses its
//! fences and binary-searches them. A wrong candidate costs an extra block read
//! but never a wrong answer: the cursor re-verifies the loaded data block and its
//! fence. A directory that fits in a single 4 KiB block therefore yields a
//! single-entry index — not actually sparse, which is fine.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::bloom::Bloom;
use crate::crc::crc32c;
use crate::keys;
use crate::memtable::Memtable;

const MAGIC: &[u8; 8] = b"FDBTILE3";
// The trailer is byte-identical to FDBTILE2's, including its magic, so every
// non-superblock structure stays unchanged across the format bump.
const TRAILER_MAGIC: &[u8; 9] = b"FDBTILE2E";
/// Default page size in bytes. A default-layout tile stores this in its
/// superblock; a stored page-size field of 0 (absent/unknown, e.g. from a writer
/// that predates the field) is also read as this value, so a bad or absent layout
/// value is slow, never wrong (I2).
pub(crate) const DEFAULT_PAGE_SIZE: u32 = 4096;
/// Allowed per-tile page-size range. The value is both the superblock page and
/// the data/directory block target. A stored value outside this range is
/// rejected at open (`InvalidData`); a stored 0 is read as [`DEFAULT_PAGE_SIZE`].
pub(crate) const MIN_PAGE_SIZE: u32 = 4096;
pub(crate) const MAX_PAGE_SIZE: u32 = 65536;
// magic(8) + cutoff(8) + digest(32) + count(8) + dir_offset(8) + dir_len(8)
// + page_size(4) + bloom_offset(8) + bloom_len(8) + bloom_crc32c(4)
// + bits_per_key(4) + hash_count(1) + reserved(3) + crc(4)
const SUPERBLOCK: usize = 108;
const SUPERBLOCK_CRC: usize = 104;
const BLOOM_OFFSET_AT: usize = 76;
const BLOOM_LEN_AT: usize = 84;
const BLOOM_CRC_AT: usize = 92;
const BITS_PER_KEY_AT: usize = 96;
const HASH_COUNT_AT: usize = 100;
// magic(9) + dir_offset(8) + dir_len(8) + crc(4)
const TRAILER: usize = 29;
const TRAILER_CRC: usize = 25;
// block_len(4) + block_crc(4) + first_key_len(4) + entry_count(2)
const BLOCK_HEADER: usize = 14;
// dir_len(4) + dir_crc(4) + fence_count(2)
const DIR_HEADER: usize = 10;
const MAX_KEY: usize = 1 << 24;
const MAX_VALUE: usize = 1 << 26;
// One entry's length prefixes plus the widest legal key and value. The block
// encoder additionally checks the fully framed block fits the u32 `block_len`.
const MAX_ENTRY: usize = 8 + MAX_KEY + MAX_VALUE;
const _: () = assert!(MAX_ENTRY <= u32::MAX as usize);

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

fn valid_key(key: &[u8]) -> bool {
    if key.len() < 18 { return false; }
    let Some(region) = keys::region_prefix(key) else { return false };
    // `region` is `MARKER | entity | 0`; a nonempty entity needs at least 3 bytes,
    // which also rejects the empty-entity spelling.
    if region.len() < 3 { return false; }
    let (family, body) = match region[0] {
        // Family-first: the marker IS the family, and the body is the remainder
        // after `entity | 0`.
        keys::P | keys::U | keys::R => (region[0], &key[region.len()..]),
        // Entity-first: after `entity | 0` comes the family byte, then the body.
        keys::ENTITY_FIRST => match key.get(region.len()) {
            Some(&f) if f == keys::P || f == keys::U || f == keys::R => (f, &key[region.len() + 1..]),
            _ => return false,
        },
        _ => return false,
    };
    match family {
        keys::P => body.len() == 16,
        keys::U | keys::R => {
            let Some(field_end) = body.iter().position(|&b| b == 0) else { return false };
            if field_end == 0 { return false; }
            let rest = &body[field_end + 1..];
            if family == keys::R { rest.len() == 24 }
            else {
                rest.len() >= 20 && usize::try_from(u32::from_be_bytes(rest[..4].try_into().unwrap()))
                    .ok().and_then(|n| n.checked_add(20)) == Some(rest.len())
            }
        }
        _ => false,
    }
}

/// One directory entry: the first key of a data block and where that block lives.
struct Fence {
    first_key: Vec<u8>,
    offset: u64,
    len: u32,
}

/// Retained RAM footprint of one on-disk directory block. The fence records
/// themselves stay on disk; this holds only where the block lives, how many data
/// blocks it precedes (cumulative) and its first fence key, so the sparse index
/// grows with the number of directory blocks, not the number of data blocks.
struct DirBlockRef {
    offset: u64,
    len: u32,
    /// Global data-block index of the block's first fence.
    first_block: u64,
    fence_count: u16,
    first_key: Vec<u8>,
}

/// Directory-block index of the greatest block whose first fence key is `<= key`,
/// or 0 when the key sorts below the first block's first key. The caller reads
/// that one block and binary-searches its fences with [`dir_search`].
fn sparse_search(dir: &[DirBlockRef], key: &[u8]) -> usize {
    let mut lo = 0usize;
    let mut hi = dir.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if dir[mid].first_key.as_slice() <= key { lo = mid + 1; } else { hi = mid; }
    }
    if lo == 0 { 0 } else { lo - 1 }
}

/// Candidate data block for `key`: the greatest fence whose first key is `<= key`,
/// or 0 when every fence is greater (and for an empty directory). The caller
/// still verifies the block it reads, so a wrong candidate only ever costs work,
/// never a wrong answer.
fn dir_search(dir: &[Fence], key: &[u8]) -> usize {
    let mut lo = 0usize;
    let mut hi = dir.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if dir[mid].first_key.as_slice() <= key { lo = mid + 1; } else { hi = mid; }
    }
    if lo == 0 { 0 } else { lo - 1 }
}

#[allow(clippy::too_many_arguments)]
fn superblock_bytes(
    cutoff: u64,
    digest: &[u8; 32],
    count: u64,
    dir_offset: u64,
    dir_len: u64,
    page_size: u32,
    bloom_offset: u64,
    bloom_len: u64,
    bloom_crc32c: u32,
    bits_per_key: u32,
    hash_count: u8,
) -> [u8; SUPERBLOCK] {
    let mut h = [0; SUPERBLOCK];
    h[..8].copy_from_slice(MAGIC);
    h[8..16].copy_from_slice(&cutoff.to_le_bytes());
    h[16..48].copy_from_slice(digest);
    h[48..56].copy_from_slice(&count.to_le_bytes());
    h[56..64].copy_from_slice(&dir_offset.to_le_bytes());
    h[64..72].copy_from_slice(&dir_len.to_le_bytes());
    h[72..76].copy_from_slice(&page_size.to_le_bytes());
    h[BLOOM_OFFSET_AT..BLOOM_LEN_AT].copy_from_slice(&bloom_offset.to_le_bytes());
    h[BLOOM_LEN_AT..BLOOM_CRC_AT].copy_from_slice(&bloom_len.to_le_bytes());
    h[BLOOM_CRC_AT..BITS_PER_KEY_AT].copy_from_slice(&bloom_crc32c.to_le_bytes());
    h[BITS_PER_KEY_AT..HASH_COUNT_AT].copy_from_slice(&bits_per_key.to_le_bytes());
    h[HASH_COUNT_AT] = hash_count;
    let sum = crc32c(&h[..SUPERBLOCK_CRC]);
    h[SUPERBLOCK_CRC..].copy_from_slice(&sum.to_le_bytes());
    h
}

fn trailer_bytes(dir_offset: u64, dir_len: u64) -> [u8; TRAILER] {
    let mut t = [0; TRAILER];
    t[..9].copy_from_slice(TRAILER_MAGIC);
    t[9..17].copy_from_slice(&dir_offset.to_le_bytes());
    t[17..25].copy_from_slice(&dir_len.to_le_bytes());
    let sum = crc32c(&t[..TRAILER_CRC]);
    t[TRAILER_CRC..].copy_from_slice(&sum.to_le_bytes());
    t
}

fn encode_block(first_key: &[u8], entries: &[u8], entry_count: u16) -> io::Result<Vec<u8>> {
    let total = BLOCK_HEADER as u64 + first_key.len() as u64 + entries.len() as u64;
    if total > u32::MAX as u64 { return Err(invalid("tile block exceeds u32 length")); }
    let mut b = vec![0u8; total as usize];
    b[..4].copy_from_slice(&(total as u32).to_le_bytes());
    b[8..12].copy_from_slice(&(first_key.len() as u32).to_le_bytes());
    b[12..14].copy_from_slice(&entry_count.to_le_bytes());
    b[BLOCK_HEADER..BLOCK_HEADER + first_key.len()].copy_from_slice(first_key);
    b[BLOCK_HEADER + first_key.len()..].copy_from_slice(entries);
    let sum = crc32c(&b[8..]);
    b[4..8].copy_from_slice(&sum.to_le_bytes());
    Ok(b)
}

fn encode_dir_block(records: &[u8], fence_count: u16) -> io::Result<Vec<u8>> {
    let total = DIR_HEADER as u64 + records.len() as u64;
    if total > u32::MAX as u64 { return Err(invalid("tile directory block exceeds u32 length")); }
    let mut b = vec![0u8; total as usize];
    b[..4].copy_from_slice(&(total as u32).to_le_bytes());
    b[8..10].copy_from_slice(&fence_count.to_le_bytes());
    b[DIR_HEADER..].copy_from_slice(records);
    let sum = crc32c(&b[8..]);
    b[4..8].copy_from_slice(&sum.to_le_bytes());
    Ok(b)
}

/// CRC-verify one directory block (its own bytes, length prefix included) and
/// parse its fence records. Framing rules match what `Tile::open` enforces: the
/// declared length must equal the buffer, the CRC covers `block[8..]`, and the
/// records must fill the block exactly.
fn decode_dir_block(block: &[u8]) -> io::Result<Vec<Fence>> {
    if block.len() < DIR_HEADER { return Err(invalid("tile directory block truncated")); }
    let blen = u32::from_le_bytes(block[..4].try_into().unwrap()) as usize;
    if blen != block.len() { return Err(invalid("tile directory block length")); }
    let block_crc = u32::from_le_bytes(block[4..8].try_into().unwrap());
    if crc32c(&block[8..]) != block_crc { return Err(invalid("tile directory checksum")); }
    let fence_count = u16::from_le_bytes(block[8..10].try_into().unwrap()) as usize;
    let mut fences = Vec::with_capacity(fence_count);
    let mut q = DIR_HEADER;
    for _ in 0..fence_count {
        if blen - q < 16 { return Err(invalid("tile fence record truncated")); }
        let first_key_len = u32::from_le_bytes(block[q..q + 4].try_into().unwrap()) as usize;
        let block_offset = u64::from_le_bytes(block[q + 4..q + 12].try_into().unwrap());
        let block_len = u32::from_le_bytes(block[q + 12..q + 16].try_into().unwrap());
        q += 16;
        if first_key_len < 1 || first_key_len > MAX_KEY || blen - q < first_key_len {
            return Err(invalid("tile fence key length"));
        }
        fences.push(Fence { first_key: block[q..q + first_key_len].to_vec(), offset: block_offset, len: block_len });
        q += first_key_len;
    }
    if q != blen { return Err(invalid("tile directory block trailing bytes")); }
    Ok(fences)
}

// Positional reads never expose borrowed file-backed bytes. Truncation and
// corruption yield io::Error, never SIGBUS/panic. Open validates the file's
// structure (superblock, trailer, directory, block headers) without touching
// data blocks; `entries()` verifies each data block's CRC as it is read, so a
// corrupt block fails the read that reaches it (the multi-tile merge
// pre-advances every published tile, so corruption reached by any read fails
// closed).
fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match file.read_at(bytes, offset) {
            Ok(0) => return Err(io::Error::new(ErrorKind::UnexpectedEof, "tile truncated during read")),
            Ok(n) => {
                offset = offset.checked_add(n as u64).ok_or_else(|| invalid("tile offset overflow"))?;
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn read_range(file: &File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    let len = usize::try_from(len).map_err(|_| invalid("tile range too large"))?;
    let mut buf = vec![0u8; len];
    read_exact_at(file, &mut buf, offset)?;
    Ok(buf)
}

/// Bloom over exactly the keys a `Tile::write` of `mt` at `cutoff` would store
/// (same `csn <= cutoff` filter). Test-only helper shared by the whole-memtable
/// and streaming sides of the byte-identity test so both build the same filter.
#[cfg(test)]
fn bloom_for_memtable(mt: &Memtable, cutoff: u64, bits_per_key: u32) -> Bloom {
    let mut bloom = crate::bloom::BloomBuilder::new(mt.len() as u64, bits_per_key);
    for (key, _) in mt.entries() {
        if keys::key_csn(key).is_none_or(|csn| csn > cutoff) { continue; }
        bloom.insert(key);
    }
    bloom.finish()
}

pub(crate) struct Tile {
    file: File,
    superblock: [u8; SUPERBLOCK],
    trailer: [u8; TRAILER],
    cutoff: u64,
    digest: [u8; 32],
    count: u64,
    size: u64,
    /// Sparse directory index: one entry per on-disk directory block. The fence
    /// records stay on disk; see the module docs.
    dir: Vec<DirBlockRef>,
    /// Total number of data blocks (directory fence records) in the tile.
    data_blocks: u64,
    /// First key in the tile (the first fence's key). Empty for an empty tile.
    /// Cached in RAM with `last_key` so [`Tile::could_contain_prefix`] can reject
    /// a whole tile without touching a cursor or the directory; the footprint is
    /// two byte strings per tile, bounded by the tile count rather than by data.
    first_key: Vec<u8>,
    /// Last key in the tile, read once from the final data block at open. `None`
    /// for an empty tile, and also when that single read could not be trusted
    /// (framing or CRC failure): a missing upper bound only makes
    /// [`Tile::could_contain_prefix`] more conservative, never wrong. Data blocks
    /// are still CRC-verified when actually read, so a corrupt tile still fails
    /// closed on any read that reaches it.
    last_key: Option<Vec<u8>>,
    /// The tile's persisted bloom filter, loaded and structurally validated at
    /// open. `None` means the tile has no bloom (`bloom_len == 0`), in which case
    /// [`Tile::might_contain_unique_prefix`] answers `true` for every prefix.
    bloom: Option<Bloom>,
}

/// Smallest key strictly greater than every key that starts with `prefix`:
/// increment the last non-`0xff` byte and drop everything after it. `None` when
/// every byte is `0xff`, i.e. there is no finite upper bound. Used by
/// [`Tile::could_contain_prefix`] to build the exclusive lower bound of the next
/// key range. Conservative: a `false` answer is a proof, a `true` answer is only
/// a maybe.
fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut i = prefix.len();
    while i > 0 {
        i -= 1;
        if prefix[i] != 0xff {
            let mut s = prefix[..=i].to_vec();
            s[i] += 1;
            return Some(s);
        }
    }
    None
}

/// Cursor-local cache of exactly one on-disk directory block. Advancing through
/// the data blocks only re-reads the directory when it crosses a block boundary,
/// so a full scan reads each directory block at most once while retaining no
/// more fences than a single directory block holds.
struct DirCache {
    index: Option<usize>,
    base: u64,
    fences: Vec<Fence>,
}

impl DirCache {
    fn new() -> Self {
        DirCache { index: None, base: 0, fences: Vec::new() }
    }

    /// The fence for global data block `block`, loading the containing directory
    /// block on a miss. The returned reference borrows the cache, so callers do
    /// not copy each fence out of it.
    fn fence<'t>(&'t mut self, tile: &'t Tile, block: usize) -> io::Result<&'t Fence> {
        let index = tile.dir_block_of(block);
        if self.index != Some(index) {
            self.fences = tile.read_dir_fences(index)?;
            self.base = tile.dir[index].first_block;
            self.index = Some(index);
        }
        self.fences.get(block - self.base as usize).ok_or_else(|| invalid("tile directory block changed"))
    }
}

/// Pull cursor over a tile's data blocks. Entries borrow the cursor's own block
/// buffer, so streaming allocates nothing per entry (only the reused prefix and
/// last-key buffers). Without a prefix it yields every entry; with one it starts
/// at the block `candidate` selects, skips entries sorting below the prefix,
/// yields only keys that begin with it, and stops at the first key past the
/// prefix range without reading further blocks. Yielded keys must be strictly
/// increasing; any other order is `InvalidData`.
pub(crate) struct TileCursor<'a> {
    tile: &'a Tile,
    prefix: Option<Vec<u8>>,
    checked: bool,
    block: usize,
    buffer: Vec<u8>,
    pos: usize,
    left: u16,
    /// Byte range of the parsed-but-not-yet-returned entry: key start, key end, value end.
    current: Option<(usize, usize, usize)>,
    /// Last yielded key, reused to prove strict order without a per-entry allocation.
    last: Vec<u8>,
    started: bool,
    done: bool,
    /// At most one parsed directory block, read on demand from the sparse index.
    dir: DirCache,
    /// Data-block bytes read through this cursor so far. Purely observational:
    /// no read decision in this module consults it, and it changes no on-disk
    /// byte. Directory blocks, the superblock and the trailer are not counted;
    /// this is exactly the data-block traffic a probe or scan pays, which is
    /// what a bloom reject avoids.
    bytes_read: u64,
}

impl<'a> TileCursor<'a> {
    /// Data-block bytes this cursor has read from disk so far. Observability
    /// only: nothing in this module reads it, and it changes no on-disk byte.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    fn new(tile: &'a Tile, prefix: Option<&[u8]>, block: usize) -> Self {
        TileCursor {
            tile,
            prefix: prefix.map(<[u8]>::to_vec),
            checked: false,
            block,
            buffer: Vec::new(),
            pos: 0,
            left: 0,
            current: None,
            last: Vec::new(),
            started: false,
            done: false,
            dir: DirCache::new(),
            bytes_read: 0,
        }
    }

    /// Read and validate the block selected by `self.block`, resetting the entry
    /// position to its first entry.
    fn load_block(&mut self) -> io::Result<()> {
        let (offset, len) = {
            let fence = self.dir.fence(self.tile, self.block)?;
            (fence.offset, fence.len as usize)
        };
        self.buffer.resize(len, 0);
        read_exact_at(&self.tile.file, &mut self.buffer, offset)?;
        // Observability counter (never consulted here): the block length actually
        // read. `len` is the fence's `len`, so this is the on-disk data-block size.
        self.bytes_read = self.bytes_read.saturating_add(self.buffer.len() as u64);
        if self.buffer.len() < BLOCK_HEADER { return Err(invalid("tile block truncated")); }
        let blen = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        let block_crc = u32::from_le_bytes(self.buffer[4..8].try_into().unwrap());
        if blen != self.buffer.len() { return Err(invalid("tile block length changed")); }
        if crc32c(&self.buffer[8..]) != block_crc { return Err(invalid("tile block checksum")); }
        let first_key_len = u32::from_le_bytes(self.buffer[8..12].try_into().unwrap()) as usize;
        if BLOCK_HEADER + first_key_len > blen { return Err(invalid("tile block first key out of bounds")); }
        // Re-bind the block to its fence on every load. Open verified this once,
        // but the fence now comes from a per-read directory block: a
        // CRC-consistent rewrite of the file could otherwise serve a block whose
        // first key disagrees with the fence and, because the order check only
        // sees *yielded* keys, a prefix cursor could stop early and silently drop
        // matches.
        let rebound = {
            let fence = self.dir.fence(self.tile, self.block)?;
            self.buffer[BLOCK_HEADER..BLOCK_HEADER + first_key_len] == *fence.first_key
        };
        if !rebound { return Err(invalid("tile block first key disagrees with its fence")); }
        self.pos = BLOCK_HEADER + first_key_len;
        self.left = u16::from_le_bytes(self.buffer[12..14].try_into().unwrap());
        Ok(())
    }

    /// Position `self.current` on the next acceptable entry, loading blocks as
    /// needed; returns false once the stream (or prefix range) is exhausted.
    fn fill(&mut self) -> io::Result<bool> {
        if self.current.is_some() { return Ok(true); }
        if self.done { return Ok(false); }
        if !self.checked {
            self.checked = true;
            if self.tile.file.metadata()?.len() != self.tile.size { return Err(invalid("tile size changed")); }
            let mut sb = [0u8; SUPERBLOCK];
            read_exact_at(&self.tile.file, &mut sb, 0)?;
            if sb != self.tile.superblock { return Err(invalid("tile superblock changed")); }
            let mut tr = [0u8; TRAILER];
            read_exact_at(&self.tile.file, &mut tr, self.tile.size - TRAILER as u64)?;
            if tr != self.tile.trailer { return Err(invalid("tile trailer changed")); }
        }
        loop {
            if self.left == 0 {
                if self.block as u64 >= self.tile.data_blocks { self.done = true; return Ok(false); }
                // A block whose first key sorts past the prefix without starting
                // with it, and every later block, cannot match: stop before
                // reading. The fence comes from the cached directory block.
                if let Some(prefix) = &self.prefix {
                    let first = { let fence = self.dir.fence(self.tile, self.block)?; fence.first_key.as_slice() > prefix.as_slice() && !fence.first_key.starts_with(prefix.as_slice()) };
                    if first {
                        self.done = true;
                        return Ok(false);
                    }
                }
                self.load_block()?;
                if self.left == 0 { return Err(invalid("tile block declares no entries")); }
            }
            let buf = &self.buffer;
            if self.pos + 8 > buf.len() { return Err(invalid("tile entry header truncated")); }
            let k = u32::from_le_bytes(buf[self.pos..self.pos + 4].try_into().unwrap()) as usize;
            let v = u32::from_le_bytes(buf[self.pos + 4..self.pos + 8].try_into().unwrap()) as usize;
            if !(18..=MAX_KEY).contains(&k) || v > MAX_VALUE { return Err(invalid("tile entry length out of bounds")); }
            let start = self.pos + 8;
            let mid = start.checked_add(k).ok_or_else(|| invalid("tile key size overflow"))?;
            let end = mid.checked_add(v).ok_or_else(|| invalid("tile value size overflow"))?;
            if end > buf.len() { return Err(invalid("tile entry exceeds block")); }
            let key = &buf[start..mid];
            if let Some(prefix) = &self.prefix {
                if !(key.len() >= prefix.len() && &key[..prefix.len()] == prefix.as_slice()) {
                    if key < prefix.as_slice() {
                        // Below the prefix range: skip and keep scanning.
                        self.pos = end;
                        self.left -= 1;
                        if self.left == 0 { self.block += 1; }
                        continue;
                    }
                    // Past the prefix range: no later key can match.
                    self.done = true;
                    return Ok(false);
                }
            }
            if self.started && key <= self.last.as_slice() {
                return Err(invalid("tile keys out of order"));
            }
            self.last.clear();
            self.last.extend_from_slice(key);
            self.started = true;
            self.current = Some((start, mid, end));
            self.pos = end;
            self.left -= 1;
            if self.left == 0 { self.block += 1; }
            return Ok(true);
        }
    }

    /// The pending entry without consuming it, borrowing the block buffer.
    pub(crate) fn peek(&mut self) -> io::Result<Option<(&[u8], &[u8])>> {
        if !self.fill()? { return Ok(None); }
        let (start, mid, end) = self.current.unwrap();
        Ok(Some((&self.buffer[start..mid], &self.buffer[mid..end])))
    }

    /// Consume and return the pending entry, borrowing the block buffer.
    pub(crate) fn next(&mut self) -> io::Result<Option<(&[u8], &[u8])>> {
        if !self.fill()? { return Ok(None); }
        let (start, mid, end) = self.current.take().unwrap();
        Ok(Some((&self.buffer[start..mid], &self.buffer[mid..end])))
    }
}

/// Owned (copying) view over a cursor, preserving `Tile::entries`'s
/// `io::Result<(Vec<u8>, Vec<u8>)>` item type for callers that need owned keys.
struct Entries<'a> {
    cursor: TileCursor<'a>,
    done: bool,
}

impl Iterator for Entries<'_> {
    type Item = io::Result<(Vec<u8>, Vec<u8>)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done { return None; }
        match self.cursor.next() {
            Ok(Some((key, value))) => Some(Ok((key.to_vec(), value.to_vec()))),
            Ok(None) => { self.done = true; None }
            Err(e) => { self.done = true; Some(Err(e)) }
        }
    }
}

/// Streaming FDBTILE3 encoder: buffers only the in-progress block, so a tile can be
/// produced from a key-ordered iterator without materializing its entries. `push`
/// requires strictly increasing keys (the memtable used to provide that ordering
/// implicitly); [`TileWriter::set_bloom`] optionally attaches a filter that `finish`
/// writes between the data blocks and the directory; `finish` writes the directory,
/// the trailer and - last, so a torn file cannot have a valid magic - the superblock,
/// then syncs and reopens for the same self-check `Tile::write` performs. `Tile::write`
/// is implemented on top of this so there is exactly one encoder.
pub(crate) struct TileWriter {
    file: File,
    path: PathBuf,
    cutoff: u64,
    digest: [u8; 32],
    /// The tile's page size: the reserved superblock page and the data/directory
    /// block target. Validated at construction; written into the superblock.
    page_size: u32,
    offset: u64,
    count: u64,
    fences: Vec<Fence>,
    first_key: Vec<u8>,
    entries: Vec<u8>,
    block_entries: u16,
    last_key: Vec<u8>,
    started: bool,
    finished: bool,
    bloom: Option<Bloom>,
    /// Opt-in bounded durability cadence: `flush_block` calls `sync_data` once
    /// the accumulated unsynced data-block bytes reach this threshold. Off by
    /// default (the ordinary publish path keeps one final `sync_data`); the
    /// off-path compaction build sets it so a large merge does not flush its
    /// whole dirty set in a single `finish` `sync_data`.
    sync_every: Option<u64>,
    unsynced: u64,
}

impl TileWriter {
    /// `create_new` never replaces an existing tile. A failed or crashed build leaves a
    /// partial candidate that cannot change the engine's serving state; the caller may
    /// remove it explicitly. `page_size` is the per-tile block target and superblock
    /// page; it must be in [`MIN_PAGE_SIZE`]..=[`MAX_PAGE_SIZE`] or the build is
    /// rejected (`InvalidData`).
    pub(crate) fn new(path: &Path, cutoff: u64, digest: &[u8; 32], page_size: u32) -> io::Result<Self> {
        if !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size) {
            return Err(invalid("tile page size out of range"));
        }
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        // Reserve the superblock page; the real superblock is written last, once the
        // directory offset/length are known. This puts the first data block on the
        // page-size boundary.
        file.write_all(&vec![0u8; page_size as usize])?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            cutoff,
            digest: *digest,
            page_size,
            offset: page_size as u64,
            count: 0,
            fences: Vec::new(),
            first_key: Vec::new(),
            entries: Vec::new(),
            block_entries: 0,
            last_key: Vec::new(),
            started: false,
            finished: false,
            bloom: None,
            sync_every: None,
            unsynced: 0,
        })
    }

    /// Enable a bounded durability cadence: every `bytes` of data-block output
    /// (measured since the last sync) is made durable with `sync_data` before
    /// more is buffered. `0` disables the cadence (the default). Syncing never
    /// changes the encoded bytes, so the written tile is byte-identical to an
    /// un-synced write of the same stream.
    pub(crate) fn set_sync_every(&mut self, bytes: u64) {
        self.sync_every = (bytes > 0).then_some(bytes);
        self.unsynced = 0;
    }

    /// Append one entry. Keys must be strictly increasing; anything a `Tile::write`
    /// of the same sequence would reject is rejected here with the same error.
    pub(crate) fn push(&mut self, key: &[u8], value: &[u8]) -> io::Result<()> {
        if self.finished { return Err(invalid("tile writer already finished")); }
        if !valid_key(key) || key.len() > MAX_KEY || value.len() > MAX_VALUE {
            return Err(invalid("unsupported tile entry; memtable unchanged"));
        }
        if self.started && key <= self.last_key.as_slice() {
            return Err(invalid("tile keys must be strictly increasing"));
        }
        let entry_size = 8u64 + key.len() as u64 + value.len() as u64;
        if entry_size > MAX_ENTRY as u64 { return Err(invalid("tile entry size overflow")); }
        // Whole entries only: start a new block when the next one would not fit.
        if !self.entries.is_empty() && self.entries.len() as u64 + entry_size > self.page_size as u64 {
            self.flush_block()?;
        }
        if self.block_entries == u16::MAX { return Err(invalid("tile block entry count overflow")); }
        if self.block_entries == 0 { self.first_key = key.to_vec(); }
        self.entries.extend_from_slice(&(key.len() as u32).to_le_bytes());
        self.entries.extend_from_slice(&(value.len() as u32).to_le_bytes());
        self.entries.extend_from_slice(key);
        self.entries.extend_from_slice(value);
        self.block_entries += 1;
        self.count = self.count.checked_add(1).ok_or_else(|| invalid("tile count overflow"))?;
        self.last_key.clear();
        self.last_key.extend_from_slice(key);
        self.started = true;
        Ok(())
    }

    /// Attach the persisted bloom filter this tile will carry. Must be called
    /// before `finish`; calling it twice (or on an already-finished writer) is an
    /// error so a tile can never silently carry the wrong filter. The filter is
    /// independent of the entry stream, so it may be set before or after `push`.
    pub(crate) fn set_bloom(&mut self, bloom: Bloom) -> io::Result<()> {
        if self.finished { return Err(invalid("tile writer already finished")); }
        if self.bloom.is_some() { return Err(invalid("tile bloom already set")); }
        self.bloom = Some(bloom);
        Ok(())
    }

    fn flush_block(&mut self) -> io::Result<()> {
        if self.entries.is_empty() { return Ok(()); }
        let block = encode_block(&self.first_key, &self.entries, self.block_entries)?;
        self.fences.push(Fence { first_key: std::mem::take(&mut self.first_key), offset: self.offset, len: block.len() as u32 });
        self.file.write_all(&block)?;
        self.offset = self.offset.checked_add(block.len() as u64).ok_or_else(|| invalid("tile offset overflow"))?;
        if let Some(every) = self.sync_every {
            self.unsynced = self.unsynced.saturating_add(block.len() as u64);
            if self.unsynced >= every {
                self.file.sync_data()?;
                self.unsynced = 0;
            }
        }
        self.entries.clear();
        self.block_entries = 0;
        Ok(())
    }

    /// Directory: one fence record per data block, chunked into CRC-protected blocks.
    fn write_directory(&mut self) -> io::Result<(u64, u64)> {
        let dir_offset = self.offset;
        let mut dir_len = 0u64;
        let mut records: Vec<u8> = Vec::new();
        let mut rec_count: u16 = 0;
        for fence in &self.fences {
            let rec_len = 16usize + fence.first_key.len();
            if !records.is_empty() && DIR_HEADER + records.len() + rec_len > self.page_size as usize {
                let block = encode_dir_block(&records, rec_count)?;
                self.file.write_all(&block)?;
                dir_len = dir_len.checked_add(block.len() as u64).ok_or_else(|| invalid("tile directory size overflow"))?;
                records.clear();
                rec_count = 0;
            }
            if rec_count == u16::MAX { return Err(invalid("tile directory fence count overflow")); }
            records.extend_from_slice(&(fence.first_key.len() as u32).to_le_bytes());
            records.extend_from_slice(&fence.offset.to_le_bytes());
            records.extend_from_slice(&fence.len.to_le_bytes());
            records.extend_from_slice(&fence.first_key);
            rec_count += 1;
        }
        if !records.is_empty() {
            let block = encode_dir_block(&records, rec_count)?;
            self.file.write_all(&block)?;
            dir_len = dir_len.checked_add(block.len() as u64).ok_or_else(|| invalid("tile directory size overflow"))?;
        }
        Ok((dir_len, dir_offset))
    }

    pub(crate) fn finish(mut self) -> io::Result<Tile> {
        if self.finished { return Err(invalid("tile writer already finished")); }
        self.finished = true;
        self.flush_block()?;
        // Bloom section, when present: between the data blocks and the directory.
        // Absent (`bloom_len == 0`) is canonical and means "no bloom".
        let (bloom_offset, bloom_len, bloom_crc32c, bits_per_key, hash_count) = match &self.bloom {
            Some(bloom) => {
                let bytes = bloom.to_bytes();
                let offset = self.offset;
                self.file.write_all(&bytes)?;
                self.offset = self.offset.checked_add(bytes.len() as u64).ok_or_else(|| invalid("tile offset overflow"))?;
                (offset, bytes.len() as u64, crc32c(&bytes), bloom.bits_per_key(), bloom.hash_count())
            }
            None => (self.offset, 0, 0, 0, 0),
        };
        let (dir_len, dir_offset) = self.write_directory()?;
        self.file.write_all(&trailer_bytes(dir_offset, dir_len))?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&superblock_bytes(
            self.cutoff,
            &self.digest,
            self.count,
            dir_offset,
            dir_len,
            self.page_size,
            bloom_offset,
            bloom_len,
            bloom_crc32c,
            bits_per_key,
            hash_count,
        ))?;
        self.file.sync_data()?; // No parent-directory durability claim.
        let tile = Tile::open(&self.path)?;
        if tile.cutoff != self.cutoff || tile.digest != self.digest || tile.count() != self.count {
            return Err(invalid("tile verification mismatch"));
        }
        Ok(tile)
    }
}

impl Tile {
    /// Whole-memtable convenience writer. Production publishes stream through
    /// [`TileWriter`] directly (10c-2); this stays as the test-facing entry point that
    /// the byte-identity test compares the streaming API against.
    #[cfg(test)]
    pub(crate) fn write(path: &Path, cutoff: u64, digest: &[u8; 32], mt: &Memtable) -> io::Result<Self> {
        let mut writer = TileWriter::new(path, cutoff, digest, DEFAULT_PAGE_SIZE)?;
        for (key, value) in mt.entries() {
            if keys::key_csn(key).is_none_or(|csn| csn > cutoff) { continue; }
            writer.push(key, value)?;
        }
        writer.finish()
    }

    /// Whole-memtable writer that also attaches a bloom over each stored key. The
    /// test-facing counterpart of streaming `TileWriter` + [`TileWriter::set_bloom`];
    /// the byte-identity test drives both so a change to either encoder fails it.
    #[cfg(test)]
    pub(crate) fn write_with_bloom(path: &Path, cutoff: u64, digest: &[u8; 32], mt: &Memtable, bits_per_key: u32) -> io::Result<Self> {
        let bloom = bloom_for_memtable(mt, cutoff, bits_per_key);
        let mut writer = TileWriter::new(path, cutoff, digest, DEFAULT_PAGE_SIZE)?;
        for (key, value) in mt.entries() {
            if keys::key_csn(key).is_none_or(|csn| csn > cutoff) { continue; }
            writer.push(key, value)?;
        }
        writer.set_bloom(bloom)?;
        writer.finish()
    }

    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < SUPERBLOCK as u64 { return Err(invalid("tile too small")); }
        let mut sb = [0u8; SUPERBLOCK];
        read_exact_at(&file, &mut sb, 0)?;
        if &sb[..8] != MAGIC { return Err(invalid("tile superblock magic")); }
        if crc32c(&sb[..SUPERBLOCK_CRC]) != u32::from_le_bytes(sb[SUPERBLOCK_CRC..].try_into().unwrap()) {
            return Err(invalid("tile superblock checksum"));
        }
        let cutoff = u64::from_le_bytes(sb[8..16].try_into().unwrap());
        let digest: [u8; 32] = sb[16..48].try_into().unwrap();
        let count = u64::from_le_bytes(sb[48..56].try_into().unwrap());
        let dir_offset = u64::from_le_bytes(sb[56..64].try_into().unwrap());
        let dir_len = u64::from_le_bytes(sb[64..72].try_into().unwrap());
        // A layout value must never make a read wrong (I2): a stored 0 means
        // "absent/unknown" and is read as the default page size, while a nonzero
        // value outside the allowed range fails closed. The value is honored as
        // the data region's lower bound (the superblock page) below.
        let stored_page_size = u32::from_le_bytes(sb[72..76].try_into().unwrap());
        let page_size = if stored_page_size == 0 {
            DEFAULT_PAGE_SIZE
        } else if (MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&stored_page_size) {
            stored_page_size
        } else {
            return Err(invalid("tile page size out of range"));
        };
        if size < page_size as u64 + TRAILER as u64 { return Err(invalid("tile too small")); }
        let bloom_offset = u64::from_le_bytes(sb[BLOOM_OFFSET_AT..BLOOM_LEN_AT].try_into().unwrap());
        let bloom_len = u64::from_le_bytes(sb[BLOOM_LEN_AT..BLOOM_CRC_AT].try_into().unwrap());
        let bloom_crc32c = u32::from_le_bytes(sb[BLOOM_CRC_AT..BITS_PER_KEY_AT].try_into().unwrap());
        let bloom_bits_per_key = u32::from_le_bytes(sb[BITS_PER_KEY_AT..HASH_COUNT_AT].try_into().unwrap());
        let bloom_hash_count = sb[HASH_COUNT_AT];
        let mut tr = [0u8; TRAILER];
        read_exact_at(&file, &mut tr, size - TRAILER as u64)?;
        if &tr[..9] != TRAILER_MAGIC { return Err(invalid("tile trailer magic")); }
        if crc32c(&tr[..TRAILER_CRC]) != u32::from_le_bytes(tr[TRAILER_CRC..].try_into().unwrap()) {
            return Err(invalid("tile trailer checksum"));
        }
        if u64::from_le_bytes(tr[9..17].try_into().unwrap()) != dir_offset
            || u64::from_le_bytes(tr[17..25].try_into().unwrap()) != dir_len {
            return Err(invalid("tile trailer disagrees with superblock"));
        }
        // The directory must end exactly at the trailer, and start after the
        // superblock page, so no gap or overlap can hide data blocks.
        if dir_offset < page_size as u64 || dir_offset.checked_add(dir_len) != Some(size - TRAILER as u64) {
            return Err(invalid("tile directory bounds"));
        }
        // Stream the directory: one block at a time, validating and then dropping
        // its fences, so open RAM is the sparse index (one entry per directory
        // block) rather than one entry per data block. Every check the old in-RAM
        // pass made is still performed here (CRC, framing, fence monotonicity
        // across the whole directory, data-block offset monotonicity and
        // non-overlap, and each block header); only the fence catalog is not
        // retained. Validation cost stays O(directory); its RAM does not.
        let mut dir: Vec<DirBlockRef> = Vec::new();
        let mut data_blocks: u64 = 0;
        let mut previous_key: Option<Vec<u8>> = None;
        let mut previous_end: Option<u64> = None;
        let mut last_fence: Option<(u64, u32)> = None;
        let mut total: u64 = 0;
        let mut hdr = [0u8; BLOCK_HEADER];
        let mut key_buf: Vec<u8> = Vec::new();
        if dir_len > 0 {
            let dir_end = dir_offset + dir_len; // == size - TRAILER, checked above
            let mut pos = dir_offset;
            while pos < dir_end {
                let mut len_buf = [0u8; 4];
                read_exact_at(&file, &mut len_buf, pos)?;
                let blen = u32::from_le_bytes(len_buf) as u64;
                if blen < DIR_HEADER as u64 || blen > dir_end - pos {
                    return Err(invalid("tile directory block length"));
                }
                let block = read_range(&file, pos, blen)?;
                let block_fences = decode_dir_block(&block)?;
                dir.push(DirBlockRef {
                    offset: pos,
                    len: blen as u32,
                    first_block: data_blocks,
                    fence_count: block_fences.len() as u16,
                    first_key: block_fences.first().map(|f| f.first_key.clone()).unwrap_or_default(),
                });
                for i in 0..block_fences.len() {
                    let fence = &block_fences[i];
                    if fence.offset < page_size as u64 || (fence.len as usize) < BLOCK_HEADER {
                        return Err(invalid("tile block offset/length"));
                    }
                    let end = fence.offset.checked_add(fence.len as u64).ok_or_else(|| invalid("tile block overflow"))?;
                    if end > dir_offset { return Err(invalid("tile block outside data region")); }
                    // Data blocks must be strictly increasing and non-overlapping: a
                    // directory whose offsets collide or overlap can alias blocks.
                    if previous_end.is_some_and(|prev_end| fence.offset < prev_end) {
                        return Err(invalid("tile data blocks overlap or are out of order"));
                    }
                    // Fence keys must be strictly increasing across the whole
                    // directory, including across directory-block boundaries
                    // (within a block, compare against the previous record directly).
                    let prev = if i == 0 { previous_key.as_deref() } else { Some(block_fences[i - 1].first_key.as_slice()) };
                    if prev.is_some_and(|prev| prev >= fence.first_key.as_slice()) {
                        return Err(invalid("tile fence keys not strictly increasing"));
                    }
                    previous_end = Some(end);
                    last_fence = Some((fence.offset, fence.len));
                    // Block header consistency: the header's length, first key and entry
                    // count must agree with the directory (data CRC is checked on read).
                    read_exact_at(&file, &mut hdr, fence.offset)?;
                    let hlen = u32::from_le_bytes(hdr[..4].try_into().unwrap());
                    let first_key_len = u32::from_le_bytes(hdr[8..12].try_into().unwrap()) as usize;
                    let entry_count = u16::from_le_bytes(hdr[12..14].try_into().unwrap());
                    if hlen != fence.len { return Err(invalid("tile block length mismatch")); }
                    if first_key_len != fence.first_key.len() { return Err(invalid("tile block first key length mismatch")); }
                    if BLOCK_HEADER + first_key_len > fence.len as usize { return Err(invalid("tile block first key out of bounds")); }
                    if entry_count == 0 { return Err(invalid("tile block declares no entries")); }
                    key_buf.resize(first_key_len, 0);
                    read_exact_at(&file, &mut key_buf, fence.offset + BLOCK_HEADER as u64)?;
                    if key_buf != fence.first_key { return Err(invalid("tile block first key mismatch")); }
                    total = total.checked_add(entry_count as u64).ok_or_else(|| invalid("tile entry count overflow"))?;
                }
                if let Some(last) = block_fences.last() { previous_key = Some(last.first_key.clone()); }
                data_blocks += block_fences.len() as u64;
                pos += blen;
            }
            if pos != dir_end { return Err(invalid("tile directory bounds")); }
        } else if count != 0 {
            return Err(invalid("tile directory missing for nonempty tile"));
        }
        if total != count { return Err(invalid("tile block entry count disagrees")); }
        // Bloom section: between the data blocks and the directory. `bloom_len == 0`
        // is the canonical "no bloom" case (the empty tile and any tile whose writer
        // never called `set_bloom`). When present, open validates its bounds (it must
        // start at or after the last data block and end at or before the directory, so
        // it can neither overlap a data block nor hide inside the directory), its CRC,
        // and its structure via `Bloom::from_bytes`, all without reading data blocks.
        let bloom = if bloom_len == 0 {
            None
        } else {
            let bloom_end = bloom_offset.checked_add(bloom_len).ok_or_else(|| invalid("tile bloom bounds"))?;
            if bloom_offset < page_size as u64 || bloom_end > dir_offset {
                return Err(invalid("tile bloom bounds"));
            }
            let data_end = match last_fence {
                Some((offset, len)) => offset.checked_add(len as u64).ok_or_else(|| invalid("tile block overflow"))?,
                None => page_size as u64,
            };
            if bloom_offset < data_end { return Err(invalid("tile bloom overlaps data blocks")); }
            let bytes = read_range(&file, bloom_offset, bloom_len)?;
            if crc32c(&bytes) != bloom_crc32c { return Err(invalid("tile bloom checksum")); }
            let bloom = Bloom::from_bytes(&bytes)?;
            if bloom.bits_per_key() != bloom_bits_per_key || bloom.hash_count() != bloom_hash_count {
                return Err(invalid("tile bloom header disagrees with superblock"));
            }
            Some(bloom)
        };
        let first_key = dir.first().map(|r| r.first_key.clone()).unwrap_or_default();
        let last_key = match (count, last_fence) {
            (0, _) | (_, None) => None,
            (_, Some((offset, len))) => Self::read_last_key(&file, offset, len),
        };
        Ok(Tile {
            file, superblock: sb, trailer: tr, cutoff, digest, count, size, dir, data_blocks,
            first_key, last_key, bloom,
        })
    }

    /// Best-effort last key of one data block: checks length and CRC, then walks
    /// its entries requiring them strictly increasing (the writer's invariant and
    /// what the read path's order check enforces), returning the final key. `None`
    /// on any framing/CRC/order failure so the caller falls back to "no upper
    /// bound" rather than storing a key that could reject a tile which does contain
    /// the prefix. The block is not re-verified on later reads through this path;
    /// the read path still CRC-checks it.
    fn read_last_key(file: &File, offset: u64, len: u32) -> Option<Vec<u8>> {
        let block = read_range(file, offset, len as u64).ok()?;
        if block.len() < BLOCK_HEADER { return None; }
        let blen = u32::from_le_bytes(block[..4].try_into().unwrap()) as usize;
        if blen != block.len() { return None; }
        if crc32c(&block[8..]) != u32::from_le_bytes(block[4..8].try_into().unwrap()) { return None; }
        let first_key_len = u32::from_le_bytes(block[8..12].try_into().unwrap()) as usize;
        if BLOCK_HEADER + first_key_len > blen { return None; }
        let entry_count = u16::from_le_bytes(block[12..14].try_into().unwrap());
        let mut pos = BLOCK_HEADER + first_key_len;
        let mut last: Option<Vec<u8>> = None;
        for _ in 0..entry_count {
            if pos + 8 > block.len() { return None; }
            let k = u32::from_le_bytes(block[pos..pos + 4].try_into().unwrap()) as usize;
            let v = u32::from_le_bytes(block[pos + 4..pos + 8].try_into().unwrap()) as usize;
            if !(18..=MAX_KEY).contains(&k) || v > MAX_VALUE { return None; }
            let start = pos + 8;
            let mid = start.checked_add(k)?;
            let end = mid.checked_add(v)?;
            if end > block.len() { return None; }
            let key = &block[start..mid];
            if last.as_deref().is_some_and(|prev| key <= prev) { return None; }
            last = Some(key.to_vec());
            pos = end;
        }
        last
    }

    /// Directory-block index whose fence range contains global data block
    /// `block`: the last block whose `first_block` is `<= block`.
    fn dir_block_of(&self, block: usize) -> usize {
        let block = block as u64;
        let mut lo = 0usize;
        let mut hi = self.dir.len();
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.dir[mid].first_block <= block { lo = mid + 1; } else { hi = mid; }
        }
        if lo == 0 { 0 } else { lo - 1 }
    }

    /// Read, CRC-verify and parse one directory block on demand. The block is
    /// checked against the open-time sparse entry so a CRC-consistent rewrite
    /// cannot make the cursor index past the block's fences.
    fn read_dir_fences(&self, index: usize) -> io::Result<Vec<Fence>> {
        let r = self.dir.get(index).ok_or_else(|| invalid("tile directory index"))?;
        let block = read_range(&self.file, r.offset, r.len as u64)?;
        let fences = decode_dir_block(&block)?;
        if fences.len() != r.fence_count as usize
            || fences.first().map(|f| f.first_key.as_slice()).unwrap_or_default() != r.first_key.as_slice() {
            return Err(invalid("tile directory block changed"));
        }
        Ok(fences)
    }

    /// Global data-block index of the greatest fence whose first key is `<= key`,
    /// or 0 when every fence is greater (and for an empty directory). Reads at
    /// most one directory block; the caller still verifies the block it loads, so
    /// a wrong candidate only ever costs an extra read, never a wrong answer.
    fn candidate(&self, key: &[u8]) -> io::Result<usize> {
        if self.dir.is_empty() { return Ok(0); }
        let di = sparse_search(&self.dir, key);
        let fences = self.read_dir_fences(di)?;
        let li = dir_search(&fences, key);
        Ok(self.dir[di].first_block as usize + li)
    }

    pub(crate) fn cutoff(&self) -> u64 { self.cutoff }
    pub(crate) fn digest(&self) -> &[u8; 32] { &self.digest }
    pub(crate) fn count(&self) -> u64 { self.count }

    /// The tile's smallest stored key, or `None` for an empty tile. Cached in
    /// RAM at open/write; used by recovery to derive the tile's region for the
    /// `(cutoff, region)` ordering and per-region `lower` bound.
    pub(crate) fn first_key(&self) -> Option<&[u8]> {
        (!self.first_key.is_empty()).then_some(self.first_key.as_slice())
    }

    /// The tile's largest stored key, or `None` for an empty tile or a tile
    /// whose final block was not trusted. A single-region tile has
    /// `region_prefix(first) == region_prefix(last)`.
    pub(crate) fn last_key(&self) -> Option<&[u8]> { self.last_key.as_deref() }

    /// Conservative prefix fast-reject: returns `false` ONLY when it is certain
    /// that no stored key starts with `prefix`, so a `false` never hides a match
    /// (a false NEGATIVE would be a correctness bug; a false positive only costs
    /// the work the filter exists to avoid). The two sufficient conditions:
    ///
    /// - `prefix > last_key`: every stored key is `<= last_key < prefix`, and any
    ///   key starting with `prefix` sorts at or above `prefix`.
    /// - `prefix_successor(prefix) <= first_key`: every key starting with `prefix`
    ///   sorts strictly below its successor, which is already at or below the
    ///   tile's first key.
    ///
    /// An empty tile answers `false` (it holds no keys). A tile whose cached last
    /// key was untrustworthy answers with only the lower-bound test. An empty
    /// `prefix` matches every key, so it always answers `true`.
    pub(crate) fn could_contain_prefix(&self, prefix: &[u8]) -> bool {
        if self.count == 0 { return false; }
        if let Some(last) = &self.last_key {
            if prefix > last.as_slice() { return false; }
        }
        if let Some(successor) = prefix_successor(prefix) {
            if successor.as_slice() <= self.first_key.as_slice() { return false; }
        }
        true
    }

    /// Bloom fast-reject for a unique-index prefix: `false` ONLY when the tile's
    /// persisted bloom is certain no key with this exact byte string was inserted,
    /// so a `false` never hides a match (a false negative would be a correctness
    /// bug; a false positive only costs the work the filter exists to avoid). A
    /// tile with no bloom (`bloom_len == 0`) answers `true` for every prefix.
    ///
    /// The bloom is keyed by whatever strings its builder inserted; the caller
    /// must query with those same strings (08d-2b builds it over the unique
    /// prefixes it will probe). It is orthogonal to [`Tile::could_contain_prefix`]:
    /// that one is a range proof over the tile's key span, this one is membership.
    pub(crate) fn might_contain_unique_prefix(&self, prefix: &[u8]) -> bool {
        match &self.bloom {
            None => true,
            Some(bloom) => bloom.might_contain(prefix),
        }
    }
    pub(crate) fn entries(&self) -> impl Iterator<Item = io::Result<(Vec<u8>, Vec<u8>)>> + '_ {
        Entries { cursor: TileCursor::new(self, None, 0), done: false }
    }

    /// Cursor starting at the block `candidate` picks for `prefix`, yielding
    /// only keys that begin with it. The prefix may be shorter than a stored key
    /// (e.g. a unique-index prefix); the cursor stops at the first key past the
    /// prefix range. Reading begins at the selected block, never block 0.
    pub(crate) fn prefix_cursor(&self, prefix: &[u8]) -> io::Result<TileCursor<'_>> {
        let block = self.candidate(prefix)?;
        let mut cursor = TileCursor::new(self, Some(prefix), block);
        cursor.fill()?;
        Ok(cursor)
    }

    /// Exact comparison against the WAL projection in the CSN range
    /// `(lower, cutoff]`: every expected entry must match a tile entry in key
    /// order, and any leftover tile entry is an error. Tile ranges are disjoint
    /// (`lower` is the previous tile's cutoff, 0 for the oldest), so a tile
    /// holding another range's versions fails closed here.
    pub(crate) fn verify_projection(&self, expected: &Memtable, lower: u64, cutoff: u64) -> io::Result<()> {
        // Key-addressed 1:1 comparison instead of positional pairing: a floor-
        // spanning compacted tile legitimately holds retired-prefix entries
        // (csn <= lower) that were verified when compaction published it - the
        // retired WAL can never be replayed to re-derive them.
        let mut expect: std::collections::BTreeMap<Vec<u8>, Vec<u8>> = expected.entries()
            .filter(|(key, _)| keys::key_csn(key).is_some_and(|csn| csn > lower && csn <= cutoff))
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect();
        // A region-partitioned tile (slice 24) holds exactly one `(family,
        // entity)` region: its first and last stored keys share a region prefix.
        // Such a tile is only obligated for that region's projection - its
        // sibling tiles at the same cutoff carry the other regions - so the
        // leftover check below is restricted to the region it covers, and any
        // entry from another region is a hard error: the `(cutoff, region)`
        // ordering and the disjoint per-region ranges assume no tile spans two
        // regions. A whole-keyspace tile (first and last region differ, or an
        // empty tile) stays exactly checked, preserving the pre-partitioning
        // guarantee.
        let first_region = self.first_key().and_then(keys::region_prefix);
        let single = first_region.is_some()
            && first_region == self.last_key().and_then(keys::region_prefix);
        let mut actual = self.entries();
        loop {
            let (key, value) = match actual.next() {
                None => break,
                Some(entry) => entry?,
            };
            if single && keys::region_prefix(&key) != first_region {
                return Err(invalid("tile contains a key outside its region"));
            }
            let csn = keys::key_csn(&key).ok_or_else(|| invalid("tile entry without CSN"))?;
            if csn <= lower { continue; }
            if csn > cutoff { return Err(invalid("tile entry above cutoff")); }
            match expect.remove(&key) {
                Some(v) if v == value => {}
                Some(_) => return Err(invalid("tile entry disagrees with WAL")),
                None => return Err(invalid("tile contains extra WAL entry")),
            }
        }
        let missing = match (single, first_region) {
            (true, Some(region)) => expect.keys().any(|key| keys::region_prefix(key) == Some(region)),
            _ => !expect.is_empty(),
        };
        if missing { return Err(invalid("tile missing WAL entry")); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bloom::BloomBuilder;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("flashdb-tile-unit-{}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), name))
    }
    fn fence(key: &[u8]) -> Fence { Fence { first_key: key.to_vec(), offset: 0, len: 0 } }
    fn put(mt: &mut Memtable, handle: u64, csn: u64, value: &[u8]) {
        mt.apply(keys::primary_key(b"E", handle, csn).unwrap(), value.to_vec());
    }
    fn pkey(handle: u64, csn: u64) -> Vec<u8> { keys::primary_key(b"E", handle, csn).unwrap() }
    fn collect(tile: &Tile) -> Vec<(Vec<u8>, Vec<u8>)> {
        tile.entries().collect::<io::Result<Vec<_>>>().unwrap()
    }
    fn write_tile(name: &str, cutoff: u64, mt: &Memtable) -> (PathBuf, Tile) {
        let path = tmp(name);
        let tile = Tile::write(&path, cutoff, &[9u8; 32], mt).unwrap();
        (path, tile)
    }

    /// Recompute the superblock CRC after a test edits the header in place, so
    /// only the rule under test can reject the file.
    fn repair_superblock_crc(bytes: &mut [u8]) {
        let crc = crc32c(&bytes[..SUPERBLOCK_CRC]);
        bytes[SUPERBLOCK_CRC..SUPERBLOCK].copy_from_slice(&crc.to_le_bytes());
    }

    fn superblock_u64(bytes: &[u8], at: usize) -> u64 { u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) }

    fn write_bloomed_tile(name: &str, cutoff: u64, mt: &Memtable, bits_per_key: u32) -> (PathBuf, Tile) {
        let path = tmp(name);
        let tile = Tile::write_with_bloom(&path, cutoff, &[7u8; 32], mt, bits_per_key).unwrap();
        (path, tile)
    }

    #[test]
    fn dir_search_first_middle_last_and_absent() {
        let dir = [fence(b"b"), fence(b"d"), fence(b"f")];
        assert_eq!(dir_search(&dir, b"b"), 0, "exact first fence");
        assert_eq!(dir_search(&dir, b"d"), 1, "exact middle fence");
        assert_eq!(dir_search(&dir, b"f"), 2, "exact last fence");
        assert_eq!(dir_search(&dir, b"a"), 0, "absent below first -> first candidate");
        assert_eq!(dir_search(&dir, b"e"), 1, "absent between -> lower fence");
        assert_eq!(dir_search(&dir, b"g"), 2, "absent above last -> last fence");
        assert_eq!(dir_search(&[], b"a"), 0, "empty directory -> 0");
    }

    #[test]
    fn empty_tile_roundtrips() {
        let mt = Memtable::new();
        let (path, tile) = write_tile("empty.tile", 5, &mt);
        assert_eq!(tile.count(), 0);
        assert_eq!(tile.data_blocks, 0, "no data blocks means no fences");
        assert!(tile.dir.is_empty(), "an empty tile has no sparse index entries");
        assert!(collect(&tile).is_empty());
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(reopened.count(), 0);
        assert!(collect(&reopened).is_empty());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn single_entry_roundtrips() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"value");
        let (path, tile) = write_tile("one.tile", 5, &mt);
        assert_eq!(tile.count(), 1);
        assert_eq!(tile.data_blocks, 1);
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(collect(&reopened), vec![(pkey(1, 1), b"value".to_vec())]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn many_entries_form_at_least_eight_blocks() {
        let mut mt = Memtable::new();
        let n = 1400u64;
        for handle in 1..=n { put(&mut mt, handle, 1, b"v"); }
        let (path, tile) = write_tile("many.tile", 100, &mt);
        assert_eq!(tile.count(), n);
        assert!(tile.data_blocks >= 8, "4 KiB target yields multiple blocks: {}", tile.data_blocks);
        assert_eq!(tile.dir.len(), 1, "eight data blocks fit one directory block, so the sparse index is single-entry");
        let entries = collect(&tile);
        assert_eq!(entries.len() as u64, n);
        assert!(entries.windows(2).all(|w| w[0].0 < w[1].0), "entries stream in key order");
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(collect(&reopened), entries);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn oversized_entry_gets_its_own_block() {
        let mut mt = Memtable::new();
        let big = vec![b'x'; 3 * DEFAULT_PAGE_SIZE as usize];
        put(&mut mt, 1, 1, &big);
        put(&mut mt, 2, 1, b"small");
        let (path, tile) = write_tile("oversized.tile", 100, &mt);
        assert_eq!(tile.count(), 2);
        assert_eq!(tile.data_blocks, 2, "the oversized entry occupies its own block");
        let entries = collect(&tile);
        assert_eq!(entries[0].1, big);
        assert_eq!(entries[1].1, b"small".to_vec());
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(collect(&reopened), entries);
        std::fs::remove_file(path).unwrap();
    }

    /// A tile written at a non-default page size is self-describing: the superblock
    /// records the value, larger pages pack the same entries into fewer blocks, and
    /// reopening honors the stored value.
    #[test]
    fn custom_page_size_roundtrips_and_packs_larger_blocks() {
        let mut mt = Memtable::new();
        for handle in 1..=1400u64 { put(&mut mt, handle, 1, &vec![b'v'; 200]); }
        let default_path = tmp("page-size-default.tile");
        let default_tile = Tile::write(&default_path, 100, &[9u8; 32], &mt).unwrap();

        let path = tmp("page-size-64k.tile");
        let mut writer = TileWriter::new(&path, 100, &[9u8; 32], 65536).unwrap();
        for (key, value) in mt.entries() { writer.push(key, value).unwrap(); }
        let tile = writer.finish().unwrap();
        assert_eq!(tile.count(), 1400);
        assert!(tile.data_blocks < default_tile.data_blocks,
            "64 KiB pages pack fewer blocks than 4 KiB pages: {} vs {}",
            tile.data_blocks, default_tile.data_blocks);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[72..76].try_into().unwrap()), 65536,
            "the superblock records the page size");
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(collect(&reopened), collect(&tile));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(default_path).unwrap();
    }

    /// A page size outside the allowed range is rejected at build time, before a
    /// candidate file is created.
    #[test]
    fn writer_rejects_out_of_range_page_size() {
        let path = tmp("page-size-bad-writer.tile");
        for bad in [0u32, 1, 4095, 65537, u32::MAX] {
            let err = TileWriter::new(&path, 1, &[9u8; 32], bad).err().expect("out-of-range page size must be rejected");
            assert_eq!(err.kind(), ErrorKind::InvalidData, "page size {bad}");
        }
        assert!(!path.exists(), "a rejected build leaves no candidate file");
    }

    /// A stored page size outside the allowed range fails closed at open (`InvalidData`).
    #[test]
    fn out_of_range_stored_page_size_rejected_at_open() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"v");
        let (path, _) = write_tile("page-size-bad.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[72..76].copy_from_slice(&4095u32.to_le_bytes());
        repair_superblock_crc(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    /// A stored page-size field of 0 means "absent/unknown" and is read as the
    /// default, so an older or partially zeroed header never fails closed on a
    /// layout value (I2).
    #[test]
    fn absent_page_size_field_is_read_as_default() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"v");
        let (path, _) = write_tile("page-size-absent.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[72..76].copy_from_slice(&0u32.to_le_bytes());
        repair_superblock_crc(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        let tile = Tile::open(&path).unwrap();
        assert_eq!(collect(&tile), vec![(pkey(1, 1), b"v".to_vec())]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn flip_inside_data_block_detected_when_read_not_at_open() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"payload");
        let (path, _) = write_tile("flip-data.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        let start = DEFAULT_PAGE_SIZE as usize;
        let block_len = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
        bytes[start + block_len - 1] ^= 1; // value tail: block CRC now wrong, header intact
        std::fs::write(&path, &bytes).unwrap();
        // Open must succeed: it deliberately does not read data blocks.
        let tile = Tile::open(&path).unwrap();
        let err = tile.entries().next().unwrap().unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn flip_inside_directory_detected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=200 { put(&mut mt, handle, 1, b"v"); }
        let (path, _) = write_tile("flip-dir.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        let dir_offset = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize;
        assert!(u64::from_le_bytes(bytes[64..72].try_into().unwrap()) > 0, "directory is nonempty");
        bytes[dir_offset + 12] ^= 1; // inside the first fence record
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn truncated_file_detected_at_open() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"v");
        let (path, _) = write_tile("truncated.tile", 5, &mt);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn out_of_order_fence_keys_detected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=600 { put(&mut mt, handle, 1, b"v"); }
        let (path, tile) = write_tile("order.tile", 5, &mt);
        assert!(tile.data_blocks >= 2, "need two fences to reorder");
        drop(tile);
        let mut bytes = std::fs::read(&path).unwrap();
        let dir_offset = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize;
        let blen = u32::from_le_bytes(bytes[dir_offset..dir_offset + 4].try_into().unwrap()) as usize;
        let key0_len = u32::from_le_bytes(bytes[dir_offset + DIR_HEADER..dir_offset + DIR_HEADER + 4].try_into().unwrap()) as usize;
        let key0 = dir_offset + DIR_HEADER + 16;
        let rec0 = 16 + key0_len;
        let key1_len = u32::from_le_bytes(bytes[dir_offset + DIR_HEADER + rec0..dir_offset + DIR_HEADER + rec0 + 4].try_into().unwrap()) as usize;
        let key1 = dir_offset + DIR_HEADER + rec0 + 16;
        assert_eq!(key0_len, key1_len, "equal-length P keys make the swap clean");
        for i in 0..key0_len { bytes.swap(key0 + i, key1 + i); }
        // Repair the directory block CRC so only the ordering rule can reject it.
        let crc = crc32c(&bytes[dir_offset + 8..dir_offset + blen]);
        bytes[dir_offset + 4..dir_offset + 8].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn prefix_cursor_seeks_the_prefix_range_and_stops() {
        let mut mt = Memtable::new();
        let mut expected = Vec::new();
        for handle in 1..=600u64 {
            let key = keys::unique_key(b"E", b"f", b"v", handle, 1).unwrap();
            mt.apply(key.clone(), handle.to_be_bytes().to_vec());
            expected.push((key, handle.to_be_bytes().to_vec()));
        }
        // A longer value whose key shares a byte prefix with "v" but not the
        // length-delimited unique prefix; it must never be yielded.
        mt.apply(keys::unique_key(b"E", b"f", b"vv", 99, 1).unwrap(), 99u64.to_be_bytes().to_vec());
        let (path, tile) = write_tile("prefix.tile", 5, &mt);
        assert!(tile.data_blocks >= 2, "the prefix range must span blocks: {}", tile.data_blocks);

        let prefix = keys::unique_prefix(b"E", b"f", b"v").unwrap();
        assert!(prefix.len() < expected[0].0.len(), "prefix is shorter than a stored key");
        let mut cursor = tile.prefix_cursor(&prefix).unwrap();
        let mut got = Vec::new();
        while let Some((key, value)) = cursor.next().unwrap() {
            assert!(key.starts_with(&prefix), "only prefix-range keys are yielded");
            got.push((key.to_vec(), value.to_vec()));
        }
        assert_eq!(got, expected, "the cursor yields exactly the strict-prefix range, in order");

        // A value shared by no key yields nothing.
        let missing = keys::unique_prefix(b"E", b"f", b"zzz").unwrap();
        let mut cursor = tile.prefix_cursor(&missing).unwrap();
        assert!(cursor.next().unwrap().is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reordered_but_crc_valid_data_block_fails_closed_when_read() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"v");
        put(&mut mt, 2, 1, b"v");
        put(&mut mt, 3, 1, b"v");
        let (path, _) = write_tile("reorder-data.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        let start = DEFAULT_PAGE_SIZE as usize;
        let blen = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
        let first_key_len = u32::from_le_bytes(bytes[start + 8..start + 12].try_into().unwrap()) as usize;
        let entry_count = u16::from_le_bytes(bytes[start + 12..start + 14].try_into().unwrap()) as usize;
        assert_eq!(entry_count, 3);
        let mut ranges = Vec::new();
        let mut q = start + BLOCK_HEADER + first_key_len;
        for _ in 0..entry_count {
            let k = u32::from_le_bytes(bytes[q..q + 4].try_into().unwrap()) as usize;
            let v = u32::from_le_bytes(bytes[q + 4..q + 8].try_into().unwrap()) as usize;
            ranges.push((q, q + 8 + k + v));
            q += 8 + k + v;
        }
        let (a0, a1) = ranges[0];
        let (b0, b1) = ranges[1];
        assert_eq!(a1 - a0, b1 - b0, "equal-length entries make the swap clean");
        let first = bytes[a0..a1].to_vec();
        let second = bytes[b0..b1].to_vec();
        bytes[a0..a1].copy_from_slice(&second);
        bytes[b0..b1].copy_from_slice(&first);
        // Repair the block CRC so only the ordering rule can reject the tile.
        let crc = crc32c(&bytes[start + 8..start + blen]);
        bytes[start + 4..start + 8].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        // Open validates headers and the directory but never entry order in data.
        let tile = Tile::open(&path).unwrap();
        let mut cursor = tile.entries();
        assert!(cursor.next().unwrap().is_ok(), "the first stored entry still reads");
        assert_eq!(cursor.next().unwrap().unwrap_err().kind(), ErrorKind::InvalidData,
            "a reordered but CRC-valid entry stream is rejected");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn overlapping_data_block_offsets_rejected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=600 { put(&mut mt, handle, 1, b"v"); }
        let (path, tile) = write_tile("overlap.tile", 5, &mt);
        assert!(tile.data_blocks >= 2, "need two data blocks");
        drop(tile);
        let mut bytes = std::fs::read(&path).unwrap();
        let dir_offset = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize;
        let blen = u32::from_le_bytes(bytes[dir_offset..dir_offset + 4].try_into().unwrap()) as usize;
        let key0_len = u32::from_le_bytes(bytes[dir_offset + DIR_HEADER..dir_offset + DIR_HEADER + 4].try_into().unwrap()) as usize;
        let off0 = u64::from_le_bytes(bytes[dir_offset + DIR_HEADER + 4..dir_offset + DIR_HEADER + 12].try_into().unwrap());
        let off1_field = dir_offset + DIR_HEADER + 16 + key0_len + 4;
        // Strictly increasing (off0 + 1 > off0) but inside block 0's extent.
        bytes[off1_field..off1_field + 8].copy_from_slice(&(off0 + 1).to_le_bytes());
        // Repair the directory block CRC so only the overlap rule can reject it.
        let crc = crc32c(&bytes[dir_offset + 8..dir_offset + blen]);
        bytes[dir_offset + 4..dir_offset + 8].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn streaming_writer_matches_memtable_write_byte_for_byte() {
        let mut mt = Memtable::new();
        for handle in 1..=3000u64 { put(&mut mt, handle, 1, &vec![b'v'; 300]); }
        let (path_mt, tile_mt) = write_tile("writer-identical-mt", 1, &mt);
        let path_stream = tmp("writer-identical-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        for (key, value) in mt.entries() { w.push(key, value).unwrap(); }
        let tile_stream = w.finish().unwrap();
        assert_eq!(tile_mt.count(), 3000);
        assert_eq!(tile_stream.count(), 3000);
        assert!(tile_mt.data_blocks > 1, "multi-block tile");
        let a = std::fs::read(&path_mt).unwrap();
        let b = std::fs::read(&path_stream).unwrap();
        assert_eq!(a.len(), b.len(), "file sizes must match");
        assert_eq!(a, b, "streaming writer must be byte-identical to Tile::write");
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    /// The compaction build opts into a bounded `sync_data` cadence so a large
    /// merge does not flush its whole dirty set in one `finish` burst. The cadence
    /// must not change the encoded tile: a writer with an aggressive sync interval
    /// is still byte-identical to the un-synced stream, and the tile reopens.
    #[test]
    fn streaming_writer_periodic_sync_is_byte_identical() {
        let mut mt = Memtable::new();
        for handle in 1..=3000u64 { put(&mut mt, handle, 1, &vec![b'v'; 300]); }
        let (path_mt, tile_mt) = write_tile("writer-sync-mt", 1, &mt);
        let path_stream = tmp("writer-sync-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        // One sync per block (the default page is 4096 B), so every `flush_block`
        // crosses the threshold and exercises the sync branch repeatedly.
        w.set_sync_every(1);
        for (key, value) in mt.entries() { w.push(key, value).unwrap(); }
        let tile_stream = w.finish().unwrap();
        assert_eq!(tile_mt.count(), 3000);
        assert_eq!(tile_stream.count(), 3000);
        let a = std::fs::read(&path_mt).unwrap();
        let b = std::fs::read(&path_stream).unwrap();
        assert_eq!(a, b, "periodic sync must not change the encoded tile bytes");
        // The reopened tile must read back the same entries.
        let reopened = Tile::open(&path_stream).unwrap();
        assert_eq!(reopened.count(), 3000);
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    #[test]
    fn streaming_writer_handles_empty_and_exact_block_fill() {
        let empty = Memtable::new();
        let (path_mt, tile_mt) = write_tile("writer-empty-mt", 0, &empty);
        let path_stream = tmp("writer-empty-stream");
        let tile_stream = TileWriter::new(&path_stream, 0, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap().finish().unwrap();
        assert_eq!(tile_mt.count(), 0);
        assert_eq!(tile_stream.count(), 0);
        assert_eq!(std::fs::read(&path_mt).unwrap(), std::fs::read(&path_stream).unwrap());
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);

        // 14 (block header) + 18 (first key) + 8 * (8 + 18 + 482) = exactly 4096, so the
        // ninth entry must open a second block.
        let value = vec![b'x'; 482];
        let mut mt = Memtable::new();
        for handle in 1..=9u64 { put(&mut mt, handle, 1, &value); }
        let (path_mt, tile_mt) = write_tile("writer-exact-mt", 1, &mt);
        assert_eq!(tile_mt.data_blocks, 2, "exact fill then spill");
        let path_stream = tmp("writer-exact-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        for (key, val) in mt.entries() { w.push(key, val).unwrap(); }
        let tile_stream = w.finish().unwrap();
        assert_eq!(tile_stream.count(), 9);
        assert_eq!(std::fs::read(&path_mt).unwrap(), std::fs::read(&path_stream).unwrap());
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    #[test]
    fn streaming_writer_gives_an_oversized_entry_its_own_block() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, &vec![b'a'; 100]);
        put(&mut mt, 2, 1, &vec![b'b'; 5000]);
        put(&mut mt, 3, 1, &vec![b'c'; 100]);
        let (path_mt, tile_mt) = write_tile("writer-big-mt", 1, &mt);
        assert_eq!(tile_mt.data_blocks, 3, "oversized entry alone in its block");
        let path_stream = tmp("writer-big-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        for (key, val) in mt.entries() { w.push(key, val).unwrap(); }
        assert_eq!(w.finish().unwrap().count(), 3);
        assert_eq!(std::fs::read(&path_mt).unwrap(), std::fs::read(&path_stream).unwrap());
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    #[test]
    fn streaming_writer_rejects_out_of_order_and_duplicate_keys() {
        let path = tmp("writer-order");
        let mut w = TileWriter::new(&path, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        w.push(&pkey(2, 1), b"x").unwrap();
        assert_eq!(w.push(&pkey(1, 1), b"y").unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(w.push(&pkey(2, 1), b"z").unwrap_err().kind(), ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }

    /// A tile whose directory needs more than one block: the retained index has
    /// one entry per directory block (not per data block), and a lookup that
    /// crosses a directory-block boundary still lands on the right data block.
    #[test]
    fn sparse_index_spans_directory_blocks_and_seeks_correctly() {
        let mut mt = Memtable::new();
        let n = 1200u64;
        let value = vec![b'v'; 500];
        for handle in 1..=n { put(&mut mt, handle, 1, &value); }
        let (path, tile) = write_tile("sparse.tile", 100, &mt);
        assert!(tile.data_blocks > 100, "need many data blocks: {}", tile.data_blocks);
        assert!(tile.dir.len() >= 2, "many data blocks must span directory blocks: {}", tile.dir.len());
        // The sparse index is one entry per directory block, far smaller than the
        // data-block count it replaces.
        assert!(tile.dir.len() < tile.data_blocks as usize);

        let entries = collect(&tile);
        assert_eq!(entries.len() as u64, n);
        assert!(entries.windows(2).all(|w| w[0].0 < w[1].0), "entries stream in key order");

        // Every fence key probes blocks across the directory-block boundaries and
        // must land on the block that actually stores it.
        let mut probed = 0usize;
        for i in 0..tile.dir.len() {
            for f in &tile.read_dir_fences(i).unwrap() {
                let block = tile.candidate(&f.first_key).unwrap();
                let mut cursor = TileCursor::new(&tile, None, block);
                let (key, _) = cursor.peek().unwrap().unwrap();
                assert_eq!(key, f.first_key.as_slice(), "candidate block stores its fence key");
                probed += 1;
            }
        }
        assert_eq!(probed as u64, tile.data_blocks);
        // Below the first fence and above the last both select a valid block.
        assert_eq!(tile.candidate(b"\x00").unwrap(), 0);
        let last = tile.candidate(b"\xff\xff\xff\xff").unwrap();
        let mut cursor = TileCursor::new(&tile, None, last);
        assert!(cursor.peek().unwrap().is_some());

        let reopened = Tile::open(&path).unwrap();
        assert_eq!(reopened.dir.len(), tile.dir.len());
        assert_eq!(collect(&reopened), entries);
        std::fs::remove_file(path).unwrap();
    }

    /// Fence monotonicity is enforced across directory-block boundaries, not just
    /// within one block: making the second block's first fence equal the first
    /// block's last fence is rejected at open.
    #[test]
    fn out_of_order_fence_keys_across_directory_blocks_detected_at_open() {
        let mut mt = Memtable::new();
        let value = vec![b'v'; 500];
        for handle in 1..=1200u64 { put(&mut mt, handle, 1, &value); }
        let (path, tile) = write_tile("order-cross.tile", 100, &mt);
        assert!(tile.dir.len() >= 2, "need two directory blocks: {}", tile.dir.len());
        drop(tile);
        let mut bytes = std::fs::read(&path).unwrap();
        let dir_offset = u64::from_le_bytes(bytes[56..64].try_into().unwrap()) as usize;
        let blen0 = u32::from_le_bytes(bytes[dir_offset..dir_offset + 4].try_into().unwrap()) as usize;
        let last0 = decode_dir_block(&bytes[dir_offset..dir_offset + blen0]).unwrap().pop().unwrap().first_key;
        let off1 = dir_offset + blen0;
        let blen1 = u32::from_le_bytes(bytes[off1..off1 + 4].try_into().unwrap()) as usize;
        let key1_len = u32::from_le_bytes(bytes[off1 + DIR_HEADER..off1 + DIR_HEADER + 4].try_into().unwrap()) as usize;
        assert_eq!(key1_len, last0.len(), "equal-length P keys make the rewrite clean");
        let key1 = off1 + DIR_HEADER + 16;
        bytes[key1..key1 + key1_len].copy_from_slice(&last0);
        // Repair the second directory block's CRC so only ordering can reject it.
        let crc = crc32c(&bytes[off1 + 8..off1 + blen1]);
        bytes[off1 + 4..off1 + 8].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn prefix_successor_rule() {
        assert_eq!(prefix_successor(b"abc"), Some(b"abd".to_vec()), "increment the last byte");
        assert_eq!(prefix_successor(b"ab\xff"), Some(b"ac".to_vec()), "carry past a 0xff byte");
        assert_eq!(prefix_successor(b"a\xff\xff"), Some(b"b".to_vec()), "truncate after the increment");
        assert_eq!(prefix_successor(b"ab\x00"), Some(b"ab\x01".to_vec()), "0x00 is a valid byte to increment");
        assert_eq!(prefix_successor(b""), None, "the empty prefix has no finite upper bound");
        assert_eq!(prefix_successor(b"\xff"), None, "every byte 0xff has no upper bound");
        assert_eq!(prefix_successor(b"\xff\xff\xff"), None, "all-0xff has no upper bound");
    }

    /// The filter's contract: it may over-approximate (a false positive only
    /// costs work) but must never reject a prefix that a stored key actually
    /// starts with. Exhaustive over every present key and every prefix of it.
    #[test]
    fn could_contain_prefix_has_no_false_negatives() {
        let mut mt = Memtable::new();
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for handle in 1..=300u64 {
            let k = pkey(handle, 1);
            mt.apply(k.clone(), b"v".to_vec());
            keys.push(k);
        }
        // Values chosen to exercise arbitrary bytes and lengths, including 0xff
        // runs (the boundary `prefix_successor` has to carry through) and the
        // length-delimited empty value.
        for (i, value) in [&b""[..], b"\x00", b"\xff", b"\xff\xff", b"zz\xff"].into_iter().enumerate() {
            let k = keys::unique_key(b"E", b"f", value, i as u64 + 1, 1).unwrap();
            mt.apply(k.clone(), b"v".to_vec());
            keys.push(k);
        }
        let (path, tile) = write_tile("no-false-negative.tile", 100, &mt);
        assert_eq!(tile.count(), keys.len() as u64);
        // The cached upper bound is exactly the greatest stored key.
        let max = keys.iter().max().unwrap().clone();
        assert_eq!(tile.last_key.as_ref(), Some(&max));
        for key in &keys {
            assert!(tile.could_contain_prefix(key), "a stored key must never be rejected");
            for n in 0..=key.len() {
                assert!(
                    tile.could_contain_prefix(&key[..n]),
                    "prefix len {n} of a stored key (len {}) must never be rejected",
                    key.len()
                );
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn could_contain_prefix_boundaries() {
        let empty = Memtable::new();
        let (epath, etile) = write_tile("boundary-empty.tile", 5, &empty);
        assert_eq!(etile.count(), 0);
        assert!(etile.first_key.is_empty());
        assert!(etile.last_key.is_none());
        assert!(!etile.could_contain_prefix(b""), "an empty tile holds no keys");
        assert!(!etile.could_contain_prefix(b"P"));
        std::fs::remove_file(epath).unwrap();

        let mut mt = Memtable::new();
        for handle in 1..=50u64 { put(&mut mt, handle, 1, b"v"); }
        let (path, tile) = write_tile("boundary.tile", 5, &mt);
        assert_eq!(tile.first_key, pkey(1, 1));
        let last = tile.last_key.clone().expect("a nonempty tile caches its last key");
        assert_eq!(last, pkey(50, 1));

        assert!(tile.could_contain_prefix(b""), "the empty prefix matches everything");
        assert!(tile.could_contain_prefix(&last), "the last key itself");
        assert!(tile.could_contain_prefix(&last[..last.len() - 1]), "a proper prefix of the last key");
        assert!(tile.could_contain_prefix(&last[..1]), "the shortest shared prefix");
        assert!(tile.could_contain_prefix(&tile.first_key), "the first key itself");

        let mut above = last.clone();
        above.push(0);
        assert!(!tile.could_contain_prefix(&above), "strictly above the last key");
        assert!(!tile.could_contain_prefix(&[0xff; 8]), "an all-0xff prefix sorts above every key");
        assert!(!tile.could_contain_prefix(b"O"), "successor(\"O\") = \"P\" <= first_key");
        std::fs::remove_file(path).unwrap();
    }

    /// A final data block that fails its CRC is not trusted for the upper bound:
    /// open still succeeds (data blocks are verified on read, not at open) and the
    /// tile falls back to "no upper bound", which only weakens the filter.
    #[test]
    fn untrusted_final_block_leaves_no_upper_bound() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"payload");
        let (path, _) = write_tile("untrusted-last.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        let start = DEFAULT_PAGE_SIZE as usize;
        let block_len = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
        bytes[start + block_len - 1] ^= 1; // value tail: block CRC now wrong, header intact
        std::fs::write(&path, &bytes).unwrap();
        let tile = Tile::open(&path).unwrap();
        assert!(tile.last_key.is_none(), "a corrupt final block is not trusted for the upper bound");
        assert_eq!(tile.first_key, pkey(1, 1));
        assert!(tile.could_contain_prefix(b"zzzz"), "no upper bound must not reject above the tile");
        assert!(tile.entries().next().unwrap().unwrap_err().kind() == ErrorKind::InvalidData,
            "the corruption is still detected on the read that reaches it");
        std::fs::remove_file(path).unwrap();
    }

    /// A bloomed tile round-trips, and the reopen path finds every stored key in
    /// the filter: no false negatives, the correctness invariant. A tile written
    /// without a bloom answers `true` ("no bloom") for every prefix.
    #[test]
    fn bloomed_tile_roundtrips_with_no_false_negatives_and_no_bloom_answers_true() {
        let mut mt = Memtable::new();
        let mut keys = Vec::new();
        for handle in 1..=500u64 {
            let key = pkey(handle, 1);
            mt.apply(key.clone(), b"v".to_vec());
            keys.push(key);
        }
        let (path, tile) = write_bloomed_tile("bloomed.tile", 100, &mt, 12);
        assert!(tile.bloom.is_some(), "the writer attached a bloom");
        for key in &keys { assert!(tile.might_contain_unique_prefix(key), "stored key must never be rejected"); }

        let reopened = Tile::open(&path).unwrap();
        let bloom = reopened.bloom.as_ref().expect("the bloom survives the round trip");
        assert_eq!(bloom.entry_count(), 500);
        assert_eq!(bloom.bits_per_key(), 12);
        for key in &keys { assert!(reopened.might_contain_unique_prefix(key), "reopened filter must not reject a stored key"); }
        std::fs::remove_file(path).unwrap();

        // An empty tile and a tile written without `set_bloom` carry no filter and
        // therefore cannot reject anything.
        let empty = Memtable::new();
        let (epath, etile) = write_tile("bloomless-empty.tile", 0, &empty);
        assert!(etile.bloom.is_none());
        assert!(etile.might_contain_unique_prefix(b"anything"));
        std::fs::remove_file(epath).unwrap();
        let mut one = Memtable::new();
        put(&mut one, 1, 1, b"v");
        let (npath, ntile) = write_tile("bloomless.tile", 1, &one);
        assert!(ntile.bloom.is_none());
        assert!(ntile.might_contain_unique_prefix(b"never inserted"), "no bloom means always maybe");
        std::fs::remove_file(npath).unwrap();
    }

    /// Only the CRC can catch a flipped bit in the bloom's bit array: the structure
    /// is still valid, so open must reject on the checksum.
    #[test]
    fn bloom_bad_crc_rejected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=200u64 { put(&mut mt, handle, 1, b"v"); }
        let (path, _) = write_bloomed_tile("bloom-crc.tile", 100, &mt, 10);
        let mut bytes = std::fs::read(&path).unwrap();
        let bloom_offset = superblock_u64(&bytes, BLOOM_OFFSET_AT) as usize;
        let bloom_len = superblock_u64(&bytes, BLOOM_LEN_AT) as usize;
        assert!(bloom_len > 0, "a bloom section is present");
        bytes[bloom_offset + bloom_len - 1] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    /// The bloom cannot overlap the data blocks: moving its start into the data
    /// region is rejected (with a repaired superblock CRC, so the bounds rule is
    /// what fires).
    #[test]
    fn bloom_overlapping_data_region_rejected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=200u64 { put(&mut mt, handle, 1, b"v"); }
        let (path, _) = write_bloomed_tile("bloom-over-data.tile", 100, &mt, 10);
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(superblock_u64(&bytes, BLOOM_OFFSET_AT) > DEFAULT_PAGE_SIZE as u64, "the bloom starts after the data blocks");
        bytes[BLOOM_OFFSET_AT..BLOOM_LEN_AT].copy_from_slice(&(DEFAULT_PAGE_SIZE as u64).to_le_bytes());
        repair_superblock_crc(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    /// The bloom cannot reach past the directory: a section that starts at or after
    /// the directory is rejected.
    #[test]
    fn bloom_past_directory_rejected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=200u64 { put(&mut mt, handle, 1, b"v"); }
        let (path, _) = write_bloomed_tile("bloom-past-dir.tile", 100, &mt, 10);
        let mut bytes = std::fs::read(&path).unwrap();
        let dir_offset = superblock_u64(&bytes, 56);
        // Start exactly at the directory with a one-byte section: its end is > dir_offset.
        bytes[BLOOM_OFFSET_AT..BLOOM_LEN_AT].copy_from_slice(&dir_offset.to_le_bytes());
        bytes[BLOOM_LEN_AT..BLOOM_CRC_AT].copy_from_slice(&1u64.to_le_bytes());
        repair_superblock_crc(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    /// `Tile::open` runs `Bloom::from_bytes` on the section: a truncated section, an
    /// impossible `hash_count`, a header whose length disagrees with its bit count,
    /// and a header that disagrees with the superblock are each rejected even when
    /// the frame CRC is repaired to match.
    #[test]
    fn bloom_structure_rejected_at_open() {
        let mut mt = Memtable::new();
        for handle in 1..=200u64 { put(&mut mt, handle, 1, b"v"); }
        let (path, _) = write_bloomed_tile("bloom-structure.tile", 100, &mt, 10);
        let pristine = std::fs::read(&path).unwrap();
        let bloom_offset = superblock_u64(&pristine, BLOOM_OFFSET_AT) as usize;
        let bloom_len = superblock_u64(&pristine, BLOOM_LEN_AT) as usize;
        // Assistants edit the section, then refresh both CRCs so each case reaches from_bytes.
        let reframe = |bytes: &mut Vec<u8>| {
            let crc = crc32c(&bytes[bloom_offset..bloom_offset + bloom_len]);
            bytes[BLOOM_CRC_AT..BITS_PER_KEY_AT].copy_from_slice(&crc.to_le_bytes());
            repair_superblock_crc(bytes);
        };

        // Truncated section: a valid frame CRC over a sub-header-length slice.
        let mut truncated = pristine.clone();
        truncated[BLOOM_LEN_AT..BLOOM_CRC_AT].copy_from_slice(&10u64.to_le_bytes());
        let crc = crc32c(&truncated[bloom_offset..bloom_offset + 10]);
        truncated[BLOOM_CRC_AT..BITS_PER_KEY_AT].copy_from_slice(&crc.to_le_bytes());
        repair_superblock_crc(&mut truncated);
        std::fs::write(&path, &truncated).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData, "truncated section");

        // Impossible hash_count inside the section.
        let mut bad_hash = pristine.clone();
        bad_hash[bloom_offset + 20] = 0;
        reframe(&mut bad_hash);
        std::fs::write(&path, &bad_hash).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData, "hash_count 0");

        // Header length disagrees with the bit array.
        let mut bad_len = pristine.clone();
        let bit_count = superblock_u64(&bad_len, bloom_offset) + 8;
        bad_len[bloom_offset..bloom_offset + 8].copy_from_slice(&bit_count.to_le_bytes());
        reframe(&mut bad_len);
        std::fs::write(&path, &bad_len).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData, "header length mismatch");

        // Superblock disagrees with the (valid) bloom header.
        let mut bad_super = pristine.clone();
        bad_super[BITS_PER_KEY_AT..HASH_COUNT_AT].copy_from_slice(&11u32.to_le_bytes());
        repair_superblock_crc(&mut bad_super);
        std::fs::write(&path, &bad_super).unwrap();
        assert_eq!(Tile::open(&path).err().unwrap().kind(), ErrorKind::InvalidData, "superblock disagrees with bloom");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn set_bloom_twice_is_rejected() {
        let path = tmp("bloom-twice");
        let mut writer = TileWriter::new(&path, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        writer.set_bloom(BloomBuilder::new(4, 10).finish()).unwrap();
        assert_eq!(writer.set_bloom(BloomBuilder::new(4, 10).finish()).unwrap_err().kind(), ErrorKind::InvalidData);
        std::fs::remove_file(&path).unwrap();
    }

    /// The streaming writer and the whole-memtable writer stay byte-identical both
    /// without a bloom and with the same bloom attached to both.
    #[test]
    fn streaming_writer_matches_memtable_write_with_and_without_bloom() {
        let mut mt = Memtable::new();
        for handle in 1..=3000u64 { put(&mut mt, handle, 1, &vec![b'v'; 300]); }

        let (path_mt, _) = write_tile("bloom-ident-mt-none", 1, &mt);
        let path_stream = tmp("bloom-ident-stream-none");
        let mut writer = TileWriter::new(&path_stream, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        for (key, value) in mt.entries() { writer.push(key, value).unwrap(); }
        assert_eq!(writer.finish().unwrap().count(), 3000);
        assert_eq!(std::fs::read(&path_mt).unwrap(), std::fs::read(&path_stream).unwrap(), "no-bloom byte identity");

        let path_mt_bloom = tmp("bloom-ident-mt");
        let tile_mt_bloom = Tile::write_with_bloom(&path_mt_bloom, 1, &[9u8; 32], &mt, 10).unwrap();
        let path_stream_bloom = tmp("bloom-ident-stream");
        let mut writer = TileWriter::new(&path_stream_bloom, 1, &[9u8; 32], DEFAULT_PAGE_SIZE).unwrap();
        for (key, value) in mt.entries() { writer.push(key, value).unwrap(); }
        writer.set_bloom(bloom_for_memtable(&mt, 1, 10)).unwrap();
        assert_eq!(writer.finish().unwrap().count(), 3000);
        let bloomed = std::fs::read(&path_mt_bloom).unwrap();
        assert_eq!(bloomed, std::fs::read(&path_stream_bloom).unwrap(), "bloom byte identity");
        assert!(tile_mt_bloom.bloom.is_some());
        assert_ne!(bloomed, std::fs::read(&path_mt).unwrap(), "a bloom changes the file bytes");

        for path in [path_mt, path_stream, path_mt_bloom, path_stream_bloom] { std::fs::remove_file(path).unwrap(); }
    }
}
