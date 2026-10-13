//! Minimal durable A/B manifest root for published tiles. The root records an
//! **ordered list of active tiles** (ascending, strictly increasing cutoffs, so
//! their CSN ranges never overlap) plus the **current WAL segment** reference
//! once rotation has happened: records at or below the segment's start CSN are
//! disposable (a published tile covers them), so the named segment file holds
//! only the live suffix and the root must name it — otherwise recovery could
//! not find the post-rotation WAL at all.
//!
//! Layout. The active-tile list is no longer bounded by a single 4 KiB page;
//! [`MAX_TILES`] is now a policy constant and the root chains continuation
//! pages when the records do not fit inline.
//!
//! ```text
//! offset 0                         root slot A   (PAGE bytes)
//! offset PAGE                      root slot B   (PAGE bytes)
//! offset 2*PAGE                    slot A chain region (MAX_CHAIN_PAGES * PAGE)
//! offset (2+MAX_CHAIN_PAGES)*PAGE  slot B chain region (MAX_CHAIN_PAGES * PAGE)
//! ```
//!
//! The two root slots keep the historical A/B role and frame:
//! `[seq u64 LE][crc32c(payload) u32 LE][payload PAGE-12]`, highest valid seq
//! wins. The payload is one of two forms, told apart by its first `u32`:
//!
//! - **inline** (byte-compatible with the original single-page manifest): the
//!   `u32` is the tile count, followed by the records, the 16-byte checkpoint
//!   and the optional segment reference, zero-padded. Any historical file
//!   therefore still opens as long as its list still fits one payload.
//! - **chained**: the `u32` is [`CHAIN_MARKER`] (`u32::MAX`), followed by
//!   `total_tiles u32`, `chain_pages u32`, `chain_start u32`, the checkpoint and
//!   the optional segment reference. All tile records live in continuation
//!   pages in the owning slot's chain region, starting at page `chain_start`.
//!
//! Each continuation page is framed and CRC-protected:
//! `[seq u64 LE][crc32c(bytes 12..) u32 LE][page_index u32][page_count u32][record_count u32][records...]`,
//! zero-padded. Together the fields make every malformed shape detectable:
//! `seq` must equal the owning root's seq (a page from another root's chain is
//! rejected), `page_index` must be its position, `page_count` must match the
//! root descriptor, and the sum of `record_count` must equal `total_tiles`.
//! There is no next-pointer: the chain is the fixed `chain_start..chain_start +
//! chain_pages` window of the slot's region, so it cannot loop or grow
//! unboundedly. `chain_pages` is bounded by [`MAX_CHAIN_PAGES`], and the
//! compile-time assertion below proves [`MAX_TILES`] fits that bound.
//!
//! Publication protocol (SS-26 discipline): write the *inactive* slot's chain
//! pages (when any) and then the inactive root page at `root.seq + 1` →
//! `fdatasync` the manifest file → `fsync` the parent directory → flip the
//! in-memory root. One file sync commits the chain and its referencing root; a
//! crash before it leaves a torn root whose CRC fails, and a root whose chain
//! is not durable is read as invalid, so the other slot still wins. The caller
//! must already have synced and verified the tile file (including its directory
//! entry) before publishing a reference to it. A slot's chain region may be
//! overwritten when that slot is reused: it then holds the seq-2 root, which A/B
//! selection never needs once the other slot's seq-1 page is valid.
//!
//! Validity rules:
//! - A page with a CRC mismatch, a short write (file ends mid-page), or an
//!   absent slot is **invalid**: reopening selects the highest *valid* seq page
//!   instead.
//! - A CRC-valid inline page whose payload is structurally impossible (zero
//!   tiles at `seq > 0`, a non-plain or oversized filename, an out-of-bounds
//!   count, a duplicated name, unparsed trailing bytes) is **corruption**: open
//!   fails closed with an error rather than silently falling back to the older
//!   page.
//! - A chained page whose descriptor or any link is malformed — bad link CRC, a
//!   bound-disagreeing page count, a record-count mismatch, a foreign sequence,
//!   a truncated chain, a duplicated name — is **invalid**, so the other valid
//!   A/B page can still win; if neither page is valid, open fails closed.
//! - Two valid pages with equal seq are ambiguous and fail closed.
//! - A missing manifest file is the empty initial state (no checkpoint yet); a
//!   file shorter than one page or longer than `2 + 2*MAX_CHAIN_PAGES` pages is
//!   corruption. Reopen after any manifest I/O error, never retry in place: any
//!   publish I/O error poisons the writer until a fresh [`Manifest::open`]
//!   selects the highest valid page.

use std::collections::HashSet;
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

/// Bytes one tile record costs independently of the name: `name_len u32` +
/// `cutoff u64` + `digest [u8; 32]`.
const RECORD_FIXED: usize = 4 + 8 + 32;
/// Continuation-page frame before the records: `seq u64` + `crc32c u32` +
/// `page_index u32` + `page_count u32` + `record_count u32`.
const CHAIN_HEADER: usize = 24;
const CHAIN_PAYLOAD: usize = PAGE - CHAIN_HEADER;
/// Worst-case records one continuation page holds, i.e. all [`MAX_NAME`]-long.
/// This is the conservative per-page capacity the `MAX_TILES` self-consistency
/// assertion uses.
const CHAIN_RECORDS_PER_PAGE: usize = CHAIN_PAYLOAD / (RECORD_FIXED + MAX_NAME);
/// Most records a continuation page can ever hold (one-byte names); the
/// decoder's sanity bound on a page's `record_count`.
const CHAIN_MAX_RECORDS: usize = CHAIN_PAYLOAD / (RECORD_FIXED + 1);

/// First `u32` of a root payload in chained form. Inline counts can never reach
/// it because an inline payload is at most one page.
const CHAIN_MARKER: u32 = u32::MAX;

/// The active-tile bound is a **policy constant**, not a page-capacity
/// artefact. The manifest chains continuation pages, so this can be raised
/// without touching the on-disk root protocol; it is bounded by the compile-time
/// assertion below so a change to any page/frame field size is a build failure,
/// not a silently torn chain. A publish beyond it is refused as `InvalidInput`;
/// a CRC-valid page claiming more is corruption.
pub(crate) const MAX_TILES: usize = 1024;

/// Hard cap on continuation pages per root slot. This is the chain's termination
/// bound: neither `chain_pages` nor `chain_start + chain_pages` may exceed it,
/// and the file may not be longer than `2 + 2*MAX_CHAIN_PAGES` pages.
const MAX_CHAIN_PAGES: usize = 96;
const MANIFEST_MAX_BYTES: u64 = (2 + 2 * MAX_CHAIN_PAGES) as u64 * PAGE as u64;

// The chained root descriptor (marker/total/chain_pages/chain_start + checkpoint
// + the longest legal segment reference) must fit one payload.
const _: () = assert!(
    4 + 4 + 4 + 4 + 16 + (4 + MAX_NAME + 8) <= PAYLOAD,
    "the chained root descriptor plus checkpoint/segment must fit one payload"
);
// MAX_TILES must be reachable inside the chosen chain bound; failing this is a
// compile error, so the policy constant can never silently exceed the layout.
const _: () = assert!(
    MAX_TILES <= MAX_CHAIN_PAGES * CHAIN_RECORDS_PER_PAGE,
    "MAX_TILES must fit the bounded continuation-page chain"
);

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

/// Absolute byte offset of the `index`-th continuation page of `slot`'s chain
/// region. `slot` is 0 or 1; the regions are disjoint, so reusing a slot never
/// overwrites the other slot's live chain.
fn chain_region_offset(slot: usize, index: usize) -> u64 {
    ((2 + slot * MAX_CHAIN_PAGES + index) * PAGE) as u64
}

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

/// Active tiles must be non-decreasing by cutoff. A region-partitioned publish
/// (slice 24) emits one tile per `(entity, key family)` region that SHARE the
/// publish cutoff, so the historical strictly-increasing rule is relaxed to
/// non-decreasing; the engine owns the finer `(cutoff, region)` strict ordering
/// and enforces it in `open_impl` by reading each tile's own region. On-disk
/// format is unchanged.
fn cutoffs_non_decreasing(tiles: &[TileRef]) -> bool {
    tiles.windows(2).all(|w| w[0].cutoff <= w[1].cutoff)
}

/// Names a manifest entry may never take: they would collide with the manifest
/// itself or the conventional WAL file in the same directory.
pub(crate) fn valid_tile_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name != "." && name != ".."
        && name != MANIFEST_NAME && name != WAL_NAME
        && !name.bytes().any(|b| b == 0 || b == b'/')
}

/// Structural rules shared by the encoder and the chained decoder: at most
/// [`MAX_TILES`], non-decreasing cutoffs, plain unique names.
fn validate_tiles(tiles: &[TileRef]) -> io::Result<()> {
    if tiles.len() > MAX_TILES {
        return Err(invalid("manifest tile list exceeds the active-tile policy bound"));
    }
    if !cutoffs_non_decreasing(tiles) {
        return Err(invalid("manifest tile cutoffs are not non-decreasing"));
    }
    let mut seen = HashSet::with_capacity(tiles.len());
    for t in tiles {
        if !valid_tile_name(&t.name) {
            return Err(invalid("tile name is not a plain directory-local file"));
        }
        if !seen.insert(t.name.as_str()) {
            return Err(invalid("manifest references a tile name twice"));
        }
    }
    Ok(())
}

fn validate_root(root: &Root) -> io::Result<()> {
    if root.tiles.is_empty() && root.seq != 0 {
        return Err(invalid("empty manifest requires seq 0"));
    }
    validate_tiles(&root.tiles)
}

fn write_record(out: &mut Vec<u8>, t: &TileRef) {
    let name = t.name.as_bytes();
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name);
    out.extend_from_slice(&t.cutoff.to_le_bytes());
    out.extend_from_slice(&t.digest);
}

fn write_segment(out: &mut Vec<u8>, segment: &Option<SegmentRef>) -> io::Result<()> {
    match segment {
        None => out.extend_from_slice(&0u32.to_le_bytes()), // name_len 0 = no segment
        Some(s) => {
            if !valid_tile_name(&s.name) {
                return Err(invalid("segment name is not a plain directory-local file"));
            }
            let name = s.name.as_bytes();
            out.extend_from_slice(&(name.len() as u32).to_le_bytes());
            out.extend_from_slice(name);
            out.extend_from_slice(&s.start_csn.to_le_bytes());
        }
    }
    Ok(())
}

/// Inline payload when the whole list fits one page, `None` when it must chain.
fn encode_inline_payload(root: &Root) -> io::Result<Option<[u8; PAYLOAD]>> {
    let mut out = Vec::with_capacity(64 + root.tiles.len() * 64);
    out.extend_from_slice(&(root.tiles.len() as u32).to_le_bytes());
    for t in &root.tiles {
        write_record(&mut out, t);
    }
    out.extend_from_slice(&root.checkpoint.csn.to_le_bytes());
    out.extend_from_slice(&root.checkpoint.handle_watermark.to_le_bytes());
    write_segment(&mut out, &root.segment)?;
    if out.len() > PAYLOAD {
        return Ok(None);
    }
    let mut p = [0u8; PAYLOAD];
    p[..out.len()].copy_from_slice(&out);
    Ok(Some(p))
}

struct EncodedPage {
    page: [u8; PAGE],
    chain: Vec<[u8; PAGE]>,
}

/// Encode the root page plus every continuation page it points at.
fn encode_root_page(seq: u64, root: &Root) -> io::Result<EncodedPage> {
    validate_root(root)?;
    let (payload, chain) = match encode_inline_payload(root)? {
        Some(inline) => (inline, Vec::new()),
        None => {
            // Pack records at natural boundaries; each record is at most
            // RECORD_FIXED + MAX_NAME bytes (validated names), so it always fits
            // one page and packing terminates.
            let mut packed: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut cur: Vec<u8> = Vec::new();
            let mut count = 0u32;
            for t in &root.tiles {
                let mut rec = Vec::new();
                write_record(&mut rec, t);
                if cur.len() + rec.len() > CHAIN_PAYLOAD {
                    packed.push((count, std::mem::take(&mut cur)));
                    count = 0;
                }
                cur.extend_from_slice(&rec);
                count += 1;
            }
            if count > 0 {
                packed.push((count, cur));
            }
            let chain_pages = packed.len();
            if chain_pages == 0 || chain_pages > MAX_CHAIN_PAGES {
                return Err(invalid("chain page count outside the bounded manifest layout"));
            }
            let mut out = Vec::with_capacity(PAYLOAD);
            let chain_start = 0u32;
            out.extend_from_slice(&CHAIN_MARKER.to_le_bytes());
            out.extend_from_slice(&(root.tiles.len() as u32).to_le_bytes());
            out.extend_from_slice(&(chain_pages as u32).to_le_bytes());
            out.extend_from_slice(&chain_start.to_le_bytes());
            out.extend_from_slice(&root.checkpoint.csn.to_le_bytes());
            out.extend_from_slice(&root.checkpoint.handle_watermark.to_le_bytes());
            write_segment(&mut out, &root.segment)?;
            let mut payload = [0u8; PAYLOAD];
            payload[..out.len()].copy_from_slice(&out);
            let chain = packed
                .into_iter()
                .enumerate()
                .map(|(i, (records, bytes))| {
                    let mut page = [0u8; PAGE];
                    page[..8].copy_from_slice(&seq.to_le_bytes());
                    page[12..16].copy_from_slice(&(chain_start + i as u32).to_le_bytes());
                    page[16..20].copy_from_slice(&(chain_pages as u32).to_le_bytes());
                    page[20..24].copy_from_slice(&records.to_le_bytes());
                    page[CHAIN_HEADER..CHAIN_HEADER + bytes.len()].copy_from_slice(&bytes);
                    let crc = crc32c(&page[12..]);
                    page[8..12].copy_from_slice(&crc.to_le_bytes());
                    page
                })
                .collect();
            (payload, chain)
        }
    };
    let mut page = [0u8; PAGE];
    page[..8].copy_from_slice(&seq.to_le_bytes());
    page[8..HEADER].copy_from_slice(&crc32c(&payload).to_le_bytes());
    page[HEADER..].copy_from_slice(&payload);
    Ok(EncodedPage { page, chain })
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

fn parse_records(c: &mut Cur, count: usize) -> io::Result<Vec<TileRef>> {
    let mut tiles = Vec::with_capacity(count.min(MAX_TILES));
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
    Ok(tiles)
}

fn parse_tail(c: &mut Cur) -> io::Result<(Checkpoint, Option<SegmentRef>)> {
    let checkpoint = Checkpoint { csn: c.u64()?, handle_watermark: c.u64()? };
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
    Ok((checkpoint, segment))
}

/// Semantic checks common to both payload forms. An `Err` here means
/// CRC-valid-but-impossible bytes; the chained reader converts it to an invalid
/// page, the inline reader fails closed.
fn finish_root(seq: u64, tiles: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<Root> {
    validate_tiles(&tiles)?;
    if tiles.last().is_some_and(|t| checkpoint.csn != t.cutoff) {
        return Err(invalid("manifest checkpoint CSN disagrees with newest tile cutoff"));
    }
    if tiles.is_empty() && segment.is_some() {
        return Err(invalid("empty manifest cannot reference a WAL segment"));
    }
    if segment.as_ref().is_some_and(|s| s.start_csn > checkpoint.csn) {
        return Err(invalid("manifest segment starts above the checkpoint CSN"));
    }
    Ok(Root { seq, tiles, checkpoint, segment })
}

/// CRC-valid but structurally impossible inline payloads are corruption.
fn decode_inline_payload(seq: u64, payload: &[u8]) -> io::Result<Root> {
    let mut c = Cur { b: payload, i: 0 };
    let count = c.u32()? as usize;
    if count == 0 {
        if seq != 0 { return Err(invalid("manifest lists zero tiles at nonzero seq")); }
    } else if count > MAX_TILES {
        return Err(invalid("manifest lists more tiles than the policy bound"));
    }
    let tiles = parse_records(&mut c, count)?;
    let (checkpoint, segment) = parse_tail(&mut c)?;
    if payload[c.i..].iter().any(|&b| b != 0) {
        return Err(invalid("manifest payload has unparsed trailing bytes"));
    }
    finish_root(seq, tiles, checkpoint, segment)
}

/// A chained root page. Any failure parses as `Err`, which [`decode_page`]
/// turns into an invalid page so A/B selection can fall back; the bounds checked
/// here are what stop a chain from looping or growing past the layout.
fn decode_chained_descriptor(seq: u64, payload: &[u8]) -> io::Result<DecodedRoot> {
    let mut c = Cur { b: payload, i: 0 };
    let _marker = c.u32()?;
    let total_tiles = c.u32()?;
    let chain_pages = c.u32()?;
    let chain_start = c.u32()?;
    let (checkpoint, segment) = parse_tail(&mut c)?;
    if payload[c.i..].iter().any(|&b| b != 0) {
        return Err(invalid("manifest payload has unparsed trailing bytes"));
    }
    if total_tiles == 0 || total_tiles as usize > MAX_TILES {
        return Err(invalid("chained manifest tile count outside the policy bound"));
    }
    if chain_pages == 0 || chain_pages as usize > MAX_CHAIN_PAGES {
        return Err(invalid("chain page count outside the bounded manifest layout"));
    }
    if chain_start as usize + chain_pages as usize > MAX_CHAIN_PAGES {
        return Err(invalid("chain page range outside the bounded manifest layout"));
    }
    Ok(DecodedRoot::Chained { seq, total_tiles, chain_pages, chain_start, checkpoint, segment })
}

enum DecodedRoot {
    Inline(Root),
    Chained {
        seq: u64,
        total_tiles: u32,
        chain_pages: u32,
        chain_start: u32,
        checkpoint: Checkpoint,
        segment: Option<SegmentRef>,
    },
}

/// `Ok(None)` marks a torn/absent/invalid page that A/B selection may skip; an
/// `Err` means the bytes were CRC-valid but impossible, which must fail closed.
fn decode_page(page: &[u8; PAGE]) -> io::Result<Option<DecodedRoot>> {
    let seq = u64::from_le_bytes(page[..8].try_into().unwrap());
    let payload = &page[HEADER..];
    if crc32c(payload) != u32::from_le_bytes(page[8..HEADER].try_into().unwrap()) {
        return Ok(None);
    }
    let first = u32::from_le_bytes(payload[..4].try_into().unwrap());
    if first == CHAIN_MARKER {
        // Every chained-descriptor failure makes the page invalid, never a hard
        // corruption, so the other A/B side can still win.
        Ok(decode_chained_descriptor(seq, payload).ok())
    } else {
        decode_inline_payload(seq, payload).map(|root| Some(DecodedRoot::Inline(root)))
    }
}

/// Decode one continuation page, checking it against the owning root: CRC,
/// foreign/absent sequence, its position, the root's page count, and the record
/// count. `Ok(None)` = invalid link (the whole chained root is then invalid).
fn decode_chain_page(root_seq: u64, expect_index: u32, expect_count: u32, page: &[u8; PAGE]) -> io::Result<Option<Vec<TileRef>>> {
    let seq = u64::from_le_bytes(page[..8].try_into().unwrap());
    if crc32c(&page[12..]) != u32::from_le_bytes(page[8..12].try_into().unwrap()) {
        return Ok(None);
    }
    let index = u32::from_le_bytes(page[12..16].try_into().unwrap());
    let page_count = u32::from_le_bytes(page[16..20].try_into().unwrap());
    let record_count = u32::from_le_bytes(page[20..24].try_into().unwrap());
    if seq != root_seq || index != expect_index || page_count != expect_count {
        return Ok(None);
    }
    if record_count == 0 || record_count as usize > CHAIN_MAX_RECORDS {
        return Ok(None);
    }
    let mut c = Cur { b: &page[CHAIN_HEADER..], i: 0 };
    let tiles = match parse_records(&mut c, record_count as usize) {
        Ok(tiles) => tiles,
        Err(_) => return Ok(None),
    };
    if page[CHAIN_HEADER + c.i..].iter().any(|&b| b != 0) {
        return Ok(None);
    }
    Ok(Some(tiles))
}

/// Read and assemble the root in `slot`, following its chain when present.
/// `len` is the file length; a chain that runs past it is invalid (truncated).
fn read_slot_root(file: &File, len: u64, slot: usize) -> io::Result<Option<Root>> {
    let mut raw = [0u8; PAGE];
    file.read_exact_at(&mut raw, (slot * PAGE) as u64)?;
    match decode_page(&raw)? {
        None => Ok(None),
        Some(DecodedRoot::Inline(root)) => Ok(Some(root)),
        Some(DecodedRoot::Chained { seq, total_tiles, chain_pages, chain_start, checkpoint, segment }) => {
            let base = chain_region_offset(slot, chain_start as usize);
            if base + chain_pages as u64 * PAGE as u64 > len {
                return Ok(None); // truncated chain: invalid page, not corruption
            }
            let mut tiles = Vec::with_capacity(total_tiles as usize);
            for i in 0..chain_pages {
                let mut cp = [0u8; PAGE];
                file.read_exact_at(&mut cp, base + i as u64 * PAGE as u64)?;
                match decode_chain_page(seq, chain_start + i, chain_pages, &cp)? {
                    Some(records) => tiles.extend(records),
                    None => return Ok(None),
                }
            }
            if tiles.len() as u32 != total_tiles {
                return Ok(None); // page count disagrees with the record count
            }
            match finish_root(seq, tiles, checkpoint, segment) {
                Ok(root) => Ok(Some(root)),
                Err(_) => Ok(None),
            }
        }
    }
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
        if len > MANIFEST_MAX_BYTES { return Err(invalid("manifest file larger than the chain-bounded maximum")); }
        let page_a = read_slot_root(&file, len, 0)?;
        let page_b = if len >= 2 * PAGE as u64 { read_slot_root(&file, len, 1)? } else { None };
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
        let encoded = encode_root_page(0, &initial)?;
        file.write_all_at(&encoded.page, 0)?;
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
        let result = self.publish_inner(tiles, checkpoint, segment, true);
        if result.is_err() { self.poisoned = true; }
        result
    }

    /// [`Manifest::publish`] without the file and parent-directory syncs. The
    /// in-memory root still flips, so a later durable publish carries this page
    /// with it; until then a crash can revert to the previous durable root (whose
    /// tiles are all still present), which is the documented lazy-compaction
    /// trade: reorganisation may be lost, never data.
    pub(crate) fn publish_lazy(&mut self, tiles: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        let result = self.publish_inner(tiles, checkpoint, segment, false);
        if result.is_err() { self.poisoned = true; }
        result
    }

    fn publish_inner(&mut self, tiles: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>, durable: bool) -> io::Result<()> {
        let seq = self.root.seq.checked_add(1).ok_or_else(|| invalid("manifest seq exhausted"))?;
        if tiles.is_empty() || tiles.len() > MAX_TILES {
            return Err(invalid("publish requires one to MAX_TILES active tiles"));
        }
        if !cutoffs_non_decreasing(&tiles) {
            return Err(invalid("manifest tile cutoffs are not non-decreasing"));
        }
        if checkpoint.csn != tiles.last().unwrap().cutoff {
            return Err(invalid("manifest checkpoint CSN disagrees with newest tile cutoff"));
        }
        let root = Root { seq, tiles, checkpoint, segment };
        let encoded = encode_root_page(seq, &root)?;
        let slot = 1 - self.slot;
        #[cfg(test)]
        if self.inject_page_write_error {
            self.inject_page_write_error = false;
            return Err(io::Error::new(io::ErrorKind::Other, "injected manifest page write failure"));
        }
        // Chain pages first, then the root page that points at them; the single
        // file sync below commits both before the A/B flip.
        for (i, chain_page) in encoded.chain.iter().enumerate() {
            self.file.write_all_at(chain_page, chain_region_offset(slot, i))?;
        }
        self.file.write_all_at(&encoded.page, (slot * PAGE) as u64)?;
        #[cfg(test)]
        if self.inject_sync_error {
            self.inject_sync_error = false;
            return Err(io::Error::new(io::ErrorKind::Other, "injected manifest sync failure"));
        }
        if durable {
            self.file.sync_data()?;
            File::open(Self::parent(&self.path))?.sync_all()?;
        }
        self.root = root;
        self.slot = slot;
        Ok(())
    }

    /// Range-preserving replacement: atomically retire a contiguous run of the
    /// current active list and install one `replacement` tile whose cutoff is the
    /// run's newest cutoff. The retired CSN range is `(previous cutoff, run newest
    /// cutoff]`; carrying that same cutoff into the replacement keeps the
    /// resulting list strictly increasing (disjoint, non-overlapping ranges), so
    /// `open_discover`/`open_impl` accept it unchanged. `retire` names the run's
    /// tiles in list order and must match exactly one contiguous window.
    ///
    /// Fails closed, poisoning the writer like [`Manifest::publish`], if the
    /// retired set is not a contiguous run of the current list, if the
    /// replacement's cutoff is not that run's newest cutoff, or if the resulting
    /// list would not be strictly increasing. The durable protocol is unchanged:
    /// inactive page at `seq + 1` → sync manifest file → sync parent dir → flip.
    pub(crate) fn publish_replacing(
        &mut self,
        retire: &[&str],
        replacement: TileRef,
        checkpoint: Checkpoint,
        segment: Option<SegmentRef>,
    ) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        let result = self.publish_replacing_inner(retire, replacement, checkpoint, segment, true);
        if result.is_err() { self.poisoned = true; }
        result
    }

    /// [`Manifest::publish_replacing`] without the file and parent-directory
    /// syncs; see [`Manifest::publish_lazy`].
    pub(crate) fn publish_replacing_lazy(
        &mut self,
        retire: &[&str],
        replacement: TileRef,
        checkpoint: Checkpoint,
        segment: Option<SegmentRef>,
    ) -> io::Result<()> {
        if self.poisoned {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        let result = self.publish_replacing_inner(retire, replacement, checkpoint, segment, false);
        if result.is_err() { self.poisoned = true; }
        result
    }

    fn publish_replacing_inner(
        &mut self,
        retire: &[&str],
        replacement: TileRef,
        checkpoint: Checkpoint,
        segment: Option<SegmentRef>,
        durable: bool,
    ) -> io::Result<()> {
        if retire.is_empty() {
            return Err(invalid("range replacement requires a non-empty retired run"));
        }
        if retire.len() > self.root.tiles.len() {
            return Err(invalid("retired set is not a contiguous run of the active tile list"));
        }
        // Locate the run as exactly one contiguous window of the current list,
        // matched by name in order; an absent or ambiguous window is corruption
        // of the caller's intent, not a list to guess at.
        let mut start = None;
        for i in 0..=self.root.tiles.len().saturating_sub(retire.len()) {
            let window = &self.root.tiles[i..i + retire.len()];
            if window.iter().zip(retire).all(|(t, name)| t.name.as_str() == *name) {
                if start.is_some() {
                    return Err(invalid("retired run matches the active tile list ambiguously"));
                }
                start = Some(i);
            }
        }
        let start = start.ok_or_else(|| invalid("retired set is not a contiguous run of the active tile list"))?;
        let end = start + retire.len();
        if self.root.tiles[end - 1].cutoff != replacement.cutoff {
            return Err(invalid("replacement cutoff is not the retired run's newest cutoff"));
        }
        let mut tiles: Vec<TileRef> = Vec::with_capacity(self.root.tiles.len() - retire.len() + 1);
        tiles.extend_from_slice(&self.root.tiles[..start]);
        let replacement_name = replacement.name.clone();
        tiles.push(replacement);
        tiles.extend_from_slice(&self.root.tiles[end..]);
        if !cutoffs_non_decreasing(&tiles) {
            return Err(invalid("replacement would make tile cutoffs decrease"));
        }
        // Defense in depth: the replacement name must not collide with a tile that
        // survives the replacement. The caller checks this against the live tile
        // list, but a duplicate-name root would otherwise only be rejected at
        // recovery, so the operation refuses it itself.
        if tiles.iter().filter(|t| t.name == replacement_name).count() > 1 {
            return Err(invalid("replacement name collides with a surviving active tile"));
        }
        self.publish_inner(tiles, checkpoint, segment, durable)
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

    fn many_tiles(n: usize) -> Vec<TileRef> {
        (0..n).map(|i| tile(&format!("t{i:03}.tile"), i as u64 + 1)).collect()
    }

    fn empty_root() -> Root {
        Root { seq: 0, tiles: Vec::new(), checkpoint: Checkpoint { csn: 0, handle_watermark: 0 }, segment: None }
    }

    fn root_of(m: &Option<Manifest>) -> &Root { &m.as_ref().unwrap().root() }

    fn inline_root(page: &[u8; PAGE]) -> Option<Root> {
        match decode_page(page).unwrap()? {
            DecodedRoot::Inline(root) => Some(root),
            DecodedRoot::Chained { .. } => None,
        }
    }

    fn encode_page(seq: u64, root: &Root) -> io::Result<[u8; PAGE]> {
        Ok(encode_root_page(seq, root)?.page)
    }

    /// Write a manifest whose highest page is a chained root in the slot implied
    /// by `seq` parity (slot A also gets the valid seq-0 page so A/B fallback is
    /// observable). Returns the slot and the number of chain pages.
    fn write_chained_manifest(path: &Path, seq: u64, root: &Root) -> (usize, usize) {
        let encoded = encode_root_page(seq, root).unwrap();
        assert!(!encoded.chain.is_empty(), "test root must chain");
        let slot = (seq % 2) as usize;
        let mut bytes = vec![0u8; MANIFEST_MAX_BYTES as usize];
        if slot != 0 {
            bytes[..PAGE].copy_from_slice(&encode_root_page(0, &empty_root()).unwrap().page);
        }
        bytes[slot * PAGE..slot * PAGE + PAGE].copy_from_slice(&encoded.page);
        for (i, chain_page) in encoded.chain.iter().enumerate() {
            let off = chain_region_offset(slot, i) as usize;
            bytes[off..off + PAGE].copy_from_slice(chain_page);
        }
        fs::write(path, &bytes).unwrap();
        (slot, encoded.chain.len())
    }

    #[test]
    fn page_roundtrips_tiles_and_checkpoint() {
        let root = Root { seq: 9, tiles: vec![tile("a.tile", 40), tile("b.tile", 42)], checkpoint: Checkpoint { csn: 42, handle_watermark: 43 }, segment: None };
        let page = encode_page(9, &root).unwrap();
        let decoded = inline_root(&page).unwrap();
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
        assert_eq!(decode_inline_payload(1, &p).unwrap_err().kind(), ErrorKind::InvalidData);
        let p = [0u8; PAYLOAD];
        assert_eq!(decode_inline_payload(1, &p).unwrap_err().to_string(), "manifest lists zero tiles at nonzero seq");
        let mut p = [0u8; PAYLOAD];
        p[24] = 9; // unparsed trailing byte after the checkpoint fields
        assert_eq!(decode_inline_payload(0, &p).unwrap_err().kind(), ErrorKind::InvalidData);
        let mut p = [0u8; PAYLOAD];
        p[..4].copy_from_slice(&1u32.to_le_bytes());
        p[4..8].copy_from_slice(&5u32.to_le_bytes());
        p[8..13].copy_from_slice(b"a/b\0c");
        assert_eq!(decode_inline_payload(1, &p).unwrap_err().kind(), ErrorKind::InvalidData);

        // CRC-valid but structurally impossible tile lists are corruption:
        // decreasing cutoffs, a checkpoint that is not the newest cutoff, or a
        // duplicated tile name. Equal adjacent cutoffs are NOT corruption since
        // slice 24 (region-partitioned tiles share the publish cutoff); the
        // engine enforces `(cutoff, region)` strictness at open.
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
        assert!(decode_inline_payload(3, &hand_built(&[(b"b.tile".to_vec(), 3), (b"a.tile".to_vec(), 3)], (3, 3))).is_ok(),
            "equal cutoffs are legal: region-partitioned tiles share a cutoff");
        assert_eq!(decode_inline_payload(3, &hand_built(&[(b"b.tile".to_vec(), 3), (b"a.tile".to_vec(), 2)], (2, 3))).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(decode_inline_payload(3, &hand_built(&[(b"a.tile".to_vec(), 2), (b"b.tile".to_vec(), 3)], (2, 3))).unwrap_err().to_string(),
            "manifest checkpoint CSN disagrees with newest tile cutoff");
        assert_eq!(decode_inline_payload(3, &hand_built(&[(b"dup.tile".to_vec(), 2), (b"dup.tile".to_vec(), 3)], (3, 3))).unwrap_err().to_string(),
            "manifest references a tile name twice");
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
        let decoded = inline_root(&encode_page(7, &root).unwrap()).unwrap();
        assert_eq!(decoded.segment.as_ref().unwrap(), &seg("data-4.wal", 4));
        // A root without rotation decodes with no segment reference.
        let plain = Root { segment: None, ..root.clone() };
        let decoded = inline_root(&encode_page(7, &plain).unwrap()).unwrap();
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
        assert_eq!(decode_inline_payload(0, &p).unwrap_err().to_string(), "empty manifest cannot reference a WAL segment");
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
        assert_eq!(decode_inline_payload(7, &p).unwrap_err().to_string(), "manifest segment name length out of bounds");
        let p = one_tile_seg(&{
            let mut t = 4u32.to_le_bytes().to_vec();
            t.extend_from_slice(b"a/b\0c");
            t.extend_from_slice(&4u64.to_le_bytes());
            t
        });
        assert_eq!(decode_inline_payload(7, &p).unwrap_err().to_string(), "manifest segment name is not a plain directory-local file");
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
    fn chained_root_roundtrips_and_reopens() {
        // A one-tile root stays inline and round-trips through a real file.
        let dir = tmp_dir("chain-single");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(vec![tile("solo.tile", 1)], Checkpoint { csn: 1, handle_watermark: 2 }, None).unwrap();
        assert!(encode_root_page(m.root().seq, m.root()).unwrap().chain.is_empty());
        drop(m);
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).tiles.len(), 1);

        // 130 records of 53 bytes exceed one payload and split over two chain
        // pages; reopening must reassemble the whole list (and re-verify the
        // checkpoint binding).
        let dir = tmp_dir("chain-many");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        let tiles = many_tiles(130);
        m.publish(tiles, Checkpoint { csn: 130, handle_watermark: 131 }, None).unwrap();
        let encoded = encode_root_page(1, m.root()).unwrap();
        assert_eq!(encoded.chain.len(), 2, "130 short-name records need two continuation pages");
        // seq 1 lives in slot B; the root payload carries the chain marker.
        let slot = 1usize;
        let root_bytes = fs::read(&path).unwrap();
        let marker = u32::from_le_bytes(root_bytes[slot * PAGE + HEADER..slot * PAGE + HEADER + 4].try_into().unwrap());
        assert_eq!(marker, CHAIN_MARKER);
        drop(m);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().seq, 1);
        assert_eq!(reopened.root().tiles.len(), 130);
        for (i, t) in reopened.root().tiles.iter().enumerate() {
            assert_eq!(t.name, format!("t{i:03}.tile"));
            assert_eq!(t.cutoff, i as u64 + 1);
        }
        assert_eq!(reopened.root().checkpoint, Checkpoint { csn: 130, handle_watermark: 131 });
    }

    #[test]
    fn inline_root_files_still_open_byte_compatibly() {
        // Assemble the historical single-page layout by hand: a two-page file
        // whose slot B payload is exactly `count + record + checkpoint + no
        // segment`.
        let dir = tmp_dir("byte-compat");
        let path = dir.join(MANIFEST_NAME);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&encode_root_page(0, &empty_root()).unwrap().page);
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u32.to_le_bytes());
        payload.extend_from_slice(&8u32.to_le_bytes());
        payload.extend_from_slice(b"old.tile");
        payload.extend_from_slice(&7u64.to_le_bytes());
        payload.extend_from_slice(&[3u8; 32]);
        payload.extend_from_slice(&7u64.to_le_bytes()); // checkpoint csn
        payload.extend_from_slice(&9u64.to_le_bytes()); // handle watermark
        payload.extend_from_slice(&0u32.to_le_bytes()); // no segment
        let mut page = [0u8; PAGE];
        page[..8].copy_from_slice(&1u64.to_le_bytes());
        page[HEADER..HEADER + payload.len()].copy_from_slice(&payload);
        let crc = crc32c(&page[HEADER..]);
        page[8..HEADER].copy_from_slice(&crc.to_le_bytes());
        bytes.extend_from_slice(&page);
        assert_eq!(bytes.len(), 2 * PAGE, "historical files are exactly two pages");
        fs::write(&path, &bytes).unwrap();

        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().seq, 1);
        assert_eq!(reopened.root().tiles.len(), 1);
        assert_eq!(reopened.root().tiles[0].name, "old.tile");
        assert_eq!(reopened.root().tiles[0].digest, [3u8; 32]);
        assert_eq!(reopened.root().checkpoint, Checkpoint { csn: 7, handle_watermark: 9 });
    }

    #[test]
    fn max_tiles_round_trips_within_the_chain_bound() {
        // The compile-time self-consistency assertion made concrete: MAX_TILES
        // worst-case (255-byte) names must encode into at most MAX_CHAIN_PAGES
        // continuation pages, and the whole list must reassemble on reopen.
        let tiles: Vec<TileRef> = (0..MAX_TILES)
            .map(|i| tile(&format!("{i:0>255}"), i as u64 + 1))
            .collect();
        let root = Root {
            seq: 1,
            tiles,
            checkpoint: Checkpoint { csn: MAX_TILES as u64, handle_watermark: MAX_TILES as u64 + 1 },
            segment: None,
        };
        let encoded = encode_root_page(1, &root).unwrap();
        assert!(encoded.chain.len() >= 2 && encoded.chain.len() <= MAX_CHAIN_PAGES,
            "{} records fit the bounded chain in {} pages", MAX_TILES, encoded.chain.len());
        let dir = tmp_dir("chain-max");
        let path = dir.join(MANIFEST_NAME);
        write_chained_manifest(&path, 1, &root);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().tiles.len(), MAX_TILES);
        assert_eq!(reopened.root().tiles[0].name.len(), MAX_NAME);
        assert_eq!(reopened.root().tiles[MAX_TILES - 1].cutoff, MAX_TILES as u64);
    }

    #[test]
    fn chain_failures_make_that_page_invalid_and_fail_closed_without_fallback() {
        let chained = Root { seq: 1, tiles: many_tiles(130), checkpoint: Checkpoint { csn: 130, handle_watermark: 131 }, segment: None };
        let rebuild = |name: &str| {
            let dir = tmp_dir(name);
            let path = dir.join(MANIFEST_NAME);
            let (slot, pages) = write_chained_manifest(&path, 1, &chained);
            (dir, path, slot, pages)
        };

        // (a) A bad CRC on one link invalidates the whole chained root, so the
        // seq-0 page in the other slot still wins; with no other page, open fails.
        let (_dir, path, slot, _pages) = rebuild("chain-crc");
        let mut bytes = fs::read(&path).unwrap();
        let link = chain_region_offset(slot, 1) as usize;
        bytes[link + CHAIN_HEADER] ^= 0x80;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).seq, 0, "broken link falls back to the other valid page");
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        // (b) A truncated chain (region beyond EOF) is an invalid page.
        let (_dir, path, _slot, _pages) = rebuild("chain-trunc");
        let f = fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(2 * PAGE as u64).unwrap();
        drop(f);
        assert_eq!(root_of(&Manifest::open(&path).unwrap()).seq, 0, "truncated chain falls back");
        let mut bytes = fs::read(&path).unwrap();
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        // (c) A chain_pages bound that exceeds MAX_CHAIN_PAGES: the descriptor is
        // rewritten and re-CRC'd so only the bound can fire.
        let (_dir, path, slot, _pages) = rebuild("chain-bound");
        let mut bytes = fs::read(&path).unwrap();
        let off = slot * PAGE + HEADER;
        bytes[off + 8..off + 12].copy_from_slice(&(MAX_CHAIN_PAGES as u32 + 1).to_le_bytes());
        let crc = crc32c(&bytes[off..off + PAYLOAD]);
        bytes[slot * PAGE + 8..slot * PAGE + 12].copy_from_slice(&crc.to_le_bytes());
        let mut page = [0u8; PAGE];
        page.copy_from_slice(&bytes[slot * PAGE..slot * PAGE + PAGE]);
        assert!(decode_page(&page).unwrap().is_none(), "a bound-disagreeing descriptor is an invalid page");
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        // (d) A link whose page_count disagrees with the root descriptor.
        let (_dir, path, slot, pages) = rebuild("chain-count");
        let mut bytes = fs::read(&path).unwrap();
        let link = chain_region_offset(slot, 0) as usize;
        bytes[link + 16..link + 20].copy_from_slice(&(pages as u32 + 1).to_le_bytes());
        let crc = crc32c(&bytes[link + 12..link + PAGE]);
        bytes[link + 8..link + 12].copy_from_slice(&crc.to_le_bytes());
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        // (e) A link carrying a foreign sequence is rejected.
        let (_dir, path, slot, _pages) = rebuild("chain-seq");
        let mut bytes = fs::read(&path).unwrap();
        let link = chain_region_offset(slot, 0) as usize;
        bytes[link..link + 8].copy_from_slice(&99u64.to_le_bytes());
        bytes[..PAGE].fill(0);
        fs::write(&path, &bytes).unwrap();
        assert_eq!(Manifest::open(&path).unwrap_err().kind(), ErrorKind::InvalidData);

        // (f) A duplicated name in the assembled record list is rejected.
        let dup = vec![tile("dup.tile", 1), tile("dup.tile", 2)];
        assert_eq!(finish_root(1, dup, Checkpoint { csn: 2, handle_watermark: 2 }, None).unwrap_err().to_string(),
            "manifest references a tile name twice");
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
    fn chained_publish_write_failure_poisons_and_previous_page_wins() {
        let dir = tmp_dir("chain-poison");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        // First chained publish succeeds (seq 1 >= one chain page).
        m.publish(many_tiles(130), Checkpoint { csn: 130, handle_watermark: 131 }, None).unwrap();
        assert_eq!(m.root().seq, 1);
        assert!(!encode_root_page(1, m.root()).unwrap().chain.is_empty());
        // A later chained publish that fails while writing poisons the writer; the
        // durable seq-1 page (chain included) still wins on reopen.
        m.inject_page_write_error = true;
        let before = m.root().clone();
        assert!(m.publish(many_tiles(200), Checkpoint { csn: 200, handle_watermark: 201 }, None).is_err());
        assert!(m.poisoned());
        assert_eq!(m.root().seq, before.seq, "failed publish changed no durable state");
        drop(m);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().seq, 1);
        assert_eq!(reopened.root().tiles.len(), 130);
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
        case("refuse-decreasing", vec![tile("a.tile", 5), tile("b.tile", 4)], Checkpoint { csn: 5, handle_watermark: 0 });
        case("refuse-duplicate", vec![tile("a.tile", 5), tile("a.tile", 6)], Checkpoint { csn: 6, handle_watermark: 0 });
        case("refuse-too-many", many_tiles(MAX_TILES + 1), Checkpoint { csn: MAX_TILES as u64 + 1, handle_watermark: 0 });
        // Equal adjacent cutoffs are accepted since slice 24: a region-partitioned
        // publish emits several tiles sharing the publish cutoff.
        let path = tmp_dir("accept-equal").join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(vec![tile("a.tile", 5), tile("b.tile", 5)], Checkpoint { csn: 5, handle_watermark: 0 }, None).unwrap();
        assert_eq!(m.root().tiles.len(), 2);
    }

    #[test]
    fn publish_replacing_retires_a_contiguous_run_and_fails_closed() {
        let dir = tmp_dir("replace");
        let path = dir.join(MANIFEST_NAME);
        let mut m = Manifest::create(&path).unwrap();
        m.publish(vec![tile("a.tile", 1), tile("b.tile", 2), tile("c.tile", 3)],
            Checkpoint { csn: 3, handle_watermark: 4 }, None).unwrap();
        let seq_before = m.root().seq;
        // Retire the oldest two, installing a replacement bound to the run's
        // newest cutoff (2); the surviving tile at cutoff 3 keeps the list
        // strictly increasing and the checkpoint untouched.
        m.publish_replacing(&["a.tile", "b.tile"], tile("m.tile", 2),
            Checkpoint { csn: 3, handle_watermark: 4 }, None).unwrap();
        assert_eq!(m.root().seq, seq_before + 1, "range replacement follows the seq+1 publication protocol");
        let names: Vec<&str> = m.root().tiles.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["m.tile", "c.tile"]);
        assert_eq!(m.root().tiles[0].cutoff, 2);
        assert_eq!(m.root().tiles[0].digest, [7u8; 32]);
        drop(m);
        let reopened = Manifest::open(&path).unwrap().unwrap();
        assert_eq!(reopened.root().tiles[0].name, "m.tile");
        assert_eq!(reopened.root().tiles[1].name, "c.tile");
        assert_eq!(reopened.root().checkpoint.csn, 3);

        // Fail-closed cases: a non-contiguous retired set, a replacement whose
        // cutoff is not the run's newest, and a name absent from the list. Each
        // needs a fresh manifest because a refused publish poisons the writer.
        let case = |name: &str, retire: &[&str], repl: TileRef| {
            let path = tmp_dir(name).join(MANIFEST_NAME);
            let mut m = Manifest::create(&path).unwrap();
            m.publish(vec![tile("a.tile", 1), tile("b.tile", 2), tile("c.tile", 3)],
                Checkpoint { csn: 3, handle_watermark: 4 }, None).unwrap();
            let seq = m.root().seq;
            assert_eq!(m.publish_replacing(retire, repl, Checkpoint { csn: 3, handle_watermark: 4 }, None).unwrap_err().kind(), ErrorKind::InvalidData);
            assert!(m.poisoned());
            assert_eq!(m.root().seq, seq, "refused replacement changed no durable state");
        };
        case("replace-gap", &["a.tile", "c.tile"], tile("m.tile", 3));
        case("replace-wrong-cutoff", &["a.tile", "b.tile"], tile("m.tile", 3));
        case("replace-absent", &["zz.tile"], tile("m.tile", 1));
        case("replace-too-long", &["a.tile", "b.tile", "c.tile", "d.tile"], tile("m.tile", 3));
        // Defense in depth: the replacement must not take the name of a tile that
        // survives (a duplicate-name root would otherwise only be caught at
        // recovery). Retiring a+b and installing "c.tile" would collide with the
        // surviving tile c.
        case("replace-name-collision", &["a.tile", "b.tile"], tile("c.tile", 2));
    }
}
