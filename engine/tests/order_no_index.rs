// WP-M: the derived `order_no -> StockMovement handle` secondary access path is
// never on the correctness path — a hit is cross-checked, a miss or an absent or
// a stale index always falls back to the authoritative full-entity scan.

use flashdb_engine::engine::{Engine, Op, RegionMode};
use flashdb_engine::index::{build_order_no_index, movement_region_rows, OrderNoIndexBuild, RegionRows, INDEX_ENTITY, INDEX_FIELD};
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
/// driver passes in production, so rows still in the serving memtable are covered.
fn build(e: &Engine, dir: &std::path::Path) -> std::io::Result<OrderNoIndexBuild> {
    build_order_no_index(
        dir,
        e.key_encoding(),
        |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
        movement_region_rows(e),
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
    assert_eq!(build.index.entries, 3);
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
    assert_eq!(build.index.entries, 1);
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
    assert_eq!(build.index.entries, orders.len() as u64, "one entry per memtable row");
    assert_eq!(build.index.coverage_csn, 3, "the covered CSN is the max row CSN, not the last row's");
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
    assert_eq!(movement_region_rows(&e), RegionRows::InMemtable { entries: 1 },
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
    assert_eq!(build.index.entries, 2, "one entry per live row: handle 1's newest and handle 2");
    assert_eq!(build.index.coverage_csn, 2, "the max csn over LIVING versions, not the tombstone's");
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
        "index-order-no.tile",
    );
    let err = result.err().expect("an empty build over a non-empty region is a defect, not an empty index");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("produced 0 entries"), "names the produced entries: {message}");
    assert!(message.contains("visited 0 live"), "names the visited rows: {message}");
    assert!(message.contains("the serving memtable holds 2 entries"), "names the signal: {message}");
    assert!(!dir.join("index-order-no.tile").exists(), "a rejected build writes no sidecar");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The `U|StockMovement.order_no` key spelling is what the index probes.
#[test]
fn index_field_and_entity_names_are_the_observed_predicate() {
    assert_eq!(INDEX_ENTITY, b"StockMovement");
    assert_eq!(INDEX_FIELD, b"order_no");
}
