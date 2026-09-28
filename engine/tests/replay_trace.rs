use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::replay::replay_jsonl;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufReader, Cursor};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
const TRACE: &str = include_str!("../../harness/tests/fixtures/mixed_seed42_20.jsonl");
const SHA: &str = "929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264";

fn path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("flashdb-replay-{}-{}.wal", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
}

#[test]
fn mixed_generator_corpus_executes_and_recovers_with_independent_stock_oracle() {
    let p = path();
    let mut engine = Engine::create(&p).unwrap();
    let requests: Vec<Value> = TRACE.lines().map(|s| serde_json::from_str(s).unwrap()).collect();
    let mut stock = BTreeSet::new();
    let mut customers = BTreeSet::new();
    for req in &requests {
        if req["blocks"].get("take").is_some() {
            let params = &req["params"];
            stock.insert((params["sku"].as_str().unwrap().to_string(), params["loc"].as_str().unwrap().to_string()));
            customers.insert(params["email"].as_str().unwrap().to_string());
        }
    }
    let mut ops = Vec::new();
    let mut handle = 1u64;
    for (sku, loc) in &stock {
        let row = json!({"sku":sku,"loc":loc,"on_hand":6});
        ops.push(Op::PutDoc {entity:b"Stock".to_vec(), handle, doc:serde_json::to_vec(&row).unwrap()});
        ops.push(Op::PutUnique {entity:b"Stock".to_vec(), field:b"sku_loc".to_vec(), value:serde_json::to_vec(&[sku,loc]).unwrap(), handle});
        handle += 1;
    }
    for email in &customers {
        ops.push(Op::PutDoc {entity:b"Customer".to_vec(), handle, doc:serde_json::to_vec(&json!({"email":email})).unwrap()});
        ops.push(Op::PutUnique {entity:b"Customer".to_vec(), field:b"email".to_vec(), value:email.as_bytes().to_vec(), handle});
        handle += 1;
    }
    engine.commit_block(b"bootstrap", &ops).unwrap();
    let initial_csn = engine.csn();
    let mut expected: BTreeMap<_, i64> = stock.iter().cloned().map(|k| (k, 6)).collect();
    let mut successful_orders = BTreeSet::new();
    let mut failures = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let summary = replay_jsonl(&mut engine, BufReader::new(TRACE.as_bytes()), |line, req, result| {
        assert_eq!(line as usize, kinds.len() + 1);
        if let Some(take) = result.get("take") {
            kinds.insert(format!("order-{line}"));
            let p = &req["params"];
            let key = (p["sku"].as_str().unwrap().to_string(), p["loc"].as_str().unwrap().to_string());
            let qty = p["qty"].as_i64().unwrap();
            let order_no = p["order_no"].as_str().unwrap().to_string();
            let current = expected.get_mut(&key).unwrap();
            if *current >= qty {
                assert_eq!(take.status, "ok");
                assert!(result.get("take/else").is_none());
                *current -= qty;
                successful_orders.insert(order_no);
            } else {
                assert_eq!(take.status, "failed");
                assert_eq!(result["take/else"].status, "ok");
                failures.insert(order_no);
            }
        } else {
            kinds.insert(format!("read-{line}"));
            assert!(matches!(result["q"].status, "ok" | "empty"));
            if req["blocks"]["q"].get("get").is_some() {
                let w = &req["blocks"]["q"]["get"]["Stock"]["where"];
                let key = (w["sku"].as_str().unwrap().to_string(), w["loc"].as_str().unwrap().to_string());
                if let Some(count) = expected.get(&key) {
                    assert_eq!(result["q"].row.as_ref().unwrap()["on_hand"], *count);
                } else { assert_eq!(result["q"].status, "empty"); }
            } else if let Some(customer) = req["blocks"]["q"]["find"].get("Customer") {
                let email = customer["email"].as_str().unwrap();
                if customers.contains(email) {
                    assert_eq!(result["q"].row.as_ref().unwrap()["email"], email);
                } else { assert_eq!(result["q"].status, "empty"); }
            } else {
                let order_no = req["blocks"]["q"]["find"]["StockMovement"]["where"]["order_no"].as_str().unwrap();
                if successful_orders.contains(order_no) {
                    let rows = result["q"].row.as_ref().unwrap().as_array().unwrap();
                    assert_eq!(rows.len(), 1);
                    assert_eq!(rows[0]["order_no"], order_no);
                } else { assert_eq!(result["q"].status, "empty"); }
            }
        }
        Ok(())
    }).unwrap();
    assert_eq!(summary.requests, 20);
    assert_eq!(summary.sha256, SHA);
    assert_eq!(kinds.len(), 20);
    assert!(kinds.iter().any(|s| s.starts_with("read-")));
    assert!(!successful_orders.is_empty());
    let after_csn = engine.csn();
    assert_eq!(after_csn, initial_csn + successful_orders.len() as u64 + failures.len() as u64);
    for ((sku, loc), count) in &expected {
        let key = serde_json::to_vec(&[sku, loc]).unwrap();
        let h = engine.unique_lookup(b"Stock", b"sku_loc", &key, after_csn).unwrap().unwrap();
        let doc: Value = serde_json::from_slice(engine.get(b"Stock", h, after_csn).unwrap().unwrap()).unwrap();
        assert_eq!(doc["on_hand"], *count);
        assert!(*count >= 0);
    }
    for order_no in successful_orders.union(&failures) {
        let order = engine.unique_lookup(b"Order", b"order_no", order_no.as_bytes(), after_csn).unwrap();
        let back = engine.unique_lookup(b"Backorder", b"order_no", order_no.as_bytes(), after_csn).unwrap();
        assert_ne!(order.is_some(), back.is_some());
    }
    drop(engine);
    let mut reopened = Engine::open(&p).unwrap();
    assert_eq!(reopened.csn(), after_csn);
    // Read results need not repeat their original observations; writes must dedup.
    let replayed = replay_jsonl(&mut reopened, Cursor::new(TRACE.as_bytes()), |_, _, _| Ok(())).unwrap();
    assert_eq!(replayed.sha256, SHA);
    assert_eq!(reopened.csn(), after_csn);
    drop(reopened);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_flashdb-replay"))
        .arg(&p)
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../harness/tests/fixtures/mixed_seed42_20.jsonl"))
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stderr).contains(SHA));
    let lines: Vec<_> = output.stdout.split(|&b| b == b'\n').filter(|s| !s.is_empty()).collect();
    assert_eq!(lines.len(), 20);
    for (i, line) in lines.iter().enumerate() {
        let out: Value = serde_json::from_slice(line).unwrap();
        assert_eq!(out["line"], i + 1);
        assert!(out["blocks"].is_object());
    }
    assert_eq!(Engine::open(&p).unwrap().csn(), after_csn);
    std::fs::remove_file(p).unwrap();
}

#[test]
fn malformed_or_partial_trace_stops_at_line_without_claiming_hash() {
    let cases = [
        (b"{}\n".as_slice(), io::ErrorKind::InvalidInput),
        (b"not-json\n".as_slice(), io::ErrorKind::InvalidData),
        (b"{\"id\":1}".as_slice(), io::ErrorKind::InvalidData),
    ];
    for (input, kind) in cases {
        let p = path();
        let mut e = Engine::create(&p).unwrap();
        let err = replay_jsonl(&mut e, Cursor::new(input), |_, _, _| Ok(())).unwrap_err();
        assert_eq!(err.kind(), kind);
        assert!(err.to_string().contains("trace line 1"));
        assert_eq!(e.csn(), 0);
        std::fs::remove_file(p).unwrap();
    }
    let p = path();
    let mut e = Engine::create(&p).unwrap();
    let long = vec![b' '; 65_538];
    let err = replay_jsonl(&mut e, Cursor::new(long), |_, _, _| Ok(())).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    assert!(err.to_string().contains("trace line 1"));
    std::fs::remove_file(p).unwrap();
}
