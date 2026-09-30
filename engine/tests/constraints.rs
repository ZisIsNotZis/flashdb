//! Projected post-state constraint validation: a block is validated against the
//! state it *would* produce (published state ∪ the block's own net effect), not
//! against the state before it. This is what makes intra-block joint feasibility
//! legal: A alone may be unable to insert and B alone unable, while A+B together
//! are consistent (release-then-reuse, swaps, and future expression/aggregate
//! constraints of the same shape).
//!
//! Author requirement, 2026-09-30: "MVCC 先尝试写入，再检查，检查通过才落盘，
//! 落盘完成才 claim 成功."

use flashdb_engine::engine::{Engine, Op, Outcome};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-constraints-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn put_doc(h: u64) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: format!("doc{h}").into_bytes() } }
fn put_u(h: u64, value: &str) -> Op {
    Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into(), handle: h }
}
fn del_u(h: u64, value: &str) -> Op {
    Op::DelUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.into(), handle: h }
}

/// The author's core case: the owner of `v` releases it in the same block in
/// which another row claims it. Pre-state validation would reject this.
#[test]
fn intra_block_release_then_reuse_is_legal() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    assert_eq!(
        e.commit_block(b"seed", &[put_doc(1), put_u(1, "v")]).unwrap(),
        Outcome::Committed { csn: 1 }
    );
    // Neither op is legal against the pre-state on its own: DelUnique(1,"v") is a
    // removal (fine) but PutUnique(2,"v") alone would collide with handle 1.
    let outcome = e.commit_block(b"handover", &[put_doc(2), del_u(1, "v"), put_u(2, "v")]).unwrap();
    assert_eq!(outcome, Outcome::Committed { csn: 2 });
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2));
}

/// Joint feasibility across two different values, each illegal alone.
#[test]
fn intra_block_unique_swap_is_legal() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1), put_doc(2), put_u(1, "a"), put_u(2, "b")]).unwrap();
    let outcome = e.commit_block(
        b"swap",
        &[del_u(1, "a"), put_u(1, "b"), del_u(2, "b"), put_u(2, "a")],
    ).unwrap();
    assert_eq!(outcome, Outcome::Committed { csn: 2 }, "each single claim collides pre-state; the pair is consistent");
    assert_eq!(e.unique_lookup(b"E", b"u", b"a").unwrap(), Some(2));
    assert_eq!(e.unique_lookup(b"E", b"u", b"b").unwrap(), Some(1));
}

/// Two claims for one value inside one block are a real violation and must be a
/// terminal conflict, with nothing durable: no CSN advance, no WAL growth.
#[test]
fn intra_block_double_claim_is_rejected_and_persists_nothing() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1)]).unwrap();
    let wal_len = fs::metadata(&files.wal).unwrap().len();
    let outcome = e.commit_block(b"clash", &[put_doc(1), put_doc(2), put_u(1, "v"), put_u(2, "v")]).unwrap();
    assert!(matches!(outcome, Outcome::Conflict(c) if c.kind == "x_unique"));
    assert_eq!(e.csn(), 1, "conflict must not advance the CSN");
    assert_eq!(fs::metadata(&files.wal).unwrap().len(), wal_len, "conflict must not reach the WAL");
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), None);
    assert_eq!(e.get(b"E", 2).unwrap(), None, "no partial block survives");
}

/// The same joint-feasibility rule must hold when the published owner lives in a
/// tile (disk) rather than the memtable — i.e. the overlay merge is not an
/// accident of everything being in RAM.
#[test]
fn intra_block_release_then_reuse_works_across_a_published_tile() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1), put_u(1, "v")]).unwrap();
    e.publish_tile(files.dir.join("tile-1.tile"), 1).unwrap();
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(1));
    let outcome = e.commit_block(b"handover", &[put_doc(2), del_u(1, "v"), put_u(2, "v")]).unwrap();
    assert_eq!(outcome, Outcome::Committed { csn: 2 });
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2));
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2));
}
