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

/// The off-path build + lazy apply must collapse the same run the synchronous
/// `maybe_compact_shape` collapses on an identical twin engine, keep every read
/// identical, and leave a manifest root that reopens with the same reads and
/// the same strictly-increasing, disjoint active cutoffs. Comparing against a
/// real synchronous merge (not a hand-written expectation) means a wrong-pair
/// selection or a divergent reduction fails the test.
#[test]
fn job_build_apply_matches_sync_merge_and_reopens() {
    let job_files = Files::new();
    let sync_files = Files::new();
    let mut job_e = Engine::create(&job_files.wal).unwrap();
    let mut sync_e = Engine::create(&sync_files.wal).unwrap();
    for i in 1..=5u64 {
        let block = format!("b{i}");
        let op = doc(b"Order", i, format!("v{i}").as_bytes());
        job_e.commit_block(block.as_bytes(), &[op.clone()]).unwrap();
        job_e.publish_tile(job_files.dir.join(format!("t{i}.tile")), i).unwrap();
        sync_e.commit_block(block.as_bytes(), &[op]).unwrap();
        sync_e.publish_tile(sync_files.dir.join(format!("t{i}.tile")), i).unwrap();
    }
    let before = readings(&job_e);
    assert_eq!(readings(&sync_e), before, "the twins start identical");

    // Reference: the synchronous path on the twin, then compare everything.
    let ref_merged = sync_e.maybe_compact_shape(u64::MAX, 2, CompactShape::CheapestPair).unwrap()
        .expect("five tiles have an eligible cheapest pair");
    assert!(ref_merged > 0);
    let ref_cutoffs = sync_e.active_tile_cutoffs();
    let ref_reads = readings(&sync_e);

    let job = job_e.compaction_job(u64::MAX, 2, CompactShape::CheapestPair).unwrap()
        .expect("five tiles have an eligible cheapest pair");
    assert_eq!(job.input_names.len(), 2, "CheapestPair merges exactly one pair");
    let outcome = build_compaction_job(&job_files.dir, &job).unwrap();
    let out = job_e.apply_compaction_job(&outcome).unwrap();
    assert!(out > 0, "the replacement tile has on-disk bytes");

    let cutoffs = job_e.active_tile_cutoffs();
    assert_eq!(cutoffs, ref_cutoffs, "the job path retires exactly the synchronous run");
    assert!(cutoffs.windows(2).all(|w| w[0] < w[1]), "cutoffs stay strictly increasing: {cutoffs:?}");
    assert_eq!(readings(&job_e), ref_reads, "every read matches the synchronous twin");

    // Both directories reopen with identical reads and cutoffs.
    drop(job_e);
    drop(sync_e);
    let job_reopened = Engine::open_discover(&job_files.dir).unwrap();
    let sync_reopened = Engine::open_discover(&sync_files.dir).unwrap();
    assert_eq!(readings(&job_reopened), readings(&sync_reopened), "reopened reads match");
    assert_eq!(job_reopened.active_tile_cutoffs(), sync_reopened.active_tile_cutoffs(), "reopened cutoffs match");
}

/// `apply_compaction_job` must re-verify the replacement tile's own header
/// against the built outcome before installing it, so a swapped or stale file
/// cannot be flipped into the active list.
#[test]
fn apply_compaction_job_rejects_mismatched_digest_or_cutoff() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    for i in 1..=3u64 {
        e.commit_block(format!("b{i}").as_bytes(), &[doc(b"Order", i, format!("v{i}").as_bytes())]).unwrap();
        e.publish_tile(files.dir.join(format!("t{i}.tile")), i).unwrap();
    }
    let job = e.compaction_job(u64::MAX, 2, CompactShape::CheapestPair).unwrap().expect("pair");
    let mut outcome = build_compaction_job(&files.dir, &job).unwrap();
    let real_digest = outcome.digest;

    outcome.digest = [0u8; 32];
    assert_eq!(e.apply_compaction_job(&outcome).unwrap_err().kind(), std::io::ErrorKind::InvalidData);

    outcome.digest = real_digest;
    outcome.cutoff += 1;
    assert_eq!(e.apply_compaction_job(&outcome).unwrap_err().kind(), std::io::ErrorKind::InvalidData);

    assert_eq!(e.active_tile_cutoffs(), vec![1, 2, 3], "failed applies changed nothing");
}

/// Crash-consistency approximation for the lazy apply's ordering: once the
/// lazy apply has written the new manifest page, forcing that page durable
/// (simulating the OS writing it back without any explicit sync) must still
/// reopen, because `build_compaction_job` already made the replacement tile's
/// file and directory entry durable before the outcome was applied.
///
/// What this proves: a manifest page referencing the replacement tile can
/// become durable at any point after `apply_compaction_job` and still name a
/// tile that exists and matches the outcome. What it does NOT prove: it runs in
/// one process and never drops not-yet-durable writes, so it cannot reproduce a
/// real power loss that discards a directory entry; the VFS would still expose
/// the dirent even if the build-side `sync_all` were removed.
#[test]
fn lazy_apply_can_be_made_manifest_durable_and_still_reopens() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    for i in 1..=5u64 {
        e.commit_block(format!("b{i}").as_bytes(), &[doc(b"Order", i, format!("v{i}").as_bytes())]).unwrap();
        e.publish_tile(files.dir.join(format!("t{i}.tile")), i).unwrap();
    }
    let before = readings(&e);
    let job = e.compaction_job(u64::MAX, 2, CompactShape::CheapestPair).unwrap().expect("pair");
    let outcome = build_compaction_job(&files.dir, &job).unwrap();
    let out = e.apply_compaction_job(&outcome).unwrap();
    assert!(out > 0);

    // Simulate the OS writing back the just-written lazy manifest page with no
    // further directory sync; the build already made the tile durable.
    fs::File::open(files.dir.join("manifest")).unwrap().sync_data().unwrap();

    drop(e);
    let reopened = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(readings(&reopened), before, "the durable manifest page still reopens with the same reads");
    assert_eq!(reopened.active_tile_cutoffs().len(), 4, "the merge survived the manifest writeback");
}

/// Negative control for the approximation above: if the replacement tile's
/// directory entry were lost (the exact failure the build-side `sync_all`
/// prevents), the forced-durable manifest page would make `open_discover` fail
/// with `NotFound`. This shows the harness can detect the failure mode even
/// though it cannot simulate the power loss that would drop the dirent.
#[test]
fn missing_replacement_tile_fails_reopen_after_manifest_writeback() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    for i in 1..=3u64 {
        e.commit_block(format!("b{i}").as_bytes(), &[doc(b"Order", i, format!("v{i}").as_bytes())]).unwrap();
        e.publish_tile(files.dir.join(format!("t{i}.tile")), i).unwrap();
    }
    let job = e.compaction_job(u64::MAX, 2, CompactShape::CheapestPair).unwrap().expect("pair");
    let outcome = build_compaction_job(&files.dir, &job).unwrap();
    e.apply_compaction_job(&outcome).unwrap();
    fs::File::open(files.dir.join("manifest")).unwrap().sync_data().unwrap();
    // Simulate the lost dirent the fix prevents.
    fs::remove_file(files.dir.join(&outcome.name)).unwrap();
    drop(e);
    let err = Engine::open_discover(&files.dir).map(|_| ()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
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
