// WP-M: the derived `order_no -> StockMovement handle` secondary access path is
// never on the correctness path — a hit is cross-checked, a miss or an absent or
// a stale index always falls back to the authoritative full-entity scan.

use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::index::{build_order_no_index, OrderNoIndexBuild, INDEX_ENTITY, INDEX_FIELD};
use flashdb_engine::keys::KeyEncoding;
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
#[test]
fn index_build_covers_rows_that_live_only_in_the_memtable() {
    let dir = tmp_dir("memtable");
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    let orders = ["O00000001", "O00000002", "O00000003"];
    for (i, order) in orders.iter().enumerate() {
        e.commit_block(format!("m-{i}").as_bytes(), &[movement(i as u64 + 1, order, &format!("M{i}"))])
            .unwrap();
    }
    assert!(e.active_tile_regions().is_empty(), "no tile exists, so a tile-only build sees zero rows");

    let build = build(&e, &dir).unwrap();
    assert_eq!(build.index.entries, orders.len() as u64, "one entry per memtable row");
    assert_eq!(build.index.coverage_csn, e.csn(), "the covered CSN is the max row CSN");
    e.adopt_order_no_index(build.index);

    assert_eq!(e.lookup_movement_by_order_no(b"O00000002").unwrap(), Some(2));
    let out = execute_request(&mut e, &scan_req("O00000003")).unwrap();
    assert_eq!(out["q"].status, "ok");
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["order_no"], "O00000003");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// A build that visits rows but produces no entries would write an empty sidecar
/// and silently leave every scan on the full-scan fallback. The scan reports what
/// it visited, so any disagreement is a hard error naming both counts.
#[test]
fn index_build_fails_loudly_when_visited_rows_and_entries_disagree() {
    let dir = tmp_dir("disagree");
    let result = build_order_no_index(
        &dir,
        KeyEncoding::FamilyFirst,
        |_visit| Ok(2),
        "index-order-no.tile",
    );
    let err = result.err().expect("rows visited with no entries is a defect, not an empty index");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let message = err.to_string();
    assert!(message.contains("visited 2 live"), "names the visited rows: {message}");
    assert!(message.contains("produced 0 entries"), "names the produced entries: {message}");
    assert!(!dir.join("index-order-no.tile").exists(), "a rejected build writes no sidecar");

    std::fs::remove_dir_all(&dir).unwrap();
}

/// The `U|StockMovement.order_no` key spelling is what the index probes.
#[test]
fn index_field_and_entity_names_are_the_observed_predicate() {
    assert_eq!(INDEX_ENTITY, b"StockMovement");
    assert_eq!(INDEX_FIELD, b"order_no");
}
