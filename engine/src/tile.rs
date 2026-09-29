//! Explicit, WAL-retained immutable tile prototype. No manifest, checkpoint, or WAL retirement.
//! The file is validated before it can displace any serving memtable entries. Reads use
//! owned, checksum-verified record buffers; this is not a bounded-recovery claim.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::LazyLock;

use crate::keys;
use crate::memtable::Memtable;

const MAGIC: &[u8; 8] = b"FDBTILE1";
const HEADER: usize = 8 + 8 + 32 + 8 + 8 + 4; // magic, cutoff, WAL digest, count, size, crc
const RECORD: usize = 12; // key len, value len, crc32c(key || value)
const MAX_KEY: usize = 1 << 24;
const MAX_VALUE: usize = 1 << 26;

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

// Incremental CRC-32C permits large values without an entry-sized temporary buffer.
fn crc(mut state: u32, bytes: &[u8]) -> u32 {
    static TABLE: LazyLock<[u32; 256]> = LazyLock::new(|| {
        let mut table = [0; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 { c = (c >> 1) ^ (if c & 1 != 0 { 0x82f6_3b78 } else { 0 }); }
            *slot = c;
        }
        table
    });
    for &byte in bytes { state = TABLE[((state ^ byte as u32) & 255) as usize] ^ (state >> 8); }
    state
}
fn checksum(bytes: &[u8]) -> u32 { !crc(!0, bytes) }
fn entry_checksum(key: &[u8], value: &[u8]) -> u32 { !crc(crc(!0, key), value) }

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

fn header(cutoff: u64, digest: &[u8; 32], count: u64, size: u64) -> [u8; HEADER] {
    let mut h = [0; HEADER];
    h[..8].copy_from_slice(MAGIC);
    h[8..16].copy_from_slice(&cutoff.to_le_bytes());
    h[16..48].copy_from_slice(digest);
    h[48..56].copy_from_slice(&count.to_le_bytes());
    h[56..64].copy_from_slice(&size.to_le_bytes());
    let sum = checksum(&h[..64]);
    h[64..].copy_from_slice(&sum.to_le_bytes());
    h
}

// Positional reads never expose borrowed file-backed bytes. Truncation and
// corruption yield io::Error, never SIGBUS/panic. The multi-tile merge
// pre-advances every published tile, so corruption anywhere in a tile fails
// every read closed (stronger than the earlier single-tile qualification).
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

pub(crate) struct Tile { file: File, header: [u8; HEADER], cutoff: u64, digest: [u8; 32], count: u64, size: u64 }

struct Entries<'a> { tile: &'a Tile, pos: u64, left: u64, checked: bool, done: bool }
impl Iterator for Entries<'_> {
    type Item = io::Result<(Vec<u8>, Vec<u8>)>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done { return None; }
        let result = (|| {
            if !self.checked {
                self.checked = true;
                if self.tile.file.metadata()?.len() != self.tile.size { return Err(invalid("tile size changed")); }
                let mut h = [0; HEADER];
                read_exact_at(&self.tile.file, &mut h, 0)?;
                if h != self.tile.header { return Err(invalid("tile header changed")); }
            }
            if self.left == 0 {
                if self.pos != self.tile.size { return Err(invalid("tile trailing bytes")); }
                return Ok(None);
            }
            let mut hdr = [0; RECORD];
            read_exact_at(&self.tile.file, &mut hdr, self.pos)?;
            let k = u32::from_le_bytes(hdr[..4].try_into().unwrap()) as usize;
            let v = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
            if !(18..=MAX_KEY).contains(&k) || v > MAX_VALUE { return Err(invalid("tile entry length out of bounds")); }
            let start = self.pos.checked_add(RECORD as u64).ok_or_else(|| invalid("tile offset overflow"))?;
            let mid = start.checked_add(k as u64).ok_or_else(|| invalid("tile key size overflow"))?;
            let end = mid.checked_add(v as u64).ok_or_else(|| invalid("tile value size overflow"))?;
            if end > self.tile.size { return Err(invalid("tile entry exceeds file")); }
            let mut key = vec![0; k];
            let mut value = vec![0; v];
            read_exact_at(&self.tile.file, &mut key, start)?;
            read_exact_at(&self.tile.file, &mut value, mid)?;
            if entry_checksum(&key, &value) != u32::from_le_bytes(hdr[8..12].try_into().unwrap()) {
                return Err(invalid("tile record checksum"));
            }
            self.pos = end;
            self.left -= 1;
            Ok(Some((key, value)))
        })();
        match result {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => { self.done = true; None }
            Err(e) => { self.done = true; Some(Err(e)) }
        }
    }
}

impl Tile {
    pub(crate) fn write(path: &Path, cutoff: u64, digest: &[u8; 32], mt: &Memtable) -> io::Result<Self> {
        // create_new never replaces a verified tile. A failed/crashed build leaves a partial
        // candidate, but cannot change the Engine's serving state; caller may remove it explicitly.
        let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
        f.write_all(&header(cutoff, digest, 0, HEADER as u64))?;
        let mut count = 0u64;
        let mut size = HEADER as u64;
        for (key, value) in mt.entries() {
            if keys::key_csn(key).is_none_or(|csn| csn > cutoff) { continue; }
            if !valid_key(key) || key.len() > MAX_KEY || value.len() > MAX_VALUE {
                return Err(invalid("unsupported tile entry; memtable unchanged"));
            }
            let entry_size = RECORD as u64 + key.len() as u64 + value.len() as u64;
            size = size.checked_add(entry_size).ok_or_else(|| invalid("tile size overflow"))?;
            count = count.checked_add(1).ok_or_else(|| invalid("tile count overflow"))?;
            f.write_all(&(key.len() as u32).to_le_bytes())?;
            f.write_all(&(value.len() as u32).to_le_bytes())?;
            f.write_all(&entry_checksum(key, value).to_le_bytes())?;
            f.write_all(key)?;
            f.write_all(value)?;
        }
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&header(cutoff, digest, count, size))?;
        f.sync_data()?; // No parent-directory durability claim.
        drop(f);
        let tile = Self::open(path)?;
        if tile.cutoff != cutoff || tile.digest != *digest { return Err(invalid("tile verification mismatch")); }
        Ok(tile)
    }

    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        if size < HEADER as u64 { return Err(invalid("tile header truncated")); }
        let mut h = [0; HEADER];
        read_exact_at(&file, &mut h, 0)?;
        if &h[..8] != MAGIC || checksum(&h[..64]) != u32::from_le_bytes(h[64..68].try_into().unwrap()) {
            return Err(invalid("tile header checksum/magic"));
        }
        let cutoff = u64::from_le_bytes(h[8..16].try_into().unwrap());
        let digest = h[16..48].try_into().unwrap();
        let count = u64::from_le_bytes(h[48..56].try_into().unwrap());
        if u64::from_le_bytes(h[56..64].try_into().unwrap()) != size
            || count > (size - HEADER as u64) / (RECORD as u64 + 18) {
            return Err(invalid("tile size/count invalid"));
        }
        let tile = Self { file, header: h, cutoff, digest, count, size };
        let mut previous: Option<Vec<u8>> = None;
        for entry in tile.entries() {
            let (key, _) = entry?;
            if !valid_key(&key) || keys::key_csn(&key).is_none_or(|csn| csn > cutoff)
                || previous.as_ref().is_some_and(|prev| prev >= &key) {
                return Err(invalid("tile key order/encoding/cutoff"));
            }
            previous = Some(key);
        }
        Ok(tile)
    }
    pub(crate) fn cutoff(&self) -> u64 { self.cutoff }
    pub(crate) fn digest(&self) -> &[u8; 32] { &self.digest }
    pub(crate) fn entries(&self) -> impl Iterator<Item = io::Result<(Vec<u8>, Vec<u8>)>> + '_ {
        Entries { tile: self, pos: HEADER as u64, left: self.count, checked: false, done: false }
    }

    /// Exact comparison against the WAL projection in the CSN range
    /// `(lower, cutoff]`: every expected entry must match a tile entry in key
    /// order, and any leftover tile entry is an error. Tile ranges are disjoint
    /// (`lower` is the previous tile's cutoff, 0 for the oldest), so a tile
    /// holding another range's versions fails closed here.
    pub(crate) fn verify_projection(&self, expected: &Memtable, lower: u64, cutoff: u64) -> io::Result<()> {
        let mut actual = self.entries();
        for (key, value) in expected.entries().filter(|(key, _)| {
            keys::key_csn(key).is_some_and(|csn| csn > lower && csn <= cutoff)
        }) {
            let (got_key, got_value) = actual.next().ok_or_else(|| invalid("tile missing WAL entry"))??;
            if got_key != key || got_value != value { return Err(invalid("tile entry disagrees with WAL")); }
        }
        if let Some(entry) = actual.next() { entry?; return Err(invalid("tile contains extra WAL entry")); }
        Ok(())
    }
}
