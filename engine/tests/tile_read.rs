use flashdb_engine::engine::{Engine, Op, Outcome};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf, tile: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-tile-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), tile: dir.join("data.tile"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { fs::remove_dir_all(&self.dir).unwrap(); } }
fn doc(h: u64, value: &[u8]) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: value.to_vec() } }
fn del_doc(h: u64) -> Op { Op::DelDoc { entity: b"E".to_vec(), handle: h } }
fn unique(h: u64) -> Op { Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"v".to_vec(), handle: h } }
fn del_unique(h: u64) -> Op { Op::DelUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"v".to_vec(), handle: h } }
fn reverse(source: u64) -> Op { Op::PutReverse { entity: b"E".to_vec(), field: b"r".to_vec(), target: 10, source } }
fn del_reverse(source: u64) -> Op { Op::DelReverse { entity: b"E".to_vec(), field: b"r".to_vec(), target: 10, source } }
fn scan(e: &Engine, snapshot: u64) -> Vec<(u64, Vec<u8>)> {
    let mut docs = Vec::new();
    e.scan_primary(b"E", snapshot, |h, bytes| { docs.push((h, bytes.to_vec())); Ok(()) }).unwrap();
    docs
}

#[test]
fn tile_evicts_verified_p_u_r_and_merges_snapshots_tombstones_and_publish_uniqueness() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"old"), doc(2, b"keep"), unique(1), reverse(21)]).unwrap();
    e.commit_block(b"two", &[doc(1, b"covered-update"), reverse(22)]).unwrap();
    assert_eq!(e.serving_memtable_entries(), 6);
    e.build_tile(&files.tile, 2).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "all P/U/R covered versions actually left serving memtable");
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"old".as_slice()));
    assert_eq!(e.get(b"E", 1, 2).unwrap(), Some(b"covered-update".as_slice()));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 2).unwrap(), Some(1));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10, 2).unwrap(), vec![21, 22]);
    assert_eq!(scan(&e, 2), vec![(1, b"covered-update".to_vec()), (2, b"keep".to_vec())]);
    assert!(matches!(e.commit_block(b"conflict", &[unique(3), doc(3, b"no")]).unwrap(), Outcome::Conflict(_)));
    assert_eq!(e.csn(), 2);
    e.commit_block(b"three", &[del_doc(1), del_reverse(21), del_unique(1), unique(3), reverse(23)]).unwrap();
    e.commit_block(b"four", &[doc(1, b"new"), del_reverse(22), del_unique(3)]).unwrap();
    assert_eq!(e.serving_memtable_entries(), 8);
    for snapshot in [1, 2, 3, 4] {
        let docs = scan(&e, snapshot);
        assert_eq!(docs.iter().find(|(h, _)| *h == 1).map(|(_, d)| d.as_slice()),
            match snapshot { 1 => Some(b"old".as_slice()), 2 => Some(b"covered-update".as_slice()), 3 => None, _ => Some(b"new".as_slice()) });
        assert_eq!(e.unique_lookup(b"E", b"u", b"v", snapshot).unwrap(),
            match snapshot { 1 | 2 => Some(1), 3 => Some(3), _ => None });
        assert_eq!(e.reverse_lookup(b"E", b"r", 10, snapshot).unwrap(),
            match snapshot { 1 => vec![21], 2 => vec![21, 22], 3 => vec![22, 23], _ => vec![23] });
    }
    assert_eq!(e.get(b"E", 1, 3).unwrap(), None);
    assert_eq!(e.get(b"E", 1, 4).unwrap(), Some(b"new".as_slice()));
    drop(e);
    let mut e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 8, "replay did not rematerialize verified <=2 versions");
    assert_eq!(e.csn(), 4);
    assert_eq!(e.next_handle().unwrap(), 24);
    assert_eq!(e.committed_block_csn(b"one"), Some(1));
    assert_eq!(e.commit_block(b"one", &[doc(99, b"ignored")]).unwrap(), Outcome::AlreadyCommitted { csn: 1 });
    assert!(matches!(e.commit_block(b"new-owner", &[unique(4)]).unwrap(), Outcome::Committed { csn: 5 }));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 2).unwrap(), Some(1));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 5).unwrap(), Some(4));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10, 4).unwrap(), vec![23]);
    assert_eq!(scan(&e, 4), vec![(1, b"new".to_vec()), (2, b"keep".to_vec())]);
}

#[test]
fn cutoff_before_latest_replays_only_uncovered_versions_and_checks_tile_owner() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"one"), unique(1), reverse(3)]).unwrap();
    e.commit_block(b"two", &[doc(1, b"two"), del_reverse(3)]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    assert_eq!(e.serving_memtable_entries(), 2);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"one".as_slice()));
    assert_eq!(e.get(b"E", 1, 2).unwrap(), Some(b"two".as_slice()));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10, 2).unwrap(), Vec::<u64>::new());
    drop(e);
    let mut e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 2);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"one".as_slice()));
    assert!(matches!(e.commit_block(b"reject", &[unique(2)]).unwrap(), Outcome::Conflict(_)));
    e.commit_block(b"transfer", &[del_unique(1), unique(2)]).unwrap();
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 1).unwrap(), Some(1));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 3).unwrap(), Some(2));
}

#[test]
fn covered_tombstones_hide_older_tile_versions_and_allow_new_owners() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"original"), unique(1), reverse(3)]).unwrap();
    e.commit_block(b"two", &[del_doc(1), del_unique(1), del_reverse(3)]).unwrap();
    e.build_tile(&files.tile, 2).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"original".as_slice()));
    assert_eq!(e.get(b"E", 1, 2).unwrap(), None);
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 2).unwrap(), None);
    assert!(e.reverse_lookup(b"E", b"r", 10, 2).unwrap().is_empty());
    e.commit_block(b"three", &[doc(1, b"revived"), unique(2), reverse(3)]).unwrap();
    drop(e);
    let e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 3);
    assert_eq!(scan(&e, 1), vec![(1, b"original".to_vec())]);
    assert!(scan(&e, 2).is_empty());
    assert_eq!(scan(&e, 3), vec![(1, b"revived".to_vec())]);
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 1).unwrap(), Some(1));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 2).unwrap(), None);
    assert_eq!(e.unique_lookup(b"E", b"u", b"v", 3).unwrap(), Some(2));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10, 1).unwrap(), vec![3]);
    assert!(e.reverse_lookup(b"E", b"r", 10, 2).unwrap().is_empty());
    assert_eq!(e.reverse_lookup(b"E", b"r", 10, 3).unwrap(), vec![3]);
}

#[test]
fn interrupted_build_and_corrupt_or_missing_tile_fail_closed_wal_rebuild_explicit() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"persisted"), unique(1)]).unwrap();
    fs::write(&files.tile, b"interrupted header").unwrap();
    assert_eq!(e.build_tile(&files.tile, 1).unwrap_err().kind(), ErrorKind::AlreadyExists);
    assert_eq!(e.serving_memtable_entries(), 2);
    drop(e);
    assert!(Engine::open_with_tile(&files.wal, &files.tile).is_err());
    let mut from_wal = Engine::open(&files.wal).unwrap();
    assert_eq!(from_wal.get(b"E", 1, 1).unwrap(), Some(b"persisted".as_slice()));
    fs::remove_file(&files.tile).unwrap();
    from_wal.build_tile(&files.tile, 1).unwrap();
    assert_eq!(from_wal.serving_memtable_entries(), 0);
    drop(from_wal);
    let bytes = fs::read(&files.tile).unwrap();
    for damaged in [bytes[..bytes.len() - 1].to_vec(), {
        let mut b = bytes.clone(); b[68 + 12 + 18] ^= 0x80; b
    }, {
        let mut b = bytes.clone(); b[64] ^= 0x80; b
    }] {
        fs::write(&files.tile, damaged).unwrap();
        assert!(Engine::open_with_tile(&files.wal, &files.tile).is_err());
        assert_eq!(Engine::open(&files.wal).unwrap().get(b"E", 1, 1).unwrap(), Some(b"persisted".as_slice()));
    }
    fs::remove_file(&files.tile).unwrap();
    assert!(Engine::open_with_tile(&files.wal, &files.tile).is_err());
    assert_eq!(Engine::open(&files.wal).unwrap().csn(), 1);
}

#[test]
fn tile_requires_complete_matching_wal_prefix_and_survives_torn_newer_tail() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"one")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    e.commit_block(b"two", &[doc(2, b"two")]).unwrap();
    drop(e);
    let good = fs::metadata(&files.wal).unwrap().len();
    OpenOptions::new().append(true).open(&files.wal).unwrap().write_all(b"torn").unwrap();
    let e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.csn(), 2);
    assert_eq!(fs::metadata(&files.wal).unwrap().len(), good);
    drop(e);
    // A WAL with another valid block at the same CSN cannot authenticate this tile.
    let other = files.dir.join("other.wal");
    let mut e = Engine::create(&other).unwrap();
    e.commit_block(b"other", &[doc(1, b"other")]).unwrap();
    drop(e);
    assert!(Engine::open_with_tile(&other, &files.tile).is_err());
    // Removing the covered WAL prefix cannot silently use the tile as recovery authority.
    let f = OpenOptions::new().write(true).open(&files.wal).unwrap();
    f.set_len(0).unwrap();
    drop(f);
    assert!(Engine::open_with_tile(&files.wal, &files.tile).is_err());
}

#[test]
fn large_document_stays_readable_and_unsupported_key_never_evicts_any_entry() {
    let files = Files::new();
    let large = vec![b'x'; 2 * 1024 * 1024];
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"large", &[doc(1, &large)]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(large.as_slice()));
    drop(e);
    let e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(large.as_slice()));
    drop(e);

    let other = files.dir.join("oversized.wal");
    let candidate = files.dir.join("oversized.tile");
    let mut e = Engine::create(&other).unwrap();
    let mut key = vec![b'v'; 17 * 1024 * 1024];
    key[0] = b'x';
    e.commit_block(b"key", &[doc(2, b"safe"), Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: key.clone(), handle: 2 }]).unwrap();
    assert_eq!(e.serving_memtable_entries(), 2);
    assert_eq!(e.build_tile(&candidate, 1).unwrap_err().kind(), ErrorKind::InvalidData);
    assert_eq!(e.serving_memtable_entries(), 2);
    assert!(Engine::open_with_tile(&other, &candidate).is_err(), "partial unsupported candidate cannot serve reads");
    assert_eq!(e.get(b"E", 2, 1).unwrap(), Some(b"safe".as_slice()));
    assert_eq!(e.unique_lookup(b"E", b"u", &key, 1).unwrap(), Some(2));
    drop(e);
    let e = Engine::open(&other).unwrap();
    assert_eq!(e.get(b"E", 2, 1).unwrap(), Some(b"safe".as_slice()));
}
