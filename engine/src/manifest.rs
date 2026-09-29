//! Minimal durable A/B manifest root for published tiles. This slice records **at
//! most one** active tile; it never retires or truncates the WAL, and the dedup
//! map stays derivable by full replay of the retained WAL.
//!
//! File layout: exactly two fixed [`PAGE`] (4 KiB) slots, each
//! `[seq u64 LE][crc32c(payload) u32 LE][payload]` with a fixed 4092-byte payload
//! (unused tail zero-filled and covered by the CRC). The payload holds the ordered
//! list of active tile filenames (here: 0 or 1 entry: filename + cutoff + WAL
//! digest) plus this slice's checkpoint: the CSN frontier the tile makes durable
//! (= the tile cutoff) and the handle high-water mark at publication.
//!
//! Publication protocol (SS-26 discipline): write the *inactive* slot at
//! `root.seq + 1` → `fdatasync` the manifest file → `fsync` the parent directory →
//! flip the in-memory root. The caller must already have synced and verified the
//! tile file (including its directory entry) before publishing a reference to it.
//!
//! Validity rules:
//! - A page with a CRC mismatch, a short write (file ends mid-page), or an absent
//!   slot is **invalid**: reopening selects the highest *valid* seq page instead.
//! - A CRC-valid page whose payload is structurally impossible (more than one
//!   tile, zero tiles at `seq > 0`, a non-plain or oversized filename, unparsed
//!   trailing bytes) is **corruption**: open fails closed with an error rather
//!   than silently falling back to the older page.
//! - Two valid pages with equal seq are ambiguous and fail closed.
//! - A missing manifest file is the empty initial state (no checkpoint yet); a
//!   file shorter than one page or longer than two is corruption. Reopen after any
//!   manifest I/O error, never retry in place: any publish I/O error poisons the
//!   writer until a fresh [`Manifest::open`] selects the highest valid page.

use std::fs::{File, OpenOptions};
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::wal::crc32c;

/// Fixed manifest filename inside the engine directory.
pub(crate) const MANIFEST_NAME: &str = "manifest";
/// Conventional WAL filename used by [`crate::engine::Engine::open_discover`].
pub(crate) const WAL_NAME: &str = "data.wal";

const PAGE: usize = 4096;
const HEADER: usize = 12; // seq u64 LE + crc32c u32 LE
const PAYLOAD: usize = PAGE - HEADER;
const MAX_NAME: usize = 255;

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

/// One active tile referenced by a manifest page: a plain filename inside the
/// engine directory plus the cutoff/WAL-digest binding verified at open.
#[derive(Clone, Debug)]
pub(crate) struct TileRef {
    pub(crate) name: String,
    pub(crate) cutoff: u64,
    pub(crate) digest: [u8; 32],
}

/// CSN/handle-watermark checkpoint recorded by this slice. `csn` is the durable
/// frontier established by the published tile (its cutoff), not the full replay
/// CSN; the retained WAL remains the authority for everything above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    pub(crate) csn: u64,
    pub(crate) handle_watermark: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Root {
    pub(crate) seq: u64,
    pub(crate) tile: Option<TileRef>,
    pub(crate) checkpoint: Checkpoint,
}

/// Names a manifest entry may never take: they would collide with the manifest
/// itself or the conventional WAL file in the same directory.
pub(crate) fn valid_tile_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name != "." && name != ".."
        && name != MANIFEST_NAME && name != WAL_NAME
        && !name.bytes().any(|b| b == 0 || b == b'/')
}

fn encode_root(root: &Root) -> io::Result<[u8; PAYLOAD]> {
    let mut p = [0u8; PAYLOAD];
    let mut i = 0;
    match &root.tile {
        None => {
            if root.seq != 0 {
                return Err(invalid("empty manifest requires seq 0"));
            }
            i = 4; // tile_count = 0, rest zeroed
        }
        Some(t) => {
            let name = t.name.as_bytes();
            if !valid_tile_name(&t.name) {
                return Err(invalid("tile name is not a plain directory-local file"));
            }
            p[..4].copy_from_slice(&1u32.to_le_bytes());
            p[4..8].copy_from_slice(&(name.len() as u32).to_le_bytes());
            p[8..8 + name.len()].copy_from_slice(name);
            i = 8 + name.len();
            p[i..i + 8].copy_from_slice(&t.cutoff.to_le_bytes());
            i += 8;
            p[i..i + 32].copy_from_slice(&t.digest);
            i += 32;
        }
    }
    p[i..i + 8].copy_from_slice(&root.checkpoint.csn.to_le_bytes());
    p[i + 8..i + 16].copy_from_slice(&root.checkpoint.handle_watermark.to_le_bytes());
    Ok(p)
}

fn encode_page(seq: u64, root: &Root) -> io::Result<[u8; PAGE]> {
    let payload = encode_root(root)?;
    let mut page = [0u8; PAGE];
    page[..8].copy_from_slice(&seq.to_le_bytes());
    page[8..HEADER].copy_from_slice(&crc32c(&payload).to_le_bytes());
    page[HEADER..].copy_from_slice(&payload);
    Ok(page)
}

struct Cur<'a> { b: &'a [u8], i: usize }

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.b.len() - self.i < n { return Err(invalid("manifest payload truncated")); }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// CRC-valid but structurally impossible payloads are corruption, not fallback.
fn decode_payload(seq: u64, payload: &[u8]) -> io::Result<Root> {
    let mut c = Cur { b: payload, i: 0 };
    let tile = match c.u32()? {
        0 => {
            if seq != 0 { return Err(invalid("manifest lists zero tiles at nonzero seq")); }
            None
        }
        1 => {
            let n = c.u32()? as usize;
            if !(1..=MAX_NAME).contains(&n) { return Err(invalid("manifest tile name length out of bounds")); }
            let name = std::str::from_utf8(c.take(n)?)
                .map_err(|_| invalid("manifest tile name is not UTF-8"))?;
            if !valid_tile_name(name) {
                return Err(invalid("manifest tile name is not a plain directory-local file"));
            }
            let cutoff = c.u64()?;
            let digest = c.take(32)?.try_into().unwrap();
            Some(TileRef { name: name.to_string(), cutoff, digest })
        }
        _ => return Err(invalid("manifest lists more than one tile")),
    };
    let checkpoint = Checkpoint { csn: c.u64()?, handle_watermark: c.u64()? };
    if payload[c.i..].iter().any(|&b| b != 0) {
        return Err(invalid("manifest payload has unparsed trailing bytes"));
    }
    Ok(Root { seq, tile, checkpoint })
}

/// `Ok(None)` marks a torn/absent page that A/B selection may skip; an `Err`
/// means the bytes were CRC-valid but impossible, which must fail closed.
fn decode_page(page: &[u8; PAGE]) -> io::Result<Option<Root>> {
    let seq = u64::from_le_bytes(page[..8].try_into().unwrap());
    let payload = &page[HEADER..];
    if crc32c(payload) != u32::from_le_bytes(page[8..HEADER].try_into().unwrap()) {
        return Ok(None);
    }
    decode_payload(seq, payload).map(Some)
}

/// Open manifest handle rooted at the highest valid page. Single writer; any
/// publish I/O error poisons it until the engine is reopened.
#[derive(Debug)]
pub(crate) struct Manifest {
    file: File,
    path: PathBuf,
    root: Root,
    slot: usize,
    poisoned: bool,
    #[cfg(test)]
    pub(crate) inject_page_write_error: bool,
    #[cfg(test)]
    pub(crate) inject_sync_error: bool,
}

impl Manifest {
    fn parent(path: &Path) -> &Path {
        path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."))
    }

    /// Select the highest valid seq page of an existing manifest. A missing file
    /// is the empty initial state (`Ok(None)`); any other unreadable or
    /// structurally corrupt manifest fails closed.
    pub(crate) fn open(path: &Path) -> io::Result<Option<Manifest>> {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < PAGE as u64 { return Err(invalid("manifest file shorter than one page")); }
        if len > 2 * PAGE as u64 { return Err(invalid("manifest file larger than two pages")); }
        let mut raw = [0u8; PAGE];
        file.read_exact_at(&mut raw, 0)?;
        let page_a = decode_page(&raw)?;
        let page_b = if len >= 2 * PAGE as u64 {
            file.read_exact_at(&mut raw, PAGE as u64)?;
            decode_page(&raw)?
        } else {
            None // torn or never-written second slot: A/B selection skips it
        };
        let (root, slot) = match (&page_a, &page_b) {
            (Some(a), Some(b)) => {
                if a.seq == b.seq { return Err(invalid("manifest pages have equal seq")); }
                if a.seq > b.seq { (a.clone(), 0) } else { (b.clone(), 1) }
            }
            (Some(a), None) => (a.clone(), 0),
            (None, Some(b)) => (b.clone(), 1),
            (None, None) => return Err(invalid("manifest has no valid page")),
        };
        Ok(Some(Manifest {
            file,
            path: path.to_path_buf(),
            root,
            slot,
            poisoned: false,
            #[cfg(test)] inject_page_write_error: false,
            #[cfg(test)] inject_sync_error: false,
        }))
    }

    /// Create the manifest file with the empty initial page (seq 0) in slot A,
    /// durable before any tile can be referenced: create_new, page write, sync
    /// file, sync parent directory.
    pub(crate) fn create(path: &Path) -> io::Result<Manifest> {
        let file = OpenOptions::new().read(true).write(true).create_new(true).open(path)?;
        let initial = Root { seq: 0, tile: None, checkpoint: Checkpoint { csn: 0, handle_watermark: 0 } };
        file.write_all_at(&encode_page(0, &initial)?, 0)?;
        file.sync_data()?;
        File::open(Self::parent(path))?.sync_all()?;
        Ok(Manifest {
            file,
            path: path.to_path_buf(),
            root: initial,
            slot: 0,
            poisoned: false,
            #[cfg(test)] inject_page_write_error: false,
            #[cfg(test)] inject_sync_error: false,
        })
    }

    pub(crate) fn root(&self) -> &Root { &self.root }
    pub(crate) fn poisoned(&self) -> bool { self.poisoned }

    /// Write the inactive slot at `root.seq + 1`, sync the file, sync the parent
    /// directory, then flip the in-memory root. Any error poisons this writer.
    pub(crate) fn publish(&mut self, tile: TileRef, checkpoint: Checkpoint) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        let result = self.publish_inner(tile, checkpoint);
        if result.is_err() { self.poisoned = true; }
        result
    }

    fn publish_inner(&mut self, tile: TileRef, checkpoint: Checkpoint) -> io::Result<()> {
        let seq = self.root.seq.checked_add(1).ok_or_else(|| invalid("manifest seq exhausted"))?;
        let root = Root { seq, tile: Some(tile), checkpoint };
        let page = encode_page(seq, &root)?;
        let slot = 1 - self.slot;
        #[cfg(test)]
        if self.inject_page_write_error {
            self.inject_page_write_error = false;
            return Err(io::Error::new(io::ErrorKind::Other, "injected manifest page write failure"));
        }
        self.file.write_all_at(&page, (slot * PAGE) as u64)?;
        #[cfg(test)]
        if self.inject_sync_error {
            self.inject_sync_error = false;
            return Err(io::Error::new(io::ErrorKind::Other, "injected manifest sync failure"));
        }
        self.file.sync_data()?;
        File::open(Self::parent(&self.path))?.sync_all()?;
        self.root = root;
        self.slot = slot;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-manifest-{}-{name}", process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn tile(name: &str, cutoff: u64) -> TileRef {
        TileRef { name: name.to_string(), cutoff, digest: [7u8; 32] }
    }

    fn root_of(m: &Option<Manifest>) -> &Root { &m.as_ref().unwrap().root() }

    #[test]
    fn page_roundtrips_tile_and_checkpoint() {
        let root = Root { seq: 9, tile: Some(tile("data.tile", 42)), checkpoint: Checkpoint { csn: 42, handle_watermark: 43 } };
        let page = encode_page(9, &root).unwrap();
        let decoded = decode_page(&page).unwrap().unwrap();
        assert_eq!(decoded.seq, 9);
        assert_eq!(decoded.tile.as_ref().unwrap().name, "data.tile");
        assert_eq!(decoded.tile.as_ref().unwrap().cutoff, 42);
        assert_eq!(decoded.tile.as_ref().unwrap().digest, [7u8; 32]);
        assert_eq!(decoded.checkpoint, Checkpoint { csn: 42, handle_watermark: 43 });
    }

    #[test]
    fn crc_torn_page_is_invalid_but_impossible_payload_is_corruption() {
        let root = Root { seq: 1, tile: Some(tile("t.tile", 1)), checkpoint: Checkpoint { csn: 1, handle_watermark: 1 } };
        let mut page = encode_page(1, &root).unwrap();
        page[20] ^= 0x80; // flip a payload byte under the CRC
        assert!(decode_page(&page).unwrap().is_none(), "CRC mismatch must be an invalid page, not corruption");
        page[8] ^= 0x80; // now the stored CRC itself disagrees
        assert!(decode_page(&page).unwrap().is_none());

        // CRC-valid garbage: two tiles, zero tiles at nonzero seq, junk tail.
        let mut p = [0u8; PAYLOAD];
        p[..4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(decode_payload(1, &p).unwrap_err().kind(), ErrorKind::InvalidData);
        let mut p = [0u8; PAYLOAD];
        assert_eq!(decode_payload(1, &p).unwrap_err().to_string(), "manifest lists zero tiles at nonzero seq");
        let mut p = [0u8; PAYLOAD];
        p[24] = 9; // unparsed trailing byte after the checkpoint fields
        assert_eq!(decode_payload(0, &p).unwrap_err().kind(), ErrorKind::InvalidData);
        let mut p = [0u8; PAYLOAD];
        p[..4].copy_from_slice(&1u32.to_le_bytes());
        p[4..8].copy_from_slice(&5u32.to_le_bytes());
        p[8..13].copy_from_slice(b"a/b\0c");
        assert_eq!(decode_payload(1, &p).unwrap_err().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn valid_tile_name_bounds() {
        assert!(valid_tile_name("data.tile"));
        assert!(!valid_tile_name(""));
        assert!(!valid_tile_name("."));
        assert!(!valid_tile_name(".."));
        assert!(!valid_tile_name(MANIFEST_NAME));
        assert!(!valid_tile_name(WAL_NAME));
        assert!(!valid_tile_name("a/b"));
        assert!(!valid_tile_name(&"x".repeat(MAX_NAME + 1)));
    }

    #[test]
    fn open_missing_is_empty_and_created_manifest_starts_at_seq_zero() {
        let dir = tmp_dir("open-missing");
        assert!(Manifest::open(&dir.join(MANIFEST_NAME)).unwrap().is_none());
        let path = dir.join(MANIFEST_NAME);
        let m = Manifest::create(&path).unwrap();
        assert_eq!(m.root().seq, 0);
        assert!(m.root().tile.is_none());
        assert_eq!(fs::metadata(&path).unwrap().len(), PAGE as u64);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().seq, 0);
        assert!(reopened.root().tile.is_none());
    }

    #[test]
    fn open_selects_highest_valid_page_and_fails_closed_on_impossible_files() {
        let dir = tmp_dir("page-selection");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(tile("one.tile", 1), Checkpoint { csn: 1, handle_watermark: 2 }).unwrap();
        m.publish(tile("two.tile", 2), Checkpoint { csn: 2, handle_watermark: 3 }).unwrap();
        drop(m);
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).seq, 2);

        // Torn newest-slot tail: publish seq1 -> slot B, seq2 -> slot A. The
        // appended garbage truncates slot B's readable bytes, but slot A still
        // holds the valid seq-2 page, so it remains the selected root.
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(PAGE as u64 + 100).unwrap();
        drop(f);
        let opened = Manifest::open(&path).unwrap();
        let selected = root_of(&opened);
        assert_eq!(selected.seq, 2);
        assert_eq!(selected.tile.as_ref().unwrap().name, "two.tile");

        // Full-length file with a zero-filled (CRC-invalid) slot B keeps slot A.
        let mut bytes = fs::read(&path).unwrap();
        bytes.resize(2 * PAGE, 0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).seq, 2);

        // Zeroing page A selects the older but fully valid seq-1 page: the
        // zero-padded payload tail means a truncated page restores byte-identical
        // content, so "torn" detection cannot fire for pure truncation. This is
        // safe A/B selection (highest valid seq wins), not corruption.
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        let opened = Manifest::open(&path).unwrap();
        let selected = root_of(&opened);
        assert_eq!(selected.seq, 1);
        assert_eq!(selected.tile.as_ref().unwrap().name, "one.tile");

        // Both pages invalid (CRC), or structurally impossible files, fail closed.
        let mut bytes = fs::read(&path).unwrap();
        bytes[..PAGE].fill(0);
        bytes[2 * PAGE - 1] ^= 0xFF; // break slot B's payload CRC (last byte)
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);
        fs::write(&path, &bytes[..200]).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);
        let mut oversized = bytes.clone();
        oversized.extend_from_slice(&[0u8; 8]);
        fs::write(&path, &oversized).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn equal_seq_pages_fail_closed() {
        let dir = tmp_dir("equal-seq");
        let path = dir.join(MANIFEST_NAME);
        let a = Root { seq: 1, tile: Some(tile("a.tile", 1)), checkpoint: Checkpoint { csn: 1, handle_watermark: 1 } };
        let b = Root { seq: 1, tile: Some(tile("b.tile", 1)), checkpoint: Checkpoint { csn: 1, handle_watermark: 1 } };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&encode_page(1, &a).unwrap());
        bytes.extend_from_slice(&encode_page(1, &b).unwrap());
        fs::write(&path, bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().to_string(), "manifest pages have equal seq");
    }

    #[test]
    fn publish_flips_slots_and_poisons_on_injected_io_errors() {
        let dir = tmp_dir("poison");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(tile("one.tile", 1), Checkpoint { csn: 1, handle_watermark: 2 }).unwrap();
        assert_eq!(m.root().seq, 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), 2 * PAGE as u64);

        m.inject_page_write_error = true;
        let err = m.publish(tile("two.tile", 2), Checkpoint { csn: 2, handle_watermark: 3 }).unwrap_err();
        assert!(err.to_string().contains("injected manifest page write failure"));
        assert!(m.poisoned());
        m.inject_page_write_error = false;
        assert_eq!(m.publish(tile("two.tile", 2), Checkpoint { csn: 2, handle_watermark: 3 }).unwrap_err().to_string(),
            "manifest writer poisoned; reopen to recover");
        drop(m);

        // Reopen selects the highest valid page (seq 1); the failed publish left
        // the newer slot torn but the root intact.
        let mut m = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(m.root().seq, 1);
        m.publish(tile("two.tile", 2), Checkpoint { csn: 2, handle_watermark: 3 }).unwrap();
        assert_eq!(m.root().seq, 2);
        drop(m);

        let mut m = Manifest::open(&path).unwrap().unwrap();
        m.inject_sync_error = true;
        assert!(m.publish(tile("three.tile", 3), Checkpoint { csn: 3, handle_watermark: 4 }).is_err());
        assert!(m.poisoned());
        drop(m);
        // The page write landed even though its sync failed; reopen takes the
        // highest valid page — the same recoverable ambiguity as an unacked
        // complete WAL record, never a silent loss of the previous root.
        let opened = Manifest::open(&path).unwrap();
        let selected = root_of(&opened);
        assert_eq!(selected.seq, 3);
        assert_eq!(selected.tile.as_ref().unwrap().name, "three.tile");
    }
}
