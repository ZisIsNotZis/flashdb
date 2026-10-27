//! F-M1 (ticket 83): the index defect proven gone by counting WORK PER REQUEST,
//! not a rate.
//!
//! The defect: `build_order_no_index` read tiles only, so on an append-heavy
//! instance — every movement still in the serving memtable — it wrote **0
//! entries** and every movement scan paid the full merged scan. The scan's cost
//! is not the movement rows (they are in the memtable, which costs no tile
//! bytes): it is **one 4 KiB block read from every active tile**, because in the
//! default global region mode each tile's key range straddles the
//! `P|StockMovement` prefix, so `scan_merged` opens a cursor on all of them and
//! each cursor reads a block. The M1 record measured exactly this: 156 active
//! tiles (`P|Customer` 3, `P|Stock` 153) and **638,938 B per scan**
//! = 4,096 B x 156 tiles (`.scratch/82-controlled-m1/report.md` §5;
//! `.scratch/83-index-probe/issues.md` §The defect; `docs/6_ARCHITECTURE.md`
//! §7 fix #1 predicted 638,938 -> 4,096 B per scan).
//!
//! This test rebuilds that instance shape (3 customer tiles + 153 stock tiles
//! published, then 2,785 movements appended into the memtable only), serves the
//! same 188 scan requests through the real request path in two arms on the same
//! instance, and prints the engine's own counters for each:
//!
//! - `arm=before`: no index adopted — the observable state the pre-fix build
//!   left behind (it adopted an index with 0 entries, and a lookup on an empty
//!   index returns `None`, so the read path ran the full scan);
//! - `arm=after`: the index built from the merged memtable + tile view and
//!   adopted;
//! - `arm=after-tiled`: the same, once the movements are published into a tile,
//!   so the index's cross-check `get` is a dependent page probe — the 4,096 B
//!   the gap-analysis predicted per scan.
//!
//! Every number printed is a count, a byte total, or a ratio of two counts from
//! this same run. No throughput is measured or reported.
//!
//! The `arm=before` / `arm=after` pair is what a pre-fix binary cannot produce:
//! the pre-fix build writes 0 entries, so its "after" arm is its "before" arm.
//! `.scratch/84-index-defect-mechanism/pre-fix-probe.rs` is this file with the
//! pre-fix build call, run against `f584ecc` (the commit before WP-1) to show
//! that.

use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::index::{build_order_no_index, INDEX_ENTITY};
use flashdb_engine::metrics::WindowSnapshot;
use flashdb_engine::request::execute_request;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// The M1 instance's active-tile shape: 3 `P|Customer`, 153 `P|Stock` tiles.
const CUSTOMER_TILES: usize = 3;
const STOCK_TILES: usize = 153;
const CUSTOMERS_PER_TILE: u64 = 28;
const STOCKS_PER_TILE: u64 = 28;
/// 2,785 movements is the row count the M1 record measured on the same trace
/// (`.scratch/83-index-probe/issues.md`: `scan_primary` returned 2785 rows while
/// the tile-only build produced 0 entries).
const MOVEMENTS: usize = 2_785;
/// 188 of the 4,000 requests in the M1 trace scan the movement region (4.7%).
const SCANS: usize = 188;
/// One 4 KiB data block, the unit `bytes_read` counts.
const BLOCK: u64 = 4_096;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "flashdb-index-defect-mechanism-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        name
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// One customer write: the document plus the unique-index key an order flow
/// writes with it, so the resulting tile straddles the movement prefix exactly as
/// the M1 instance's tiles did (`P|Customer` keys below it, `U|Customer` above).
fn customer_ops(handle: u64) -> Vec<Op> {
    let email = format!("c{handle:08}@example.com");
    let body = json!({"email": email, "name": format!("C{handle:08}")});
    vec![
        Op::PutDoc { entity: b"Customer".to_vec(), handle, doc: serde_json::to_vec(&body).unwrap() },
        Op::PutUnique { entity: b"Customer".to_vec(), field: b"email".to_vec(),
                        value: email.into_bytes(), handle },
    ]
}

/// One stock write, same shape: `P|Stock` below the movement prefix and
/// `U|Stock` above it.
fn stock_ops(handle: u64) -> Vec<Op> {
    let sku = format!("S{:06}", handle % 2_650_000);
    let loc = format!("L{}", handle % 8);
    let value = format!("{sku}|{loc}");
    let body = json!({"sku": sku, "loc": loc, "qty": (handle % 97) as i64, "rev": handle});
    vec![
        Op::PutDoc { entity: b"Stock".to_vec(), handle, doc: serde_json::to_vec(&body).unwrap() },
        Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku_loc".to_vec(),
                        value: value.into_bytes(), handle },
    ]
}

/// One movement append. `order_no` is `O<row>` and independent of the handle, so
/// the scan requests below name rows that exist (the M1 trace scans for order
/// numbers its own writes created).
fn movement_ops(handle: u64, row: usize) -> Vec<Op> {
    let order_no = format!("O{:08}", row + 1);
    let movement_no = format!("M{:08}", row + 1);
    let body = json!({"order_no": order_no, "movement_no": movement_no,
                      "sku": format!("S{:06}", handle % 2_650_000), "loc": format!("L{}", handle % 8),
                      "delta": -1});
    vec![
        Op::PutDoc { entity: INDEX_ENTITY.to_vec(), handle, doc: serde_json::to_vec(&body).unwrap() },
        Op::PutUnique { entity: INDEX_ENTITY.to_vec(), field: b"movement_no".to_vec(),
                        value: movement_no.into_bytes(), handle },
    ]
}

fn scan_req(order_no: &str) -> Value {
    serde_json::from_str(&format!(
        r#"{{"blocks":{{"q":{{"find":{{"StockMovement":{{"where":{{"order_no":"{order_no}"}}}},"limit":100,"order":["movement_no"]}}}}}},"class":{{"durability":"batched","max_staleness":5}},"id":"scan"}}"#
    ))
    .unwrap()
}

/// The instance the defect needed: a region of movements that is append-only and
/// still entirely in the serving memtable, on top of an instance that already has
/// 156 published tiles from the other regions.
fn append_heavy_instance(dir: &std::path::Path) -> Engine {
    let mut e = Engine::create(&dir.join("data.wal")).unwrap();
    let mut handle = 0u64;
    // 3 customer tiles + 153 stock tiles: one publish per tile, the same shape a
    // window-paced run produces.
    for t in 0..CUSTOMER_TILES {
        let mut ops = Vec::new();
        for _ in 0..CUSTOMERS_PER_TILE {
            handle += 1;
            ops.extend(customer_ops(handle));
        }
        e.commit_block(format!("c{t}").as_bytes(), &ops).unwrap();
        e.publish_tile(dir.join(format!("tile-customer-{t}.tile")), e.csn()).unwrap();
    }
    for t in 0..STOCK_TILES {
        let mut ops = Vec::new();
        for _ in 0..STOCKS_PER_TILE {
            handle += 1;
            ops.extend(stock_ops(handle));
        }
        e.commit_block(format!("s{t}").as_bytes(), &ops).unwrap();
        e.publish_tile(dir.join(format!("tile-stock-{t}.tile")), e.csn()).unwrap();
    }
    // Every movement is appended AFTER the last publish, so no tile holds one:
    // this is the append-heavy case, and the reason a tile-only build saw zero.
    let base = handle;
    let mut i = 0usize;
    while i < MOVEMENTS {
        e.begin_group().unwrap();
        let end = (i + 64).min(MOVEMENTS);
        for n in i..end {
            let ops = movement_ops(base + n as u64 + 1, n);
            e.commit_block_grouped(format!("m{n}").as_bytes(), &ops).unwrap();
        }
        e.end_group().unwrap();
        i = end;
    }
    // The row count the index must cover, and the witness that no tile holds one.
    assert_eq!(e.serving_memtable_entries(), MOVEMENTS * 2,
        "every movement (doc + unique key) is in the serving memtable: the append-heavy case");
    assert!(
        !e.active_tile_regions().iter().flatten().any(|r| r.contains("StockMovement")),
        "no published tile names the movement region, which is why a tile-only build saw 0 rows"
    );
    e
}

/// The counters one arm reads out of the engine, as a delta over the arm.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counters {
    requests: u64,
    scans_entered: u64,
    bytes_read: u64,
    ok: u64,
    empty: u64,
    movement_probes: u64,
}

fn region_row<'a>(snap: &'a WindowSnapshot, region: &str) -> Option<&'a flashdb_engine::metrics::RegionSummary> {
    snap.regions.iter().find(|r| r.region == region)
}

fn pattern_count(snap: &WindowSnapshot, pattern: &str) -> u64 {
    snap.patterns.iter().find(|p| p.pattern == pattern).map(|p| p.count).unwrap_or(0)
}

fn arm(e: &mut Engine, label: &str) -> (Counters, Vec<String>) {
    let before = e.metrics_snapshot();
    let mut answers = Vec::with_capacity(SCANS);
    for n in 0..SCANS {
        let order = format!("O{:08}", (n % MOVEMENTS) + 1);
        let out = execute_request(e, &scan_req(&order)).unwrap();
        // The answer string carries the status AND the serialized row, so
        // `before_answers == after_answers` compares the bytes the request
        // returned. A row count alone would let two arms agree while
        // returning different documents (review of ticket 84, note 1).
        let row = out["q"].row.as_ref().map(|r| r.to_string()).unwrap_or_else(|| "-".to_string());
        answers.push(format!("{}:{}", out["q"].status, row));
    }
    let after = e.metrics_snapshot();
    let c = Counters {
        requests: SCANS as u64,
        scans_entered: pattern_count(&after, "scan:StockMovement") - pattern_count(&before, "scan:StockMovement"),
        bytes_read: region_row(&after, "P|StockMovement").map(|r| r.bytes_read).unwrap_or(0)
            - region_row(&before, "P|StockMovement").map(|r| r.bytes_read).unwrap_or(0),
        ok: answers.iter().filter(|a| a.starts_with("ok:")).count() as u64,
        empty: answers.iter().filter(|a| a.starts_with("empty:")).count() as u64,
        movement_probes: region_row(&after, "P|StockMovement").map(|r| r.probes).unwrap_or(0)
            - region_row(&before, "P|StockMovement").map(|r| r.probes).unwrap_or(0),
    };
    println!(
        "arm={label} requests={} scans_entered={} bytes_read={} bytes_per_scan={} ok={} empty={} movement_row_probes={}",
        c.requests,
        c.scans_entered,
        c.bytes_read,
        c.bytes_read / c.requests,
        c.ok,
        c.empty,
        c.movement_probes,
    );
    (c, answers)
}

/// F-M1: on the append-heavy instance the derived `order_no` index is non-empty,
/// the merged scan is not entered for a query the index can answer, and the
/// per-request tile-byte cost of the scan is gone.
#[test]
fn index_defect_mechanism_is_gone_on_the_append_heavy_instance() {
    let dir = tmp_dir("mechanism");
    let mut e = append_heavy_instance(&dir);
    let tiles = e.active_tile_regions().len() as u64;
    println!("instance=tiles={tiles} movements={MOVEMENTS} scans={SCANS} region_mode=global");

    // BEFORE: the observable state the pre-fix build left behind. No index is
    // adopted here; a lookup on the pre-fix 0-entry index returned `None` too, so
    // the read path is the same one.
    let (before, before_answers) = arm(&mut e, "before");
    // Bytes one scan request pays: every request in an arm is a movement scan.
    let before_scan_budget = before.bytes_read / before.requests;

    // AFTER: the fix — build from the merged memtable + tile view and adopt.
    let build = build_order_no_index(
        &dir,
        e.key_encoding(),
        |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
        flashdb_engine::index::movement_region_rows(&e),
        0,
        "order-no-index.tile",
    )
    .expect("the merged-view build must not refuse: the movement region holds rows in the memtable");
    let entries = build.index.entries();
    e.adopt_order_no_index(build.index);
    assert!(e.has_order_no_index());
    println!("arm=build index_entries={entries} movements={MOVEMENTS} tiles={e_tiles}", e_tiles = e.active_tile_regions().len());

    let lookups_before = e.order_no_index_lookups();
    let (after, after_answers) = arm(&mut e, "after");
    let lookups = e.order_no_index_lookups() - lookups_before;
    println!("arm=after index_lookups={lookups}");

    assert_eq!(entries, MOVEMENTS as u64, "one index entry per movement row, from the merged view");
    assert_eq!(before.scans_entered, SCANS as u64, "without the index every scan enters the merged scan");
    assert_eq!(after.scans_entered, 0, "the index answers every scan, so the merged scan is not entered");
    assert_eq!(lookups, SCANS as u64, "the index was consulted on every scan request");
    assert_eq!(before.ok, SCANS as u64);
    assert!(
        before_answers.iter().all(|a| a.starts_with("ok:") && !a.ends_with(":-")),
        "each answer carries a row, so the byte comparison below is not vacuous"
    );
    assert_eq!(after.ok, SCANS as u64, "the same answers, from the index");
    assert_eq!(
        before_answers, after_answers,
        "the index changed the work, not the answers: status and serialized row, all {SCANS} requests"
    );
    assert!(
        before_scan_budget >= BLOCK * tiles / 2,
        "the full scan reads a block per active tile: {before_scan_budget} B per scan over {tiles} tiles"
    );
    assert_eq!(after.bytes_read, 0,
        "the cross-check `get` hits the serving memtable, so the index path reads no tile byte");

    // AFTER-TILED: publish the movements, so the index's cross-check `get` is a
    // dependent page probe. This is the 4,096 B per scan the gap-analysis
    // predicted as the post-fix cost.
    e.publish_tile(dir.join("tile-movements.tile"), e.csn()).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "the movements are now tile-resident");
    let lookups_before = e.order_no_index_lookups();
    let (tiled, tiled_answers) = arm(&mut e, "after-tiled");
    let lookups = e.order_no_index_lookups() - lookups_before;
    println!("arm=after-tiled index_lookups={lookups}");
    assert_eq!(tiled.scans_entered, 0, "the index still answers, so the merged scan is still not entered");
    assert_eq!(lookups, SCANS as u64);
    assert_eq!(tiled_answers, before_answers);
    // The predicted post-fix cost of a scan whose row is tile-resident: one
    // dependent page probe (4,096 B), not one block per tile.
    let tiled_scan_budget = tiled.bytes_read / tiled.requests;
    assert!(
        tiled_scan_budget <= 2 * BLOCK,
        "the index path costs about one page probe per scan, not one block per tile: {tiled_scan_budget} B"
    );
    println!(
        "summary bytes_per_scan_before={} bytes_per_scan_after={} bytes_per_scan_after_tiled={} \
         scans_entered_before={} scans_entered_after={} index_entries={}",
        before_scan_budget,
        after.bytes_read / after.requests,
        tiled_scan_budget,
        before.scans_entered, after.scans_entered, entries
    );

    std::fs::remove_dir_all(&dir).unwrap();
}
