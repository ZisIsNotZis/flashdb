// WP-M: the derived `order_no -> StockMovement handle` secondary access path is
// never on the correctness path — a hit is cross-checked, a miss or an absent or
// a stale index always falls back to the authoritative full-entity scan.

use flashdb_engine::engine::{Engine, Op, RegionMode};
use flashdb_engine::index::{
    build_order_no_index, movement_region_rows, rebuild_trigger, IndexProduct, IndexRebuildSlot,
    MemtableRows, MemtableSnapshot, OrderNoIndexBuild, RebuildResult, RegionRows, INDEX_ENTITY,
    INDEX_FIELD, INLINE_SNAPSHOT_MAX_BYTES,
};
use flashdb_engine::request::execute_request;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "flashdb-order-no-index-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        name
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn movement(handle: u64, order_no: &str, movement_no: &str) -> Op {
    Op::PutDoc {
        entity: INDEX_ENTITY.to_vec(),
        handle,
        doc: serde_json::to_vec(&json!({
            "order_no": order_no, "movement_no": movement_no, "delta": -1
        }))
        .unwrap(),
    }
}

/// Build the index from the engine's merged memtable + tile view: the source the
/// driver's startup build uses, so rows still in the serving memtable are covered.
/// The coverage floor is 0 here, which claims nothing beyond what the entries
/// themselves prove, so a test can bind the entry-derived maximum; the driver
/// passes `engine.csn()` (see `coverage_floor_covers_tombstones_and_superseded_versions`).
fn build(e: &Engine, dir: &std::path::Path) -> std::io::Result<OrderNoIndexBuild> {
    build_order_no_index(
        dir,
        e.key_encoding(),
        |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
        movement_region_rows(e),
        0,
        "index-order-no.tile",
    )
}

fn scan_req(order_no: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{"blocks":{{"q":{{"find":{{"StockMovement":{{"where":{{"order_no":"{order_no}"}}}},"limit":100,"order":["movement_no"]}}}}}},"class":{{"durability":"batched","max_staleness":5}},"id":"m"}}"#
    ))
    .unwrap()
}

/// Movements committed to tiles, then indexed: the scan short-circuits to a point
/// lookup and returns the right document.
#[test]
fn movement_scan_uses_order_no_index_when_present() {
    let dir = tmp_dir("present");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    for (i, order) in ["O00000001", "O00000002", "O00000003"].iter().enumerate() {
        e.commit_block(format!("m-{i}").as_bytes(), &[movement(i as u64 + 1, order, &format!("M{i}"))])
            .unwrap();
    }
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert!(e.serving_memtable_entries() == 0, "movements are now tile-resident");

    let build = build(&e, &dir).unwrap();
    assert_eq!(build.index.entries(), 3);
    e.adopt_order_no_index(build.index);
    assert!(e.has_order_no_index());

    // Hit: the index resolves the handle and the scan returns exactly one row.
    let out = execute_request(&mut e, &scan_req("O00000002")).unwrap();
    assert_eq!(out["q"].status, "ok");
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["order_no"], "O00000002");

    // Miss: the index holds no entry for an unknown order, so the authoritative
    // scan runs and returns empty.
    let out = execute_request(&mut e, &scan_req("O00009999")).unwrap();
    assert_eq!(out["q"].status, "empty");

    // The index lookup itself resolves the right handle.
    assert_eq!(e.lookup_movement_by_order_no(b"O00000003").unwrap(), Some(3));
    assert_eq!(e.lookup_movement_by_order_no(b"O00009999").unwrap(), None);

    std::fs::remove_dir_all(&dir).unwrap();
}

/// No index adopted: the scan must still answer correctly (the missing-access-path
/// fallback).
#[test]
fn movement_scan_falls_back_to_full_scan_when_index_absent() {
    let dir = tmp_dir("absent");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"m0", &[movement(1, "O00000001", "M0")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();

    // No index adopted.
    assert!(!e.has_order_no_index());
    let out = execute_request(&mut e, &scan_req("O00000001")).unwrap();
    assert_eq!(out["q"].status, "ok");
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows[0]["order_no"], "O00000001");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A movement written AFTER the index build is not in the index; the stale index
/// must fall back to the scan (which sees the memtable) and still find it.
#[test]
fn movement_scan_falls_back_for_a_movement_newer_than_the_index() {
    let dir = tmp_dir("stale");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"m0", &[movement(1, "O00000001", "M0")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();

    let build = build(&e, &dir).unwrap();
    assert_eq!(build.index.entries(), 1);
    e.adopt_order_no_index(build.index);

    // A new movement lands in the serving memtable, not the index.
    e.commit_block(b"m1", &[movement(2, "O00000002", "M1")]).unwrap();
    assert_eq!(e.lookup_movement_by_order_no(b"O00000002").unwrap(), None);

    let out = execute_request(&mut e, &scan_req("O00000002")).unwrap();
    assert_eq!(out["q"].status, "ok");
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["order_no"], "O00000002");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The index builder rejects a document whose `order_no` field is not a string,
/// rather than silently skipping or mis-indexing it.
#[test]
fn index_builder_rejects_a_non_string_order_no() {
    let dir = tmp_dir("badfield");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"bad", &[Op::PutDoc {
        entity: INDEX_ENTITY.to_vec(),
        handle: 1,
        doc: serde_json::to_vec(&json!({"order_no": 42, "movement_no": "M0"})).unwrap(),
    }])
    .unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    let result = build(&e, &dir);
    assert_eq!(result.as_ref().err().map(|e| e.kind()), Some(std::io::ErrorKind::InvalidData));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Ticket 83: on an append-heavy region every movement is still in the serving
/// memtable, so a tile-only build wrote an empty sidecar (`entries = 0`,
/// `coverage_csn = 0`) and every scan silently fell back to the full scan. The
/// build must cover the memtable rows through the merged view.
///
/// The handles are committed OUT OF ORDER (3, then 1, then 2), so the scan visits
/// them in handle order with the CSNs 2, 3, 1: the covered CSN is the maximum over
/// the rows, which "the last row's csn" (1) would fail.
#[test]
fn index_build_covers_rows_that_live_only_in_the_memtable() {
    let dir = tmp_dir("memtable");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    let orders = [(3u64, "O00000003"), (1, "O00000001"), (2, "O00000002")];
    for (i, (handle, order)) in orders.iter().enumerate() {
        e.commit_block(format!("m-{i}").as_bytes(), &[movement(*handle, order, &format!("M{i}"))])
            .unwrap();
    }
    assert!(e.active_tile_regions().is_empty(), "no tile exists, so a tile-only build sees zero rows");

    let build = build(&e, &dir).unwrap();
    assert_eq!(build.index.entries(), orders.len() as u64, "one entry per memtable row");
    assert_eq!(build.index.coverage_csn(), 3, "the covered CSN is the max row CSN, not the last row's");
    e.adopt_order_no_index(build.index);

    assert_eq!(e.lookup_movement_by_order_no(b"O00000002").unwrap(), Some(2));
    let out = execute_request(&mut e, &scan_req("O00000003")).unwrap();
    assert_eq!(out["q"].status, "ok");
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["order_no"], "O00000003");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The non-emptiness signal the loud-error seam reads is derived from engine
/// structures, not from the scan: an empty engine reports none, rows still in the
/// serving memtable report the memtable, and a published region tile reports the
/// tile.
#[test]
fn region_signal_reads_the_tile_before_the_memtable() {
    let dir = tmp_dir("signal");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    assert_eq!(movement_region_rows(&e), RegionRows::Empty, "an empty engine holds no rows");
    e.commit_block(b"m0", &[movement(1, "O00000001", "M0")]).unwrap();
    assert_eq!(movement_region_rows(&e), RegionRows::InMemtable { rows: 1 },
        "the only row is still in the serving memtable");
    e.set_region_mode(RegionMode::PerRegion);
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert_eq!(movement_region_rows(&e), RegionRows::InTiles,
        "the published tile's first key names the region");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// An updated handle and a deleted handle: the index holds exactly one entry per
/// LIVE row. The re-versioned handle contributes its newest order_no only (its
/// superseded version is not indexed), the deleted handle is absent, and
/// `coverage_csn` is the max CSN over the live versions - not the tombstone's,
/// which is the largest CSN in the region.
#[test]
fn index_holds_one_entry_per_live_row_across_versions_and_deletes() {
    let dir = tmp_dir("versions");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    // csn 1: handles 1 and 2.
    e.commit_block(b"v0", &[movement(1, "O1", "M1"), movement(2, "O2", "M2")]).unwrap();
    // csn 2: handle 1 is re-versioned under a new order_no.
    e.commit_block(b"v1", &[movement(1, "O1b", "M1")]).unwrap();
    // csn 3: handle 3 appears, so the region has a row newer than BOTH live rows.
    e.commit_block(b"v2", &[movement(3, "O3", "M3")]).unwrap();
    // csn 4: handle 3 is deleted. The max CSN over LIVE rows stays 2.
    e.commit_block(b"v3", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 3 }]).unwrap();
    assert_eq!(e.csn(), 4, "the tombstone is the newest version in the region");

    let build = build(&e, &dir).unwrap();
    assert_eq!(build.index.entries(), 2, "one entry per live row: handle 1's newest and handle 2");
    assert_eq!(build.index.coverage_csn(), 2, "the max csn over LIVING versions, not the tombstone's");
    e.adopt_order_no_index(build.index);

    assert_eq!(e.lookup_movement_by_order_no(b"O1b").unwrap(), Some(1), "the newest version is indexed");
    assert_eq!(e.lookup_movement_by_order_no(b"O1").unwrap(), None, "the superseded version is not");
    assert_eq!(e.lookup_movement_by_order_no(b"O2").unwrap(), Some(2));
    assert_eq!(e.lookup_movement_by_order_no(b"O3").unwrap(), None, "the deleted handle is absent");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The loud-error seam that guards ticket 83: a build whose source visits NOTHING
/// while the region holds rows must fail instead of writing an empty sidecar. The
/// scan's own row count cannot witness this (a source that misses the region
/// reports zero rows and produces zero entries, which is exactly what an empty
/// region looks like), so the build is given the engine-derived non-emptiness
/// signal and must fail on it. This test supplies the ticket-83 state - rows only
/// in the serving memtable, a source that visits none - and a real engine, so the
/// signal comes from real engine structures, not from the test.
#[test]
fn index_build_fails_when_the_source_visits_nothing_for_a_region_that_holds_rows() {
    let dir = tmp_dir("emptysource");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"m", &[movement(1, "O00000001", "M0"), movement(2, "O00000002", "M1")]).unwrap();
    assert!(e.active_tile_regions().is_empty(), "no tile exists, so only the memtable holds the rows");
    let region_rows = movement_region_rows(&e);
    assert_ne!(region_rows, RegionRows::Empty, "the engine reports the region holds rows");

    let result = build_order_no_index(
        &dir,
        e.key_encoding(),
        |_visit| Ok(0),
        region_rows,
        0,
        "index-order-no.tile",
    );
    let err = result.err().expect("an empty build over a non-empty region is a defect, not an empty index");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("produced 0 entries"), "names the produced entries: {message}");
    assert!(message.contains("visited 0 live"), "names the visited rows: {message}");
    assert!(message.contains("the serving memtable holds 2 live rows of this region"), "names the signal: {message}");
    assert!(!dir.join("index-order-no.tile").exists(), "a rejected build writes no sidecar");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The `U|StockMovement.order_no` key spelling is what the index probes.
#[test]
fn index_field_and_entity_names_are_the_observed_predicate() {
    assert_eq!(INDEX_ENTITY, b"StockMovement");
    assert_eq!(INDEX_FIELD, b"order_no");
}

// --- WP-8: the hybrid rebuild (memtable half inline, tile half off-thread) -----

/// The bounded inline memtable snapshot of the region above `after_csn`, as the
/// driver takes it.
fn memtable_rows(e: &Engine, after_csn: u64, max_bytes: usize) -> MemtableRows {
    match e.snapshot_primary_memtable(INDEX_ENTITY, after_csn, max_bytes).unwrap() {
        MemtableSnapshot::Copied(rows) => rows,
        MemtableSnapshot::OverBound(over) => panic!("the region does not fit the bound: {over}"),
    }
}

/// Wait (bounded) for the slot's one job and return its product.
fn settle(slot: &mut IndexRebuildSlot) -> RebuildResult {
    for _ in 0..400 {
        if let Some(finished) = slot.try_recv() {
            return finished.result.expect("the index job succeeds");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the index job never finished");
}

/// The base index inside a finished product.
fn base_of(result: &RebuildResult) -> &flashdb_engine::index::OrderNoIndex {
    match &result.product {
        IndexProduct::Index(build) => &build.index,
        IndexProduct::Delta(_) => panic!("the job built a delta, not a base index"),
    }
}

/// One full hybrid base build, exactly as the driver schedules it: the tile half
/// runs on the builder thread, the memtable half was snapshotted here.
fn rebuild(e: &Engine, dir: &std::path::Path) -> RebuildResult {
    let mut slot = IndexRebuildSlot::new();
    slot.schedule_base(
        dir.to_path_buf(),
        e.key_encoding(),
        e.primary_tile_view(),
        |view, visit| view.scan_primary(INDEX_ENTITY, visit),
        memtable_rows(e, 0, INLINE_SNAPSHOT_MAX_BYTES),
        movement_region_rows(e),
        "scan_demand",
    )
    .unwrap();
    settle(&mut slot)
}

/// One delta build over the rows above `after_csn`, as the driver schedules it.
fn rebuild_delta(e: &Engine, after_csn: u64) -> RebuildResult {
    let mut slot = IndexRebuildSlot::new();
    slot.schedule_delta(
        e.order_no_index_base_coverage_csn().expect("a base is adopted"),
        e.primary_tile_view(),
        move |view, visit| view.scan_primary_after(INDEX_ENTITY, after_csn, visit),
        memtable_rows(e, after_csn, INLINE_SNAPSHOT_MAX_BYTES),
        "scan_demand",
    )
    .unwrap();
    settle(&mut slot)
}

/// The rebuild's merge is the scan's newest-wins rule, not a second one: the version
/// still in the serving memtable is what the index answers, the version it supersedes
/// is not indexed, and a memtable tombstone removes the handle even though an older
/// live version of it sits in a tile.
#[test]
fn rebuilt_index_takes_the_memtable_version_and_drops_a_tombstoned_handle() {
    let dir = tmp_dir("hybrid");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // csn 1: handle 5 as order A, handle 6 as order C - published, so tile-resident.
    e.commit_block(b"v0", &[movement(5, "A", "M5"), movement(6, "C", "M6")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "both versions are tile-resident");
    // csn 2: handle 5 re-versioned to order B. csn 3: handle 6 deleted. Both live only
    // in the serving memtable, which is the state ticket 83 was measured in.
    e.commit_block(b"v1", &[movement(5, "B", "M5")]).unwrap();
    e.commit_block(b"v2", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 6 }]).unwrap();

    let result = rebuild(&e, &dir);
    assert_eq!(base_of(&result).entries(), 1, "one live handle: 5 at its newest version");
    assert_eq!(result.memtable_rows, 2, "one snapshot row per memtable handle");
    assert_eq!(result.stale_rows, 3,
        "two tile versions are superseded by the memtable and the tombstone removes its handle");
    assert_eq!(base_of(&result).coverage_csn(), e.csn(),
        "coverage is the snapshot's CSN, past the tombstone no entry records");
    assert_eq!(base_of(&result).lookup(b"B").unwrap(), Some(5), "the memtable version wins");
    assert_eq!(base_of(&result).lookup(b"A").unwrap(), None, "the superseded tile version is not indexed");
    assert_eq!(base_of(&result).lookup(b"C").unwrap(), None, "the tombstoned handle is gone");

    // Adopted, the rebuilt index answers the live state and the read path falls back
    // for the handle that no longer exists.
    let index = match result.product {
        IndexProduct::Index(build) => build.index,
        IndexProduct::Delta(_) => panic!("the base build produced a base"),
    };
    e.adopt_order_no_index(index);
    assert_eq!(e.lookup_movement_by_order_no(b"B").unwrap(), Some(5));
    assert_eq!(e.lookup_movement_by_order_no(b"A").unwrap(), None);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The coverage floor is what makes `coverage_csn` a statement about the engine state
/// instead of about the survivors: a tombstone produces no entry, so the newest entry
/// CSN alone would leave the coverage below the state that was actually covered and a
/// rebuild trigger reading it would rebuild the same region forever.
#[test]
fn coverage_floor_covers_the_state_a_tombstone_hides() {
    let dir = tmp_dir("floor");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"v0", &[movement(1, "O1", "M1")]).unwrap();
    let live_only = build_order_no_index(
        &dir,
        e.key_encoding(),
        |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
        movement_region_rows(&e),
        0,
        "floor-0.tile",
    )
    .unwrap();
    assert_eq!(live_only.index.coverage_csn(), 1, "without a floor the coverage is the newest entry's CSN");

    // csn 2 deletes the only handle: no entry, and the state still moved.
    e.commit_block(b"v1", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 1 }]).unwrap();
    let floored = build_order_no_index(
        &dir,
        e.key_encoding(),
        |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
        movement_region_rows(&e),
        e.csn(),
        "floored.tile",
    )
    .unwrap();
    assert_eq!(floored.index.entries(), 0, "a deleted region indexes nothing");
    assert_eq!(floored.index.coverage_csn(), 2, "and the floor carries the state it covered");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The inline snapshot is bounded by the bytes it holds AND refuses before it copies,
/// so the thread that schedules a rebuild never pays for a region over the bound. The
/// charge is the documents plus the per-row tuple, which is what the copy owns.
#[test]
fn the_inline_memtable_snapshot_is_bounded_and_copies_nothing_over_the_bound() {
    let dir = tmp_dir("bound");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"v0", &[movement(1, "O1", "M1"), movement(2, "O2", "M2"), movement(3, "O3", "M3")])
        .unwrap();

    let over = match e.snapshot_primary_memtable(INDEX_ENTITY, 0, 10).unwrap() {
        MemtableSnapshot::OverBound(over) => over,
        MemtableSnapshot::Copied(_) => panic!("three movements do not fit 10 document bytes"),
    };
    assert_eq!((over.rows, over.live_rows, over.bound), (3, 3, 10));
    assert!(over.bytes > 10, "the refusal names what the region holds: {over}");
    let reason = over.to_string();
    assert!(reason.contains("would exceed the 10-byte bound"), "the reason is stated: {reason}");
    assert!(reason.contains("3 handles, 3 live"), "and counts the region: {reason}");

    let copied = memtable_rows(&e, 0, INLINE_SNAPSHOT_MAX_BYTES);
    assert_eq!(copied.rows.len(), 3, "one row per handle");
    assert_eq!(copied.live_rows, 3);
    assert_eq!(copied.csn, e.csn(), "the snapshot states the CSN it speaks for");
    assert_eq!(
        copied.bytes,
        copied.rows.iter().map(|(_, _, doc)| doc.len()).sum::<usize>()
            + copied.rows.len() * flashdb_engine::index::MEMTABLE_ROW_OVERHEAD_BYTES,
        "the bound is charged against the documents plus one owned row per handle"
    );
    // The filter is above the coverage: the same rows are copied for `after_csn` 0 and
    // nothing is copied for a coverage at the newest committed version.
    assert_eq!(memtable_rows(&e, 0, INLINE_SNAPSHOT_MAX_BYTES).rows.len(), 3);
    assert_eq!(memtable_rows(&e, e.csn(), INLINE_SNAPSHOT_MAX_BYTES).rows.len(), 0,
        "a snapshot above the coverage holds the interval's rows, not the region's");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Over a TILED region the tile half is a cold device stream. It must run on the
/// builder thread and never on the thread that schedules the rebuild - that is the
/// WP-1 exposure `movement_rows` left on the pacer, and the seam the slot's base
/// schedule takes is what makes it observable.
#[test]
fn a_rebuild_reads_no_tile_on_the_thread_that_schedules_it() {
    let dir = tmp_dir("thread");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"v0", &[movement(1, "O1", "M1"), movement(2, "O2", "M2")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "the tile half is the only source of these rows");

    let seller = std::thread::current().id();
    let walked_on: std::sync::Arc<std::sync::Mutex<Vec<std::thread::ThreadId>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let record = std::sync::Arc::clone(&walked_on);
    let mut slot = IndexRebuildSlot::new();
    slot.schedule_base(
        dir.to_path_buf(),
        e.key_encoding(),
        e.primary_tile_view(),
        move |view, visit| {
            record.lock().unwrap().push(std::thread::current().id());
            view.scan_primary(INDEX_ENTITY, visit)
        },
        memtable_rows(&e, 0, INLINE_SNAPSHOT_MAX_BYTES),
        movement_region_rows(&e),
        "scan_demand",
    )
    .unwrap();
    let result = settle(&mut slot);
    let threads = walked_on.lock().unwrap().clone();
    assert_eq!(threads.len(), 1, "the tile half is walked exactly once");
    assert_ne!(threads[0], seller, "and not on the thread that scheduled the rebuild");
    assert_eq!(base_of(&result).entries(), 2, "the tile half still covered the region");
    assert_eq!(base_of(&result).lookup(b"O2").unwrap(), Some(2));
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The witness's tile arm reads a tile's FIRST key, which is exact only for a
/// region-partitioned tile. Under the default whole-keyspace layout it reports that
/// key's region and nothing about the rest of the tile, so it is wrong in either
/// direction - and this binds both, because the loud guard rests on it. Reads stay
/// correct either way: the build's source walks the tiles themselves.
#[test]
fn region_witness_reads_a_global_tiles_first_key_in_both_directions() {
    // False negative: a whole-keyspace tile whose lowest key belongs to another
    // region hides the movement rows from the witness.
    let dir = tmp_dir("globalunder");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"c0", &[Op::PutDoc {
        entity: b"Customer".to_vec(), handle: 1, doc: br#"{"email":"a@b"}"#.to_vec(),
    }]).unwrap();
    e.commit_block(b"m0", &[movement(1, "O1", "M1")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert_eq!(e.region_mode(), RegionMode::Global, "the default layout writes one whole-keyspace tile");
    assert_eq!(e.serving_memtable_entries(), 0, "the movement is tile-resident");
    assert_eq!(e.active_tile_regions(), vec![Some("P|Customer".to_string())],
        "the tile reports its lowest key's region");
    assert_eq!(movement_region_rows(&e), RegionRows::Empty,
        "so the witness cannot see the tile-resident movement (a false negative)");
    let mut visited = 0u64;
    let rows = e.scan_primary_with_csn(INDEX_ENTITY, |_, _, _| { visited += 1; Ok(()) }).unwrap();
    assert_eq!((rows, visited), (1, 1), "the merged source still serves it, which keeps reads correct");
    std::fs::remove_dir_all(&dir).unwrap();

    // False positive: a whole-keyspace tile whose lowest key IS a movement names the
    // region, whether or not it holds a live movement row.
    let dir = tmp_dir("globalover");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"m0", &[movement(1, "O1", "M1")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    assert_eq!(e.active_tile_regions(), vec![Some("P|StockMovement".to_string())]);
    assert_eq!(movement_region_rows(&e), RegionRows::InTiles);
    // ... even after the row is deleted, so this arm cannot tell live rows from
    // tombstones: a fully deleted region refuses an empty build instead of adopting it.
    e.commit_block(b"m1", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 1 }]).unwrap();
    assert_eq!(movement_region_rows(&e), RegionRows::InTiles);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The rebuild rule itself: moved state is necessary, a reason is required, and a lag
/// the caller has not asked to honour schedules nothing.
#[test]
fn rebuild_trigger_needs_moved_state_and_a_reason() {
    assert_eq!(rebuild_trigger(10, 10, true, 4), None, "no lag, nothing to rebuild");
    assert_eq!(rebuild_trigger(10, 9, true, 4), None, "state behind the coverage never triggers");
    assert_eq!(rebuild_trigger(10, 11, true, 4), Some("scan_demand"), "a scanned region is kept current");
    assert_eq!(rebuild_trigger(10, 11, false, 4), None, "a small lag with no demand waits");
    assert_eq!(rebuild_trigger(10, 14, false, 4), Some("stale"), "at the stated bound it fires on its own");
    assert_eq!(rebuild_trigger(10, 14, true, 4), Some("scan_demand"), "demand outranks the bound's reason");
}

/// A tombstone can be tile-resident: a handle deleted after its live version was
/// published lands in a newer tile. The tile walk yields that row precisely so the
/// merge can see it - a walk that dropped tombstones would index the older live
/// version from the previous tile and resurrect a deleted movement.
#[test]
fn rebuilt_index_drops_a_handle_whose_tombstone_is_tile_resident() {
    let dir = tmp_dir("tiletombstone");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // csn 1: handles 1 and 2 live; published, so tile-resident.
    e.commit_block(b"v0", &[movement(1, "A", "M1"), movement(2, "B", "M2")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    // csn 2: handle 1 is deleted and handle 2 re-versioned, then both are published,
    // so the tombstone lives in the newer tile and no memtable row remains.
    e.commit_block(b"v1", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 1 }, movement(2, "B2", "M2")])
        .unwrap();
    e.publish_tile(dir.join("tile-1.tile"), e.csn()).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "everything is tile-resident");

    let result = rebuild(&e, &dir);
    assert_eq!(base_of(&result).entries(), 1, "only handle 2 is live");
    assert_eq!(base_of(&result).lookup(b"B2").unwrap(), Some(2), "at its newest version");
    assert_eq!(base_of(&result).lookup(b"B").unwrap(), None, "the superseded version is not indexed");
    assert_eq!(base_of(&result).lookup(b"A").unwrap(), None,
        "the tile-resident tombstone removed handle 1, so its older live version is not indexed");

    // The authoritative scan agrees: the same rule, so the index and the scan cannot
    // disagree about whether handle 1 exists.
    let mut served = Vec::new();
    e.scan_primary_with_csn(INDEX_ENTITY, |handle, _, _| { served.push(handle); Ok(()) }).unwrap();
    assert_eq!(served, vec![2], "the scan serves exactly the live handle the index names");
    std::fs::remove_dir_all(&dir).unwrap();
}

// --- WP-9: incremental maintenance (durable base tile + in-RAM delta) ----------

/// The answers the read path gives for a set of order numbers, so a test can
/// compare them across a maintenance step.
fn answers(e: &Engine, order_nos: &[String]) -> Vec<Option<u64>> {
    order_nos.iter().map(|o| e.lookup_movement_by_order_no(o.as_bytes()).unwrap()).collect()
}

/// WP-9 acceptance 1, in one sequence: an entry written after the base coverage is
/// answered by the delta without a base rewrite; a delta tombstone hides the base
/// entry; and the merge of base + delta answers identically and drops the delta.
#[test]
fn the_delta_answers_rows_above_the_coverage_and_hides_base_entries_until_the_merge() {
    let dir = tmp_dir("delta");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    // One base block of 1025 movements. The merge threshold is
    // max(INDEX_MERGE_MIN_DELTA_ENTRIES, base entries) = 1025, so one interval of the
    // same size reaches it without committing a second region.
    let base_ops: Vec<Op> = (1..=1025).map(|h| movement(h, &format!("O{h:08}"), "M")).collect();
    e.commit_block(b"base", &base_ops).unwrap();
    let base = build(&e, &dir).unwrap();
    let base_coverage = base.index.base_coverage_csn();
    e.adopt_order_no_index(base.index);
    assert_eq!(e.order_no_index_base_coverage_csn(), Some(base_coverage));
    assert_eq!(e.lookup_movement_by_order_no(b"O00000001").unwrap(), Some(1), "the base answers");

    // The interval: handle 1 deleted, handles 1026..=2050 appended.
    let mut ops = vec![Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 1 }];
    ops.extend((1026..=2050).map(|h| movement(h, &format!("O{h:08}"), "M")));
    e.commit_block(b"interval", &ops).unwrap();

    let delta = rebuild_delta(&e, base_coverage);
    let IndexProduct::Delta(build) = delta.product else { panic!("the interval build produces a delta") };
    assert_eq!(build.delta.live_entries(), 1025, "one entry per appended row");
    assert_eq!(build.delta.entries(), 1026, "plus the handle the interval deleted");
    e.adopt_order_no_delta(build).expect("the delta attaches to the base it was built against");

    assert_eq!(e.order_no_index_coverage_csn(), Some(e.csn()), "the pair covers the interval");
    assert_eq!(e.order_no_index_base_coverage_csn(), Some(base_coverage),
        "and the base was not rewritten: its coverage did not move");
    assert_eq!(e.lookup_movement_by_order_no(b"O00001026").unwrap(), Some(1026),
        "the delta answers a row written after the base coverage");
    assert_eq!(e.lookup_movement_by_order_no(b"O00000001").unwrap(), None,
        "the delta tombstone hides the base entry for the deleted handle");
    assert_eq!(e.lookup_movement_by_order_no(b"O00000002").unwrap(), Some(2),
        "and the untouched rest of the base still answers");

    // The pair is at the merge threshold; the merge answers identically and drops
    // the delta.
    let sample: Vec<String> = (1..=2050).step_by(37).map(|h| format!("O{h:08}")).collect();
    let before = answers(&e, &sample);
    assert!(e.order_no_index_needs_merge(), "1026 delta entries against a 1025-entry base is due");
    let snapshot = e.order_no_index_merge_snapshot().expect("a due merge has a snapshot");
    let mut slot = IndexRebuildSlot::new();
    slot.schedule_merge(dir.to_path_buf(), snapshot, "merge").unwrap();
    let merged = settle(&mut slot);
    let IndexProduct::Index(build) = merged.product else { panic!("the merge produces a base index") };
    assert!(build.output_bytes > 0, "the merge writes the base tile");
    e.adopt_order_no_index(build.index);

    assert_eq!(e.order_no_index_base_coverage_csn(), Some(e.csn()), "the merged base covers the delta");
    assert_eq!(e.order_no_index_delta_entries(), Some(0), "the delta is dropped");
    assert_eq!(e.order_no_index_entries(), Some(2049), "1024 base survivors + 1025 appended rows");
    assert_eq!(answers(&e, &sample), before, "the merge changes no answer");
    assert_eq!(e.lookup_movement_by_order_no(b"O00000001").unwrap(), None, "the deleted handle stays dead");
    assert!(!e.order_no_index_needs_merge(), "the merged delta is empty, so no further merge is due");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// WP-9 item 5: a change already published into a tile is picked up by the delta's
/// tile walk, which reads only the tiles above the coverage and only their rows above
/// the coverage - so a delta hides a base entry even when the delete is tile-resident
/// and the memtable is empty.
#[test]
fn a_delta_reads_a_tile_resident_tombstone_above_the_coverage() {
    let dir = tmp_dir("deltatile");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"v0", &[movement(1, "A", "M1"), movement(2, "B", "M2")]).unwrap();
    e.publish_tile(dir.join("tile-0.tile"), e.csn()).unwrap();
    let base = build(&e, &dir).unwrap();
    let base_coverage = base.index.base_coverage_csn();
    e.adopt_order_no_index(base.index);
    assert_eq!(e.lookup_movement_by_order_no(b"A").unwrap(), Some(1), "the base answers");

    // csn 2 deletes handle 1 and publishes, so the only source of the change is the
    // newer tile: the serving memtable is empty afterwards.
    e.commit_block(b"v1", &[Op::DelDoc { entity: INDEX_ENTITY.to_vec(), handle: 1 }]).unwrap();
    e.publish_tile(dir.join("tile-1.tile"), e.csn()).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "the change is tile-resident, not memtable-resident");

    let delta = rebuild_delta(&e, base_coverage);
    let IndexProduct::Delta(build) = delta.product else { panic!("the interval build produces a delta") };
    assert_eq!(build.delta.live_entries(), 0, "the only change above the coverage is a delete");
    assert_eq!(build.delta.entries(), 1, "which the delta names as a tombstone");
    e.adopt_order_no_delta(build).unwrap();
    assert_eq!(e.lookup_movement_by_order_no(b"A").unwrap(), None,
        "the tile-resident tombstone hides the base entry");
    assert_eq!(e.lookup_movement_by_order_no(b"B").unwrap(), Some(2), "the unchanged row still answers");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// WP-9 review P2-4: the slot is the single-flight guard, so a second caller gets an
/// explicit refusal instead of a second writer racing the same sidecar, and the
/// in-flight job is untouched by the refused call.
#[test]
fn a_second_concurrent_index_job_is_refused() {
    let dir = tmp_dir("singleflight");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    e.commit_block(b"v0", &[movement(1, "A", "M1")]).unwrap();
    let mut slot = IndexRebuildSlot::new();
    slot.schedule_base(
        dir.to_path_buf(),
        e.key_encoding(),
        e.primary_tile_view(),
        |view, visit| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            view.scan_primary(INDEX_ENTITY, visit)
        },
        memtable_rows(&e, 0, INLINE_SNAPSHOT_MAX_BYTES),
        movement_region_rows(&e),
        "policy",
    )
    .unwrap();
    assert!(slot.is_busy());
    let second = slot.schedule_base(
        dir.to_path_buf(),
        e.key_encoding(),
        e.primary_tile_view(),
        |view, visit| view.scan_primary(INDEX_ENTITY, visit),
        memtable_rows(&e, 0, INLINE_SNAPSHOT_MAX_BYTES),
        movement_region_rows(&e),
        "policy",
    );
    let error = match second {
        Ok(()) => panic!("a second concurrent build must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert!(error.to_string().contains("already in flight"), "{error}");
    // The first job is still the slot's and finishes normally.
    let result = settle(&mut slot);
    assert_eq!(base_of(&result).entries(), 1);
    std::fs::remove_dir_all(&dir).unwrap();
}
