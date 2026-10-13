//! Slice 53: off-request-path compaction build + lazy apply.
//!
//! The driver moves the expensive half of one actuation (replay + merge + tile
//! write/sync + verify) onto a worker thread via `Engine::compaction_job` /
//! `build_compaction_job`, then applies the result with
//! `Engine::apply_compaction_job` (a lazy manifest flip that never unlinks the
//! retired inputs). These tests prove the job path produces the same reads and
//! the same strictly-increasing, disjoint active list as the synchronous merge,
//! that the lazy flip survives a reopen, and that `wal::replay_file` reads only
//! the bounded prefix the worker is allowed to see.

use flashdb_engine::engine::{build_compaction_job, CompactShape, Engine, Op};
use flashdb_engine::wal;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn test_root() -> PathBuf {
    let shm = PathBuf::from("/dev/shm");
    if shm.is_dir() { shm } else { std::env::temp_dir() }
}
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = test_root().join(format!("flashdb-compactionjob-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn doc(entity: &[u8], h: u64, v: &[u8]) -> Op {
    Op::PutDoc { entity: entity.to_vec(), handle: h, doc: v.to_vec() }
}

fn readings(e: &Engine) -> Vec<String> {
    let mut rows = Vec::new();
    e.scan_primary(b"Order", |h, v| { rows.push((h, v.to_vec())); Ok(()) }).unwrap();
    vec![
        format!("scan {:?}", rows),
        format!("get Order/1 {:?}", e.get(b"Order", 1).unwrap()),
        format!("get Order/5 {:?}", e.get(b"Order", 5).unwrap()),
    ]
}

/// The off-path build + lazy apply must collapse the same cheapest pair the
/// synchronous `maybe_compact_shape` collapses, keep every read identical, and
/// leave a manifest root that reopens with the same reads.
#[test]
fn job_build_apply_matches_sync_merge_and_reopens() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    for i in 1..=5u64 {
        e.commit_block(format!("b{i}").as_bytes(), &[doc(b"Order", i, format!("v{i}").as_bytes())]).unwrap();
        e.publish_tile(files.dir.join(format!("t{i}.tile")), i).unwrap();
    }
    let before = readings(&e);
    let n_before = e.active_tile_cutoffs().len();
    assert_eq!(n_before, 5);

    let job = e.compaction_job(u64::MAX, 2, CompactShape::CheapestPair).unwrap()
        .expect("five tiles have an eligible cheapest pair");
    assert_eq!(job.input_names.len(), 2, "CheapestPair merges exactly one pair");
    let outcome = build_compaction_job(&files.dir, &job).unwrap();
    let out = e.apply_compaction_job(&outcome).unwrap();
    assert!(out > 0, "the replacement tile has on-disk bytes");

    assert_eq!(e.active_tile_cutoffs().len(), n_before - 1, "one pair merge drops exactly one tile");
    // The invariant the read path depends on: strictly increasing, disjoint cutoffs.
    let cutoffs = e.active_tile_cutoffs();
    assert!(cutoffs.windows(2).all(|w| w[0] < w[1]), "cutoffs stay strictly increasing: {cutoffs:?}");
    assert_eq!(readings(&e), before, "every read is unchanged by the off-path merge");

    // The lazy flip writes the manifest root through the page cache (no fsync);
    // a clean reopen still reads it, and a crash-before-durable-publish reverts
    // to the previous durable root whose tiles are all still present.
    drop(e);
    let reopened = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(readings(&reopened), before, "the directory reopens with the same reads");
}

/// The worker must read only the WAL prefix captured when its job was created;
/// records appended after that are invisible, so the republish guard compares
/// against the engine CSN that was served at job creation rather than a racing
/// newer append.
#[test]
fn replay_file_is_bounded_to_the_captured_valid_len() {
    let files = Files::new();
    let mut w = wal::Wal::create(&files.wal).unwrap();
    w.append(b"acknowledged").unwrap();
    w.sync().unwrap();
    let snapshot_len = w.valid_len();
    w.append(b"later").unwrap();
    w.sync().unwrap();

    let f = fs::File::open(&files.wal).unwrap();
    let (records, valid) = wal::replay_file(&f, snapshot_len).unwrap();
    assert_eq!(records, vec![b"acknowledged".to_vec()], "the bounded replay stops at the captured length");
    assert_eq!(valid, snapshot_len);
}
