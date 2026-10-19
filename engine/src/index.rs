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
//! rebuilt at open when lost.

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
    /// The serving memtable holds entries. The memtable is shared by every
    /// region, so this witnesses "some rows are still live in the memtable",
    /// not "rows of this region are": it can over-report, never under-report the
    /// state a missed scan leaves behind.
    InMemtable { entries: usize },
    /// No engine structure holds a row in the region, so an empty build is
    /// legitimate.
    Empty,
}

impl RegionRows {
    fn holds_rows(self) -> bool { !matches!(self, RegionRows::Empty) }
}

impl fmt::Display for RegionRows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionRows::InTiles => write!(f, "an active tile holds rows of this region"),
            RegionRows::InMemtable { entries } => write!(f, "the serving memtable holds {entries} entries"),
            RegionRows::Empty => write!(f, "no engine structure holds rows of this region"),
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

/// Whether the index's region holds rows, read from the engine structures the
/// build's scan does not consume - the independent witness [`RegionRows`]
/// describes. The caller reads it at the same boundary as the scan, so the build
/// can fail on an empty result instead of writing a sidecar that would leave
/// every scan on the full-entity fallback.
///
/// A tile witnesses the region only through its first key, which is exact for a
/// region-partitioned tile and absent for a whole-keyspace one; the memtable arm
/// covers the case a whole-keyspace tile or no tile at all leaves behind. Under a
/// `RegionMode::Global` publish the only rows are tile-resident, so this signal
/// cannot see them: it under-reports there, and the merged-view scan (which always
/// reads tiles) is what keeps that state correct.
pub fn movement_region_rows(engine: &Engine) -> RegionRows {
    if let Some(region) = region_name(engine.key_encoding()) {
        if engine.active_tile_regions().iter().flatten().any(|r| *r == region) {
            return RegionRows::InTiles;
        }
    }
    match engine.serving_memtable_entries() {
        0 => RegionRows::Empty,
        entries => RegionRows::InMemtable { entries },
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
/// key plus its 8-byte handle), not one per document. The production caller adds
/// the scan source's own footprint on top: the engine's memtable cannot be shared
/// with the builder thread, so the driver snapshots every movement document into
/// a `Vec` first (one copy of the region's documents, held until the build
/// returns). Neither side keeps a second copy of a document.
///
/// `scan_primary` walks the rows at latest committed state and returns how many
/// live rows it visited; the engine supplies the merged memtable + tile source so
/// rows still in the serving memtable are covered. `region_rows` is the
/// independent non-emptiness witness: an empty result for a region that holds
/// rows fails here rather than writing an empty sidecar. The scan's own count is
/// quoted in that failure but never compared against the entry count, because
/// both come from the same iteration and can only agree.
pub fn build_order_no_index(
    dir: &Path,
    key_encoding: keys::KeyEncoding,
    scan_primary: impl FnOnce(&mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
    region_rows: RegionRows,
    name: &str,
) -> io::Result<OrderNoIndexBuild> {
    // (encoded key, value). Keys must be strictly increasing, so collect and
    // sort before pushing to the writer.
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut coverage_csn: u64 = 0;
    let rows = scan_primary(&mut |handle, csn, doc| {
        let row: serde_json::Value = serde_json::from_slice(doc)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let order_no = row[std::str::from_utf8(INDEX_FIELD).expect("ascii field name")].as_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "StockMovement.order_no is not a string"))?;
        let key = keys::unique_key_with(key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no.as_bytes(), handle, csn)?;
        entries.push((key, handle.to_be_bytes().to_vec()));
        coverage_csn = coverage_csn.max(csn);
        Ok(())
    })?;
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
