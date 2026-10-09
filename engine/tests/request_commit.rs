//! Request-level uniqueness in a commit group (ticket 41).
//!
//! The settled contract: a block is the atomicity unit; a commit group is NOT a
//! validation unit. Uniqueness is evaluated per REQUEST against the projected
//! post-state (the published state plus the effects of the earlier ACCEPTED
//! requests in the same group, in input order). A request that would leave more
//! than one live owner for a touched unique prefix is dropped entirely: no block
//! appended, no CSN consumed, and the other requests commit normally.
//!
//! Every test asserts the DURABLE post-state (reopening the engine from the WAL),
//! not only the return value. The invariant after every call: at most one live
//! owner per unique prefix.
//!
//! Evidence map: only `t4`, `t5` and `duplicate_block_id_within_one_request_is_refused`
//! discriminate the per-request contract from the pre-change whole-group code, and
//! `duplicate_block_id_within_one_request_is_refused` (a1) is the specific regression
//! test for the half-applied-accept defect fixed on top of it. `t1`/`t2`/`t3` are
//! REGRESSION PINS: an independent reconstruction showed the pre-change
//! implementation produced their durable assertions too, so they guard against future
//! regressions but do NOT evidence the per-request unit.

use flashdb_engine::engine::{Conflict, Engine, Op, Outcome, RequestOutcome};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-reqcommit-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn put_doc(h: u64, doc: &str) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: doc.as_bytes().to_vec() } }
fn put_u(h: u64, value: &str) -> Op {
    Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into(), handle: h }
}
fn del_u(h: u64, value: &str) -> Op {
    Op::DelUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into(), handle: h }
}
fn conflict(value: &str) -> Conflict {
    Conflict { kind: "x_unique", entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into() }
}
fn one(block_id: &str, ops: Vec<Op>) -> Vec<(Vec<u8>, Vec<Op>)> {
    vec![(block_id.as_bytes().to_vec(), ops)]
}

// t1 (regression pin, not per-request evidence): one block, two rows swapping a value under a unique key, commits.
#[test]
fn swap_a_value_between_two_rows_in_one_block_commits() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "X"), put_doc(2, "row2"), put_u(2, "Y")]).unwrap();
    let out = e.commit_batch(&[one("swap", vec![del_u(1, "X"), put_u(2, "X"), del_u(2, "Y"), put_u(1, "Y")])]).unwrap();
    assert_eq!(out, vec![RequestOutcome::Committed(vec![Outcome::Committed { csn: 2 }])]);
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 2);
    assert_eq!(e.unique_lookup(b"E", b"u", b"X").unwrap(), Some(2), "X is now owned by row 2");
    assert_eq!(e.unique_lookup(b"E", b"u", b"Y").unwrap(), Some(1), "Y is now owned by row 1");
}

// t2 (regression pin, not per-request evidence): one block, delete then re-insert the same unique value with a new handle, commits.
#[test]
fn delete_then_reinsert_the_same_value_with_a_new_handle_commits() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "X")]).unwrap();
    let out = e.commit_batch(&[one("move", vec![del_u(1, "X"), put_doc(2, "row2"), put_u(2, "X")])]).unwrap();
    assert_eq!(out, vec![RequestOutcome::Committed(vec![Outcome::Committed { csn: 2 }])]);
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.unique_lookup(b"E", b"u", b"X").unwrap(), Some(2));
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"row2".to_vec()));
}

// t3 (regression pin, not per-request evidence): one group, two independent requests; the first releases X, the second takes it.
#[test]
fn earlier_request_releases_a_value_a_later_request_takes() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "X")]).unwrap();
    let out = e.commit_batch(&[
        one("release", vec![del_u(1, "X")]),
        one("take", vec![put_doc(2, "row2"), put_u(2, "X")]),
    ]).unwrap();
    assert_eq!(out, vec![
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 2 }]),
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 3 }]),
    ]);
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 3);
    assert_eq!(e.unique_lookup(b"E", b"u", b"X").unwrap(), Some(2));
}

// t4: one group of N requests where exactly one conflicts.
#[test]
fn one_conflicting_request_leaves_its_neighbours_committed_without_a_csn_gap() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "seed")]).unwrap();
    let out = e.commit_batch(&[
        one("r0", vec![put_doc(2, "row2"), put_u(2, "a")]),
        // request 1 conflicts: `seed` is already owned by handle 1 in the published state.
        one("r1", vec![put_doc(3, "row3"), put_u(3, "seed")]),
        one("r2", vec![put_doc(4, "row4"), put_u(4, "b")]),
        one("r3", vec![put_doc(5, "row5"), put_u(5, "c")]),
    ]).unwrap();
    assert_eq!(out, vec![
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 2 }]),
        RequestOutcome::Conflict(conflict("seed")),
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 3 }]),
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 4 }]),
    ]);
    assert_eq!(e.csn(), 4, "three accepted requests consume CSNs 2, 3, 4 with no gap");
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 4);
    assert_eq!(e.unique_lookup(b"E", b"u", b"a").unwrap(), Some(2));
    assert_eq!(e.unique_lookup(b"E", b"u", b"seed").unwrap(), Some(1), "exactly one live owner for the conflicted prefix");
    assert_eq!(e.unique_lookup(b"E", b"u", b"b").unwrap(), Some(4));
    assert_eq!(e.unique_lookup(b"E", b"u", b"c").unwrap(), Some(5));
    assert_eq!(e.get(b"E", 3).unwrap(), None, "the dropped request's row is not durable");
    assert_eq!(e.committed_block_csn(b"r1"), None, "the dropped request consumes no CSN");
    assert_eq!(e.committed_block_csn(b"r2"), Some(3), "an accepted neighbour keeps its own CSN");
}

// t5: one group, two independent requests both inserting the same unique value.
#[test]
fn two_requests_inserting_the_same_value_first_in_input_order_wins() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    let out = e.commit_batch(&[
        one("r0", vec![put_doc(1, "row1"), put_u(1, "dup")]),
        one("r1", vec![put_doc(2, "row2"), put_u(2, "dup")]),
    ]).unwrap();
    assert_eq!(out, vec![
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 1 }]),
        RequestOutcome::Conflict(conflict("dup")),
    ]);
    assert_eq!(e.csn(), 1, "the dropped request consumes no CSN");
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 1);
    assert_eq!(e.unique_lookup(b"E", b"u", b"dup").unwrap(), Some(1), "the first request in input order owns the value");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"row1".to_vec()));
    assert_eq!(e.get(b"E", 2).unwrap(), None, "the dropped request's row is not durable");
}

// a1 (P0 regression for the half-applied-accept defect): a repeat of one block_id
// INSIDE one request is refused as invalid input, before the overlay fold and
// before any append. The review's counterexample: before the fix the overlay
// folded BOTH blocks of the request (h2 live, h1 dead) while only the first block
// was appended, so the request was ACCEPTED and the durable state ended with two
// live owners for P (unique_lookup then failed with InvalidData).
#[test]
fn duplicate_block_id_within_one_request_is_refused_and_changes_nothing() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "P")]).unwrap();
    let len = fs::metadata(&files.wal).unwrap().len();

    let err = e.commit_batch(&[vec![
        (b"x".to_vec(), vec![put_u(2, "P")]),
        (b"x".to_vec(), vec![del_u(1, "P")]),
    ]]).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "a duplicate block_id inside one request is invalid input");
    assert_eq!(e.csn(), 1, "a refused call consumes no CSN");
    assert_eq!(fs::metadata(&files.wal).unwrap().len(), len, "a refused call appends nothing");
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 1);
    assert_eq!(e.unique_lookup(b"E", b"u", b"P").unwrap(), Some(1), "exactly one live owner for P after reopen");
    assert_eq!(e.get(b"E", 2).unwrap(), None, "the second block's handle is not durable");
}

// a2: retry dedup ACROSS the group's requests is unchanged - a block_id repeated
// in a later request replays the first occurrence and consumes no new CSN. Only a
// repeat WITHIN one request is refused.
#[test]
fn repeated_block_id_across_two_requests_replays_the_first_and_costs_no_csn() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    let out = e.commit_batch(&[
        one("x", vec![put_doc(1, "row1"), put_u(1, "P")]),
        one("x", vec![put_doc(1, "row1"), put_u(1, "P")]),
    ]).unwrap();
    assert_eq!(out, vec![
        RequestOutcome::Committed(vec![Outcome::Committed { csn: 1 }]),
        RequestOutcome::Committed(vec![Outcome::AlreadyCommitted { csn: 1 }]),
    ]);
    assert_eq!(e.csn(), 1, "the replayed block consumes no new CSN");
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 1);
    assert_eq!(e.unique_lookup(b"E", b"u", b"P").unwrap(), Some(1));
    assert_eq!(e.committed_block_csn(b"x"), Some(1), "one durable record for the replayed block id");
}

// a3: overlay/pending agreement - one request whose blocks delete one owner and
// add another for the SAME prefix still commits (the case the per-request
// contract exists for). Both blocks are appended and the overlay reflects both.
#[test]
fn one_request_deleting_then_adding_an_owner_for_the_same_prefix_commits() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "row1"), put_u(1, "P")]).unwrap();
    let out = e.commit_batch(&[vec![
        (b"release".to_vec(), vec![del_u(1, "P")]),
        (b"claim".to_vec(), vec![put_doc(2, "row2"), put_u(2, "P")]),
    ]]).unwrap();
    assert_eq!(out, vec![RequestOutcome::Committed(vec![
        Outcome::Committed { csn: 2 },
        Outcome::Committed { csn: 3 },
    ])]);
    drop(e);

    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 3);
    assert_eq!(e.unique_lookup(b"E", b"u", b"P").unwrap(), Some(2), "P moved from handle 1 to handle 2");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"row1".to_vec()));
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"row2".to_vec()));
    assert_eq!(e.committed_block_csn(b"release"), Some(2));
    assert_eq!(e.committed_block_csn(b"claim"), Some(3));
}
