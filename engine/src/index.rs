//! Derived secondary access path (WP-M): an off-request-path `order_no ->
//! StockMovement handle` index, built from the merged (memtable + tile) view of
//! the `P|StockMovement` rows and used by the authoritative read path only as a
//! HINT.
//!
//! Correctness is by construction: the index is never on the correctness path.
//! Every hit is cross-checked against the document's own `order_no` field, and a
//! miss / mismatch / absent / stale / corrupt index falls back to the existing
//! full-entity scan. The index only short-circuits a positive hit; it can never
//! turn a "not found" into a wrong answer.
//!
//! It reuses the `U` (unique-index) key spelling — `unique_key(entity, field,
//! value, handle, csn)` / value = `handle.to_be_bytes()` — and the existing tile
//! writer/reader, so no new keyspace family and no change to recovery ordering.
//! The tile lives OUTSIDE the engine's main `self.tiles` list (which is
//! WAL-projection-verified at recovery), as a sidecar optimization that is
//! rebuilt at every open by the driver when the policy asks for it.

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::engine::Engine;
use crate::keys;
use crate::tile::Tile;

/// The entity and field this secondary path serves. Only the movement scan
/// shape has a full-entity scan with a filterable predicate today.
pub const INDEX_ENTITY: &[u8] = b"StockMovement";
pub const INDEX_FIELD: &[u8] = b"order_no";

/// A built, durable `order_no -> handle` index tile.
pub struct OrderNoIndex {
    tile: Arc<Tile>,
    key_encoding: keys::KeyEncoding,
    /// Highest movement CSN the index covers (the max CSN of an indexed entry).
    /// Observational only: a lookup miss always falls back to the scan, so this
    /// bound is never read on a correctness path.
    pub coverage_csn: u64,
    /// Number of indexed entries.
    pub entries: u64,
}

/// The product of an off-path index build, ready to be adopted (or discarded).
pub struct OrderNoIndexBuild {
    pub name: String,
    pub index: OrderNoIndex,
    /// Durable output bytes the index tile occupies on disk (charged to the
    /// region's reorganisation term by the driver).
    pub output_bytes: u64,
}

/// Evidence, read from engine state outside the build's scan, that the index's
/// region holds at least one row.
///
/// The build cannot answer this itself: the scan reports how many rows it
/// visited, but a source that misses part of the region reports zero and sees
/// zero, so "zero rows and zero entries" looks exactly like an empty region
/// while the region is in fact full. That is ticket 83: a tiles-only source saw
/// no rows for a region that was entirely in the serving memtable, and the build
/// wrote an empty sidecar. Only a structure the scan does not walk can testify
/// that the region is non-empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionRows {
    /// At least one active tile's first key names the region.
    InTiles,
    /// The serving memtable holds live rows of this region: one per handle whose
    /// newest committed version is not a tombstone. Exact for the region (the
    /// memtable is shared by every region, so counting it whole would refuse a
    /// build on a genuinely empty movement region; see
    /// [`Engine::primary_region_live_rows`]).
    InMemtable { rows: usize },
    /// No engine structure holds a live row in the region, so an empty build is
    /// legitimate.
    Empty,
}

impl RegionRows {
    fn holds_rows(self) -> bool { !matches!(self, RegionRows::Empty) }
}

impl fmt::Display for RegionRows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionRows::InTiles => write!(f, "an active tile names this region"),
            RegionRows::InMemtable { rows } => {
                write!(f, "the serving memtable holds {rows} live rows of this region")
            }
            RegionRows::Empty => write!(f, "no engine structure holds a live row of this region"),
        }
    }
}

/// The `family|entity` name of the region the index covers, spelled from the
/// same primary prefix the authoritative scan walks, so it matches what
/// [`Engine::active_tile_regions`] reports for a tile of that region under the
/// given key encoding (family-first gives `P|StockMovement`, the entity-first
/// encoding `C|StockMovement`). `None` only for an entity the key codec rejects,
/// which cannot happen for the literal [`INDEX_ENTITY`].
fn region_name(encoding: keys::KeyEncoding) -> Option<String> {
    let mut prefix = keys::primary_prefix_with(encoding, INDEX_ENTITY, 0).ok()?;
    // Drop the 8-byte handle a prefix carries to leave `marker | entity | 0`.
    prefix.truncate(prefix.len() - 8);
    Some(keys::region_name(&prefix))
}

/// Whether the index's region holds live rows, read from the engine structures
/// the build's scan does not consume - the independent witness [`RegionRows`]
/// describes. The caller reads it at the same boundary as the scan, so the build
/// can fail on an empty result instead of writing a sidecar that would leave
/// every scan on the full-entity fallback.
///
/// The memtable arm is exact for the region ([`Engine::primary_region_live_rows`]
/// counts the region's live rows, not the whole shared memtable), so a target whose
/// memtable is busy with other regions while the movement region is genuinely empty
/// builds a legitimate empty index instead of aborting the run.
///
/// The tile arm is only as exact as a tile's first key. Under
/// [`crate::engine::RegionMode::PerRegion`] a tile holds one region, so its first
/// key names exactly that region. Under [`crate::engine::RegionMode::Global`] a tile
/// holds the whole keyspace and `active_tile_regions` reports the region of its
/// LOWEST key, so the arm is wrong in either direction there: it can report this
/// region for a tile whose movements are all tombstones or which holds no movement
/// at all (a false positive refuses an empty build), and it can stay silent while
/// live movement rows sit in a tile whose lowest key is another region (a false
/// negative leaves the ticket-83 state unguarded). Reads are unaffected either way:
/// the build's source walks the tiles themselves, so a false negative costs a loud
/// guard, never a wrong answer, and the authoritative scan is merged in both modes.
pub fn movement_region_rows(engine: &Engine) -> RegionRows {
    if let Some(region) = region_name(engine.key_encoding()) {
        if engine.active_tile_regions().iter().flatten().any(|r| *r == region) {
            return RegionRows::InTiles;
        }
    }
    match engine.primary_region_live_rows(INDEX_ENTITY).expect("index entity is a valid name") {
        0 => RegionRows::Empty,
        rows => RegionRows::InMemtable { rows },
    }
}

/// Build the `order_no -> handle` index tile from the live `P|StockMovement`
/// rows `scan_primary` visits. Runs off the request path (the caller owns the
/// thread): it streams the whole entity prefix once, JSON-parses each document
/// once, sorts the `(order_no, handle, csn)` entries, and writes ONE 4 KiB-page,
/// no-bloom tile with the existing [`crate::tile::TileWriter`] (which syncs the
/// file). The tile is written to a scratch name and atomically renamed over the
/// final `name`, so a rebuild after a retire replaces the previous sidecar file
/// rather than failing on `create_new`. The caller must sync the parent directory
/// before adopting the tile, matching the compaction apply ordering.
///
/// Memory: this build holds one sort buffer of index entries (each a `U` unique
/// key plus its 8-byte handle), not one per document. What the source costs is
/// the source's business: the offline caller passes a scan closure that streams
/// the merged view, while the driver's rebuild ([`spawn_rebuild`]) hands over an
/// inline memtable snapshot plus a tile walk, so neither side keeps a second copy
/// of a document.
///
/// `scan_primary` walks the rows at latest committed state - one row per LIVE
/// handle, at the CSN of the version that won - and returns how many it visited.
/// It is the caller's job to have reduced the source that way (`scan_primary_with_csn`
/// does it for the merged view, and [`spawn_rebuild`]'s merge applies the same rule
/// to the tile walk and the memtable snapshot). `region_rows` is the independent
/// non-emptiness witness: an empty result for a region that holds rows fails here
/// rather than writing an empty sidecar. The scan's own count is quoted in that
/// failure but never compared against the entry count, because both come from the
/// same iteration and can only agree.
///
/// `coverage_floor_csn` is the CSN the caller's source snapshot speaks for, and
/// the index covers up to `max(its newest entry's CSN, the floor)`. The floor is
/// what makes `coverage_csn` a claim about the engine state instead of a claim
/// about the survivors: a tombstone or a superseded version produces no entry, so
/// without the floor the coverage would sit at the newest LIVE row and a rebuild
/// trigger reading it would re-index the region forever. The driver passes
/// `engine.csn()` at the instant it snapshotted the source; passing 0 claims only
/// what the entries themselves prove.
pub fn build_order_no_index(
    dir: &Path,
    key_encoding: keys::KeyEncoding,
    scan_primary: impl FnOnce(&mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
    region_rows: RegionRows,
    coverage_floor_csn: u64,
    name: &str,
) -> io::Result<OrderNoIndexBuild> {
    // (encoded key, value). Keys must be strictly increasing, so collect and
    // sort before pushing to the writer.
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut newest_entry_csn: u64 = 0;
    let rows = scan_primary(&mut |handle, csn, doc| {
        let row: serde_json::Value = serde_json::from_slice(doc)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let order_no = row[std::str::from_utf8(INDEX_FIELD).expect("ascii field name")].as_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "StockMovement.order_no is not a string"))?;
        let key = keys::unique_key_with(key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no.as_bytes(), handle, csn)?;
        entries.push((key, handle.to_be_bytes().to_vec()));
        newest_entry_csn = newest_entry_csn.max(csn);
        Ok(())
    })?;
    let coverage_csn = newest_entry_csn.max(coverage_floor_csn);
    if entries.is_empty() && region_rows.holds_rows() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!(
            "order_no index build produced 0 entries for a region that holds rows ({region_rows}): \
             the scan visited {rows} live StockMovement rows, so its source does not cover the \
             region and the index would leave every scan on the full-entity fallback")));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let path = dir.join(name);
    // Write to a scratch name and atomically rename over `name` so a rebuild
    // after a retire replaces the stale sidecar (the writer itself refuses to
    // overwrite an existing file). The `Tile` returned by `finish` holds an open
    // fd, which stays valid across the rename.
    let tmp_path = dir.join(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp_path);
    let digest = [0u8; 32];
    // 4 KiB pages: a point probe reads one 4 KiB block, and the one-time write
    // amplification stays at the data's own size (no 64 KiB block padding).
    let mut writer = crate::tile::TileWriter::new(&tmp_path, coverage_csn, &digest, 4_096)?;
    for (key, value) in &entries {
        writer.push(key, value)?;
    }
    let tile = writer.finish()?;
    std::fs::rename(&tmp_path, &path)?;
    let output_bytes = std::fs::metadata(&path)?.len();
    Ok(OrderNoIndexBuild {
        name: name.to_string(),
        index: OrderNoIndex {
            tile: Arc::new(tile),
            key_encoding,
            coverage_csn,
            entries: entries.len() as u64,
        },
        output_bytes,
    })
}

impl OrderNoIndex {
    /// The index tile (an `Arc<Tile>`) for the read-ahead worker's warm view.
    /// Sharing it is safe: the tile is immutable after `TileWriter::finish`.
    pub(crate) fn tile(&self) -> Arc<Tile> {
        Arc::clone(&self.tile)
    }

    /// Probe the index for `order_no`. `Some(handle)` when the index holds a live
    /// entry, `None` otherwise (absent entry or no index). Never authoritative:
    /// the caller cross-checks the returned document and falls back to the scan
    /// on a miss or mismatch.
    pub fn lookup(&self, order_no: &[u8]) -> io::Result<Option<u64>> {
        let pfx = keys::unique_prefix_with(self.key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no)?;
        if !self.tile.could_contain_prefix(&pfx) { return Ok(None); }
        let mut cursor = self.tile.prefix_cursor(&pfx)?;
        if let Some((key, _)) = cursor.next()? {
            if key.len() == pfx.len() + 16 {
                return Ok(Some(u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap())));
            }
        }
        Ok(None)
    }
}

/// The sidecar filename the derived index tile is written under, in the engine's
/// directory. One spelling for the build and the rebuild so a rebuild always
/// replaces the predecessor it is replacing.
pub const INDEX_TILE_NAME: &str = "order-no-index.tile";

/// How many bytes of document the inline serving-memtable snapshot may copy on
/// the thread that schedules a rebuild.
///
/// The snapshot exists because the engine's memtable cannot cross to the builder
/// thread, so its cost is paid by the thread that is also pacing requests. The
/// basis is the workload this serves and the ceiling it paces against: ticket
/// 83's movement region is ~2.8k movements, ~640 KiB of tiles, so 4 MiB leaves
/// roughly six times the headroom, and a 4 MiB copy is on the order of a
/// millisecond at a few GB/s - an order of magnitude below the 10 ms lag ceiling. Over the bound
/// NOTHING is copied (the region is measured first) and the rebuild is skipped
/// with `skipped: <reason>` logged, so the pacer never trades a cold device
/// stream for a large copy and the region's scans keep a logged fallback.
pub const INLINE_SNAPSHOT_MAX_BYTES: usize = 4 << 20;

/// How many committed versions an adopted index may lag before a rebuild is
/// scheduled EVEN WITH NO scan demand.
///
/// Without this floor a region that is written far faster than it is scanned
/// would never be rebuilt, which is the ticket-83 state again with a log line
/// instead of silence. At the load's own commit rate this is a few seconds of
/// writes, so a quiet region costs at most one rebuild per that many versions
/// (the index tile is one file write, not a merge) and a region with demand is
/// covered by the demand arm long before.
pub const INDEX_REBUILD_STALE_CSN: u64 = 1_024;

/// The trigger labels [`rebuild_trigger`] returns and the driver logs, so a run says
/// which arm of the rule fired: `scan_demand` (the region is being scanned and the
/// index lags what the scan would serve), `stale` (the lag passed
/// [`INDEX_REBUILD_STALE_CSN`] with no scan demand), and `policy` (the policy wants the
/// index and none is adopted - the first off-path build of a run whose startup build
/// did not happen, e.g. the policy was reloaded mid-run).
pub const TRIGGER_SCAN_DEMAND: &str = "scan_demand";
pub const TRIGGER_STALE: &str = "stale";
pub const TRIGGER_POLICY: &str = "policy";

/// Whether an adopted index must be rebuilt, and if so why.
///
/// The rule, in full: a rebuild is scheduled only when the committed state has
/// moved past what the index covers (`engine_csn > coverage_csn`) AND it can pay
/// for itself, which is one of
///
/// - the region had scan demand since the index was last made current
///   ([`Engine::order_no_index_lookups`] advanced): the scans are what the index
///   makes cheap, so keeping it current is the point of having it; or
/// - the coverage lags by at least `stale_bound` committed versions
///   ([`INDEX_REBUILD_STALE_CSN`] in production): a region written much faster
///   than it is scanned still gets covered instead of waiting for a scan that may
///   never come.
///
/// A lag of zero schedules nothing: the same snapshot would be rebuilt and the
/// same bytes rewritten. A caller that has a rebuild in flight must not ask again
/// (one at a time), which is what keeps two rebuilds from racing.
pub fn rebuild_trigger(coverage_csn: u64, engine_csn: u64, scan_demand: bool, stale_bound: u64) -> Option<&'static str> {
    if engine_csn <= coverage_csn { return None; }
    if scan_demand { return Some(TRIGGER_SCAN_DEMAND); }
    if engine_csn - coverage_csn >= stale_bound { return Some(TRIGGER_STALE); }
    None
}

/// The serving-memtable half of a hybrid rebuild's source: the region's rows,
/// one per handle at its newest committed version, copied inline on the caller's
/// thread and owned by the builder thread from then on.
///
/// One row per handle is the whole point of the copy being small: the memtable
/// may hold several versions of the same handle and the index only ever needs the
/// winning one. An empty `doc` is a tombstone and is kept, because the merge must
/// be able to tell "this handle is deleted" from "this handle is absent from the
/// memtable".
#[derive(Debug, Default)]
pub struct MemtableRows {
    /// `(handle, csn, doc)` ascending by handle; empty `doc` = tombstone.
    pub rows: Vec<(u64, u64, Vec<u8>)>,
    /// Document bytes held (the bound [`INLINE_SNAPSHOT_MAX_BYTES`] is charged
    /// against exactly this).
    pub bytes: usize,
    /// Live rows in the snapshot (tombstone rows excluded), i.e. the memtable arm
    /// of the non-emptiness witness.
    pub live_rows: usize,
    /// The engine CSN the snapshot speaks for: with the tile snapshot taken at the
    /// same instant, the state up to this CSN is represented, so it is the rebuild's
    /// coverage floor. A tombstone or a superseded version raises it without
    /// producing an entry, which is why coverage cannot be derived from the entries
    /// alone.
    pub csn: u64,
}

/// Why an inline memtable snapshot was refused: the region holds more document
/// bytes than the caller's bound allows to be copied on its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemtableSnapshotOver {
    /// Handles in the region's memtable rows (tombstones included).
    pub rows: usize,
    /// Live rows among them.
    pub live_rows: usize,
    /// Document bytes the region holds.
    pub bytes: usize,
    /// The bound the caller set.
    pub bound: usize,
}

impl fmt::Display for MemtableSnapshotOver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "inline memtable snapshot would exceed the {bound}-byte bound \
             ({bytes} bytes over {rows} handles, {live_rows} live)",
            bound = self.bound,
            bytes = self.bytes,
            rows = self.rows,
            live_rows = self.live_rows,
        )
    }
}

/// The outcome of the bounded inline memtable snapshot
/// ([`Engine::snapshot_primary_memtable`]): either the region's rows were copied
/// and the rebuild can proceed, or the region is over the caller's bound and
/// nothing was copied, so the rebuild is skipped instead of stalling the thread
/// that would have paid for it.
#[derive(Debug)]
pub enum MemtableSnapshot {
    /// The region fits the bound; its rows are owned by the caller from here.
    Copied(MemtableRows),
    /// The region does not fit: no document was copied.
    OverBound(MemtableSnapshotOver),
}

/// One hybrid rebuild's product plus what the decision log line reports about it.
pub struct RebuildResult {
    pub build: OrderNoIndexBuild,
    /// Rows the merge discarded instead of indexing: superseded tile versions, and
    /// handles whose newest version is a tombstone. Never in the index.
    pub stale_rows: u64,
    /// Rows the inline serving-memtable snapshot contributed (one per handle).
    pub memtable_rows: u64,
    /// Document bytes the snapshot copied on the scheduling thread.
    pub inline_bytes: usize,
    /// The trigger [`rebuild_trigger`] returned, echoed into the log line.
    pub trigger: String,
}

/// Schedule one hybrid index rebuild and return the channel it reports through.
///
/// The source is split by where its cost lands, which is the whole reason this
/// function exists instead of a single merged scan:
///
/// - the **serving-memtable** rows are already in `memtable`, copied inline by the
///   caller because the engine's memtable cannot cross a thread boundary. The
///   caller bounds that copy ([`INLINE_SNAPSHOT_MAX_BYTES`]) and skips the rebuild
///   when it would not fit;
/// - the **tile** half - the cold device stream that a tiled region would
///   otherwise pull through the pacing thread - runs HERE, inside the spawned
///   thread, through `tile_scan`. Nothing on the caller's thread opens a tile
///   cursor: `tile_scan` is handed over before a thread exists and is invoked only
///   by the worker, which is what the seam's test binds.
///
/// Footprint: the worker holds the tile half's winning version of each tile-resident
/// handle (one document per handle, not per version), which is on the order of the
/// region itself - on a thread that is not pacing requests and that the driver waits
/// on without blocking the pacer. The bound the caller applies is on the INLINE half,
/// the only one whose cost lands on the pacer.
///
/// The merge is the engine's own newest-wins rule, not a second one: the union of
/// the two halves is sorted into the key order the authoritative scan sees (handle
/// ascending, CSN descending) and reduced by the same
/// [`crate::engine`] helper `scan_primary_with_csn` uses, so the index and the scan
/// cannot disagree about which version of a handle is current. A tie cannot arise:
/// a publish evicts exactly the versions a tile holds, so the memtable's version of
/// a handle is always newer than any tile's, and the merge skips a tile row for a
/// handle the snapshot already has.
pub fn spawn_rebuild<F>(
    dir: std::path::PathBuf,
    key_encoding: keys::KeyEncoding,
    view: crate::readahead::TileReadView,
    tile_scan: F,
    memtable: MemtableRows,
    region_rows: RegionRows,
    trigger: String,
    name: String,
) -> std::sync::mpsc::Receiver<io::Result<RebuildResult>>
where
    F: FnOnce(&crate::readahead::TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>
        + Send
        + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(rebuild_index(dir, key_encoding, view, tile_scan, memtable, region_rows, trigger, &name));
    });
    rx
}

fn rebuild_index<F>(
    dir: std::path::PathBuf,
    key_encoding: keys::KeyEncoding,
    view: crate::readahead::TileReadView,
    tile_scan: F,
    memtable: MemtableRows,
    region_rows: RegionRows,
    trigger: String,
    name: &str,
) -> io::Result<RebuildResult>
where
    F: FnOnce(&crate::readahead::TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
{
    let memtable_handles: std::collections::HashSet<u64> =
        memtable.rows.iter().map(|(handle, _, _)| *handle).collect();
    // One document per handle in the tiles: the newest version seen. A tile walk
    // can yield a handle once per tile that holds a version of it, and only the
    // newest can win, so holding the older ones would only grow the buffer.
    let mut tile_rows: std::collections::HashMap<u64, (u64, Vec<u8>)> = std::collections::HashMap::new();
    let mut stale_rows: u64 = 0;
    tile_scan(&view, &mut |handle, csn, doc| {
        if memtable_handles.contains(&handle) {
            // The memtable's version is newer and is the one the scan serves.
            stale_rows += 1;
            return Ok(());
        }
        match tile_rows.get_mut(&handle) {
            Some(current) if current.0 < csn => {
                *current = (csn, doc.to_vec());
                stale_rows += 1;
            }
            Some(_) => stale_rows += 1,
            None => {
                tile_rows.insert(handle, (csn, doc.to_vec()));
            }
        }
        Ok(())
    })?;

    // The union in the order the newest-wins rule reads: handle ascending, CSN
    // descending, so each handle's first row is the version the authoritative scan
    // would serve.
    let mut union: Vec<(u64, u64, &[u8])> = memtable
        .rows
        .iter()
        .map(|(handle, csn, doc)| (*handle, *csn, doc.as_slice()))
        .collect();
    union.extend(tile_rows.iter().map(|(handle, (csn, doc))| (*handle, *csn, doc.as_slice())));
    union.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));

    let memtable_rows = memtable.rows.len() as u64;
    let build = build_order_no_index(
        &dir,
        key_encoding,
        |visit| {
            let mut newest = crate::engine::NewestPrimaryVersion::new();
            let mut covered: u64 = 0;
            for &(handle, csn, doc) in &union {
                match newest.accept(handle, csn, doc) {
                    Some((handle, csn, doc)) => {
                        covered += 1;
                        visit(handle, csn, doc)?;
                    }
                    // A superseded version or a tombstone: the handle is not live
                    // at that CSN, so it contributes no entry.
                    None => stale_rows += 1,
                }
            }
            Ok(covered)
        },
        region_rows,
        memtable.csn,
        name,
    )?;
    Ok(RebuildResult { build, stale_rows, memtable_rows, inline_bytes: memtable.bytes, trigger })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Op};

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-index-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The negative control for the loud empty-build refusal: a region that really
    /// holds no live row is NOT a defect. The memtable is non-empty (another region
    /// is being written), which under the old whole-memtable witness refused the build
    /// and aborted the run at startup; the exact witness says Empty, the build returns
    /// Ok with no entries, and the sidecar it writes opens as a valid 0-entry tile.
    #[test]
    fn an_empty_region_builds_a_zero_entry_sidecar() {
        let dir = tmp_dir("emptyregion");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"c0", &[Op::PutDoc {
            entity: b"Customer".to_vec(),
            handle: 1,
            doc: br#"{"email":"a@b"}"#.to_vec(),
        }])
        .unwrap();
        assert!(e.serving_memtable_entries() > 0, "the memtable is busy with another region");
        assert_eq!(movement_region_rows(&e), RegionRows::Empty, "the movement region itself holds no live row");

        let build = build_order_no_index(
            &dir,
            e.key_encoding(),
            |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
            movement_region_rows(&e),
            e.csn(),
            "empty-index.tile",
        )
        .expect("an empty region is a legitimate empty index, not a defect");
        assert_eq!(build.index.entries, 0);
        assert_eq!(build.index.coverage_csn, e.csn(), "the floor records the state the empty index covers");

        let path = dir.join("empty-index.tile");
        let tile = crate::tile::Tile::open(&path).expect("a zero-entry sidecar still opens as a tile");
        assert_eq!(tile.first_key(), None, "the sidecar holds no key");
        assert_eq!(tile.last_key(), None);
        assert_eq!(tile.cutoff(), e.csn());
        assert_eq!(build.index.lookup(b"O1").unwrap(), None, "and it answers no lookup");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
