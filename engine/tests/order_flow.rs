use flashdb_engine::engine::{Engine, Op, Outcome};
use flashdb_engine::request::execute_order_flow;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn path() -> PathBuf {
    std::env::temp_dir().join(format!("flashdb-order-flow-{}-{}.wal", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}
fn fixture(engine: &mut Engine, count: i64) {
    let stock = json!({"sku":"S000001","loc":"L000","on_hand":count});
    let customer = json!({"email":"c000001@example.com"});
    assert!(matches!(engine.commit_block(b"seed", &[
        Op::PutDoc {entity:b"Stock".to_vec(), handle:10, doc:serde_json::to_vec(&stock).unwrap()},
        Op::PutUnique {entity:b"Stock".to_vec(), field:b"sku_loc".to_vec(), value:serde_json::to_vec(&["S000001","L000"]).unwrap(), handle:10},
        Op::PutDoc {entity:b"Customer".to_vec(), handle:20, doc:serde_json::to_vec(&customer).unwrap()},
        Op::PutUnique {entity:b"Customer".to_vec(), field:b"email".to_vec(), value:b"c000001@example.com".to_vec(), handle:20},
    ]).unwrap(), Outcome::Committed { .. }));
}

// Exact field shapes emitted by harness/flashdb_harness/generator.py::order_flow.
fn request() -> Value {
    json!({
        "id":"r-00000001", "class":{"durability":"batched","retry_horizon_s":300},
        "params":{"sku":"S000001","loc":"L000","qty":3,"email":"c000001@example.com","order_no":"O00000001"},
        "blocks":{
            "cust":{"find":{"Customer":{"email":"$email"}}},
            "take":{"needs":["cust"], "ops":[
                {"patch":{"Stock":{"where":{"sku":"$sku","loc":"$loc","on_hand":{"$gte":"$qty"}},"set":{"$inc":{"on_hand":"-$qty"}}}}},
                {"put":{"StockMovement":{"movement_no":"M00000001","sku":"$sku","loc":"$loc","delta":"-$qty","order_no":"$order_no","stock":{"sku":"$sku","loc":"$loc"}}}},
                {"put":{"Order":{"order_no":"$order_no","buyer":"$cust._ref","status":"open","lines":[{"sku":"$sku","qty":"$qty"}]}}}
            ],"else":[{"put":{"Backorder":{"sku":"$sku","qty":"$qty","order_no":"$order_no"}}}]}
        }
    })
}
fn get(engine: &Engine, entity: &str, handle: u64) -> Value {
    serde_json::from_slice(engine.get(entity.as_bytes(), handle, engine.csn()).unwrap().unwrap()).unwrap()
}
fn count(engine: &Engine) -> i64 { get(engine, "Stock", 10)["on_hand"].as_i64().unwrap() }

#[test]
fn generator_order_flow_atomic_success_dedup_and_reopen() {
    let p = path();
    let mut engine = Engine::create(&p).unwrap();
    fixture(&mut engine, 8);
    let request = request();
    let out = execute_order_flow(&mut engine, &request).unwrap();
    assert_eq!(out["cust"].row.as_ref().unwrap()["_ref"], 20);
    assert_eq!(out["take"].status, "ok");
    assert_eq!(engine.csn(), 2, "one WAL commit for all seven P/U/R ops");
    assert_eq!(count(&engine), 5);
    let movement = engine.unique_lookup(b"StockMovement", b"movement_no", b"M00000001", engine.csn()).unwrap().unwrap();
    let order = engine.unique_lookup(b"Order", b"order_no", b"O00000001", engine.csn()).unwrap().unwrap();
    assert_ne!(movement, order);
    assert_eq!(get(&engine, "StockMovement", movement)["delta"], -3);
    assert_eq!(get(&engine, "StockMovement", movement)["stock"], 10);
    assert_eq!(get(&engine, "Order", order)["buyer"], 20);
    assert_eq!(engine.reverse_lookup(b"Order", b"buyer", 20, engine.csn()).unwrap(), vec![order]);
    assert_eq!(engine.reverse_lookup(b"StockMovement", b"stock", 10, engine.csn()).unwrap(), vec![movement]);
    assert_eq!(engine.unique_lookup(b"Backorder", b"order_no", b"O00000001", engine.csn()).unwrap(), None);
    assert_eq!(execute_order_flow(&mut engine, &request).unwrap()["take"].csn, Some(2));
    assert_eq!(engine.csn(), 2);
    drop(engine);
    let mut engine = Engine::open(&p).unwrap();
    assert_eq!(count(&engine), 5);
    assert_eq!(engine.next_handle().unwrap(), order + 1);
    assert_eq!(execute_order_flow(&mut engine, &request).unwrap()["take"].csn, Some(2));
    assert_eq!(engine.csn(), 2);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn insufficient_stock_only_backorder_and_replay() {
    let p = path();
    let mut engine = Engine::create(&p).unwrap();
    fixture(&mut engine, 2);
    let req = request();
    let out = execute_order_flow(&mut engine, &req).unwrap();
    assert_eq!(out["take"].status, "failed");
    assert_eq!(out["take/else"].status, "ok");
    assert_eq!(engine.csn(), 2);
    assert_eq!(count(&engine), 2);
    assert_eq!(engine.unique_lookup(b"Order", b"order_no", b"O00000001", engine.csn()).unwrap(), None);
    assert_eq!(engine.unique_lookup(b"StockMovement", b"movement_no", b"M00000001", engine.csn()).unwrap(), None);
    let handle = engine.unique_lookup(b"Backorder", b"order_no", b"O00000001", engine.csn()).unwrap().unwrap();
    assert_eq!(get(&engine, "Backorder", handle)["qty"], 3);
    drop(engine);
    let mut engine = Engine::open(&p).unwrap();
    assert_eq!(execute_order_flow(&mut engine, &req).unwrap()["take/else"].csn, Some(2));
    assert_eq!(engine.csn(), 2);
    assert_eq!(engine.next_handle().unwrap(), handle + 1);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn read_fallback_binds_applied_result_and_missing_customer_skips_write() {
    let p = path();
    let mut engine = Engine::create(&p).unwrap();
    fixture(&mut engine, 5);
    let mut req = request();
    req["blocks"]["cust"]["find"]["Customer"]["email"] = json!("nobody@example.com");
    req["blocks"]["cust"]["else"] = json!({"find":{"Customer":{"email":"$email"}}});
    assert_eq!(execute_order_flow(&mut engine, &req).unwrap()["take"].status, "ok");
    req["id"] = json!("missing-customer");
    req["blocks"]["cust"]["else"]["find"]["Customer"]["email"] = json!("absent@example.com");
    assert_eq!(execute_order_flow(&mut engine, &req).unwrap()["take"].status, "skipped");
    assert_eq!(engine.csn(), 2);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn malformed_or_conflicting_request_cannot_partially_write() {
    let p = path();
    let mut engine = Engine::create(&p).unwrap();
    fixture(&mut engine, 5);
    let mut req = request();
    req["blocks"]["take"]["ops"][2]["put"]["Order"]["status"] = json!("unknown-operator");
    assert!(execute_order_flow(&mut engine, &req).is_err());
    assert_eq!(engine.csn(), 1);
    req = request();
    req["blocks"]["cust"] = json!({"get":{"Customer":{"where":{"email":"$email"}}}});
    assert!(execute_order_flow(&mut engine, &req).is_err());
    assert_eq!(engine.csn(), 1);
    req = request();
    req["class"]["max_staleness"] = json!(3);
    assert!(execute_order_flow(&mut engine, &req).is_err());
    assert_eq!(engine.csn(), 1);
    // Publish-time unique conflict: stock/movement/order all roll back.
    engine.commit_block(b"occupied", &[Op::PutUnique {entity:b"Order".to_vec(), field:b"order_no".to_vec(), value:b"O00000001".to_vec(), handle:50}]).unwrap();
    let out = execute_order_flow(&mut engine, &request()).unwrap();
    assert_eq!(out["take"].status, "failed");
    assert_eq!(out["take/else"].status, "ok");
    assert_eq!(engine.csn(), 3);
    assert_eq!(count(&engine), 5);
    assert_eq!(engine.unique_lookup(b"StockMovement", b"movement_no", b"M00000001", engine.csn()).unwrap(), None);
    assert!(engine.unique_lookup(b"Backorder", b"order_no", b"O00000001", engine.csn()).unwrap().is_some());
    std::fs::remove_file(p).unwrap();
}
