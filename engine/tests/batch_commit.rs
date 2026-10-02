//! Batch/group commit (Slice 22): N blocks validated against the batch's
//! projected post-state, appended as N records, and made durable by ONE WAL
//! barrier.
//!
//! The single durable unit is the block, as in `commit_block`; the batch only
//! amortizes the barrier. These tests pin the contract that matters:
//!
//! - a batch of one is byte-identical to `commit_block` (same WAL bytes);
//! - a batch of N commits every block with its own CSN;
//! - uniqueness is evaluated against the batch post-state, so an intra-batch
//!   release + reuse is legal;
//! - an intra-batch duplicate is a whole-batch conflict with nothing durable;
//! - an idempotent retry returns its existing CSN without re-appending and
//!   contributes no post-state;
//! - a batch-appended WAL replays in order, tolerating a torn tail;
//! - the republish guard and WAL rotation keep working after a batch.

use flashdb_engine::engine::{BatchOutcome, Engine, Op, Outcome};
use flashdb_engine::wal;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-batch-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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

/// An input block: id `b<n>`, a doc and its unique value `v<n>`.
fn block(n: u64) -> (Vec<u8>, Vec<Op>) {
    (format!("b{n}").into_bytes(), vec![put_doc(n, &format!("doc{n}")), put_u(n, &format!("v{n}"))])
}

fn wal_file_len(f: &Files) -> u64 { fs::metadata(&f.wal).unwrap().len() }
fn wal_records(f: &Files) -> usize { wal::replay(&f.wal).unwrap().0.len() }

#[test]
fn batch_of_one_is_byte_identical_to_commit_block() {
    let one = Files::new();
    let mut e = Engine::create(&one.wal).unwrap();
    e.commit_block(b"b1", &[put_doc(1, "doc1"), put_u(1, "v1")]).unwrap();
    drop(e);

    let batched = Files::new();
    let mut b = Engine::create(&batched.wal).unwrap();
    let out = b.commit_batch(&[(b"b1".to_vec(), vec![put_doc(1, "doc1"), put_u(1, "v1")])]).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![Outcome::Committed { csn: 1 }]));
    drop(b);

    assert_eq!(fs::read(&one.wal).unwrap(), fs::read(&batched.wal).unwrap(),
        "one batch record must be the same durable bytes as one commit_block");
    assert_eq!(wal_records(&one), 1);

    // A second identical batch is an idempotent retry: same CSN, no new bytes.
    let mut b = Engine::open(&batched.wal).unwrap();
    let len = fs::metadata(&batched.wal).unwrap().len();
    let out = b.commit_batch(&[(b"b1".to_vec(), vec![put_doc(1, "doc1"), put_u(1, "v1")])]).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![Outcome::AlreadyCommitted { csn: 1 }]));
    assert_eq!(fs::metadata(&batched.wal).unwrap().len(), len, "a retry-only batch must not append");
    assert_eq!(b.csn(), 1);
}

#[test]
fn batch_of_n_is_byte_identical_to_n_single_commits() {
    let single = Files::new();
    let mut one = Engine::create(&single.wal).unwrap();
    for n in 1..=4u64 {
        let (id, ops) = block(n);
        one.commit_block(&id, &ops).unwrap();
    }
    drop(one);

    let batched = Files::new();
    let mut all = Engine::create(&batched.wal).unwrap();
    let blocks: Vec<_> = (1..=4u64).map(block).collect();
    all.commit_batch(&blocks).unwrap();
    drop(all);

    assert_eq!(fs::read(&single.wal).unwrap(), fs::read(&batched.wal).unwrap(),
        "a batch is the same records in the same order with the same framing as N single commits");
}

#[test]
fn batch_of_n_commits_all_blocks_with_n_csns() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    let blocks = vec![block(1), block(2), block(3)];
    let out = e.commit_batch(&blocks).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![
        Outcome::Committed { csn: 1 },
        Outcome::Committed { csn: 2 },
        Outcome::Committed { csn: 3 },
    ]));
    assert_eq!(e.csn(), 3, "the batch advances the CSN by one per block");
    for n in 1..=3u64 {
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()));
        assert_eq!(e.unique_lookup(b"E", b"u", format!("v{n}").as_bytes()).unwrap(), Some(n));
        assert_eq!(e.committed_block_csn(format!("b{n}").as_bytes()), Some(n));
    }
    assert_eq!(wal_records(&files), 3, "one WAL record per block");
}

/// The batch analogue of the author's intra-block release-then-reuse: no single
/// block of the pair is legal against the pre-state, but the batch's post-state
/// is consistent. The owner (handle 1) releases `v` in block A; block B claims it.
#[test]
fn intra_batch_release_then_reuse_is_legal() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "doc1"), put_u(1, "v")]).unwrap();
    let out = e.commit_batch(&[
        (b"release".to_vec(), vec![del_u(1, "v")]),
        (b"reuse".to_vec(), vec![put_doc(2, "doc2"), put_u(2, "v")]),
    ]).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![Outcome::Committed { csn: 2 }, Outcome::Committed { csn: 3 }]));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2), "the batch transfers the value to handle 2");
}

#[test]
fn intra_batch_duplicate_is_rejected_and_persists_nothing() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"seed", &[put_doc(1, "doc1")]).unwrap();
    let len = wal_file_len(&files);
    let out = e.commit_batch(&[
        (b"first".to_vec(), vec![put_u(1, "v")]),
        (b"second".to_vec(), vec![put_doc(2, "doc2"), put_u(2, "v")]),
    ]).unwrap();
    match out {
        BatchOutcome::Conflict { index, conflict } => {
            assert_eq!(index, 1, "the later block leaves the second live owner");
            assert_eq!(conflict.kind, "x_unique");
            assert_eq!(conflict.value, b"v".to_vec());
        }
        other => panic!("expected a whole-batch conflict, got {other:?}"),
    }
    assert_eq!(e.csn(), 1, "a conflicted batch must not advance the CSN");
    assert_eq!(wal_file_len(&files), len, "a conflicted batch must not reach the WAL");
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), None);
    assert_eq!(e.get(b"E", 2).unwrap(), None, "no block of a rejected batch survives");
}

/// A retry inside a batch is an idempotent replay: existing CSN, no re-append,
/// and no contribution to the batch overlay. The last point is load-bearing: the
/// retried block's old effect is already published, so re-including it could
/// reject a batch the sequential order accepts.
#[test]
fn retry_inside_a_batch_returns_existing_csn_without_appending() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"done", &[put_doc(1, "doc1"), put_u(1, "v")]).unwrap();
    let len = wal_file_len(&files);
    let out = e.commit_batch(&[
        (b"done".to_vec(), vec![put_doc(1, "doc1"), put_u(1, "v")]),
        (b"next".to_vec(), vec![put_doc(2, "doc2")]),
    ]).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![Outcome::AlreadyCommitted { csn: 1 }, Outcome::Committed { csn: 2 }]));
    assert_eq!(e.csn(), 2, "only the new block consumes a CSN");
    assert_eq!(wal_records(&files), 2, "the retry is not re-appended");
    assert!(wal_file_len(&files) > len, "the new block did append");

    // The retried block's effect was released by a later commit; excluding the
    // retry from the overlay lets the batch re-claim the value.
    let mut e = Engine::open(&files.wal).unwrap();
    e.commit_block(b"release", &[del_u(1, "v")]).unwrap();
    let out = e.commit_batch(&[
        (b"done".to_vec(), vec![put_doc(1, "doc1"), put_u(1, "v")]),
        (b"taker".to_vec(), vec![put_doc(3, "doc3"), put_u(3, "v")]),
    ]).unwrap();
    assert_eq!(out, BatchOutcome::Committed(vec![Outcome::AlreadyCommitted { csn: 1 }, Outcome::Committed { csn: 4 }]));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(3));
}

#[test]
fn empty_batch_is_a_no_op() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    assert_eq!(e.commit_batch(&[]).unwrap(), BatchOutcome::Committed(vec![]));
    assert_eq!(e.csn(), 0);
    assert_eq!(wal_file_len(&files), 0, "an empty batch must not append or sync");
}

#[test]
fn crash_recovery_replays_a_batch_appended_wal_in_order() {
    let files = Files::new();
    let mut blocks = Vec::new();
    for n in 1..=3u64 { blocks.push(block(n)); }
    {
        let mut e = Engine::create(&files.wal).unwrap();
        e.commit_batch(&blocks).unwrap();
    }
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 3);
    for n in 1..=3u64 {
        assert_eq!(e.committed_block_csn(format!("b{n}").as_bytes()), Some(n), "records replay in order");
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()));
        assert_eq!(e.unique_lookup(b"E", b"u", format!("v{n}").as_bytes()).unwrap(), Some(n));
    }
    drop(e);

    // A torn tail drops only the last block, exactly as for single commits.
    let len = fs::metadata(&files.wal).unwrap().len();
    let f = fs::OpenOptions::new().write(true).open(&files.wal).unwrap();
    f.set_len(len - 8).unwrap();
    drop(f);
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.csn(), 2, "the torn block is not published");
    assert_eq!(e.committed_block_csn(b"b3"), None);
    assert_eq!(e.get(b"E", 3).unwrap(), None);
}

#[test]
fn republish_guard_and_rotation_work_after_a_batch() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_batch(&[block(1), block(2)]).unwrap();
    // The republish guard compares the WAL against the state the batch published;
    // the tile build would fail if segment_suffix_csn had not followed the batch.
    e.publish_tile(files.dir.join("tile-2.tile"), 2).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![2]);
    // Rotation requires every committed block published and an empty memtable.
    assert!(e.rotate_wal(0).unwrap(), "a non-empty WAL with a full checkpoint rotates");
    assert!(files.dir.join("data-2.wal").exists());

    // A second batch appends to the new segment, and a republish after it still
    // passes the guard.
    e.commit_batch(&[block(3), block(4)]).unwrap();
    assert_eq!(e.csn(), 4);
    e.publish_tile(files.dir.join("tile-4.tile"), 4).unwrap();
    drop(e);

    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 4);
    for n in 1..=4u64 {
        assert_eq!(e.get(b"E", n).unwrap(), Some(format!("doc{n}").into_bytes()), "handle {n} survives recovery");
    }
    assert_eq!(e.committed_block_csn(b"b3"), Some(3), "post-rotation dedup replays from the segment");
    assert_eq!(e.committed_block_csn(b"b1"), None, "pre-rotation ids are forgotten by design");
}
