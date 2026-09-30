//! Explicit, WAL-retained immutable tile prototype. No manifest, checkpoint, or WAL retirement.
//! The file is validated before it can displace any serving memtable entries. Reads use
//! owned, checksum-verified block buffers; this is not a bounded-recovery claim.
//!
//! On-disk layout is FDBTILE2 (see `.scratch/08-tile-pages/FDBTILE2.md`):
//! a padded superblock at offset 0, then variable-length 4 KiB data blocks, a
//! CRC-protected directory of fence records, and a fixed trailer. `Tile::open`
//! validates the superblock, trailer, directory structure and block headers but
//! deliberately does not read data blocks; `entries()` streams blocks in key
//! order, verifying each block's CRC on the way.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::crc::crc32c;
use crate::keys;
use crate::memtable::Memtable;

const MAGIC: &[u8; 8] = b"FDBTILE2";
// The trailer magic is exactly these nine bytes (F D B T I L E 2 E); there is
// no 8-byte variant and no separate length prefix.
const TRAILER_MAGIC: &[u8; 9] = b"FDBTILE2E";
const PAGE_SIZE: u32 = 4096;
// magic(8) + cutoff(8) + digest(32) + count(8) + dir_offset(8) + dir_len(8) + page_size(4) + crc(4)
const SUPERBLOCK: usize = 80;
const SUPERBLOCK_CRC: usize = 76;
// magic(9) + dir_offset(8) + dir_len(8) + crc(4)
const TRAILER: usize = 29;
const TRAILER_CRC: usize = 25;
// block_len(4) + block_crc(4) + first_key_len(4) + entry_count(2)
const BLOCK_HEADER: usize = 14;
// dir_len(4) + dir_crc(4) + fence_count(2)
const DIR_HEADER: usize = 10;
// Mutable target size for both data and directory blocks.
const BLOCK_TARGET: usize = PAGE_SIZE as usize;
const MAX_KEY: usize = 1 << 24;
const MAX_VALUE: usize = 1 << 26;
// One entry's length prefixes plus the widest legal key and value. The block
// encoder additionally checks the fully framed block fits the u32 `block_len`.
const MAX_ENTRY: usize = 8 + MAX_KEY + MAX_VALUE;
const _: () = assert!(MAX_ENTRY <= u32::MAX as usize);

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

fn valid_key(key: &[u8]) -> bool {
    if key.len() < 18 { return false; }
    match key[0] {
        keys::P => keys::decode_primary(key).is_some_and(|(name, _, _)| !name.is_empty()),
        keys::U | keys::R => {
            let Some(first) = key[1..].iter().position(|&b| b == 0) else { return false };
            if first == 0 { return false; }
            let Some(tail) = key.get(first + 2..) else { return false };
            let Some(second) = tail.iter().position(|&b| b == 0) else { return false };
            if second == 0 { return false; }
            let rest = &tail[second + 1..];
            if key[0] == keys::R { rest.len() == 24 }
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

fn superblock_bytes(cutoff: u64, digest: &[u8; 32], count: u64, dir_offset: u64, dir_len: u64) -> [u8; SUPERBLOCK] {
    let mut h = [0; SUPERBLOCK];
    h[..8].copy_from_slice(MAGIC);
    h[8..16].copy_from_slice(&cutoff.to_le_bytes());
    h[16..48].copy_from_slice(digest);
    h[48..56].copy_from_slice(&count.to_le_bytes());
    h[56..64].copy_from_slice(&dir_offset.to_le_bytes());
    h[64..72].copy_from_slice(&dir_len.to_le_bytes());
    h[72..76].copy_from_slice(&PAGE_SIZE.to_le_bytes());
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

pub(crate) struct Tile {
    file: File,
    superblock: [u8; SUPERBLOCK],
    trailer: [u8; TRAILER],
    cutoff: u64,
    digest: [u8; 32],
    count: u64,
    size: u64,
    fences: Vec<Fence>,
}

/// Pull cursor over a tile's data blocks. Entries borrow the cursor's own block
/// buffer, so streaming allocates nothing per entry (only the reused prefix and
/// last-key buffers). Without a prefix it yields every entry; with one it starts
/// at the block `dir_search` selects, skips entries sorting below the prefix,
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
}

impl<'a> TileCursor<'a> {
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
        }
    }

    /// Read and validate the block selected by `self.block`, resetting the entry
    /// position to its first entry.
    fn load_block(&mut self) -> io::Result<()> {
        let (offset, len) = {
            let fence = &self.tile.fences[self.block];
            (fence.offset, fence.len as usize)
        };
        self.buffer.resize(len, 0);
        read_exact_at(&self.tile.file, &mut self.buffer, offset)?;
        if self.buffer.len() < BLOCK_HEADER { return Err(invalid("tile block truncated")); }
        let blen = u32::from_le_bytes(self.buffer[..4].try_into().unwrap()) as usize;
        let block_crc = u32::from_le_bytes(self.buffer[4..8].try_into().unwrap());
        if blen != self.buffer.len() { return Err(invalid("tile block length changed")); }
        if crc32c(&self.buffer[8..]) != block_crc { return Err(invalid("tile block checksum")); }
        let first_key_len = u32::from_le_bytes(self.buffer[8..12].try_into().unwrap()) as usize;
        if BLOCK_HEADER + first_key_len > blen { return Err(invalid("tile block first key out of bounds")); }
        // Re-bind the block to its fence on every load. Open verified this once,
        // but the fence is cached in RAM: a CRC-consistent rewrite of the file
        // could otherwise serve a block whose first key disagrees with the fence
        // and, because the order check only sees *yielded* keys, a prefix cursor
        // could stop early and silently drop matches.
        if self.buffer[BLOCK_HEADER..BLOCK_HEADER + first_key_len] != *self.tile.fences[self.block].first_key {
            return Err(invalid("tile block first key disagrees with its fence"));
        }
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
                if self.block >= self.tile.fences.len() { self.done = true; return Ok(false); }
                // A block whose first key sorts past the prefix without starting
                // with it, and every later block, cannot match: stop before reading.
                if let Some(prefix) = &self.prefix {
                    let first = self.tile.fences[self.block].first_key.as_slice();
                    if first > prefix.as_slice() && !first.starts_with(prefix.as_slice()) {
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

/// Streaming FDBTILE2 encoder: buffers only the in-progress block, so a tile can be
/// produced from a key-ordered iterator without materializing its entries. `push`
/// requires strictly increasing keys (the memtable used to provide that ordering
/// implicitly); `finish` writes the directory, the trailer and - last, so a torn file
/// cannot have a valid magic - the superblock, then syncs and reopens for the same
/// self-check `Tile::write` performs. `Tile::write` is implemented on top of this so
/// there is exactly one encoder.
pub(crate) struct TileWriter {
    file: File,
    path: PathBuf,
    cutoff: u64,
    digest: [u8; 32],
    offset: u64,
    count: u64,
    fences: Vec<Fence>,
    first_key: Vec<u8>,
    entries: Vec<u8>,
    block_entries: u16,
    last_key: Vec<u8>,
    started: bool,
    finished: bool,
}

impl TileWriter {
    /// `create_new` never replaces an existing tile. A failed or crashed build leaves a
    /// partial candidate that cannot change the engine's serving state; the caller may
    /// remove it explicitly.
    pub(crate) fn new(path: &Path, cutoff: u64, digest: &[u8; 32]) -> io::Result<Self> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
        // Reserve the superblock page; the real superblock is written last, once the
        // directory offset/length are known. This puts the first data block on the
        // 4096-byte boundary.
        file.write_all(&[0u8; PAGE_SIZE as usize])?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            cutoff,
            digest: *digest,
            offset: PAGE_SIZE as u64,
            count: 0,
            fences: Vec::new(),
            first_key: Vec::new(),
            entries: Vec::new(),
            block_entries: 0,
            last_key: Vec::new(),
            started: false,
            finished: false,
        })
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
        if !self.entries.is_empty() && self.entries.len() as u64 + entry_size > BLOCK_TARGET as u64 {
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

    fn flush_block(&mut self) -> io::Result<()> {
        if self.entries.is_empty() { return Ok(()); }
        let block = encode_block(&self.first_key, &self.entries, self.block_entries)?;
        self.fences.push(Fence { first_key: std::mem::take(&mut self.first_key), offset: self.offset, len: block.len() as u32 });
        self.file.write_all(&block)?;
        self.offset = self.offset.checked_add(block.len() as u64).ok_or_else(|| invalid("tile offset overflow"))?;
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
            if !records.is_empty() && DIR_HEADER + records.len() + rec_len > BLOCK_TARGET {
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
        let (dir_len, dir_offset) = self.write_directory()?;
        self.file.write_all(&trailer_bytes(dir_offset, dir_len))?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&superblock_bytes(self.cutoff, &self.digest, self.count, dir_offset, dir_len))?;
        self.file.sync_data()?; // No parent-directory durability claim.
        let tile = Tile::open(&self.path)?;
        if tile.cutoff != self.cutoff || tile.digest != self.digest || tile.count() != self.count {
            return Err(invalid("tile verification mismatch"));
        }
        Ok(tile)
    }
}

impl Tile {
    pub(crate) fn write(path: &Path, cutoff: u64, digest: &[u8; 32], mt: &Memtable) -> io::Result<Self> {
        let mut writer = TileWriter::new(path, cutoff, digest)?;
        for (key, value) in mt.entries() {
            if keys::key_csn(key).is_none_or(|csn| csn > cutoff) { continue; }
            writer.push(key, value)?;
        }
        writer.finish()
    }

    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < PAGE_SIZE as u64 + TRAILER as u64 { return Err(invalid("tile too small")); }
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
        if u32::from_le_bytes(sb[72..76].try_into().unwrap()) != PAGE_SIZE { return Err(invalid("tile page size")); }
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
        if dir_offset < PAGE_SIZE as u64 || dir_offset.checked_add(dir_len) != Some(size - TRAILER as u64) {
            return Err(invalid("tile directory bounds"));
        }
        let mut fences: Vec<Fence> = Vec::new();
        if dir_len > 0 {
            let dir = read_range(&file, dir_offset, dir_len)?;
            let mut pos = 0usize;
            while pos < dir.len() {
                if dir.len() - pos < DIR_HEADER { return Err(invalid("tile directory block truncated")); }
                let blen = u32::from_le_bytes(dir[pos..pos + 4].try_into().unwrap()) as usize;
                if blen < DIR_HEADER || blen > dir.len() - pos { return Err(invalid("tile directory block length")); }
                let block_crc = u32::from_le_bytes(dir[pos + 4..pos + 8].try_into().unwrap());
                if crc32c(&dir[pos + 8..pos + blen]) != block_crc { return Err(invalid("tile directory checksum")); }
                let fence_count = u16::from_le_bytes(dir[pos + 8..pos + 10].try_into().unwrap()) as usize;
                let mut q = pos + DIR_HEADER;
                for _ in 0..fence_count {
                    if blen - (q - pos) < 16 { return Err(invalid("tile fence record truncated")); }
                    let first_key_len = u32::from_le_bytes(dir[q..q + 4].try_into().unwrap()) as usize;
                    let block_offset = u64::from_le_bytes(dir[q + 4..q + 12].try_into().unwrap());
                    let block_len = u32::from_le_bytes(dir[q + 12..q + 16].try_into().unwrap());
                    q += 16;
                    if first_key_len < 1 || first_key_len > MAX_KEY || blen - (q - pos) < first_key_len {
                        return Err(invalid("tile fence key length"));
                    }
                    fences.push(Fence {
                        first_key: dir[q..q + first_key_len].to_vec(),
                        offset: block_offset,
                        len: block_len,
                    });
                    q += first_key_len;
                }
                if q != pos + blen { return Err(invalid("tile directory block trailing bytes")); }
                pos += blen;
            }
        } else if count != 0 {
            return Err(invalid("tile directory missing for nonempty tile"));
        }
        let mut previous: Option<&[u8]> = None;
        let mut previous_end: Option<u64> = None;
        let mut total: u64 = 0;
        for (index, fence) in fences.iter().enumerate() {
            if fence.offset < PAGE_SIZE as u64 || (fence.len as usize) < BLOCK_HEADER {
                return Err(invalid("tile block offset/length"));
            }
            let end = fence.offset.checked_add(fence.len as u64).ok_or_else(|| invalid("tile block overflow"))?;
            if end > dir_offset { return Err(invalid("tile block outside data region")); }
            // Data blocks must be strictly increasing and non-overlapping: a
            // directory whose offsets collide or overlap can alias blocks.
            if previous_end.is_some_and(|prev_end| fence.offset < prev_end) {
                return Err(invalid("tile data blocks overlap or are out of order"));
            }
            if previous.is_some_and(|prev| prev >= fence.first_key.as_slice()) {
                return Err(invalid("tile fence keys not strictly increasing"));
            }
            previous = Some(&fence.first_key);
            previous_end = Some(end);
            // Block header consistency: the header's length, first key and entry
            // count must agree with the directory (data CRC is checked on read).
            let mut hdr = [0u8; BLOCK_HEADER];
            read_exact_at(&file, &mut hdr, fence.offset)?;
            let blen = u32::from_le_bytes(hdr[..4].try_into().unwrap());
            let first_key_len = u32::from_le_bytes(hdr[8..12].try_into().unwrap()) as usize;
            let entry_count = u16::from_le_bytes(hdr[12..14].try_into().unwrap());
            if blen != fence.len { return Err(invalid("tile block length mismatch")); }
            if first_key_len != fence.first_key.len() { return Err(invalid("tile block first key length mismatch")); }
            if BLOCK_HEADER + first_key_len > fence.len as usize { return Err(invalid("tile block first key out of bounds")); }
            if entry_count == 0 { return Err(invalid("tile block declares no entries")); }
            let mut first = vec![0u8; first_key_len];
            read_exact_at(&file, &mut first, fence.offset + BLOCK_HEADER as u64)?;
            if first != fence.first_key { return Err(invalid("tile block first key mismatch")); }
            total = total.checked_add(entry_count as u64).ok_or_else(|| invalid("tile entry count overflow"))?;
            debug_assert_eq!(dir_search(&fences, &fences[index].first_key), index);
        }
        if total != count { return Err(invalid("tile block entry count disagrees")); }
        Ok(Tile { file, superblock: sb, trailer: tr, cutoff, digest, count, size, fences })
    }

    pub(crate) fn cutoff(&self) -> u64 { self.cutoff }
    pub(crate) fn digest(&self) -> &[u8; 32] { &self.digest }
    pub(crate) fn count(&self) -> u64 { self.count }
    pub(crate) fn entries(&self) -> impl Iterator<Item = io::Result<(Vec<u8>, Vec<u8>)>> + '_ {
        Entries { cursor: TileCursor::new(self, None, 0), done: false }
    }

    /// Cursor starting at the block `dir_search` picks for `prefix`, yielding
    /// only keys that begin with it. The prefix may be shorter than a stored key
    /// (e.g. a unique-index prefix); the cursor stops at the first key past the
    /// prefix range. Reading begins at the selected block, never block 0.
    pub(crate) fn prefix_cursor(&self, prefix: &[u8]) -> io::Result<TileCursor<'_>> {
        let block = dir_search(&self.fences, prefix);
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
        let mut actual = self.entries();
        loop {
            let (key, value) = match actual.next() {
                None => break,
                Some(entry) => entry?,
            };
            let csn = keys::key_csn(&key).ok_or_else(|| invalid("tile entry without CSN"))?;
            if csn <= lower { continue; }
            if csn > cutoff { return Err(invalid("tile entry above cutoff")); }
            match expect.remove(&key) {
                Some(v) if v == value => {}
                Some(_) => return Err(invalid("tile entry disagrees with WAL")),
                None => return Err(invalid("tile contains extra WAL entry")),
            }
        }
        if !expect.is_empty() { return Err(invalid("tile missing WAL entry")); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert_eq!(tile.fences.len(), 0, "no data blocks means no fences");
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
        assert_eq!(tile.fences.len(), 1);
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
        assert!(tile.fences.len() >= 8, "4 KiB target yields multiple blocks: {}", tile.fences.len());
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
        let big = vec![b'x'; 3 * BLOCK_TARGET];
        put(&mut mt, 1, 1, &big);
        put(&mut mt, 2, 1, b"small");
        let (path, tile) = write_tile("oversized.tile", 100, &mt);
        assert_eq!(tile.count(), 2);
        assert_eq!(tile.fences.len(), 2, "the oversized entry occupies its own block");
        let entries = collect(&tile);
        assert_eq!(entries[0].1, big);
        assert_eq!(entries[1].1, b"small".to_vec());
        let reopened = Tile::open(&path).unwrap();
        assert_eq!(collect(&reopened), entries);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn flip_inside_data_block_detected_when_read_not_at_open() {
        let mut mt = Memtable::new();
        put(&mut mt, 1, 1, b"payload");
        let (path, _) = write_tile("flip-data.tile", 5, &mt);
        let mut bytes = std::fs::read(&path).unwrap();
        let start = PAGE_SIZE as usize;
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
        assert!(tile.fences.len() >= 2, "need two fences to reorder");
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
        assert!(tile.fences.len() >= 2, "the prefix range must span blocks: {}", tile.fences.len());

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
        let start = PAGE_SIZE as usize;
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
        assert!(tile.fences.len() >= 2, "need two data blocks");
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
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32]).unwrap();
        for (key, value) in mt.entries() { w.push(key, value).unwrap(); }
        let tile_stream = w.finish().unwrap();
        assert_eq!(tile_mt.count(), 3000);
        assert_eq!(tile_stream.count(), 3000);
        assert!(tile_mt.fences.len() > 1, "multi-block tile");
        let a = std::fs::read(&path_mt).unwrap();
        let b = std::fs::read(&path_stream).unwrap();
        assert_eq!(a.len(), b.len(), "file sizes must match");
        assert_eq!(a, b, "streaming writer must be byte-identical to Tile::write");
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    #[test]
    fn streaming_writer_handles_empty_and_exact_block_fill() {
        let empty = Memtable::new();
        let (path_mt, tile_mt) = write_tile("writer-empty-mt", 0, &empty);
        let path_stream = tmp("writer-empty-stream");
        let tile_stream = TileWriter::new(&path_stream, 0, &[9u8; 32]).unwrap().finish().unwrap();
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
        assert_eq!(tile_mt.fences.len(), 2, "exact fill then spill");
        let path_stream = tmp("writer-exact-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32]).unwrap();
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
        assert_eq!(tile_mt.fences.len(), 3, "oversized entry alone in its block");
        let path_stream = tmp("writer-big-stream");
        let mut w = TileWriter::new(&path_stream, 1, &[9u8; 32]).unwrap();
        for (key, val) in mt.entries() { w.push(key, val).unwrap(); }
        assert_eq!(w.finish().unwrap().count(), 3);
        assert_eq!(std::fs::read(&path_mt).unwrap(), std::fs::read(&path_stream).unwrap());
        let _ = std::fs::remove_file(&path_mt);
        let _ = std::fs::remove_file(&path_stream);
    }

    #[test]
    fn streaming_writer_rejects_out_of_order_and_duplicate_keys() {
        let path = tmp("writer-order");
        let mut w = TileWriter::new(&path, 1, &[9u8; 32]).unwrap();
        w.push(&pkey(2, 1), b"x").unwrap();
        assert_eq!(w.push(&pkey(1, 1), b"y").unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(w.push(&pkey(2, 1), b"z").unwrap_err().kind(), ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&path);
    }
}
