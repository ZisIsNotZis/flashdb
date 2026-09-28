use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::request::execute_request;
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn path() -> PathBuf {
    std::env::temp_dir().join(format!("flashdb-read-{}-{}.wal", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

// Literal JSONL records emitted by Generator(42, 200).requests() in generator.py.
const STOCK: &str = r#"{"blocks":{"q":{"get":{"Stock":{"where":{"loc":"L005","sku":"S004353"}}}}},"class":{"durability":"batched","max_staleness":2},"id":"r-00000005"}"#;
const CUSTOMER: &str = r#"{"blocks":{"q":{"find":{"Customer":{"email":"c002269@example.com"}}}},"class":{"durability":"batched"},"id":"r-00000012"}"#;
const MOVEMENTS: &str = r#"{"blocks":{"q":{"find":{"StockMovement":{"where":{"order_no":"O00000007"}},"limit":100,"order":["movement_no"]}}},"class":{"durability":"batched","max_staleness":5},"id":"r-00000018"}"#;

fn put(entity: &str, handle: u64, value: &Value) -> Op {
    Op::PutDoc { entity: entity.as_bytes().to_vec(), handle, doc: serde_json::to_vec(value).unwrap() }
}

#[test]
fn generator_point_reads_return_documents_or_empty_without_commits_and_survive_recovery() {
    let p = path();
    let mut e = Engine::create(&p).unwrap();
    let stock: Value = serde_json::from_str(STOCK).unwrap();
    let customer: Value = serde_json::from_str(CUSTOMER).unwrap();
    for request in [&stock, &customer] {
        let result = execute_request(&mut e, request).unwrap();
        assert_eq!(result["q"].status, "empty");
        assert_eq!(result["q"].row, None);
        assert_eq!(result["q"].csn, None);
    }
    assert_eq!(e.csn(), 0);
    let order_flow: Value = serde_json::from_str(include_str!("../../harness/tests/fixtures/order_flow_seed42.jsonl").trim()).unwrap();
    let result = execute_request(&mut e, &order_flow).unwrap();
    assert_eq!(result["cust"].status, "empty");
    assert_eq!(result["take"].status, "skipped");
    assert_eq!(e.csn(), 0, "dispatching an unfulfilled order does not commit");
    let stock_row = json!({"sku":"S004353", "loc":"L005", "on_hand":8});
    let customer_row = json!({"email":"c002269@example.com", "name":"A"});
    e.commit_block(b"seed", &[
        put("Stock", 10, &stock_row),
        Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku_loc".to_vec(),
            value: serde_json::to_vec(&["S004353", "L005"]).unwrap(), handle: 10 },
        put("Customer", 20, &customer_row),
        Op::PutUnique { entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"c002269@example.com".to_vec(), handle: 20 },
    ]).unwrap();
    assert_eq!(execute_request(&mut e, &stock).unwrap()["q"].row, Some(stock_row.clone()));
    assert_eq!(execute_request(&mut e, &customer).unwrap()["q"].row, Some(customer_row.clone()));
    assert_eq!(e.csn(), 1);
    drop(e);
    let mut e = Engine::open(&p).unwrap();
    assert_eq!(execute_request(&mut e, &stock).unwrap()["q"].row, Some(stock_row));
    assert_eq!(execute_request(&mut e, &customer).unwrap()["q"].row, Some(customer_row));
    e.commit_block(b"remove", &[
        Op::DelDoc { entity: b"Stock".to_vec(), handle: 10 },
        Op::DelUnique { entity: b"Stock".to_vec(), field: b"sku_loc".to_vec(),
            value: serde_json::to_vec(&["S004353", "L005"]).unwrap(), handle: 10 },
        Op::DelDoc { entity: b"Customer".to_vec(), handle: 20 },
        Op::DelUnique { entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"c002269@example.com".to_vec(), handle: 20 },
    ]).unwrap();
    assert_eq!(execute_request(&mut e, &stock).unwrap()["q"].status, "empty");
    assert_eq!(execute_request(&mut e, &customer).unwrap()["q"].status, "empty");
    assert_eq!(e.csn(), 2);
    drop(e);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn movement_scan_uses_snapshot_primary_versions_tombstones_and_ordered_top_100() {
    let p = path();
    let mut e = Engine::create(&p).unwrap();
    let req: Value = serde_json::from_str(MOVEMENTS).unwrap();
    assert_eq!(execute_request(&mut e, &req).unwrap()["q"].status, "empty");
    for n in (0..105).rev() {
        let handle = n + 1;
        e.commit_block(format!("seed-{n}").as_bytes(), &[
            put("StockMovement", handle, &json!({"order_no":"O00000007", "movement_no":format!("M{n:03}"), "delta":-1})),
        ]).unwrap();
    }
    let before = e.csn();
    e.commit_block(b"update", &[put("StockMovement", 1, &json!({"order_no":"O00000007", "movement_no":"Mzzz", "delta":-2}))]).unwrap();
    e.commit_block(b"deleted", &[Op::DelDoc { entity:b"StockMovement".to_vec(), handle: 2 }]).unwrap();
    e.commit_block(b"other-order", &[put("StockMovement", 200, &json!({"order_no":"O00000008", "movement_no":"M000"}))]).unwrap();
    e.commit_block(b"other-entity", &[put("Stock", 201, &json!({"sku":"S004353", "loc":"L005"}))]).unwrap();
    let mut old = Vec::new();
    e.scan_primary(b"StockMovement", before, |handle, bytes| {
        old.push((handle, serde_json::from_slice::<Value>(bytes).unwrap()));
        Ok(())
    }).unwrap();
    assert_eq!(old.len(), 105);
    assert_eq!(old.iter().find(|(h, _)| *h == 1).unwrap().1["movement_no"], "M000");
    assert!(old.iter().any(|(h, _)| *h == 2));
    let mut current = Vec::new();
    e.scan_primary(b"StockMovement", e.csn(), |handle, _| { current.push(handle); Ok(()) }).unwrap();
    assert_eq!(current.len(), 105, "update counts once, tombstone hides one, other order remains");
    assert!(!current.contains(&2));
    let csn = e.csn();
    let out = execute_request(&mut e, &req).unwrap();
    let rows = out["q"].row.as_ref().unwrap().as_array().unwrap();
    assert_eq!(out["q"].status, "ok");
    assert_eq!(out["q"].csn, None);
    assert_eq!(rows.len(), 100);
    assert_eq!(rows.first().unwrap()["movement_no"], "M002");
    assert_eq!(rows.last().unwrap()["movement_no"], "M101");
    assert_eq!(e.csn(), csn);
    drop(e);
    let mut e = Engine::open(&p).unwrap();
    assert_eq!(execute_request(&mut e, &req).unwrap()["q"], out["q"]);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn malformed_shapes_and_invalid_documents_fail_closed_without_read_writes() {
    let p = path();
    let mut e = Engine::create(&p).unwrap();
    let scan: Value = serde_json::from_str(MOVEMENTS).unwrap();
    let stock: Value = serde_json::from_str(STOCK).unwrap();
    let customer: Value = serde_json::from_str(CUSTOMER).unwrap();
    let mut rejected = Vec::new();
    for (base, path, value) in [
        (&scan, vec!["blocks", "q", "find", "limit"], json!(101)),
        (&scan, vec!["blocks", "q", "find", "order"], json!(["order_no"])),
        (&scan, vec!["class", "max_staleness"], json!(2)),
        (&stock, vec!["blocks", "q", "get", "Stock", "where", "sku"], json!("$sku")),
        (&customer, vec!["class", "durability"], json!("lossy")),
    ] {
        let mut req = base.clone();
        let mut node = &mut req;
        for key in path { node = &mut node[key]; }
        *node = value;
        rejected.push(req);
    }
    let mut extra = scan.clone();
    extra["blocks"]["q"]["else"] = json!({"put":{"Stock":{}}});
    rejected.push(extra);
    let mut large = stock.clone();
    large["blocks"]["q"]["get"]["Stock"]["where"]["sku"] = json!("S".repeat(128 * 1024));
    rejected.push(large);
    for req in rejected {
        assert_eq!(execute_request(&mut e, &req).unwrap_err().kind(), ErrorKind::InvalidInput);
    }
    assert_eq!(e.csn(), 0);
    e.commit_block(b"invalid-json", &[Op::PutDoc {
        entity: b"StockMovement".to_vec(), handle: 1, doc: b"not-json".to_vec(),
    }]).unwrap();
    assert_eq!(execute_request(&mut e, &scan).unwrap_err().kind(), ErrorKind::InvalidData);
    assert_eq!(e.csn(), 1);
    e.commit_block(b"tombstone", &[Op::DelDoc { entity: b"StockMovement".to_vec(), handle: 1 }]).unwrap();
    assert_eq!(execute_request(&mut e, &scan).unwrap()["q"].status, "empty");
    e.commit_block(b"bad-stock", &[
        Op::PutDoc { entity: b"Stock".to_vec(), handle: 10, doc: b"[1,2]".to_vec() },
        Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku_loc".to_vec(),
            value: serde_json::to_vec(&["S004353", "L005"]).unwrap(), handle: 10 },
    ]).unwrap();
    assert_eq!(execute_request(&mut e, &stock).unwrap_err().kind(), ErrorKind::InvalidData);
    drop(e);
    std::fs::remove_file(p).unwrap();
}
