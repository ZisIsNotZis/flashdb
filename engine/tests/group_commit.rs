//! Deferred-barrier group commit (Slice 23): a group of blocks is appended and
//! applied in order with its single WAL barrier taken only at `end_group`, so
//! every later probe/guard in the group sees every earlier member exactly as a
//! sequence of single commits would.
//!
//! The load-bearing contract pinned here:
//!
//! - in-order visibility: two stock decrements in one group cannot both pass a
//!   guard sequential execution would fail on the second (no oversell);
//! - the durable bytes are byte-identical to the same requests committed one at
//!   a time (only the barrier count differs);
//! - N grouped blocks consume N CSNs;
//! - a conflict is per-block, not a whole-group rejection;
//! - a torn tail before the barrier loses the whole group on recovery while a
//!   complete barrier replays all of it;
//! - a grouped commit outside a group, a second `end_group`, and a nested
//!   `begin_group` all fail closed, as do the other mutating paths.

use flashdb_engine::engine::{Engine, GroupPhase, Op, Outcome};
use flashdb_engine::replay::execute_requests_grouped;
use flashdb_engine::request::execute_request;
use flashdb_engine::wal;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-group-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn put_doc(h: u64, doc: &str) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: doc.as_bytes().to_vec() } }
fn put_u(h: u64, value: &str) -> Op {
    Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into(), handle: h }
}
/// Input block: id `b<n>`, doc handle `n`, unique value `v<n>`.
fn block(n: u64) -> (Vec<u8>, Vec<Op>) {
    (format!("b{n}").into_bytes(), vec![put_doc(n, &format!("doc{n}")), put_u(n, &format!("v{n}"))])
}
fn wal_records(f: &Files) -> usize { wal::replay(&f.wal).unwrap().0.len() }

// ---------- request-surface helpers ----------

/// The generator's exact `cust -> take` shape, one order per request.
fn flow_request(id: &str, qty: i64, order_no: &str, movement_no: &str) -> Value {
    json!({
        "id": id, "class": {"durability": "batched", "retry_horizon_s": 300},
        "params": {"sku": "S000001", "loc": "L000", "qty": qty, "email": "c000001@example.com", "order_no": order_no},
        "blocks": {
            "cust": {"find": {"Customer": {"email": "$email"}}},
            "take": {"needs": ["cust"], "ops": [
                {"patch": {"Stock": {"where": {"sku": "$sku", "loc": "$loc", "on_hand": {"$gte": "$qty"}},
                                      "set": {"$inc": {"on_hand": "-$qty"}}}}},
                {"put": {"StockMovement": {"movement_no": movement_no, "sku": "$sku", "loc": "$loc",
                                           "delta": "-$qty", "order_no": "$order_no", "stock": {"sku": "$sku", "loc": "$loc"}}}},
                {"put": {"Order": {"order_no": "$order_no", "buyer": "$cust._ref", "status": "open",
                                   "lines": [{"sku": "$sku", "qty": "$qty"}]}}}
            ], "else": [{"put": {"Backorder": {"sku": "$sku", "qty": "$qty", "order_no": "$order_no"}}}]}
        }
    })
}

/// Seed the one Stock row and one Customer the flow probes, in a single block.
fn seed_flow(engine: &mut Engine, on_hand: i64) {
    let stock = json!({"sku": "S000001", "loc": "L000", "on_hand": on_hand});
    let customer = json!({"email": "c000001@example.com"});
    engine.commit_block(b"fixture-seed", &[
        Op::PutDoc { entity: b"Stock".to_vec(), handle: 10, doc: serde_json::to_vec(&stock).unwrap() },
        Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku_loc".to_vec(),
            value: serde_json::to_vec(&["S000001", "L000"]).unwrap(), handle: 10 },
        Op::PutDoc { entity: b"Customer".to_vec(), handle: 20, doc: serde_json::to_vec(&customer).unwrap() },
        Op::PutUnique { entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"c000001@example.com".to_vec(), handle: 20 },
    ]).unwrap();
}
fn on_hand(engine: &Engine) -> i64 {
    let doc = engine.get(b"Stock", 10).unwrap().unwrap();
    serde_json::from_slice::<Value>(&doc).unwrap()["on_hand"].as_i64().unwrap()
}

// ---------- tests ----------

/// The point of the slice: guards inside a group are evaluated in order against
/// the group's applied state. Two 3-unit takes from a 3-unit row: the first
/// succeeds, the second must fail its guard and take its `else` branch. If every
/// guard were evaluated against the pre-group state both would pass and the row
/// would be driven to -3 (oversell).
#[test]
fn in_order_visibility_prevents_oversell_within_a_group() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    seed_flow(&mut e, 3);
    let requests = [
        flow_request("r-00000001", 3, "O00000001", "M00000001"),
        flow_request("r-00000002", 3, "O00000002", "M00000002"),
    ];
    let results = execute_requests_grouped(&mut e, &requests).unwrap();
    assert_eq!(results[0].as_ref().unwrap()["take"].status, "ok");
    assert_eq!(results[1].as_ref().unwrap()["take"].status, "failed");
    assert_eq!(results[1].as_ref().unwrap()["take/else"].status, "ok",
        "the second request must take its else branch, not oversell");
    assert_eq!(on_hand(&e), 0, "sequential semantics: 3 - 3, never negative");
    // The conflicting stock write of request two never reached the WAL: it holds
    // the seed, request one's take block, and request two's backorder block.
    assert_eq!(wal_records(&files), 3);
    assert_eq!(e.csn(), 3);
    drop(e);
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(on_hand(&e), 0, "the in-order outcome survives recovery");
}

/// The durable bytes of a grouped run are exactly the bytes of the same
/// requests executed one at a time (each taking its own barrier). Only the
/// barrier count differs, so grouping does not trade durability or content.
#[test]
fn grouped_requests_are_byte_identical_to_sequential_requests() {
    let single = Files::new();
    let mut one = Engine::create(&single.wal).unwrap();
    seed_flow(&mut one, 100);
    let requests = [
        flow_request("r-00000001", 3, "O00000001", "M00000001"),
        flow_request("r-00000002", 4, "O00000002", "M00000002"),
        flow_request("r-00000003", 1, "O00000003", "M00000003"),
    ];
    for req in &requests {
        execute_request(&mut one, req).unwrap();
    }
    drop(one);

    let grouped = Files::new();
    let mut all = Engine::create(&grouped.wal).unwrap();
    seed_flow(&mut all, 100);
    let results = execute_requests_grouped(&mut all, &requests).unwrap();
    for result in &results {
        assert_eq!(result.as_ref().unwrap()["take"].status, "ok");
    }
    drop(all);

    assert_eq!(fs::read(&single.wal).unwrap(), fs::read(&grouped.wal).unwrap(),
        "a group is the same records, order and framing as the same requests committed one at a time");
    assert_eq!(wal_records(&grouped), 4, "seed + one block per request");
}

/// N grouped blocks consume exactly N CSNs, in input order.
#[test]
fn grouped_n_blocks_produce_n_csns() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.begin_group().unwrap();
    for n in 1..=3u64 {
        let (id, ops) = block(n);
        assert_eq!(e.commit_block_grouped(&id, &ops).unwrap(), Outcome::Committed { csn: n });
    }
    e.end_group().unwrap();
    assert_eq!(e.csn(), 3);
    for n in 1..=3u64 {
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()));
        assert_eq!(e.unique_lookup(b"E", b"u", format!("v{n}").as_bytes()).unwrap(), Some(n));
    }
    assert_eq!(wal_records(&files), 3);
}

/// A conflict is a per-block outcome, exactly like a single commit: the middle
/// block is rejected with nothing durable, while the blocks before and after it
/// in the same group commit and are made durable by the one barrier.
#[test]
fn conflict_in_the_middle_of_a_group_is_per_block() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "doc1"), put_u(1, "v")]).unwrap();
    e.begin_group().unwrap();
    assert_eq!(e.commit_block_grouped(b"a", &[put_doc(2, "doc2")]).unwrap(), Outcome::Committed { csn: 2 });
    let conflict = e.commit_block_grouped(b"b", &[put_doc(3, "doc3"), put_u(3, "v")]).unwrap();
    assert!(matches!(conflict, Outcome::Conflict(ref c) if c.kind == "x_unique"),
        "the middle block leaves two live owners of v");
    assert_eq!(e.commit_block_grouped(b"c", &[put_doc(4, "doc4")]).unwrap(), Outcome::Committed { csn: 3 },
        "the conflict does not stop later grouped commits (no CSN wasted on it)");
    e.end_group().unwrap();

    assert_eq!(e.csn(), 3);
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"doc2".to_vec()));
    assert_eq!(e.get(b"E", 3).unwrap(), None, "the conflicting block is absent");
    assert_eq!(e.get(b"E", 4).unwrap(), Some(b"doc4".to_vec()));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(1), "the original owner is untouched");
    assert_eq!(e.committed_block_csn(b"b"), None);
    assert_eq!(wal_records(&files), 3, "seed + the two non-conflicting blocks");
}

/// A crash before the barrier loses the whole group: its records were appended
/// but never synced, so a torn tail at the group boundary recovers the pre-group
/// state. Nothing in the group was acknowledged.
#[test]
fn a_crash_before_the_barrier_persists_exactly_the_complete_prefix() {
    // The contract (see `Engine::end_group`): the group is NOT atomic across a crash. Each
    // grouped commit is appended and applied immediately, so recovery replays every complete,
    // CRC-valid frame and the durable state after a crash is the SEQUENTIAL PREFIX. That is the
    // sequential reference semantics and nothing in the group was acknowledged, so no acked work
    // is lost and a retry is idempotent through the business key.
    //
    // The earlier version of this test was named "..._loses_the_whole_group" and truncated the
    // file INSIDE the first group frame, so it only ever proved that a torn first frame is
    // dropped - it could not distinguish "whole group lost" from "prefix survives". Both cases
    // are asserted here instead.
    let files = Files::new();
    let base_len;
    let after_first_frame;
    {
        let mut e = Engine::create(&files.wal).unwrap();
        e.commit_block(b"base", &[put_doc(1, "doc1")]).unwrap();
        base_len = fs::metadata(&files.wal).unwrap().len();
        e.begin_group().unwrap();
        let (id2, ops2) = block(2);
        e.commit_block_grouped(&id2, &ops2).unwrap();
        after_first_frame = fs::metadata(&files.wal).unwrap().len();
        for n in 3..=4u64 {
            let (id, ops) = block(n);
            e.commit_block_grouped(&id, &ops).unwrap();
        }
        // Crash BEFORE end_group: no barrier was taken.
        drop(e);
    }

    // Case 1: a torn FIRST frame (a strict prefix of one frame) is dropped with the rest.
    let f = fs::OpenOptions::new().write(true).open(&files.wal).unwrap();
    f.set_len(base_len + 3).unwrap();
    drop(f);
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 1, "a torn frame is not replayed");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"doc1".to_vec()));
    for n in 2..=4u64 {
        assert_eq!(e.committed_block_csn(format!("b{n}").as_bytes()), None);
        assert_eq!(e.get(b"E", n).unwrap(), None);
    }
    drop(e);

    // Case 2: a complete FIRST frame survives the same crash, so b2 is durable while b3/b4 are
    // not. This is the case the old test never constructed, and it is the one that shows the
    // semantics are prefix-based rather than all-or-nothing.
    let files2 = Files::new();
    let after_first;
    {
        let mut e = Engine::create(&files2.wal).unwrap();
        e.commit_block(b"base", &[put_doc(1, "doc1")]).unwrap();
        e.begin_group().unwrap();
        let (id2, ops2) = block(2);
        e.commit_block_grouped(&id2, &ops2).unwrap();
        after_first = fs::metadata(&files2.wal).unwrap().len();
        for n in 3..=4u64 {
            let (id, ops) = block(n);
            e.commit_block_grouped(&id, &ops).unwrap();
        }
        drop(e);
    }
    assert!(after_first > base_len);
    let f = fs::OpenOptions::new().write(true).open(&files2.wal).unwrap();
    f.set_len(after_first).unwrap();
    drop(f);
    let e = Engine::open(&files2.wal).unwrap();
    assert_eq!(e.csn(), 2, "the complete first frame of the un-acknowledged group replays");
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"doc2".to_vec()));
    for n in 3..=4u64 {
        assert_eq!(e.committed_block_csn(format!("b{n}").as_bytes()), None);
        assert_eq!(e.get(b"E", n).unwrap(), None);
    }
    let _ = after_first_frame;
}

/// A complete barrier replays every member of the group after recovery.
#[test]
fn a_complete_barrier_replays_the_whole_group() {
    let files = Files::new();
    {
        let mut e = Engine::create(&files.wal).unwrap();
        e.begin_group().unwrap();
        for n in 1..=3u64 {
            let (id, ops) = block(n);
            e.commit_block_grouped(&id, &ops).unwrap();
        }
        assert_eq!(e.csn(), 3, "applied immediately, before the barrier");
        e.end_group().unwrap();
    }
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 3);
    for n in 1..=3u64 {
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()));
        assert_eq!(e.committed_block_csn(format!("b{n}").as_bytes()), Some(n));
    }
}

/// The republish guard (`verify_wal_matches_state`) and WAL rotation keep
/// working after a group's barrier, and a second group's barrier still binds the
/// right WAL suffix.
#[test]
fn republish_guard_and_rotation_work_after_a_group() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.begin_group().unwrap();
    for n in 1..=2u64 {
        let (id, ops) = block(n);
        e.commit_block_grouped(&id, &ops).unwrap();
    }
    e.end_group().unwrap();
    // The guard compares the retained WAL against the grouped state; a stale
    // segment_suffix_csn would make this fail.
    e.publish_tile(files.dir.join("tile-2.tile"), 2).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![2]);
    assert!(e.rotate_wal(0).unwrap(), "a non-empty WAL with a full checkpoint rotates");
    assert!(files.dir.join("data-2.wal").exists());

    e.begin_group().unwrap();
    for n in 3..=4u64 {
        let (id, ops) = block(n);
        e.commit_block_grouped(&id, &ops).unwrap();
    }
    e.end_group().unwrap();
    assert_eq!(e.csn(), 4);
    e.publish_tile(files.dir.join("tile-4.tile"), 4).unwrap();
    drop(e);

    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 4);
    for n in 1..=4u64 {
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()), "handle {n} survives recovery");
    }
}

/// A grouped commit outside a group, a second `end_group`, a nested
/// `begin_group`, and every other mutating path while a group is open all fail
/// closed; a clean group still works afterwards.
#[test]
fn grouped_api_fails_closed_outside_and_around_a_group() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    assert!(e.commit_block_grouped(b"x", &[put_doc(1, "d")]).is_err(), "grouped commit needs an open group");
    assert!(e.end_group().is_err(), "end_group needs an open group");

    e.begin_group().unwrap();
    assert!(e.begin_group().is_err(), "a group cannot be nested");
    assert!(e.commit_block(b"y", &[put_doc(2, "d")]).is_err(), "the single-block path refuses during a group");
    assert!(e.commit_batch(&[vec![(b"z".to_vec(), vec![put_doc(3, "d")])]]).is_err(), "commit_batch refuses during a group");
    assert!(e.publish_tile(files.dir.join("t.tile"), 0).is_err(), "publish refuses during a group");
    assert!(e.rotate_wal(0).is_err(), "rotation refuses during a group");
    assert!(e.compact().is_err(), "compaction refuses during a group");
    e.end_group().unwrap();
    assert!(e.end_group().is_err(), "ending a group twice fails closed");

    // A clean empty group is fine, and the engine still commits after it.
    e.begin_group().unwrap();
    e.end_group().unwrap();
    assert!(matches!(e.commit_block(b"ok", &[put_doc(4, "d")]).unwrap(), Outcome::Committed { .. }));
    assert_eq!(wal_records(&files), 1);
}

/// `take_group_phase_us` attributes the group's WAL append, memtable apply and
/// single barrier (slice 55): an empty group accumulates nothing and the
/// accumulator resets exactly after it is read. The phase's absolute elapsed
/// times are not asserted (they depend on the file system and this machine),
/// only the accounting contract the load driver's per-group attribution relies
/// on. A non-zero-accumulation assertion on the non-empty group is deliberately
/// omitted: the counters are `Instant::elapsed().as_micros()`, which truncates a
/// sub-microsecond step to 0, and on a fast `fdatasync` (e.g. a tmpfs-backed
/// temp dir) that makes `> 0` flaky rather than a property of the accounting.
#[test]
fn group_phase_accounting_attributes_and_resets() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();

    // An empty group appended nothing, so `end_group` owes no barrier and the
    // accumulated append/apply/fsync are all zero.
    e.begin_group().unwrap();
    e.end_group().unwrap();
    assert_eq!(e.take_group_phase_us(), GroupPhase { append_us: 0, apply_us: 0, fsync_us: 0 },
        "an empty group accumulates no WAL append/apply/barrier");

    // A group with one block takes one barrier; the elapsed time is accumulated
    // and then reset by the read, and a second read sees exactly zero.
    e.begin_group().unwrap();
    let (id, ops) = block(1);
    assert_eq!(e.commit_block_grouped(&id, &ops).unwrap(), Outcome::Committed { csn: 1 });
    e.end_group().unwrap();
    let _phase = e.take_group_phase_us(); // not asserted > 0: as_micros() truncates sub-us steps on fast filesystems
    assert_eq!(e.take_group_phase_us(), GroupPhase { append_us: 0, apply_us: 0, fsync_us: 0 },
        "the accumulator resets after it is read");
    assert_eq!(wal_records(&files), 1);
}
