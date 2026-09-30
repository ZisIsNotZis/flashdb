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
fn scan(e: &Engine) -> Vec<(u64, Vec<u8>)> {
    let mut docs = Vec::new();
    e.scan_primary(b"E", |h, bytes| { docs.push((h, bytes.to_vec())); Ok(()) }).unwrap();
    docs
}

#[test]
fn tile_evicts_verified_p_u_r_and_merges_latest_state_tombstones_and_publish_uniqueness() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"old"), doc(2, b"keep"), unique(1), reverse(21)]).unwrap();
    e.commit_block(b"two", &[doc(1, b"covered-update"), reverse(22)]).unwrap();
    assert_eq!(e.serving_memtable_entries(), 6);
    e.build_tile(&files.tile, 2).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "all P/U/R covered versions actually left serving memtable");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"covered-update".to_vec()));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(1));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10).unwrap(), vec![21, 22]);
    assert_eq!(scan(&e), vec![(1, b"covered-update".to_vec()), (2, b"keep".to_vec())]);
    assert!(matches!(e.commit_block(b"conflict", &[unique(3), doc(3, b"no")]).unwrap(), Outcome::Conflict(_)));
    assert_eq!(e.csn(), 2);
    e.commit_block(b"three", &[del_doc(1), del_reverse(21), del_unique(1), unique(3), reverse(23)]).unwrap();
    e.commit_block(b"four", &[doc(1, b"new"), del_reverse(22), del_unique(3)]).unwrap();
    assert_eq!(e.serving_memtable_entries(), 8);
    // Reads see only latest committed state (2026-09-29), merged across the
    // tile and the memtable; tombstones (empty value) are the newest state.
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"new".to_vec()));
    assert_eq!(scan(&e), vec![(1, b"new".to_vec()), (2, b"keep".to_vec())]);
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), None);
    assert_eq!(e.reverse_lookup(b"E", b"r", 10).unwrap(), vec![23]);
    drop(e);
    let mut e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 8, "replay did not rematerialize verified <=2 versions");
    assert_eq!(e.csn(), 4);
    assert_eq!(e.next_handle().unwrap(), 24);
    assert_eq!(e.committed_block_csn(b"one"), Some(1));
    assert_eq!(e.commit_block(b"one", &[doc(99, b"ignored")]).unwrap(), Outcome::AlreadyCommitted { csn: 1 });
    assert!(matches!(e.commit_block(b"new-owner", &[unique(4)]).unwrap(), Outcome::Committed { csn: 5 }));
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(4));
    assert_eq!(scan(&e), vec![(1, b"new".to_vec()), (2, b"keep".to_vec())]);
}

#[test]
fn cutoff_before_latest_replays_only_uncovered_versions_and_checks_tile_owner() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"one"), unique(1), reverse(3)]).unwrap();
    e.commit_block(b"two", &[doc(1, b"two"), del_reverse(3)]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    assert_eq!(e.serving_memtable_entries(), 2);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"two".to_vec()), "latest committed state wins over the tile's covered version");
    assert_eq!(e.reverse_lookup(b"E", b"r", 10).unwrap(), Vec::<u64>::new());
    drop(e);
    let mut e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 2);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"two".to_vec()));
    assert!(matches!(e.commit_block(b"reject", &[unique(2)]).unwrap(), Outcome::Conflict(_)));
    e.commit_block(b"transfer", &[del_unique(1), unique(2)]).unwrap();
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2));
}

#[test]
fn covered_tombstones_are_the_newest_state_and_allow_new_owners() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"original"), unique(1), reverse(3)]).unwrap();
    e.commit_block(b"two", &[del_doc(1), del_unique(1), del_reverse(3)]).unwrap();
    e.build_tile(&files.tile, 2).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1).unwrap(), None, "tombstone is the newest state");
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), None);
    assert!(e.reverse_lookup(b"E", b"r", 10).unwrap().is_empty());
    e.commit_block(b"three", &[doc(1, b"revived"), unique(2), reverse(3)]).unwrap();
    drop(e);
    let e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 3);
    assert_eq!(scan(&e), vec![(1, b"revived".to_vec())]);
    assert_eq!(e.unique_lookup(b"E", b"u", b"v").unwrap(), Some(2));
    assert_eq!(e.reverse_lookup(b"E", b"r", 10).unwrap(), vec![3]);
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
    assert_eq!(from_wal.get(b"E", 1).unwrap(), Some(b"persisted".to_vec()));
    fs::remove_file(&files.tile).unwrap();
    from_wal.build_tile(&files.tile, 1).unwrap();
    assert_eq!(from_wal.serving_memtable_entries(), 0);
    drop(from_wal);
    let bytes = fs::read(&files.tile).unwrap();
    let block_len = u32::from_le_bytes(bytes[TILE_PAGE..TILE_PAGE + 4].try_into().unwrap()) as usize;
    for damaged in [bytes[..bytes.len() - 1].to_vec(), {
        let mut b = bytes.clone(); b[TILE_PAGE + block_len - 1] ^= 0x80; b
    }, {
        let mut b = bytes.clone(); b[64] ^= 0x80; b
    }] {
        fs::write(&files.tile, damaged).unwrap();
        assert!(Engine::open_with_tile(&files.wal, &files.tile).is_err());
        assert_eq!(Engine::open(&files.wal).unwrap().get(b"E", 1).unwrap(), Some(b"persisted".to_vec()));
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
    assert_eq!(e.get(b"E", 1).unwrap(), Some(large.to_vec()));
    drop(e);
    let e = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1).unwrap(), Some(large.to_vec()));
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
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"safe".to_vec()));
    assert_eq!(e.unique_lookup(b"E", b"u", &key).unwrap(), Some(2));
    drop(e);
    let e = Engine::open(&other).unwrap();
    assert_eq!(e.get(b"E", 2).unwrap(), Some(b"safe".to_vec()));
}

// Independent CRC recomputation: the file's own checksums remain valid, but the
// unchanged WAL must still reject a different P/U/R projection on open.
fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 { crc = (crc >> 1) ^ (if crc & 1 != 0 { 0x82f6_3b78 } else { 0 }); }
    }
    !crc
}

// FDBTILE2 layout: the first data block starts at the superblock page boundary,
// with [block_len u32][block_crc u32][first_key_len u32][entry_count u16] then
// the first key and the first entry [key_len u32][value_len u32][key][value].
const TILE_PAGE: usize = 4096;

/// Byte range of the first data block's first entry key.
fn first_entry_key_range(bytes: &[u8]) -> (usize, usize) {
    let first_key_len = u32::from_le_bytes(bytes[TILE_PAGE + 8..TILE_PAGE + 12].try_into().unwrap()) as usize;
    let entry = TILE_PAGE + 14 + first_key_len;
    let key_len = u32::from_le_bytes(bytes[entry..entry + 4].try_into().unwrap()) as usize;
    (entry + 8, key_len)
}

/// Byte range of the first data block's first entry value.
fn first_entry_value_range(bytes: &[u8]) -> (usize, usize) {
    let (key_at, key_len) = first_entry_key_range(bytes);
    let value_len = u32::from_le_bytes(bytes[key_at - 4..key_at].try_into().unwrap()) as usize;
    let value_at = key_at + key_len;
    (value_at, value_len)
}

/// Recompute the first data block's CRC after an in-place edit, so an edited
/// entry is CRC-valid and only the WAL comparison can reject it.
fn repair_first_block_crc(bytes: &mut [u8]) {
    let block_len = u32::from_le_bytes(bytes[TILE_PAGE..TILE_PAGE + 4].try_into().unwrap()) as usize;
    let crc = crc32c(&bytes[TILE_PAGE + 8..TILE_PAGE + block_len]);
    bytes[TILE_PAGE + 4..TILE_PAGE + 8].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn tile_value_with_recomputed_crc_disagrees_with_retained_wal() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"original")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    drop(e);
    let mut tile = fs::read(&files.tile).unwrap();
    let (value_at, _) = first_entry_value_range(&tile);
    tile[value_at] ^= 1;
    repair_first_block_crc(&mut tile);
    fs::write(&files.tile, tile).unwrap();
    let err = Engine::open_with_tile(&files.wal, &files.tile).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::InvalidData);
    assert!(err.to_string().contains("disagrees with WAL"));
    assert_eq!(Engine::open(&files.wal).unwrap().get(b"E", 1).unwrap(), Some(b"original".to_vec()));
}

#[test]
fn tile_malformed_key_with_valid_crc_returns_error_not_panic() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"original")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    drop(e);
    let mut tile = fs::read(&files.tile).unwrap();
    let (key_at, key_len) = first_entry_key_range(&tile);
    tile[key_at..key_at + key_len].fill(b'x');
    tile[key_at] = b'U';
    tile[key_at + key_len - 1] = 0;
    repair_first_block_crc(&mut tile);
    fs::write(&files.tile, tile).unwrap();
    assert_eq!(Engine::open_with_tile(&files.wal, &files.tile).err().unwrap().kind(), ErrorKind::InvalidData);
}

#[test]
fn tile_mutation_after_open_returns_error_in_child_process() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, &vec![b'x'; 2 * 1024 * 1024])]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    drop(e);
    let pristine = fs::read(&files.tile).unwrap();
    for mode in ["truncate", "modify"] {
        fs::write(&files.tile, &pristine).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tile_mutation_child", "--nocapture"])
            .env("FLASHDB_TILE_CHILD_WAL", &files.wal)
            .env("FLASHDB_TILE_CHILD_TILE", &files.tile)
            .env("FLASHDB_TILE_CHILD_MODE", mode)
            .output().unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(child.status.success() && output.contains("test tile_mutation_child ... ok"),
            "{mode} child failed or did not run (including signal/crash): {output} {}", String::from_utf8_lossy(&child.stderr));
    }
}

#[test]
fn point_read_checks_traversed_records_not_unrelated_later_entries() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"first"), doc(2, b"second")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    let mut bytes = fs::read(&files.tile).unwrap();
    let block_len = u32::from_le_bytes(bytes[TILE_PAGE..TILE_PAGE + 4].try_into().unwrap()) as usize;
    bytes[TILE_PAGE + block_len - 1] ^= 1; // corrupt the block's last entry (the second record)
    fs::write(&files.tile, bytes).unwrap();
    // Multi-tile merge pre-advances every tile to its first matching key, so a
    // corrupted record anywhere in a published tile fails every read closed:
    // the weaker single-tile qualification (only traversed records checked)
    // was intentionally dropped during the multi-tile integration.
    assert_eq!(e.get(b"E", 1).unwrap_err().kind(), ErrorKind::InvalidData,
        "any tile corruption fails closed without process crash");
    assert_eq!(e.get(b"E", 2).unwrap_err().kind(), ErrorKind::InvalidData);
}

#[test]
fn tile_mutation_child() {
    let Ok(wal) = std::env::var("FLASHDB_TILE_CHILD_WAL") else { return };
    let tile = PathBuf::from(std::env::var("FLASHDB_TILE_CHILD_TILE").unwrap());
    let e = Engine::open_with_tile(wal, &tile).unwrap();
    match std::env::var("FLASHDB_TILE_CHILD_MODE").unwrap().as_str() {
        "truncate" => OpenOptions::new().write(true).open(&tile).unwrap().set_len(0).unwrap(),
        "modify" => {
            let f = OpenOptions::new().write(true).open(&tile).unwrap();
            use std::os::unix::fs::FileExt;
            let at = f.metadata().unwrap().len() - 1;
            f.write_at(b"z", at).unwrap();
        }
        other => panic!("unexpected mode {other}"),
    }
    let err = e.get(b"E", 1).unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::InvalidData | ErrorKind::UnexpectedEof), "{err}");
}
