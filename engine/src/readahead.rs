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

    /// True when `other` names the same immutable tile set as `self` (same tile
    /// count and every `Arc<Tile>` pointer-identical). Kept tiles keep their
    /// `Arc<Tile>` across a publish/compaction, while replaced or newly adopted
    /// tiles get a fresh one, so pointer identity detects a real snapshot change
    /// without touching file contents.
    fn same_tile_set(&self, other: &TileReadView) -> bool {
        if self.tiles.len() != other.tiles.len() { return false; }
        self.tiles.iter().zip(other.tiles.iter()).all(|(a, b)| Arc::ptr_eq(a, b))
    }

    /// Tile-only walk of an entity's whole primary prefix (the exact prefix
    /// [`crate::engine::Engine::scan_primary`] walks, minus the serving
    /// memtable), yielding `(handle, csn, doc bytes)` and returning how many rows
    /// it visited.
    ///
    /// This is the TILE HALF of the derived index's hybrid source
    /// ([`crate::index::spawn_rebuild`]): the builder thread calls it, so the cold
    /// device stream a tiled region represents never crosses the pacing thread.
    /// It is not a read: a caller must merge it with the serving memtable through
    /// the newest-wins rule (an empty `doc` is a tombstone and must be yielded, or
    /// an older live version under it would be resurrected), and an index built
    /// from this walk alone misses every row still in the memtable - the defect in
    /// ticket 83.
    ///
    /// Within one tile a handle's newest version comes first, so a repeat of the
    /// handle inside THAT tile is skipped. The dedupe does not carry across tiles: a
    /// handle can hold a version in several tiles, its newest version may be in any
    /// of them, and this walk is not a merge, so every tile's version is yielded and
    /// the caller's newest-wins merge decides which one wins.
    pub fn scan_primary(
        &self,
        entity: &[u8],
        mut visit: impl FnMut(u64, u64, &[u8]) -> io::Result<()>,
    ) -> io::Result<u64> {
        let mut prefix = keys::primary_prefix_with(self.key_encoding, entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        let mut rows = 0u64;
        for t in self.tiles.iter() {
            if !t.could_contain_prefix(&prefix) { continue; }
            // Per-tile, not per-walk: tiles are not in key order relative to each
            // other over a handle's versions, so a carry would let one tile's key
            // order hide a newer version in the next one.
            let mut last_handle: Option<u64> = None;
            let mut cursor = t.prefix_cursor(&prefix)?;
            while let Some((key, value)) = cursor.next()? {
                let Some((key_entity, handle, csn)) = keys::decode_primary(key) else { continue; };
                if key_entity != entity { continue; }
                if last_handle == Some(handle) { continue; }
                last_handle = Some(handle);
                rows += 1;
                visit(handle, csn, value)?;
            }
        }
        Ok(rows)
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
        let _ = self.warm_prefix_range(&prefix)?;
        Ok(true)
    }

    /// Walk a whole key prefix across every tile that could contain it, reading
    /// each block so it lands in the page cache. Returns how many entries the
    /// walk yielded: the startup warm-up reports it as `index_entries`, while the
    /// read-ahead worker discards it.
    fn warm_prefix_range(&self, prefix: &[u8]) -> io::Result<u64> {
        let mut entries = 0u64;
        for t in self.tiles.iter() {
            if !t.could_contain_prefix(prefix) { continue; }
            let mut cursor = t.prefix_cursor(prefix)?;
            while cursor.next()?.is_some() { entries += 1; }
        }
        Ok(entries)
    }

    /// One bounded synchronous warm-up pass at run start (WP-R): read the
    /// movement region from the authoritative source and, when the derived
    /// `order_no` index is adopted, page in the index tile's blocks, so the first
    /// timed window's movement scans are page-cache hits instead of cold page-ins
    /// on the pacer. Purely observational: the walked rows are discarded and
    /// never feed a decision.
    ///
    /// `merged_scan` is the merged memtable + tile walk
    /// ([`crate::engine::Engine::scan_primary_with_csn`]) and reports how many
    /// live rows it visited. The view is tile-only, so it cannot count those rows
    /// itself: on the workloads this serves the movement region is entirely in
    /// the serving memtable, and a tile-only walk would report zero. The caller
    /// runs this once before the timed windows begin (off the timed clock), so it
    /// is a bounded startup read, never a second full scan on the request path.
    pub fn warm_startup(
        &self,
        merged_scan: impl FnOnce(&mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
    ) -> io::Result<StartupWarm> {
        // The authoritative movement scan's source reads every tile block the
        // fallback touches (and the rows still in the memtable), so this single
        // pass both pages the region in and counts what it covers.
        let scan_entries = merged_scan(&mut |_, _, _| Ok(()))?;
        // The derived index tile is keyed under `U|StockMovement|order_no`; walking
        // that whole prefix pages in every index block the point probe could touch.
        // `unique_prefix_with` appends a length-delimited value, so strip the empty
        // value's 4-byte length to leave the shared field prefix
        // (`U | entity | 0 | field | 0`) every index entry starts with.
        let mut index_prefix = keys::unique_prefix_with(
            self.key_encoding, crate::index::INDEX_ENTITY, crate::index::INDEX_FIELD, b"")?;
        index_prefix.truncate(index_prefix.len() - 4);
        let index_entries = self.warm_prefix_range(&index_prefix)?;
        Ok(StartupWarm { scan_entries, index_entries })
    }
}

/// What the startup warm-up pass read (page-cache priming only; the rows are
/// discarded). Entry counts are the observable contract, not a correctness bound.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StartupWarm {
    pub scan_entries: u64,
    pub index_entries: u64,
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

/// Adopt a fresh tile snapshot in the worker. When the tile set actually
/// changed, every block the worker warmed ahead belongs to the old snapshot
/// (replaced tiles keep their open fds but the authoritative read now probes
/// the new files), so rewind the warm cursor to the pacer's current position to
/// re-warm the lead against the new tiles. A refresh that carries the same tile
/// set leaves the cursor alone (no redundant re-warm every window).
fn adopt_snapshot(
    view: &mut TileReadView,
    position: &AtomicU64,
    warm_index: &mut u64,
    next: TileReadView,
) {
    if !view.same_tile_set(&next) {
        *warm_index = position.load(Ordering::Relaxed);
    }
    *view = next;
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
        // Adopt any pending tile snapshot before warming against it. A snapshot
        // whose tile set changed rewinds the cursor so the lead is re-warmed
        // against the new tiles (the old lead warms stale, replaced blocks).
        loop {
            match rx.try_recv() {
                Ok(Msg::Refresh(v)) => adopt_snapshot(&mut view, &position, &mut warm_index, v),
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
            Ok(Msg::Refresh(v)) => adopt_snapshot(&mut view, &position, &mut warm_index, v),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ScanWarmState, StartupWarm};
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

    /// Build the derived `order_no` index from the engine's merged memtable + tile
    /// view, the source the driver passes in production.
    fn build_index(e: &Engine, dir: &std::path::Path) -> crate::index::OrderNoIndexBuild {
        crate::index::build_order_no_index(
            dir,
            e.key_encoding(),
            |visit| e.scan_primary_with_csn(crate::index::INDEX_ENTITY, visit),
            crate::index::movement_region_rows(e),
            0,
            "order_no.idx",
        )
        .unwrap()
    }

    /// The startup warm-up over the engine's merged view, as the driver runs it.
    fn warm_startup(e: &Engine) -> StartupWarm {
        e.tile_read_view()
            .warm_startup(|visit| e.scan_primary_with_csn(crate::index::INDEX_ENTITY, visit))
            .unwrap()
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

    /// WP-P follow-on: `Engine::tile_read_view` must include the adopted WP-M
    /// `order_no` index tile, so the read-ahead worker warms the index point
    /// probe the authoritative movement scan performs. Without it the probe is a
    /// cold page-in on the pacer (the residual WP-P found at rung 50). This
    /// fails against the pre-change behaviour, where the view held only
    /// `self.tiles` and the index tile was absent.
    #[test]
    fn tile_read_view_includes_the_adopted_order_no_index() {
        let dir = tmp_dir("indexwarm");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"m", &[
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 1, doc: br#"{"order_no":"O1","movement_no":"M1"}"#.to_vec() },
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 2, doc: br#"{"order_no":"O2","movement_no":"M2"}"#.to_vec() },
        ]).unwrap();
        e.publish_tile(dir.join("p.tile"), 1).unwrap();

        // Before adoption the warm view has no `order_no` index probe.
        let view_without = e.tile_read_view();
        assert_eq!(view_without.unique_handle_tiles(b"StockMovement", b"order_no", b"O1").unwrap(), None,
            "no index is adopted, so the warm view has no order_no index probe");

        // Build and adopt the derived index (off-path in production; inline here).
        // This test feeds the builder the tile-only view: both sources resolve the
        // same rows when every row is tile-resident and none is a tombstone.
        let view = e.tile_read_view();
        let build = crate::index::build_order_no_index(
            &dir,
            e.key_encoding(),
            |visit| view.scan_primary(crate::index::INDEX_ENTITY, visit),
            crate::index::movement_region_rows(&e),
            0,
            "order_no.idx",
        )
        .unwrap();
        std::fs::File::open(&dir).unwrap().sync_all().unwrap();
        e.adopt_order_no_index(build.index);

        // After adoption the warm view resolves the index probe from the index
        // tile, so the read-ahead worker warms the exact block the scan probes.
        let view_with = e.tile_read_view();
        assert_eq!(view_with.unique_handle_tiles(b"StockMovement", b"order_no", b"O1").unwrap(), Some(1),
            "the adopted index tile is part of the warm view");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WP-R: the bounded synchronous startup warm-up must walk the movement scan
    /// prefix AND, once the `order_no` index is adopted, the index tile, so the
    /// first timed window's scans are page-cache hits. This pins the eager warm
    /// the driver performs before the timed windows; before this package there
    /// was no such pass (the index was adopted lazily at the first boundary and
    /// the worker's view lacked it for another window).
    #[test]
    fn warm_startup_walks_the_scan_prefix_and_the_adopted_index() {
        let dir = tmp_dir("startupwarm");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"m", &[
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 1, doc: br#"{"order_no":"O1","movement_no":"M1"}"#.to_vec() },
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 2, doc: br#"{"order_no":"O2","movement_no":"M2"}"#.to_vec() },
        ]).unwrap();
        e.publish_tile(dir.join("p.tile"), 1).unwrap();

        // Without an adopted index the pass walks the region but no index rows.
        let warm_before = warm_startup(&e);
        assert!(warm_before.scan_entries >= 2, "the movement rows are counted: {}", warm_before.scan_entries);
        assert_eq!(warm_before.index_entries, 0, "no index tile is present yet");

        // Adopt the derived index; the pass must now also walk the index tile.
        let build = build_index(&e, &dir);
        std::fs::File::open(&dir).unwrap().sync_all().unwrap();
        e.adopt_order_no_index(build.index);

        let warm_after = warm_startup(&e);
        assert!(warm_after.scan_entries >= 2, "the movement rows are still counted");
        assert!(warm_after.index_entries >= 2, "the adopted index tile's entries are walked: {}", warm_after.index_entries);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A region whose rows are all still in the serving memtable has no tiles, so
    /// a tile-only warm reported `scan_entries = 0` and could not warm the region
    /// the request path actually scans. The warm now reports what the merged
    /// memtable + tile source visits.
    #[test]
    fn warm_startup_counts_rows_that_live_only_in_the_memtable() {
        let dir = tmp_dir("startupmemtable");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"m", &[
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 1, doc: br#"{"order_no":"O1","movement_no":"M1"}"#.to_vec() },
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 2, doc: br#"{"order_no":"O2","movement_no":"M2"}"#.to_vec() },
        ]).unwrap();
        assert!(e.active_tile_regions().is_empty(), "the movements are still only in the memtable");

        let warm = warm_startup(&e);
        assert_eq!(warm.scan_entries, 2, "the warm counts the memtable rows it reads");
        assert_eq!(warm.index_entries, 0, "no index tile is present");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// WP-W2: when a compaction/publish replaces tiles, the worker must re-warm
    /// the lead it already warmed against the OLD snapshot, otherwise the first
    /// reads of the replacement tiles are cold page-ins on the pacer (the rung-50
    /// residual). This pins the snapshot-change detection and the cursor rewind;
    /// before this package the worker advanced its warm cursor past the stale
    /// lead and never re-warmed it.
    #[test]
    fn adopt_snapshot_rewinds_the_warm_cursor_only_when_the_tile_set_changes() {
        use super::adopt_snapshot;
        use std::sync::atomic::AtomicU64;

        let dir = tmp_dir("rewind");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"m", &[
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 1, doc: br#"{"order_no":"O1","movement_no":"M1"}"#.to_vec() },
            Op::PutDoc { entity: b"StockMovement".to_vec(), handle: 2, doc: br#"{"order_no":"O2","movement_no":"M2"}"#.to_vec() },
        ]).unwrap();
        e.publish_tile(dir.join("p.tile"), 1).unwrap();

        // Two views of the same tile set are pointer-identical.
        let no_index = e.tile_read_view();
        assert!(no_index.same_tile_set(&e.tile_read_view()), "same tile set is detected");

        // Adopting the index appends a NEW tile, so the tile set changes.
        let build = build_index(&e, &dir);
        std::fs::File::open(&dir).unwrap().sync_all().unwrap();
        e.adopt_order_no_index(build.index);
        let with_index = e.tile_read_view();
        assert!(!no_index.same_tile_set(&with_index), "the adopted index tile changes the set");

        let position = AtomicU64::new(100);
        let mut warm_index = 164u64;
        let mut view = no_index;

        // A real snapshot change rewinds the cursor to the pacer's position so
        // the already-warmed lead is re-warmed against the new tiles.
        adopt_snapshot(&mut view, &position, &mut warm_index, with_index);
        assert_eq!(warm_index, 100, "a changed tile set rewinds the warm cursor");

        // A refresh that carries the SAME tile set leaves the cursor alone (no
        // redundant re-warm every window boundary).
        warm_index = 164;
        let with_index_same = e.tile_read_view();
        assert!(view.same_tile_set(&with_index_same));
        adopt_snapshot(&mut view, &position, &mut warm_index, with_index_same);
        assert_eq!(warm_index, 164, "an unchanged tile set keeps the warm cursor");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
