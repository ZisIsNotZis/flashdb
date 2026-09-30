//! Minimal durable A/B manifest root for published tiles. The root records an
//! **ordered list of active tiles** (ascending, strictly increasing cutoffs, so
//! their CSN ranges never overlap) bounded by [`MAX_TILES`] so the list always
//! fits one page, plus the **current WAL segment** reference once rotation has
//! happened: records at or below the segment's start CSN are disposable (a
//! published tile covers them), so the named segment file holds only the live
//! suffix and the root must name it — otherwise recovery could not find the
//! post-rotation WAL at all.
//!
//! File layout: exactly two fixed [`PAGE`] (4 KiB) slots, each
//! `[seq u64 LE][crc32c(payload) u32 LE][payload]` with a fixed 4092-byte payload
//! (unused tail zero-filled and covered by the CRC). The payload holds the
//! ordered list of active tile filenames (filename + cutoff + WAL digest each),
//! the checkpoint (the CSN frontier the newest tile makes durable = the newest
//! tile cutoff, plus the handle high-water mark at publication), and the segment
//! reference (plain filename + start CSN) when the WAL has been rotated.
//!
//! File layout: exactly two fixed [`PAGE`] (4 KiB) slots, each
//! `[seq u64 LE][crc32c(payload) u32 LE][payload]` with a fixed 4092-byte payload
//! (unused tail zero-filled and covered by the CRC). The payload holds the
//! ordered list of active tile filenames (filename + cutoff + WAL digest each)
//! plus the checkpoint: the CSN frontier the newest tile makes durable (= the
//! newest tile cutoff) and the handle high-water mark at publication.
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

use crate::crc::crc32c;

/// Fixed manifest filename inside the engine directory.
pub(crate) const MANIFEST_NAME: &str = "manifest";
/// Conventional WAL filename used by [`crate::engine::Engine::open_discover`].
pub(crate) const WAL_NAME: &str = "data.wal";

const PAGE: usize = 4096;
const HEADER: usize = 12; // seq u64 LE + crc32c u32 LE
const PAYLOAD: usize = PAGE - HEADER;
const MAX_NAME: usize = 255;

/// Small active-tile bound: [`MAX_TILES`] entries of at most
/// 4 + [`MAX_NAME`] + 8 + 32 bytes each always fit one payload with the count
/// and checkpoint fields to spare, so the bound is about the one-page invariant,
/// not an arbitrary engine limit. A publish beyond it is refused as
/// `InvalidInput`; a CRC-valid page claiming more is corruption (fail closed).
pub(crate) const MAX_TILES: usize = 8;

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

/// The engine's current WAL segment once rotation exists: a plain filename in
/// the engine directory and the CSN cutoff at which it started. Every record in
/// the named file must have a CSN strictly above `start_csn` (records at or
/// below it are dead by design — a published tile covers them); recovery fail-
/// closes on any record violating that, so a stale or foreign segment cannot be
/// mistaken for the live suffix. `None` = the WAL was never rotated and the
/// whole history lives in the conventional `data.wal`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SegmentRef {
    pub(crate) name: String,
    pub(crate) start_csn: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Root {
    pub(crate) seq: u64,
    /// Active tiles ordered by strictly increasing cutoff (non-overlapping CSN
    /// ranges). Empty only in the initial seq-0 root.
    pub(crate) tiles: Vec<TileRef>,
    pub(crate) checkpoint: Checkpoint,
    /// Current WAL segment once rotation has happened; `None` before it.
    pub(crate) segment: Option<SegmentRef>,
}

fn cutoffs_strictly_increasing(tiles: &[TileRef]) -> bool {
    tiles.windows(2).all(|w| w[0].cutoff < w[1].cutoff)
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
    if root.tiles.is_empty() && root.seq != 0 {
        return Err(invalid("empty manifest requires seq 0"));
    }
    if !root.tiles.is_empty() {
        if root.tiles.len() > MAX_TILES {
            return Err(invalid("manifest tile list exceeds one page"));
        }
        if !cutoffs_strictly_increasing(&root.tiles) {
            return Err(invalid("manifest tile cutoffs overlap or are not strictly increasing"));
        }
        p[..4].copy_from_slice(&(root.tiles.len() as u32).to_le_bytes());
    }
    // tile_count is zero for an empty manifest, then the tile records (none when empty).
    let mut i = 4;
    for t in &root.tiles {
        let name = t.name.as_bytes();
        if !valid_tile_name(&t.name) {
            return Err(invalid("tile name is not a plain directory-local file"));
        }
        // MAX_TILES * (4 + MAX_NAME + 8 + 32) + 20 stays far below PAYLOAD,
        // so these fixed-size writes cannot run past the page.
        p[i..i + 4].copy_from_slice(&(name.len() as u32).to_le_bytes());
        p[i + 4..i + 4 + name.len()].copy_from_slice(name);
        i += 4 + name.len();
        p[i..i + 8].copy_from_slice(&t.cutoff.to_le_bytes());
        i += 8;
        p[i..i + 32].copy_from_slice(&t.digest);
        i += 32;
    }
    p[i..i + 8].copy_from_slice(&root.checkpoint.csn.to_le_bytes());
    p[i + 8..i + 16].copy_from_slice(&root.checkpoint.handle_watermark.to_le_bytes());
    i += 16;
    match &root.segment {
        None => p[i..i + 4].copy_from_slice(&0u32.to_le_bytes()), // name_len 0 = no segment
        Some(s) => {
            if !valid_tile_name(&s.name) {
                return Err(invalid("segment name is not a plain directory-local file"));
            }
            let name = s.name.as_bytes();
            p[i..i + 4].copy_from_slice(&(name.len() as u32).to_le_bytes());
            i += 4;
            p[i..i + name.len()].copy_from_slice(name);
            i += name.len();
            p[i..i + 8].copy_from_slice(&s.start_csn.to_le_bytes());
        }
    }
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
    let count = c.u32()? as usize;
    let mut tiles = Vec::new();
    if count == 0 {
        if seq != 0 { return Err(invalid("manifest lists zero tiles at nonzero seq")); }
    } else {
        if count > MAX_TILES { return Err(invalid("manifest lists more tiles than fit in one page")); }
        for _ in 0..count {
            let n = c.u32()? as usize;
            if !(1..=MAX_NAME).contains(&n) { return Err(invalid("manifest tile name length out of bounds")); }
            let name = std::str::from_utf8(c.take(n)?)
                .map_err(|_| invalid("manifest tile name is not UTF-8"))?;
            if !valid_tile_name(name) {
                return Err(invalid("manifest tile name is not a plain directory-local file"));
            }
            let cutoff = c.u64()?;
            let digest = c.take(32)?.try_into().unwrap();
            tiles.push(TileRef { name: name.to_string(), cutoff, digest });
        }
        if !cutoffs_strictly_increasing(&tiles) {
            return Err(invalid("manifest tile cutoffs overlap or are not strictly increasing"));
        }
    }
    let checkpoint = Checkpoint { csn: c.u64()?, handle_watermark: c.u64()? };
    if tiles.last().is_some_and(|t| checkpoint.csn != t.cutoff) {
        return Err(invalid("manifest checkpoint CSN disagrees with newest tile cutoff"));
    }
    let segment = {
        let n = c.u32()? as usize;
        if n == 0 {
            None
        } else {
            if !(1..=MAX_NAME).contains(&n) { return Err(invalid("manifest segment name length out of bounds")); }
            let name = std::str::from_utf8(c.take(n)?)
                .map_err(|_| invalid("manifest segment name is not UTF-8"))?;
            if !valid_tile_name(name) {
                return Err(invalid("manifest segment name is not a plain directory-local file"));
            }
            Some(SegmentRef { name: name.to_string(), start_csn: c.u64()? })
        }
    };
    if tiles.is_empty() && segment.is_some() {
        return Err(invalid("empty manifest cannot reference a WAL segment"));
    }
    if segment.as_ref().is_some_and(|s| s.start_csn > checkpoint.csn) {
        return Err(invalid("manifest segment starts above the checkpoint CSN"));
    }
    if payload[c.i..].iter().any(|&b| b != 0) {
        return Err(invalid("manifest payload has unparsed trailing bytes"));
    }
    Ok(Root { seq, tiles, checkpoint, segment })
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
        if len < PAGE as u64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                "manifest file shorter than one page (a torn Manifest::create); the file is not recoverable in place - move it aside and republish the tile"));
        }
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
        let initial = Root { seq: 0, tiles: Vec::new(), checkpoint: Checkpoint { csn: 0, handle_watermark: 0 }, segment: None };
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
    /// directory, then flip the in-memory root. `tiles` replaces the full active
    /// list and `segment` the full segment reference; structural problems in
    /// them fail before any I/O. Any error poisons this writer.
    pub(crate) fn publish(&mut self, tiles: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        let result = self.publish_inner(tiles, checkpoint, segment);
        if result.is_err() { self.poisoned = true; }
        result
    }

    fn publish_inner(&mut self, tiles: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<()> {
        let seq = self.root.seq.checked_add(1).ok_or_else(|| invalid("manifest seq exhausted"))?;
        if tiles.is_empty() || tiles.len() > MAX_TILES {
            return Err(invalid("publish requires one to MAX_TILES active tiles"));
        }
        if !cutoffs_strictly_increasing(&tiles) {
            return Err(invalid("manifest tile cutoffs overlap or are not strictly increasing"));
        }
        if checkpoint.csn != tiles.last().unwrap().cutoff {
            return Err(invalid("manifest checkpoint CSN disagrees with newest tile cutoff"));
        }
        let root = Root { seq, tiles, checkpoint, segment };
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
    fn page_roundtrips_tiles_and_checkpoint() {
        let root = Root { seq: 9, tiles: vec![tile("a.tile", 40), tile("b.tile", 42)], checkpoint: Checkpoint { csn: 42, handle_watermark: 43 }, segment: None };
        let page = encode_page(9, &root).unwrap();
        let decoded = decode_page(&page).unwrap().unwrap();
        assert_eq!(decoded.seq, 9);
        assert_eq!(decoded.tiles.len(), 2);
        assert_eq!(decoded.tiles[0].name, "a.tile");
        assert_eq!(decoded.tiles[0].cutoff, 40);
        assert_eq!(decoded.tiles[0].digest, [7u8; 32]);
        assert_eq!(decoded.tiles[1].name, "b.tile");
        assert_eq!(decoded.tiles[1].cutoff, 42);
        assert_eq!(decoded.checkpoint, Checkpoint { csn: 42, handle_watermark: 43 });
    }

    #[test]
    fn crc_torn_page_is_invalid_but_impossible_payload_is_corruption() {
        let root = Root { seq: 1, tiles: vec![tile("t.tile", 1)], checkpoint: Checkpoint { csn: 1, handle_watermark: 1 }, segment: None };
        let mut page = encode_page(1, &root).unwrap();
        page[20] ^= 0x80; // flip a payload byte under the CRC
        assert!(decode_page(&page).unwrap().is_none(), "CRC mismatch must be an invalid page, not corruption");
        page[8] ^= 0x80; // now the stored CRC itself disagrees
        assert!(decode_page(&page).unwrap().is_none());

        // CRC-valid garbage: too many tiles, zero tiles at nonzero seq, junk tail.
        let mut p = [0u8; PAYLOAD];
        p[..4].copy_from_slice(&((MAX_TILES + 1) as u32).to_le_bytes());
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

        // CRC-valid but structurally impossible tile lists are corruption:
        // decreasing/equal cutoffs, or a checkpoint that is not the newest
        // cutoff. encode_root refuses to produce these, so the payloads are
        // hand-assembled the way only corruption could write them.
        let entry = |name: &[u8], cutoff: u64| {
            let mut e = Vec::new();
            e.extend_from_slice(&(name.len() as u32).to_le_bytes());
            e.extend_from_slice(name);
            e.extend_from_slice(&cutoff.to_le_bytes());
            e.extend_from_slice(&[7u8; 32]);
            e
        };
        let hand_built = |entries: &[(Vec<u8>, u64)], checkpoint: (u64, u64)| {
            let mut p = [0u8; PAYLOAD];
            p[..4].copy_from_slice(&(entries.len() as u32).to_le_bytes());
            let mut i = 4;
            for (name, cutoff) in entries {
                let e = entry(name, *cutoff);
                p[i..i + e.len()].copy_from_slice(&e);
                i += e.len();
            }
            p[i..i + 8].copy_from_slice(&checkpoint.0.to_le_bytes());
            p[i + 8..i + 16].copy_from_slice(&checkpoint.1.to_le_bytes());
            p
        };
        assert_eq!(decode_payload(3, &hand_built(&[(b"b.tile".to_vec(), 3), (b"a.tile".to_vec(), 3)], (3, 3))).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(decode_payload(3, &hand_built(&[(b"b.tile".to_vec(), 3), (b"a.tile".to_vec(), 2)], (2, 3))).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(decode_payload(3, &hand_built(&[(b"a.tile".to_vec(), 2), (b"b.tile".to_vec(), 3)], (2, 3))).unwrap_err().to_string(),
            "manifest checkpoint CSN disagrees with newest tile cutoff");
    }

    #[test]
    fn segment_roundtrips_and_structural_corruption_fails_closed() {
        let seg = |name: &str, start: u64| SegmentRef { name: name.to_string(), start_csn: start };
        let root = Root {
            seq: 7,
            tiles: vec![tile("t.tile", 4)],
            checkpoint: Checkpoint { csn: 4, handle_watermark: 9 },
            segment: Some(seg("data-4.wal", 4)),
        };
        let decoded = decode_page(&encode_page(7, &root).unwrap()).unwrap().unwrap();
        assert_eq!(decoded.segment.as_ref().unwrap(), &seg("data-4.wal", 4));
        // A root without rotation decodes with no segment reference.
        let plain = Root { segment: None, ..root.clone() };
        let decoded = decode_page(&encode_page(7, &plain).unwrap()).unwrap().unwrap();
        assert!(decoded.segment.is_none());

        // The encoder refuses segment names colliding with the manifest or the
        // conventional WAL filename (same rules as tile names).
        let mut bad = root.clone();
        bad.segment = Some(seg(MANIFEST_NAME, 4));
        assert!(encode_page(7, &bad).is_err());
        bad.segment = Some(seg(WAL_NAME, 4));
        assert!(encode_page(7, &bad).is_err());

        // Hand-built CRC-valid payloads that only corruption could produce:
        // an empty root referencing a segment, an out-of-bounds segment name
        // length, and a non-plain segment name.
        let with_segment = |tail: &[u8]| {
            let mut p = [0u8; PAYLOAD];
            p[20..20 + tail.len()].copy_from_slice(tail);
            p
        };
        // Empty root: count/checkpoint are zero, so the segment fields start at 20.
        let p = with_segment(&{
            let mut t = 4u32.to_le_bytes().to_vec();
            t.extend_from_slice(b"data");
            t.extend_from_slice(&0u64.to_le_bytes());
            t
        });
        assert_eq!(decode_payload(0, &p).unwrap_err().to_string(), "empty manifest cannot reference a WAL segment");
        // One-tile root: tile entry ends at 54, checkpoint at 54..70, segment
        // fields follow at 70.
        let one_tile_seg = |tail: &[u8]| {
            let mut p = [0u8; PAYLOAD];
            p[..4].copy_from_slice(&1u32.to_le_bytes());
            p[4..8].copy_from_slice(&6u32.to_le_bytes());
            p[8..14].copy_from_slice(b"t.tile");
            p[14..22].copy_from_slice(&4u64.to_le_bytes());
            p[22..54].copy_from_slice(&[7u8; 32]);
            p[54..62].copy_from_slice(&4u64.to_le_bytes());
            p[62..70].copy_from_slice(&9u64.to_le_bytes());
            p[70..70 + tail.len()].copy_from_slice(tail);
            p
        };
        let p = one_tile_seg(&((MAX_NAME as u32 + 1).to_le_bytes()));
        assert_eq!(decode_payload(7, &p).unwrap_err().to_string(), "manifest segment name length out of bounds");
        let p = one_tile_seg(&{
            let mut t = 4u32.to_le_bytes().to_vec();
            t.extend_from_slice(b"a/b\0c");
            t.extend_from_slice(&4u64.to_le_bytes());
            t
        });
        assert_eq!(decode_payload(7, &p).unwrap_err().to_string(), "manifest segment name is not a plain directory-local file");
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
        assert!(m.root().tiles.is_empty());
        assert_eq!(fs::metadata(&path).unwrap().len(), PAGE as u64);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().seq, 0);
        assert!(reopened.root().tiles.is_empty());
    }

    #[test]
    fn open_selects_highest_valid_page_and_fails_closed_on_impossible_files() {
        let dir = tmp_dir("page-selection");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(vec![tile("one.tile", 1)], Checkpoint { csn: 1, handle_watermark: 2 }, None).unwrap();
        // The seq-2 root carries a two-tile list: each publish replaces the full
        // active list, so an A/B fallback must restore a list, not one entry.
        m.publish(vec![tile("one.tile", 1), tile("two.tile", 2)], Checkpoint { csn: 2, handle_watermark: 3 }, None).unwrap();
        drop(m);
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).seq, 2);

        // Torn newest-slot tail: publish seq1 -> slot B, seq2 -> slot A. The
        // appended garbage truncates slot B's readable bytes, but slot A still
        // holds the valid seq-2 page with its full tile list, so it remains the
        // selected root.
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(PAGE as u64 + 100).unwrap();
        drop(f);
        let opened = Manifest::open(&path).unwrap();
        let selected = root_of(&opened);
        assert_eq!(selected.seq, 2);
        assert_eq!(selected.tiles.len(), 2, "fallback restores the full previous tile list");
        assert_eq!(selected.tiles[1].name, "two.tile");

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
        assert_eq!(selected.tiles[0].name, "one.tile");

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
        let a = Root { seq: 1, tiles: vec![tile("a.tile", 1)], checkpoint: Checkpoint { csn: 1, handle_watermark: 1 }, segment: None };
        let b = Root { seq: 1, tiles: vec![tile("b.tile", 1)], checkpoint: Checkpoint { csn: 1, handle_watermark: 1 }, segment: None };
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
        m.publish(vec![tile("one.tile", 1)], Checkpoint { csn: 1, handle_watermark: 2 }, None).unwrap();
        assert_eq!(m.root().seq, 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), 2 * PAGE as u64);

        m.inject_page_write_error = true;
        let err = m.publish(vec![tile("two.tile", 2)], Checkpoint { csn: 2, handle_watermark: 3 }, None).unwrap_err();
        assert!(err.to_string().contains("injected manifest page write failure"));
        assert!(m.poisoned());
        m.inject_page_write_error = false;
        assert_eq!(m.publish(vec![tile("two.tile", 2)], Checkpoint { csn: 2, handle_watermark: 3 }, None).unwrap_err().to_string(),
            "manifest writer poisoned; reopen to recover");
        drop(m);

        // Reopen selects the highest valid page (seq 1); the failed publish left
        // the newer slot torn but the root intact.
        let mut m = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(m.root().seq, 1);
        m.publish(vec![tile("two.tile", 2)], Checkpoint { csn: 2, handle_watermark: 3 }, None).unwrap();
        assert_eq!(m.root().seq, 2);
        drop(m);

        let mut m = Manifest::open(&path).unwrap().unwrap();
        m.inject_sync_error = true;
        assert!(m.publish(vec![tile("three.tile", 3)], Checkpoint { csn: 3, handle_watermark: 4 }, None).is_err());
        assert!(m.poisoned());
        drop(m);
        // The page write landed even though its sync failed; reopen takes the
        // highest valid page — the same recoverable ambiguity as an unacked
        // complete WAL record, never a silent loss of the previous root.
        let opened = Manifest::open(&path).unwrap();
        let selected = root_of(&opened);
        assert_eq!(selected.seq, 3);
        assert_eq!(selected.tiles[0].name, "three.tile");
    }

    #[test]
    fn publish_refuses_structurally_invalid_tile_lists_before_any_io() {
        // Each case needs a fresh manifest: a refused publish poisons the
        // writer even though nothing was ever written (root stays at seq 0).
        let case = |name: &str, tiles: Vec<TileRef>, checkpoint: Checkpoint| {
            let path = tmp_dir(name).join(MANIFEST_NAME);
            let mut m = Manifest::create(&path).unwrap();
            assert_eq!(m.publish(tiles, checkpoint, None).unwrap_err().kind(), ErrorKind::InvalidData);
            assert_eq!(m.root().seq, 0);
            assert_eq!(fs::metadata(&path).unwrap().len(), PAGE as u64);
        };
        case("refuse-empty", Vec::new(), Checkpoint { csn: 0, handle_watermark: 0 });
        case("refuse-cutoff", vec![tile("a.tile", 5)], Checkpoint { csn: 4, handle_watermark: 0 });
        case("refuse-order", vec![tile("b.tile", 6), tile("a.tile", 5)], Checkpoint { csn: 6, handle_watermark: 0 });
        case("refuse-overlap", vec![tile("a.tile", 5), tile("b.tile", 5)], Checkpoint { csn: 5, handle_watermark: 0 });
        case("refuse-too-many", (0..MAX_TILES as u64 + 1).map(|i| tile(&format!("t{i}.tile"), i + 1)).collect::<Vec<_>>(), Checkpoint { csn: MAX_TILES as u64 + 1, handle_watermark: 0 });
    }
}
