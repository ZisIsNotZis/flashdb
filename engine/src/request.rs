//! Deliberately narrow inventory generator adapter, not a general request grammar.
//!
//! Catalog for this slice: Stock has unique (sku,loc) via `sku_loc` (JSON array
//! encoding), Customer has unique email; StockMovement has unique movement_no and
//! `stock` -> Stock, Order has unique order_no and `buyer` -> Customer,
//! Backorder has unique order_no. Fixtures must populate P and U together using
//! this encoding. Unknown shapes, fields, operators, service classes, and block
//! graphs are errors *before* any block is committed. Reads are not journalled:
//! replay of a committed write reports its CSN, not its original read bindings.
//! This adapter does not claim general read-set validation, grammar normalization,
//! class scheduling, concurrent writers, or durable read-result replay.

use std::collections::BTreeMap;
use std::io::{self, ErrorKind, Write};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::engine::{Engine, Op, Outcome};

struct Catalog {
    entity: &'static str,
    unique: &'static str,
    reference: Option<&'static str>,
}

const STOCK: Catalog = Catalog { entity: "Stock", unique: "sku_loc", reference: None };
const CUSTOMER: Catalog = Catalog { entity: "Customer", unique: "email", reference: None };
const MOVEMENT: Catalog = Catalog { entity: "StockMovement", unique: "movement_no", reference: Some("stock") };
const ORDER: Catalog = Catalog { entity: "Order", unique: "order_no", reference: Some("buyer") };
const BACKORDER: Catalog = Catalog { entity: "Backorder", unique: "order_no", reference: None };

#[derive(Debug, PartialEq, Serialize)]
pub struct BlockResult {
    pub status: &'static str,
    pub csn: Option<u64>,
    pub row: Option<Value>,
}

pub type RequestResult = BTreeMap<String, BlockResult>;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message.into())
}

struct SizeLimit { bytes: usize }

impl Write for SizeLimit {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > 65_536usize.saturating_sub(self.bytes) {
            return Err(invalid("request exceeds 64 KiB"));
        }
        self.bytes += buf.len();
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

fn check_request_size(req: &Value) -> io::Result<()> {
    serde_json::to_writer(SizeLimit { bytes: 0 }, req)
        .map_err(|e| invalid(format!("request size/serialization: {e}")))
}

fn object<'a>(v: &'a Value, label: &str) -> io::Result<&'a Map<String, Value>> {
    v.as_object().ok_or_else(|| invalid(format!("{label} must be an object")))
}

fn fields(v: &Value, label: &str, expected: &[&str]) -> io::Result<()> {
    let m = object(v, label)?;
    if m.len() != expected.len() || expected.iter().any(|k| !m.contains_key(*k)) {
        return Err(invalid(format!("unsupported {label} fields")));
    }
    Ok(())
}

fn require(v: &Value, expected: Value, label: &str) -> io::Result<()> {
    if *v != expected { return Err(invalid(format!("unsupported {label}"))); }
    Ok(())
}

fn text<'a>(v: &'a Value, label: &str) -> io::Result<&'a str> {
    v.as_str().filter(|s| !s.is_empty() && !s.contains('\0'))
        .ok_or_else(|| invalid(format!("{label} must be a nonempty string without NUL")))
}

fn request_prefix(id: &str) -> Vec<u8> {
    // Length-delimited request id prevents prefix collisions between distinct ids.
    format!("{}:{id}:", id.len()).into_bytes()
}

fn intent_prefix(req: &Value) -> io::Result<Vec<u8>> {
    let mut prefix = request_prefix(req["id"].as_str().unwrap());
    // serde_json's object map is canonically ordered for this prototype. Retain
    // the complete intent rather than a non-cryptographic hash: no collision can
    // acknowledge a different payload, and WAL replay reconstructs it verbatim.
    let payload = serde_json::to_vec(req).map_err(io::Error::other)?;
    if payload.len() > 65536 { return Err(invalid("request exceeds 64 KiB")); }
    prefix.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    prefix.extend_from_slice(&payload);
    prefix.push(0);
    Ok(prefix)
}

fn block_id(intent: &[u8], name: &str) -> Vec<u8> {
    let mut id = intent.to_vec();
    id.extend_from_slice(name.as_bytes());
    id
}

fn read_shape(v: &Value, entity: &str) -> io::Result<()> {
    fields(v, "read block", &["find"])?;
    let query = &v["find"];
    fields(query, "find", &[entity])?;
    let q = &query[entity];
    match entity {
        "Customer" => {
            fields(q, "Customer lookup", &["email"])?;
            match q["email"].as_str() {
                Some(s) if !s.is_empty() && !s.contains('\0')
                    && (s == "$email" || (!s.starts_with('$') && !s.starts_with("-$"))) => Ok(()),
                _ => Err(invalid("unsupported Customer email expression")),
            }
        }
        _ => Err(invalid("unsupported read entity")),
    }
}

fn validate(req: &Value) -> io::Result<bool> {
    fields(req, "request", &["id", "class", "params", "blocks"])?;
    check_request_size(req)?;
    text(&req["id"], "request id")?;
    if req["id"].as_str().unwrap().len() > 1024 { return Err(invalid("request id too long")); }
    fields(&req["class"], "class", &["durability", "retry_horizon_s"])?;
    let class = &req["class"];
    if !matches!(class["durability"].as_str(), Some("batched" | "durable"))
        || class["retry_horizon_s"].as_u64() != Some(300) {
        return Err(invalid("unsupported service class"));
    }
    fields(&req["params"], "params", &["sku", "loc", "qty", "email", "order_no"])?;
    let p = &req["params"];
    for field in ["sku", "loc", "email", "order_no"] { text(&p[field], field)?; }
    if p["qty"].as_i64().filter(|&x| x > 0).is_none() { return Err(invalid("qty must be a positive i64")); }
    let blocks = object(&req["blocks"], "blocks")?;
    if blocks.len() != 2 || !blocks.contains_key("cust") || !blocks.contains_key("take") {
        return Err(invalid("only cust -> take order_flow is supported"));
    }
    let cust = &blocks["cust"];
    let fallback = object(cust, "cust")?.contains_key("else");
    fields(cust, "cust", if fallback { &["find", "else"] } else { &["find"] })?;
    read_shape(&json!({"find": cust["find"]}), "Customer")?;
    if fallback { read_shape(&cust["else"], "Customer")?; }
    let take = &blocks["take"];
    fields(take, "take", &["needs", "ops", "else"])?;
    require(&take["needs"], json!(["cust"]), "take.needs")?;
    let ops = take["ops"].as_array().filter(|v| v.len() == 3)
        .ok_or_else(|| invalid("take must have exactly three ops"))?;
    require(&ops[0], json!({"patch":{"Stock":{"where":{"sku":"$sku","loc":"$loc","on_hand":{"$gte":"$qty"}},"set":{"$inc":{"on_hand":"-$qty"}}}}}), "stock patch")?;
    fields(&ops[1], "movement op", &["put"])?;
    fields(&ops[1]["put"], "movement put", &["StockMovement"])?;
    let movement = &ops[1]["put"]["StockMovement"];
    fields(movement, "movement", &["movement_no", "sku", "loc", "delta", "order_no", "stock"])?;
    text(&movement["movement_no"], "movement_no")?;
    if movement["movement_no"].as_str().unwrap().starts_with('$')
        || movement["movement_no"].as_str().unwrap().starts_with("-$") {
        return Err(invalid("movement_no expressions unsupported"));
    }
    for (key, expected) in [("sku", "$sku"), ("loc", "$loc"), ("delta", "-$qty"), ("order_no", "$order_no")] {
        require(&movement[key], json!(expected), "movement field")?;
    }
    require(&movement["stock"], json!({"sku":"$sku","loc":"$loc"}), "movement stock ref")?;
    require(&ops[2], json!({"put":{"Order":{"order_no":"$order_no","buyer":"$cust._ref","status":"open","lines":[{"sku":"$sku","qty":"$qty"}]}}}), "order put")?;
    require(&take["else"], json!([{"put":{"Backorder":{"sku":"$sku","qty":"$qty","order_no":"$order_no"}}}]), "backorder fallback")?;
    Ok(fallback)
}

fn bind(value: &Value, params: &Value, bindings: &BTreeMap<String, Value>) -> io::Result<Value> {
    match value {
        Value::String(s) if s.starts_with("-$") => {
            let positive = bind(&Value::String(s[1..].to_string()), params, bindings)?;
            let n = positive.as_i64().ok_or_else(|| invalid("negation requires i64"))?;
            Ok(json!(n.checked_neg().ok_or_else(|| invalid("negation overflow"))?))
        }
        Value::String(s) if s.starts_with('$') => {
            let path = &s[1..];
            let v = if let Some((block, field)) = path.split_once('.') {
                bindings.get(block).and_then(|v| v.get(field))
            } else { params.get(path) };
            v.cloned().ok_or_else(|| invalid(format!("unbound reference {s}")))
        }
        Value::Array(a) => Ok(Value::Array(a.iter().map(|v| bind(v, params, bindings)).collect::<io::Result<_>>()?)),
        Value::Object(o) => Ok(Value::Object(o.iter().map(|(k, v)| Ok((k.clone(), bind(v, params, bindings)?))).collect::<io::Result<_>>()?)),
        _ => Ok(value.clone()),
    }
}

fn stock_key(sku: &str, loc: &str) -> Vec<u8> {
    serde_json::to_vec(&[sku, loc]).expect("string array serialization")
}

fn lookup(engine: &Engine, catalog: &Catalog, key: &[u8]) -> io::Result<Option<(u64, Value)>> {
    let snapshot = engine.csn();
    let Some(handle) = engine.unique_lookup(catalog.entity.as_bytes(), catalog.unique.as_bytes(), key, snapshot)? else { return Ok(None) };
    let doc = engine.get(catalog.entity.as_bytes(), handle, snapshot)?
        .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "dangling unique index"))?;
    let row: Value = serde_json::from_slice(doc).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
    Ok(Some((handle, row)))
}

fn document_row(bytes: &[u8], entity: &str) -> io::Result<Value> {
    let row: Value = serde_json::from_slice(bytes).map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
    if !row.is_object() {
        return Err(io::Error::new(ErrorKind::InvalidData, format!("{entity} document is not an object")));
    }
    Ok(row)
}

fn read_text<'a>(value: &'a Value, label: &str) -> io::Result<&'a str> {
    let s = text(value, label)?;
    if s.starts_with('$') || s.starts_with("-$") {
        return Err(invalid(format!("unsupported {label} expression")));
    }
    Ok(s)
}

#[derive(Clone, Copy)]
enum ReadKind { Stock, Customer, Movements }

fn validate_read(req: &Value) -> io::Result<ReadKind> {
    fields(req, "request", &["id", "class", "blocks"])?;
    let id = text(&req["id"], "request id")?;
    if id.len() > 1024 { return Err(invalid("request id too long")); }
    check_request_size(req)?;
    fields(&req["blocks"], "blocks", &["q"])?;
    let q = &req["blocks"]["q"];
    let kind = if q.get("get").is_some() {
        fields(q, "stock block", &["get"])?;
        fields(&q["get"], "get", &["Stock"])?;
        fields(&q["get"]["Stock"], "Stock lookup", &["where"])?;
        fields(&q["get"]["Stock"]["where"], "Stock where", &["sku", "loc"])?;
        read_text(&q["get"]["Stock"]["where"]["sku"], "sku")?;
        read_text(&q["get"]["Stock"]["where"]["loc"], "loc")?;
        ReadKind::Stock
    } else {
        fields(q, "find block", &["find"])?;
        let find = object(&q["find"], "find")?;
        if find.contains_key("Customer") {
            fields(&q["find"], "find", &["Customer"])?;
            fields(&q["find"]["Customer"], "Customer lookup", &["email"])?;
            read_text(&q["find"]["Customer"]["email"], "email")?;
            ReadKind::Customer
        } else {
            fields(&q["find"], "find", &["StockMovement", "order", "limit"])?;
            fields(&q["find"]["StockMovement"], "StockMovement scan", &["where"])?;
            let find = &q["find"];
            fields(&find["StockMovement"]["where"], "StockMovement where", &["order_no"])?;
            read_text(&find["StockMovement"]["where"]["order_no"], "order_no")?;
            require(&find["order"], json!(["movement_no"]), "movement order")?;
            require(&find["limit"], json!(100), "movement limit")?;
            ReadKind::Movements
        }
    };
    let class = &req["class"];
    match kind {
        ReadKind::Stock => fields(class, "class", &["durability", "max_staleness"])?,
        ReadKind::Customer => fields(class, "class", &["durability"])?,
        ReadKind::Movements => fields(class, "class", &["durability", "max_staleness"])?,
    }
    require(&class["durability"], json!("batched"), "durability")?;
    match kind {
        ReadKind::Stock => require(&class["max_staleness"], json!(2), "max_staleness")?,
        ReadKind::Movements => require(&class["max_staleness"], json!(5), "max_staleness")?,
        ReadKind::Customer => {},
    }
    Ok(kind)
}

/// Dispatch only the four inventory templates emitted by the v0 generator.
/// Read blocks observe one current CSN and never publish or journal results.
/// Movement scans traverse the P prefix (not a unique order_no index), retain
/// only the first 100 matching rows ordered by (movement_no, handle), and may
/// still cost a full entity scan. No concurrent isolation or read replay claim.
pub fn execute_request(engine: &mut Engine, req: &Value) -> io::Result<RequestResult> {
    if req.get("blocks").and_then(Value::as_object)
        .is_some_and(|blocks| blocks.contains_key("cust") && blocks.contains_key("take")) {
        return execute_order_flow(engine, req);
    }
    let kind = validate_read(req)?;
    let snapshot = engine.csn();
    let row = match kind {
        ReadKind::Stock => {
            let where_ = &req["blocks"]["q"]["get"]["Stock"]["where"];
            let sku = where_["sku"].as_str().unwrap();
            let loc = where_["loc"].as_str().unwrap();
            let match_ = lookup(engine, &STOCK, &stock_key(sku, loc))?;
            match match_ {
                Some((_, row)) if row["sku"] == sku && row["loc"] == loc => Some(row),
                Some(_) => return Err(io::Error::new(ErrorKind::InvalidData, "Stock U/P disagreement")),
                None => None,
            }
        }
        ReadKind::Customer => {
            let email = req["blocks"]["q"]["find"]["Customer"]["email"].as_str().unwrap();
            let match_ = lookup(engine, &CUSTOMER, email.as_bytes())?;
            match match_ {
                Some((_, row)) if row["email"] == email => Some(row),
                Some(_) => return Err(io::Error::new(ErrorKind::InvalidData, "Customer U/P disagreement")),
                None => None,
            }
        }
        ReadKind::Movements => {
            let order_no = req["blocks"]["q"]["find"]["StockMovement"]["where"]["order_no"].as_str().unwrap();
            let mut rows = BTreeMap::new();
            engine.scan_primary(MOVEMENT.entity.as_bytes(), snapshot, |handle, doc| {
                let row = document_row(doc, MOVEMENT.entity)?;
                let actual_order = row["order_no"].as_str()
                    .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "StockMovement.order_no is not a string"))?;
                let movement_no = row["movement_no"].as_str()
                    .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "StockMovement.movement_no is not a string"))?;
                if actual_order == order_no {
                    rows.insert((movement_no.to_owned(), handle), row);
                    if rows.len() > 100 { rows.pop_last(); }
                }
                Ok(())
            })?;
            if rows.is_empty() { None } else { Some(Value::Array(rows.into_values().collect())) }
        }
    };
    Ok(BTreeMap::from([("q".into(), result(if row.is_some() { "ok" } else { "empty" }, None, row))]))
}

fn unique(catalog: &Catalog, value: Vec<u8>, handle: u64) -> Op {
    Op::PutUnique { entity: catalog.entity.as_bytes().to_vec(), field: catalog.unique.as_bytes().to_vec(), value, handle }
}

fn doc(catalog: &Catalog, handle: u64, value: &Value) -> io::Result<Op> {
    Ok(Op::PutDoc { entity: catalog.entity.as_bytes().to_vec(), handle,
        doc: serde_json::to_vec(value).map_err(io::Error::other)? })
}

fn result(status: &'static str, csn: Option<u64>, row: Option<Value>) -> BlockResult {
    BlockResult { status, csn, row }
}

fn publish(engine: &mut Engine, id: &[u8], ops: &[Op]) -> io::Result<BlockResult> {
    match engine.commit_block(id, ops)? {
        Outcome::Committed { csn } | Outcome::AlreadyCommitted { csn } => Ok(result("ok", Some(csn), None)),
        // Only a false stock guard means "backorder". x-unique is a business
        // constraint violation, not a timing conflict and never an else trigger.
        Outcome::Conflict(conflict) if conflict.kind == "x_unique" =>
            Err(io::Error::new(ErrorKind::AlreadyExists, "unique constraint violation")),
        Outcome::Conflict(_) => Err(io::Error::other("unclassified publish conflict")),
    }
}

/// Execute the generator's exact inventory `cust -> take` template. Validate the
/// *entire* tree before reading or writing. A failed stock guard never publishes
/// movement/order/stock; only its separate backorder fallback may then commit.
/// Each write commit is one WAL record. Replays inspect the write's dedup id first.
/// Caller must serialize all mutations through this `&mut Engine` while executing.
/// Seed Stock and Customer with P/U pairs; no seed/schema endpoint is provided here.
///
/// Failed reads are not durable outcomes; clients needing exact original read
/// bindings across retries must wait for the later general request journal.
/// The engine's dedup has no expiry; this adapter persists the complete
/// canonical request payload in its write block id to reject changed intent.
/// A unique publish conflict leaves the write set unapplied and returns
/// AlreadyExists, never a backorder. Only timing contention may be retried.
pub fn execute_order_flow(engine: &mut Engine, req: &Value) -> io::Result<RequestResult> {
    let has_read_fallback = validate(req)?;
    let id = req["id"].as_str().unwrap();
    let intent = intent_prefix(req)?;
    if engine.request_intent_conflicts(&request_prefix(id), &intent) {
        return Err(invalid("request id reused with different payload"));
    }
    let params = &req["params"];
    let cust = &req["blocks"]["cust"];
    let mut results = BTreeMap::new();
    let take_id = block_id(&intent, "take");
    let else_id = block_id(&intent, "take/else");
    // Write replay must not re-evaluate stock or turn a previously committed
    // order into a backorder. The original read row is deliberately unavailable.
    if let Some(csn) = engine.committed_block_csn(&take_id) {
        results.insert("cust".into(), result("skipped", None, None));
        results.insert("take".into(), result("ok", Some(csn), None));
        return Ok(results);
    }
    if let Some(csn) = engine.committed_block_csn(&else_id) {
        results.insert("cust".into(), result("skipped", None, None));
        results.insert("take".into(), result("failed", None, None));
        results.insert("take/else".into(), result("ok", Some(csn), None));
        return Ok(results);
    }
    let mut bindings = BTreeMap::new();
    let mut selected = None;
    for read in [Some(&cust["find"]), has_read_fallback.then_some(&cust["else"]["find"])].into_iter().flatten() {
        let email = bind(&read["Customer"]["email"], params, &bindings)?;
        if let Some((handle, mut row)) = lookup(engine, &CUSTOMER, text(&email, "email")?.as_bytes())? {
            if row["email"] != email { return Err(io::Error::new(ErrorKind::InvalidData, "Customer U/P disagreement")); }
            let o = row.as_object_mut().ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Customer document is not an object"))?;
            o.insert("_ref".into(), json!(handle));
            selected = Some(row);
            break;
        }
    }
    let Some(customer) = selected else {
        results.insert("cust".into(), result("empty", None, None));
        results.insert("take".into(), result("skipped", None, None));
        return Ok(results);
    };
    results.insert("cust".into(), result("ok", None, Some(customer.clone())));
    bindings.insert("cust".into(), customer);
    // One logical order must not exist both as a fulfilled Order and a
    // Backorder. Their entity-local U prefixes cannot enforce this by
    // themselves; the single-writer &mut Engine boundary serializes the check
    // with publication in this prototype.
    let order_no = text(&params["order_no"], "order_no")?;
    for catalog in [&ORDER, &BACKORDER] {
        if engine.unique_lookup(catalog.entity.as_bytes(), catalog.unique.as_bytes(), order_no.as_bytes(), engine.csn())?.is_some() {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "order number already used"));
        }
    }
    let sku = text(&params["sku"], "sku")?;
    let loc = text(&params["loc"], "loc")?;
    let qty = params["qty"].as_i64().unwrap();
    let stock = lookup(engine, &STOCK, &stock_key(sku, loc))?;
    let available = if let Some((_, ref row)) = stock {
        if row["sku"] != sku || row["loc"] != loc { return Err(io::Error::new(ErrorKind::InvalidData, "Stock U/P disagreement")); }
        let count = row["on_hand"].as_i64().ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "Stock.on_hand is not i64"))?;
        count >= qty
    } else { false };
    if available {
        let (stock_handle, mut stock_row) = stock.unwrap();
        let new_count = stock_row["on_hand"].as_i64().unwrap().checked_sub(qty).ok_or_else(|| invalid("stock decrement overflow"))?;
        stock_row["on_hand"] = json!(new_count);
        let first = engine.next_handle()?;
        let second = first.checked_add(1).ok_or_else(|| invalid("handles exhausted"))?;
        let movement = bind(&req["blocks"]["take"]["ops"][1]["put"]["StockMovement"], params, &bindings)?;
        let mut movement = movement;
        movement["stock"] = json!(stock_handle);
        let order = bind(&req["blocks"]["take"]["ops"][2]["put"]["Order"], params, &bindings)?;
        let movement_no = text(&movement["movement_no"], "movement_no")?;
        let order_no = text(&order["order_no"], "order_no")?;
        let buyer = order["buyer"].as_u64().ok_or_else(|| invalid("buyer must resolve to handle"))?;
        let ops = [
            doc(&STOCK, stock_handle, &stock_row)?,
            doc(&MOVEMENT, first, &movement)?,
            unique(&MOVEMENT, movement_no.as_bytes().to_vec(), first),
            Op::PutReverse { entity: MOVEMENT.entity.as_bytes().to_vec(), field: MOVEMENT.reference.unwrap().as_bytes().to_vec(), target: stock_handle, source: first },
            doc(&ORDER, second, &order)?,
            unique(&ORDER, order_no.as_bytes().to_vec(), second),
            Op::PutReverse { entity: ORDER.entity.as_bytes().to_vec(), field: ORDER.reference.unwrap().as_bytes().to_vec(), target: buyer, source: second },
        ];
        results.insert("take".into(), publish(engine, &take_id, &ops)?);
    } else {
        results.insert("take".into(), result("failed", None, None));
    }
    if results["take"].status == "failed" {
        let handle = engine.next_handle()?;
        let backorder = bind(&req["blocks"]["take"]["else"][0]["put"]["Backorder"], params, &bindings)?;
        let order_no = text(&backorder["order_no"], "order_no")?;
        let ops = [doc(&BACKORDER, handle, &backorder)?, unique(&BACKORDER, order_no.as_bytes().to_vec(), handle)];
        results.insert("take/else".into(), publish(engine, &else_id, &ops)?);
    }
    Ok(results)
}
