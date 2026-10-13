//! Read-ahead tile warming (slice 58): a background worker that pre-reads the
//! tile blocks a request will probe, so the request path's authoritative read is
//! a page-cache hit and the cold page-in happened off the pacer.
//!
//! Purely observational and correctness-neutral by construction: the worker
//! parses the same trace, runs a TILE-ONLY version of each request's unique
//! lookups plus the follow-on primary `get`, and discards the results. It never
//! mutates engine state and never feeds a decision, so it cannot change read
//! semantics, durability, ordering, or the single-writer commit path. The
//! authoritative read still runs on the request path with newest-first
//! memtable-over-tile precedence.

use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::keys;
use crate::request::{request_read_lookups, request_scan_entities};
use crate::tile::Tile;

/// How many requests ahead of the pacer's dispatched position the worker keeps
/// warm. Bounded so the warmed blocks are still resident when the request is
/// served, and large enough to absorb an isolated slow worker read without
/// bursting a large read set at startup (which contends with the early WAL
/// fsync/append).
const READ_AHEAD_LEAD: u64 = 64;

/// Per-request pause after warming one request, so the warm reads are spread
/// over time instead of bursting the whole lead at startup.
const WARM_PACE_MS: u64 = 4;

/// A read-only, thread-safe snapshot of the engine's published tiles. Tiles are
/// immutable after publish (compaction/publish replace them with new files and
/// unlink the old), so `Arc<Tile>` clones can be shared with the worker while
/// the engine keeps its own ordered list.
pub struct TileReadView {
    tiles: Arc<Vec<Arc<Tile>>>,
    key_encoding: keys::KeyEncoding,
}

impl TileReadView {
    pub(crate) fn new(tiles: Arc<Vec<Arc<Tile>>>, key_encoding: keys::KeyEncoding) -> Self {
        TileReadView { tiles, key_encoding }
    }

    /// The key spelling this view's tiles were written under. The derived-index
    /// builder uses it so its index keys probe the same spelling the engine's
    /// authoritative reads accept.
    pub(crate) fn key_encoding(&self) -> keys::KeyEncoding {
        self.key_encoding
    }

    /// Tile-only walk of an entity's whole primary prefix (the exact prefix
    /// [`crate::engine::Engine::scan_primary`] walks, minus the serving
    /// memtable), yielding each live document's `(handle, csn, doc bytes)` once.
    /// Used by the off-request-path secondary-access-path builder (WP-M): the
    /// rows are exactly the tile-resident documents the authoritative scan would
    /// visit, so an index built from them maps the right handles. Purely
    /// observational; results never feed a correctness decision directly.
    pub(crate) fn scan_primary(
        &self,
        entity: &[u8],
        mut visit: impl FnMut(u64, u64, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut prefix = keys::primary_prefix_with(self.key_encoding, entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        let mut last_handle: Option<u64> = None;
        for t in self.tiles.iter() {
            if !t.could_contain_prefix(&prefix) { continue; }
            let mut cursor = t.prefix_cursor(&prefix)?;
            while let Some((key, value)) = cursor.next()? {
                let Some((key_entity, handle, csn)) = keys::decode_primary(key) else { continue; };
                if key_entity != entity { continue; }
                if last_handle == Some(handle) { continue; }
                last_handle = Some(handle);
                if !value.is_empty() {
                    visit(handle, csn, value)?;
                }
            }
        }
        Ok(())
    }

    /// Warm the unique-index block(s) for `(entity, field, value)` and, when the
    /// handle is found in a tile, the matching primary document block. The exact
    /// tile scan mirrors the authoritative read path (`scan_merged` for the
    /// unique probe, newest-first for the primary) minus the memtable, so the
    /// same data blocks land in the page cache. Results are discarded.
    fn warm_lookup(&self, entity: &[u8], field: &[u8], value: &[u8]) -> io::Result<()> {
        if let Some(handle) = self.unique_handle_tiles(entity, field, value)? {
            // A failed primary warm only loses one prefetch; the authoritative
            // read reports the real error if the block is genuinely unreadable.
            let _ = self.warm_primary(entity, handle);
        }
        Ok(())
    }

    /// Tile-only unique lookup: the handle the published tiles name for
    /// `(entity, field, value)`, ignoring the serving memtable. The handle is
    /// stable across versions (the U index entry is written once), so this is
    /// the same handle the authoritative read resolves; the worker only uses it
    /// to warm the matching primary block.
    fn unique_handle_tiles(&self, entity: &[u8], field: &[u8], value: &[u8]) -> io::Result<Option<u64>> {
        let pfx = keys::unique_prefix_with(self.key_encoding, entity, field, value)?;
        let mut handle: Option<u64> = None;
        for t in self.tiles.iter() {
            if !t.could_contain_prefix(&pfx) { continue; }
            if !t.might_contain_unique_prefix(&pfx) { continue; }
            let mut cursor = t.prefix_cursor(&pfx)?;
            if let Some((key, _)) = cursor.next()? {
                if key.len() == pfx.len() + 16 {
                    handle = Some(u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap()));
                }
            }
        }
        Ok(handle)
    }

    /// Warm the newest tile's primary block for `handle`, stopping at the first
    /// hit exactly like [`crate::engine::Engine::get`]'s tile loop.
    fn warm_primary(&self, entity: &[u8], handle: u64) -> io::Result<()> {
        let pfx = keys::primary_prefix_with(self.key_encoding, entity, handle)?;
        for t in self.tiles.iter().rev() {
            if !t.could_contain_prefix(&pfx) { continue; }
            let mut cursor = t.prefix_cursor(&pfx)?;
            if let Some((key, _)) = cursor.next()? {
                if keys::key_csn(key).is_some() { break; }
            }
        }
        Ok(())
    }

    /// Warm every data block the authoritative
    /// [`crate::engine::Engine::scan_primary`] of `entity` would read (the whole
    /// primary prefix). Memoized per snapshot so a full-entity scan is warmed
    /// once per published tile set, not once per scan request — without the memo
    /// the worker would re-scan the entity on every movement scan and turn
    /// read-ahead into a second full-scan stream. `state` survives worker ticks
    /// and refreshes; a new snapshot resets the memo.
    fn warm_scan_prefix(&self, entity: &[u8], state: &mut ScanWarmState) -> io::Result<bool> {
        if !Arc::ptr_eq(&self.tiles, &state.tiles) {
            state.tiles = Arc::clone(&self.tiles);
            state.warmed.clear();
        }
        if !state.warmed.insert(entity.to_vec()) {
            return Ok(false);
        }
        // The exact prefix the authoritative scan walks: the entity's whole
        // primary range (marker | entity | 0), not any single handle.
        let mut prefix = keys::primary_prefix_with(self.key_encoding, entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        for t in self.tiles.iter() {
            if !t.could_contain_prefix(&prefix) { continue; }
            let mut cursor = t.prefix_cursor(&prefix)?;
            while cursor.next()?.is_some() {}
        }
        Ok(true)
    }
}

/// Memoizes which entity prefixes have already been warmed for the current tile
/// snapshot. The worker owns one across ticks; a snapshot change (publish or
/// compaction replaced tiles) resets it on the next scan warm.
struct ScanWarmState {
    tiles: Arc<Vec<Arc<Tile>>>,
    warmed: HashSet<Vec<u8>>,
}

/// The driver's handle to the read-ahead worker. Dropping it disconnects the
/// channel and lets the worker thread exit.
pub struct ReadAhead {
    tx: mpsc::Sender<Msg>,
    position: Arc<AtomicU64>,
}

enum Msg {
    /// Publish/compaction replaced tiles; adopt the fresh snapshot.
    Refresh(TileReadView),
}

impl ReadAhead {
    /// Spawn the worker with the trace lines it reads ahead of and the initial
    /// tile snapshot. The worker tracks `position` (the pacer's dispatched
    /// index, updated via [`ReadAhead::set_position`]) and warms the next
    /// [`READ_AHEAD_LEAD`] requests' tile blocks.
    pub fn spawn(lines: Arc<Vec<String>>, view: TileReadView) -> ReadAhead {
        let (tx, rx) = mpsc::channel::<Msg>();
        let position = Arc::new(AtomicU64::new(0));
        let worker_position = Arc::clone(&position);
        std::thread::spawn(move || worker(lines, view, worker_position, rx));
        ReadAhead { tx, position }
    }

    /// Publish the pacer's current dispatched index so the worker stays ahead of
    /// it. Cheap (a relaxed store); call on every dispatch advance.
    pub fn set_position(&self, index: u64) {
        self.position.store(index, Ordering::Relaxed);
    }

    /// Adopt a fresh tile snapshot (publish/compaction replaced tiles). The old
    /// snapshot's `Arc<Tile>` handles stay valid (their files are unlinked but
    /// open), so a concurrent warm read never faults; it just warms stale blocks
    /// until the next refresh.
    pub fn refresh(&self, view: TileReadView) {
        let _ = self.tx.send(Msg::Refresh(view));
    }
}

fn worker(
    lines: Arc<Vec<String>>,
    mut view: TileReadView,
    position: Arc<AtomicU64>,
    rx: mpsc::Receiver<Msg>,
) {
    let total = lines.len() as u64;
    let mut warm_index: u64 = 0;
    let mut scan_warm = ScanWarmState { tiles: Arc::new(Vec::new()), warmed: HashSet::new() };
    loop {
        // Adopt any pending tile snapshot before warming against it.
        loop {
            match rx.try_recv() {
                Ok(Msg::Refresh(v)) => view = v,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let target = position.load(Ordering::Relaxed).saturating_add(READ_AHEAD_LEAD).min(total);
        if warm_index < target {
            if let Ok(req) = serde_json::from_str::<Value>(&lines[warm_index as usize]) {
                for lookup in request_read_lookups(&req) {
                    // Warm and discard. A failure (e.g. a tile concurrently
                    // replaced) only costs this prefetch; the authoritative read
                    // still reports the real error when it runs.
                    let _ = view.warm_lookup(lookup.entity.as_bytes(), lookup.field.as_bytes(), &lookup.value);
                }
                for entity in request_scan_entities(&req) {
                    // Warm and discard; memoized per snapshot so a full-entity
                    // scan is warmed once per tile set, not once per scan request.
                    let _ = view.warm_scan_prefix(entity.as_bytes(), &mut scan_warm);
                }
            }
            warm_index += 1;
            // Pace the warm: spread the block reads over time instead of
            // bursting the whole lead at startup.
            std::thread::sleep(Duration::from_millis(WARM_PACE_MS));
            continue;
        }
        match rx.recv_timeout(Duration::from_millis(2)) {
            Ok(Msg::Refresh(v)) => view = v,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScanWarmState;
    use crate::engine::{Engine, Op};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "flashdb-readahead-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn unique_op(value: &[u8], handle: u64) -> Op {
        Op::PutUnique {
            entity: b"E".to_vec(),
            field: b"u".to_vec(),
            value: value.to_vec(),
            handle,
        }
    }

    #[test]
    fn tile_read_view_resolves_published_unique_handles_without_the_memtable() {
        let dir = tmp_dir("view");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"t1", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"d1".to_vec() },
            unique_op(b"a1", 1),
        ]).unwrap();
        e.publish_tile(dir.join("tile-1.tile"), 1).unwrap();
        // A newer, still-memtable-only version must be seen by the authoritative
        // read but NOT by the tile-only warm view (warming is not a read result).
        e.commit_block(b"t2", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 11, doc: b"d11".to_vec() },
            unique_op(b"b1", 11),
        ]).unwrap();

        let view = e.tile_read_view();
        assert_eq!(view.unique_handle_tiles(b"E", b"u", b"a1").unwrap(), Some(1));
        assert_eq!(view.unique_handle_tiles(b"E", b"u", b"absent").unwrap(), None);
        assert_eq!(
            view.unique_handle_tiles(b"E", b"u", b"b1").unwrap(),
            None,
            "the warm view is tile-only and must never stand in for the authoritative read"
        );
        assert_eq!(
            e.unique_lookup(b"E", b"u", b"b1").unwrap(),
            Some(11),
            "the authoritative read still resolves the memtable version"
        );
        view.warm_lookup(b"E", b"u", b"a1").unwrap();
        view.warm_primary(b"E", 1).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_prefix_warm_is_memoized_per_snapshot_and_reads_the_whole_entity() {
        let dir = tmp_dir("scanwarm");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"t1", &[
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 1, doc: b"{\"order_no\":\"O1\",\"movement_no\":\"M1\"}".to_vec() },
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 2, doc: b"{\"order_no\":\"O2\",\"movement_no\":\"M2\"}".to_vec() },
        ]).unwrap();
        e.publish_tile(dir.join("m1.tile"), 1).unwrap();

        let mut state = ScanWarmState { tiles: Arc::new(Vec::new()), warmed: HashSet::new() };
        let view = e.tile_read_view();
        assert!(view.warm_scan_prefix(b"StockMovement", &mut state).unwrap(), "first warm does work");
        assert!(!view.warm_scan_prefix(b"StockMovement", &mut state).unwrap(), "same snapshot is memoized");

        // A refreshed snapshot is a new Arc identity, so the memo resets and the
        // (now slightly newer) entity is warmed again exactly once.
        let view2 = e.tile_read_view();
        assert!(view2.warm_scan_prefix(b"StockMovement", &mut state).unwrap(), "a new snapshot re-warms");
        assert!(!view2.warm_scan_prefix(b"StockMovement", &mut state).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
