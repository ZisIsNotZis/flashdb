//! Multi-tile engine: an ordered list of published tiles (strictly increasing
//! cutoffs), cross-tier latest-state reads spanning tiles, and the explicit
//! [`Engine::maybe_checkpoint`] trigger. The WAL stays the sole recovery
//! authority and is never rotated or truncated here.

use flashdb_engine::engine::{Engine, Op, Outcome};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-multi-tile-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { fs::remove_dir_all(&self.dir).unwrap(); } }
fn doc(h: u64, v: &[u8]) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() } }
fn del_doc(h: u64) -> Op { Op::DelDoc { entity: b"E".to_vec(), handle: h } }
fn unique(h: u64) -> Op { Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"v".to_vec(), handle: h } }
fn del_unique(h: u64) -> Op { Op::DelUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"v".to_vec(), handle: h } }
fn reverse(source: u64) -> Op { Op::PutReverse { entity: b"E".to_vec(), field: b"r".to_vec(), target: 10, source } }
fn scan(e: &Engine) -> Vec<(u64, Vec<u8>)> {
    let mut docs = Vec::new();
    e.scan_primary(b"E", |h, bytes| { docs.push((h, bytes.to_vec())); Ok(()) }).unwrap();
    docs
}
fn assert_current_state(e: &Engine, doc3: &[u8]) {
    // Reads see only latest committed state (2026-09-29), across all tiers:
    // tile one covers csn 1-2, tile two csn 3-4, the serving memtable csn 5+.
    // P/E/1 is v4, P/E/2 deleted (tombstone), P/E/3 is whatever was written last.
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v4".to_vec()), "P/E/1 latest");
    assert_eq!(e.get(b"E", 2).unwrap(), None, "P/E/2 tombstoned");
    assert_eq!(e.get(b"E", 3).unwrap(), Some(doc3.to_vec()), "P/E/3 latest");
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(3), "U latest");
    assert_eq!(e.reverse_lookup(b"E", b"r", 10).unwrap(), vec![21, 22, 23], "R latest");
    assert_eq!(scan(e), vec![(1, b"v4".to_vec()), (3, doc3.to_vec())], "scan at latest");
}

#[test]
fn multi_tile_publish_cross_tier_reads_and_checkpointed_reopen() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // Tile one covers CSN 1-2, tile two covers CSN 3-4, the serving memtable
    // holds CSN 5+. The WAL retains every version; reads see latest state only.
    e.commit_block(b"b1", &[doc(1, b"v1"), unique(1), reverse(21)]).unwrap();
    e.commit_block(b"b2", &[doc(2, b"keep"), doc(1, b"v2")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "everything through the cutoff left the memtable");
    e.commit_block(b"b3", &[del_doc(2), del_unique(1), unique(2), reverse(22)]).unwrap();
    e.commit_block(b"b4", &[doc(1, b"v4")]).unwrap();
    e.publish_tile(&files.dir.join("tile-4.tile"), 4).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    // unique(2) still owns "v"; a same-block release+take is the established
    // transfer shape and is what makes U resolve to handle 3.
    e.commit_block(b"b5", &[del_unique(2), unique(3), doc(3, b"live"), reverse(23)]).unwrap();
    assert_eq!(e.csn(), 5);
    assert_eq!(e.serving_memtable_entries(), 4, "only CSN>4 versions stay in memory");
    assert_current_state(&e, b"live");
    assert!(matches!(e.commit_block(b"b6", &[unique(4)]).unwrap(), Outcome::Conflict(_)),
        "publish-time uniqueness consults all tiers");

    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 5);
    assert_eq!(e.serving_memtable_entries(), 4, "verified tile ranges are not rematerialized");
    assert_current_state(&e, b"live");
    // Dedup ids and the handle watermark still come from the retained WAL, not
    // from any tile or manifest checkpoint.
    assert_eq!(e.committed_block_csn(b"b1"), Some(1));
    assert_eq!(e.commit_block(b"b1", &[doc(99, b"ignored")]).unwrap(), Outcome::AlreadyCommitted { csn: 1 });
    assert_eq!(e.next_handle().unwrap(), 24);

    // Discovery leaves the manifest chain writable: the next tile extends the
    // ordered list past both discovered tiles.
    e.publish_tile(&files.dir.join("tile-5.tile"), 5).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"live".to_vec()));
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_current_state(&e, b"live");
    assert_eq!(e.serving_memtable_entries(), 0);
}

#[test]
fn maybe_checkpoint_triggers_only_past_threshold_and_advances_root() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // An empty frontier cannot checkpoint regardless of threshold.
    assert_eq!(e.maybe_checkpoint(u64::MAX).unwrap(), false);
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    // valid_len must EXCEED the threshold; an infinite threshold never fires.
    assert_eq!(e.maybe_checkpoint(u64::MAX).unwrap(), false);
    assert_eq!(e.maybe_checkpoint(0).unwrap(), true, "any retained WAL byte is past a zero threshold");
    let tile_one = files.dir.join(format!("tile-{}.tile", e.csn()));
    assert!(tile_one.exists(), "engine-chosen deterministic plain tile filename");
    assert_eq!(e.serving_memtable_entries(), 0, "checkpoint evicts everything through the current CSN");
    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"a".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0, "the checkpointed tile is referenced by the advanced root");

    // The WAL is never rotated, so valid_len still exceeds the threshold and
    // the deterministic name tile-<csn>.tile already exists: a second trigger
    // at the same CSN is refused (empty CSN range / name collision), never a
    // silent no-op or reuse.
    assert_eq!(e.maybe_checkpoint(0).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(e.serving_memtable_entries(), 0, "refused checkpoint changed nothing");

    // New writes move the frontier; the next trigger publishes the next tile.
    e.commit_block(b"b2", &[doc(2, b"b")]).unwrap();
    assert_eq!(e.maybe_checkpoint(0).unwrap(), true);
    assert!(files.dir.join(format!("tile-{}.tile", e.csn())).exists());
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"a".to_vec()), "handle 1 still served from tile one");
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"b".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0);
}

#[test]
fn checkpoint_name_collision_fails_closed_without_side_effects() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    let colliding = files.dir.join(format!("tile-{}.tile", e.csn()));
    fs::write(&colliding, b"leftover junk").unwrap();
    assert_eq!(e.maybe_checkpoint(0).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(e.serving_memtable_entries(), 1, "refused checkpoint kept the memtable serving");
    assert!(!files.dir.join("manifest").exists(), "refused checkpoint wrote no manifest root");
    assert_eq!(fs::read(&colliding).unwrap(), b"leftover junk", "refused checkpoint left the file alone");
}

#[test]
fn compaction_name_owned_by_live_tile_fails_closed_without_hint_to_delete() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // Publish a tile whose name collides with the deterministic compaction
    // candidate for the next cutoff; the live tile must never be deletable.
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.publish_tile(&files.dir.join("compact-2.tile"), 1).unwrap();
    e.commit_block(b"b2", &[doc(2, b"b")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    let err = e.compact().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::AlreadyExists);
    assert!(err.to_string().contains("live published tile"), "{err}");
    assert!(err.to_string().contains("cannot proceed"), "error must not tell the user to remove a live tile: {err}");
    assert!(files.dir.join("compact-2.tile").exists(), "live tile untouched");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"a".to_vec()));
}

#[test]
fn manifest_refuses_more_than_max_tiles() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // manifest::MAX_TILES = 8: eight tiles fill the bounded page payload.
    for i in 1..=8u64 {
        e.commit_block(&format!("b{i}").into_bytes(), &[doc(i, b"d")]).unwrap();
        e.publish_tile(&files.dir.join(format!("tile-{i}.tile")), i).unwrap();
    }
    e.commit_block(b"b9", &[doc(9, b"d")]).unwrap();
    let err = e.publish_tile(&files.dir.join("tile-9.tile"), 9).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "the tile list is bounded, and a full list is InvalidInput on publish, not corruption");
    assert_eq!(e.serving_memtable_entries(), 1, "refused publish left the memtable serving");
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 9);
    assert_eq!(e.serving_memtable_entries(), 1, "all eight tiles re-verified against the WAL at open");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"d".to_vec()));
    assert_eq!(e.get(b"E", 8).unwrap(), Some(b"d".to_vec()));
}

#[test]
fn overlapping_or_out_of_order_cutoffs_are_rejected() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.commit_block(b"b2", &[doc(2, b"b")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    // Equal or lower cutoff overlaps the active CSN range: refused on publish.
    assert_eq!(e.publish_tile(&files.dir.join("again-eq.tile"), 2).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(e.publish_tile(&files.dir.join("again-low.tile"), 1).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(e.serving_memtable_entries(), 0, "refused publishes changed nothing");
    drop(e);

    // A CRC-valid manifest page listing descending cutoffs is structural
    // corruption, not a loadable older state: fail closed during manifest
    // parse, before any tile file is even opened.
    let entry = |name: &str, cutoff: u64| {
        let mut e = (name.len() as u32).to_le_bytes().to_vec();
        e.extend_from_slice(name.as_bytes());
        e.extend_from_slice(&cutoff.to_le_bytes());
        e.extend_from_slice(&[9u8; 32]);
        e
    };
    let list = |a: (u64, u64), b: (u64, u64)| {
        let mut payload = 2u32.to_le_bytes().to_vec();
        payload.extend(entry("tile-2.tile", a.0));
        payload.extend(entry("tile-1.tile", a.1));
        payload.extend_from_slice(&b.0.to_le_bytes());
        payload.extend_from_slice(&b.1.to_le_bytes());
        let mut page = vec![0u8; 4096];
        page[..8].copy_from_slice(&1u64.to_le_bytes());
        page[12..12 + payload.len()].copy_from_slice(&payload);
        let crc = crc32c(&page[12..]);
        page[8..12].copy_from_slice(&crc.to_le_bytes());
        page
    };
    // Descending cutoffs; the checkpoint matches the (bogus) newest cutoff so
    // only the ordering rule can fire.
    fs::write(files.dir.join("manifest"), list((2, 1), (1, 0))).unwrap();
    assert_eq!(Engine::open_discover(&files.dir).err().unwrap().kind(), ErrorKind::InvalidData);
    // Equal cutoffs overlap by CSN and are equally impossible.
    fs::write(files.dir.join("manifest"), list((2, 2), (2, 0))).unwrap();
    assert_eq!(Engine::open_discover(&files.dir).err().unwrap().kind(), ErrorKind::InvalidData);
    // Ordered cutoffs but a checkpoint that is not the newest cutoff.
    fs::write(files.dir.join("manifest"), list((1, 2), (1, 0))).unwrap();
    assert_eq!(Engine::open_discover(&files.dir).err().unwrap().kind(), ErrorKind::InvalidData);
    // WAL-only recovery stays authoritative throughout.
    assert_eq!(Engine::open(&files.wal).unwrap().csn(), 2);
}

#[test]
fn compact_merges_tiles_into_one_and_serves_latest_state() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // Tile one covers CSN 1-2, tile two CSN 3-4, the serving memtable CSN 5.
    e.commit_block(b"b1", &[doc(1, b"v1"), unique(1), reverse(21)]).unwrap();
    e.commit_block(b"b2", &[doc(2, b"keep"), doc(1, b"v2")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    e.commit_block(b"b3", &[del_doc(2), del_unique(1), unique(2), reverse(22)]).unwrap();
    e.commit_block(b"b4", &[doc(1, b"v4")]).unwrap();
    e.publish_tile(&files.dir.join("tile-4.tile"), 4).unwrap();
    e.commit_block(b"b5", &[del_unique(2), unique(3), doc(3, b"live"), reverse(23)]).unwrap();
    assert_current_state(&e, b"live");
    let memtable_before = e.serving_memtable_entries();
    assert_eq!(memtable_before, 4);
    // Simulate the crash window later: keep the exact bytes of one superseded
    // tile so it can be restored as a crash-before-unlink orphan afterwards.
    let orphan_bytes = fs::read(files.dir.join("tile-2.tile")).unwrap();

    e.compact().unwrap();
    assert_eq!(e.serving_memtable_entries(), memtable_before, "compaction never touches the memtable");
    assert!(!files.dir.join("tile-2.tile").exists(), "superseded tiles are unlinked only after the durable flip");
    assert!(!files.dir.join("tile-4.tile").exists());
    assert!(files.dir.join("compact-4.tile").exists(), "the merged tile uses the engine-chosen name");
    assert_current_state(&e, b"live");

    // Crash between flip and unlink: tile-2.tile survives on disk as an
    // unreferenced orphan. Discovery must ignore it (never adopt) and keep
    // serving everything from the single compacted tile.
    fs::write(files.dir.join("tile-2.tile"), &orphan_bytes).unwrap();
    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 5);
    assert_eq!(e.serving_memtable_entries(), 4);
    assert_current_state(&e, b"live");
    // Dedup ids and the handle watermark still come from the retained WAL.
    assert_eq!(e.committed_block_csn(b"b1"), Some(1));
    assert_eq!(e.commit_block(b"b1", &[doc(99, b"ignored")]).unwrap(), Outcome::AlreadyCommitted { csn: 1 });
    assert_eq!(e.next_handle().unwrap(), 24);

    // One tile left: further compaction is refused...
    assert_eq!(e.compact().unwrap_err().kind(), ErrorKind::InvalidInput);
    // ...but publishing continues the ordered list past the merged tile, and a
    // second compaction collapses the chain again.
    e.commit_block(b"b6", &[doc(3, b"live2")]).unwrap();
    e.publish_tile(&files.dir.join("tile-6.tile"), 6).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    e.compact().unwrap();
    assert!(!files.dir.join("compact-4.tile").exists());
    assert!(!files.dir.join("tile-6.tile").exists());
    assert!(files.dir.join("compact-6.tile").exists());
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v4".to_vec()));
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"live2".to_vec()));
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_current_state(&e, b"live2");
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"live2".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0);
    // The orphan from the crash window is still ignored after everything.
    assert!(files.dir.join("tile-2.tile").exists());
}

#[test]
fn compact_refuses_under_two_tiles_and_leftover_candidates() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    assert_eq!(e.compact().unwrap_err().kind(), ErrorKind::InvalidInput, "zero active tiles");
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.commit_block(b"b2", &[doc(1, b"b")]).unwrap();
    e.publish_tile(&files.dir.join("tile-1.tile"), 1).unwrap();
    assert_eq!(e.compact().unwrap_err().kind(), ErrorKind::InvalidInput, "one active tile");
    e.commit_block(b"b3", &[doc(2, b"c")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();

    // A leftover candidate (crash after tile sync, before manifest flip) with
    // the engine-chosen name fails closed and changes nothing.
    let candidate = files.dir.join("compact-2.tile");
    fs::write(&candidate, b"stale compaction candidate").unwrap();
    assert_eq!(e.compact().unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"b".to_vec()), "refused compaction changed nothing");
    assert_eq!(e.serving_memtable_entries(), 1);
    assert!(files.dir.join("tile-1.tile").exists(), "superseded tiles stay referenced until a successful compact");

    fs::remove_file(&candidate).unwrap();
    e.compact().unwrap();
    assert!(!files.dir.join("tile-1.tile").exists());
    assert!(!files.dir.join("tile-2.tile").exists());
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"b".to_vec()), "newest version survives compaction");
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"c".to_vec()));
}

#[test]
fn compact_releases_the_max_tiles_bound() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // Fill the manifest page: eight tiles is manifest::MAX_TILES.
    for i in 1..=8u64 {
        e.commit_block(&format!("b{i}").into_bytes(), &[doc(i, b"d")]).unwrap();
        e.publish_tile(&files.dir.join(format!("tile-{i}.tile")), i).unwrap();
    }
    e.commit_block(b"b9", &[doc(9, b"d")]).unwrap();
    assert_eq!(e.publish_tile(&files.dir.join("tile-9.tile"), 9).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(e.serving_memtable_entries(), 1, "refused publish left the memtable serving");
    e.compact().unwrap();
    assert!(files.dir.join("compact-8.tile").exists());
    assert!(!files.dir.join("tile-1.tile").exists());
    assert!(!files.dir.join("tile-8.tile").exists());
    assert_eq!(e.serving_memtable_entries(), 1, "compaction still never touches the memtable");
    // The freed page slot accepts new tiles again.
    e.publish_tile(&files.dir.join("tile-9.tile"), 9).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"d".to_vec()));
    assert_eq!(e.get(b"E", 8).unwrap(), Some(b"d".to_vec()));
    assert_eq!(e.get(b"E", 9).unwrap(), Some(b"d".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0);
}

#[test]
fn tiles_and_compaction_keep_only_the_newest_version_per_logical_key() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    // Two versions of one logical key inside ONE published range: the tile
    // keeps only the newest; the superseded v1 is unreadable garbage
    // (2026-09-29: reads never see historical state).
    e.commit_block(b"b1", &[doc(1, b"v1")]).unwrap();
    e.commit_block(b"b2", &[doc(1, b"v2"), doc(2, b"keep")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    assert_eq!(tile_entry_count(&files.dir.join("tile-2.tile")), 2, "superseded v1 dropped; newest doc1 + doc2 remain");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v2".to_vec()));
    // A delete is itself a newest state and is retained through compaction.
    e.commit_block(b"b3", &[del_doc(1)]).unwrap();
    e.commit_block(b"b4", &[doc(3, b"live")]).unwrap();
    e.publish_tile(&files.dir.join("tile-4.tile"), 4).unwrap();
    e.compact().unwrap();
    assert_eq!(tile_entry_count(&files.dir.join("compact-4.tile")), 3,
        "merged tile: tombstone for doc1 (newest state), keep, live — nothing else");
    assert_eq!(e.get(b"E", 1).unwrap(), None, "tombstone is the newest state");
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"keep".to_vec()));
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"live".to_vec()));
    assert_eq!(scan(&e), vec![(2, b"keep".to_vec()), (3, b"live".to_vec())]);
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap(), None);
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"keep".to_vec()));
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"live".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0, "merged tile verified against the reduced WAL projection at open");
}

fn tile_entry_count(path: &PathBuf) -> u64 {
    // Tile header: magic(8) cutoff(8) wal-digest(32) count(8) ...
    let bytes = fs::read(path).unwrap();
    u64::from_le_bytes(bytes[48..56].try_into().unwrap())
}

fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 { crc = (crc >> 1) ^ (if crc & 1 != 0 { 0x82f6_3b78 } else { 0 }); }
    }
    !crc
}

#[test]
fn wal_rotation_rebinds_to_fresh_segment_and_drops_pre_rotation_ids() {
    use flashdb_engine::wal::crc32c;
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"one")]).unwrap();
    assert_eq!(e.rotate_wal(0).unwrap(), false, "no published tiles");
    e.commit_block(b"b2", &[doc(2, b"two")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    // Unpublished frontier: cutoff(2) != csn(3) refuses with InvalidInput.
    e.commit_block(b"b3", &[doc(3, b"three")]).unwrap();
    assert_eq!(e.rotate_wal(0).unwrap_err().kind(), ErrorKind::InvalidInput);
    // Above-threshold refusal: no rotation even with a fully published state.
    e.publish_tile(&files.dir.join("tile-3.tile"), 3).unwrap();
    assert_eq!(e.rotate_wal(u64::MAX).unwrap(), false);
    assert!(e.committed_block_csn(b"b1").is_some());
    assert_eq!(e.rotate_wal(0).unwrap(), true);
    assert!(files.dir.join("data-3.wal").exists(), "new segment named for the cutoff");
    assert!(!files.wal.exists(), "old segment unlinked after the durable flip");
    // Pre-rotation block ids are forgotten by design; new commits go to the
    // new segment and reuse of an old block id commits as a new block.
    assert_eq!(e.committed_block_csn(b"b1"), None, "pre-rotation ids die with the old segment");
    assert!(matches!(e.commit_block(b"b1", &[doc(3, b"three")]).unwrap(), Outcome::Committed { csn: 4 }));
    e.commit_block(b"b4", &[doc(4, b"four")]).unwrap();
    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 5);
    assert_eq!(e.next_handle().unwrap(), 5);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"one".to_vec()));
    assert_eq!(e.get(b"E", 3).unwrap(), Some(b"three".to_vec()));
    // Rotation chain: a second rotation names the newer cutoff.
    e.publish_tile(&files.dir.join("tile-5.tile"), 5).unwrap();
    assert_eq!(e.rotate_wal(0).unwrap(), true);
    assert!(files.dir.join("data-5.wal").exists());
    assert!(!files.dir.join("data-3.wal").exists(), "data-3 was the pre-rotation data.wal");
    // Wrong-segment fail-closed: the manifest names data-5.wal; swapping in a
    // foreign suffix must be rejected at discovery.
    let good = fs::read(files.dir.join("data-5.wal")).unwrap();
    fs::write(files.dir.join("data-5.wal"), b"foreign bytes").unwrap();
    let err = Engine::open_discover(&files.dir).map(|_| ()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    // Restore, then legacy opens on a rotated dir fail closed (data.wal is gone).
    fs::write(files.dir.join("data-5.wal"), &good).unwrap();
    assert!(Engine::open(&files.wal).is_err());
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 4).unwrap(), Some(b"four".to_vec()));
    // _ = crc32c usage guard if unused
    let _ = crc32c;
}

#[test]
fn rotation_crash_windows_leave_recoverable_state() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.publish_tile(&files.dir.join("tile-1.tile"), 1).unwrap();
    e.commit_block(b"b2", &[doc(2, b"b")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    // Crash after new-segment creation but before the manifest flip: the empty
    // data-2.wal is an unreferenced orphan; retry surfaces AlreadyExists (the
    // leftover must be removed manually, never silently reused).
    fs::write(files.dir.join("data-2.wal"), b"orphan").unwrap();
    assert_eq!(e.rotate_wal(0).unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert!(files.wal.exists(), "old segment untouched by the failed rotation");
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"b".to_vec()), "failed rotation changed nothing");
    fs::remove_file(files.dir.join("data-2.wal")).unwrap();
    assert_eq!(e.rotate_wal(0).unwrap(), true);
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"b".to_vec()));
}

#[test]
fn compact_after_rotation_spans_the_segment_floor_and_reopens() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"pre")]).unwrap();
    e.publish_tile(&files.dir.join("tile-1.tile"), 1).unwrap();
    e.commit_block(b"b2", &[doc(2, b"post")]).unwrap();
    e.publish_tile(&files.dir.join("tile-1b.tile"), 2).unwrap();
    // Rotate at cutoff 2: floor = 2, the pre-rotation WAL is retired.
    assert_eq!(e.rotate_wal(0).unwrap(), true);
    e.commit_block(b"b3", &[doc(2, "later".as_bytes())]).unwrap();
    e.publish_tile(&files.dir.join("tile-3.tile"), 3).unwrap();
    // Compaction merges the below-floor tile with the suffix tile: the merged
    // tile legitimately spans the floor and must reopen.
    e.compact().unwrap();
    assert!(files.dir.join("compact-3.tile").exists());
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"pre".to_vec()));
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"later".to_vec()));
    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 3);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"pre".to_vec()));
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"later".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.commit_block(b"b4", &[doc(4, b"four")]).unwrap(), Outcome::Committed { csn: 4 });
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 4).unwrap(), Some(b"four".to_vec()));
}
