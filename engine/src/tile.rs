//! Explicit, WAL-retained immutable tile prototype. No manifest, checkpoint, or WAL retirement.
//! The file is validated before it can displace any serving memtable entries. A read-only,
//! file-backed mapping supplies borrowed values for Engine::get without copying the entire file
//! into a Vec; this is not a bounded-RAM or bounded-recovery claim.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::ptr::NonNull;
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
            let tail = &key[first + 2..];
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

// POSIX MAP_PRIVATE read-only mapping: no tile-sized allocation, stable borrowed values
// while the Engine holds the fd. The tile file must not be externally modified or truncated
// while open (immutable file contract); reopen verifies it again.
struct Mapping { ptr: NonNull<u8>, len: usize }
impl Mapping {
    fn new(file: &File) -> io::Result<Self> {
        let len = usize::try_from(file.metadata()?.len()).map_err(|_| invalid("tile size overflow"))?;
        if len < HEADER || len > isize::MAX as usize { return Err(invalid("tile size out of bounds")); }
        unsafe extern "C" {
            fn mmap(addr: *mut std::ffi::c_void, length: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut std::ffi::c_void;
        }
        let ptr = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
        if ptr as isize == -1 { return Err(io::Error::last_os_error()); }
        Ok(Self { ptr: NonNull::new(ptr.cast()).expect("mmap non-null"), len })
    }
    fn bytes(&self) -> &[u8] { unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) } }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe extern "C" { fn munmap(addr: *mut std::ffi::c_void, length: usize) -> i32; }
        unsafe { munmap(self.ptr.as_ptr().cast(), self.len); }
    }
}

pub(crate) struct Tile { _file: File, mapping: Mapping, cutoff: u64, digest: [u8; 32], count: u64 }
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
        let mapping = Mapping::new(&file)?;
        let bytes = mapping.bytes();
        if &bytes[..8] != MAGIC || checksum(&bytes[..64]) != u32::from_le_bytes(bytes[64..68].try_into().unwrap()) {
            return Err(invalid("tile header checksum/magic"));
        }
        let cutoff = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let digest = bytes[16..48].try_into().unwrap();
        let count = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
        let size = u64::from_le_bytes(bytes[56..64].try_into().unwrap());
        if size != bytes.len() as u64 || count > (bytes.len() - HEADER) as u64 / (RECORD as u64 + 18) {
            return Err(invalid("tile truncated or entry count invalid"));
        }
        let mut pos = HEADER;
        let mut previous: Option<&[u8]> = None;
        for _ in 0..count {
            let (key, _value, next) = parse(bytes, pos)?;
            if !valid_key(key) || keys::key_csn(key).is_none_or(|csn| csn > cutoff)
                || previous.is_some_and(|prev| prev >= key) {
                return Err(invalid("tile key order/encoding/cutoff"));
            }
            previous = Some(key);
            pos = next;
        }
        if pos != bytes.len() { return Err(invalid("tile trailing bytes")); }
        Ok(Self { _file: file, mapping, cutoff, digest, count })
    }
    pub(crate) fn cutoff(&self) -> u64 { self.cutoff }
    pub(crate) fn digest(&self) -> &[u8; 32] { &self.digest }
    pub(crate) fn entries(&self) -> impl Iterator<Item = (&[u8], &[u8])> {
        let bytes = self.mapping.bytes();
        let mut pos = HEADER;
        (0..self.count).map(move |_| {
            // All entries and offsets were checked by open; file is immutable while in use.
            let (key, value, next) = parse(bytes, pos).expect("verified tile entry");
            pos = next;
            (key, value)
        })
    }
}

fn parse(bytes: &[u8], pos: usize) -> io::Result<(&[u8], &[u8], usize)> {
    let hdr = bytes.get(pos..pos.checked_add(RECORD).ok_or_else(|| invalid("tile offset overflow"))?)
        .ok_or_else(|| invalid("tile record header truncated"))?;
    let k = u32::from_le_bytes(hdr[..4].try_into().unwrap()) as usize;
    let v = u32::from_le_bytes(hdr[4..8].try_into().unwrap()) as usize;
    if k < 18 || k > MAX_KEY || v > MAX_VALUE { return Err(invalid("tile entry length out of bounds")); }
    let start = pos + RECORD;
    let mid = start.checked_add(k).ok_or_else(|| invalid("tile key size overflow"))?;
    let end = mid.checked_add(v).ok_or_else(|| invalid("tile value size overflow"))?;
    let key = bytes.get(start..mid).ok_or_else(|| invalid("tile key truncated"))?;
    let value = bytes.get(mid..end).ok_or_else(|| invalid("tile value truncated"))?;
    if entry_checksum(key, value) != u32::from_le_bytes(hdr[8..12].try_into().unwrap()) {
        return Err(invalid("tile record checksum"));
    }
    Ok((key, value, end))
}
