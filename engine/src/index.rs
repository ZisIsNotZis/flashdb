//! Derived secondary access path (WP-M): an off-request-path `order_no ->
//! StockMovement handle` index, built from the `P|StockMovement` tiles and used
//! by the authoritative read path only as a HINT.
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

use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::keys;
use crate::readahead::TileReadView;
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

/// Build the `order_no -> handle` index tile from the `P|StockMovement` tiles in
/// `view`. Runs off the request path (the caller owns the thread): it streams
/// the whole entity prefix once, JSON-parses each document once, sorts the
/// `(order_no, handle, csn)` entries, and writes ONE 4 KiB-page, no-bloom tile
/// with the existing [`crate::tile::TileWriter`] (which syncs the file). The
/// tile is written to a scratch name and atomically renamed over the final
/// `name`, so a rebuild after a retire replaces the previous sidecar file rather
/// than failing on `create_new`. The caller must sync the parent directory before
/// adopting the tile, matching the compaction apply ordering. Memory is bounded
/// by the entity's index entries (one sort buffer), not the document set.
pub fn build_order_no_index(
    dir: &Path,
    view: &TileReadView,
    name: &str,
) -> io::Result<OrderNoIndexBuild> {
    let enc = view.key_encoding();
    // (encoded key, value). Keys must be strictly increasing, so collect and
    // sort before pushing to the writer.
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut coverage_csn: u64 = 0;
    view.scan_primary(INDEX_ENTITY, |handle, csn, doc| {
        let row: serde_json::Value = serde_json::from_slice(doc)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let order_no = row[std::str::from_utf8(INDEX_FIELD).expect("ascii field name")].as_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "StockMovement.order_no is not a string"))?;
        let key = keys::unique_key_with(enc, INDEX_ENTITY, INDEX_FIELD, order_no.as_bytes(), handle, csn)?;
        entries.push((key, handle.to_be_bytes().to_vec()));
        coverage_csn = coverage_csn.max(csn);
        Ok(())
    })?;
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
            key_encoding: enc,
            coverage_csn,
            entries: entries.len() as u64,
        },
        output_bytes,
    })
}

impl OrderNoIndex {
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
