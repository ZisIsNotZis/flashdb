//! FDBTILE2 page-structured tile format through the public Engine API: a
//! multi-block tile round-trips with its directory index, and directory
//! corruption is rejected at open (before any data block is read).

use flashdb_engine::engine::{Engine, Op};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf, tile: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-tile-format-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), tile: dir.join("data.tile"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { fs::remove_dir_all(&self.dir).unwrap(); } }
fn doc(h: u64, v: &[u8]) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() } }

fn superblock_field(bytes: &[u8], field: &str) -> u64 {
    match field {
        "count" => u64::from_le_bytes(bytes[48..56].try_into().unwrap()),
        "dir_offset" => u64::from_le_bytes(bytes[56..64].try_into().unwrap()),
        "dir_len" => u64::from_le_bytes(bytes[64..72].try_into().unwrap()),
        other => panic!("unknown superblock field {other}"),
    }
}

#[test]
fn multi_block_tile_roundtrips_and_directory_indexes_every_block() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    let mut handle = 0u64;
    for block in 0..20 {
        let ops: Vec<Op> = (0..60).map(|_| { handle += 1; doc(handle, b"v") }).collect();
        e.commit_block(&format!("b{block}").into_bytes(), &ops).unwrap();
    }
    assert_eq!(e.csn(), 20);
    e.build_tile(&files.tile, 20).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "every covered version left the memtable");
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v".to_vec()));
    assert_eq!(e.get(b"E", 1200).unwrap(), Some(b"v".to_vec()));
    drop(e);

    let bytes = fs::read(&files.tile).unwrap();
    assert_eq!(&bytes[..8], b"FDBTILE2");
    assert_eq!(superblock_field(&bytes, "count"), 1200);
    let dir_offset = superblock_field(&bytes, "dir_offset") as usize;
    let dir_len = superblock_field(&bytes, "dir_len") as usize;
    assert!(dir_offset >= 4096 && dir_len > 0, "directory is present after the superblock page");
    let fence_count = u16::from_le_bytes(bytes[dir_offset + 8..dir_offset + 10].try_into().unwrap());
    assert!(fence_count >= 8, "directory lists one fence per data block, got {fence_count}");

    let reopened = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(reopened.serving_memtable_entries(), 0);
    assert_eq!(reopened.get(b"E", 600).unwrap(), Some(b"v".to_vec()));
}

#[test]
fn directory_corruption_is_rejected_at_engine_open_and_wal_stays_authoritative() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    drop(e);
    let mut bytes = fs::read(&files.tile).unwrap();
    let dir_offset = superblock_field(&bytes, "dir_offset") as usize;
    bytes[dir_offset + 12] ^= 1; // inside the first fence record
    fs::write(&files.tile, bytes).unwrap();
    assert_eq!(Engine::open_with_tile(&files.wal, &files.tile).err().unwrap().kind(), ErrorKind::InvalidData);
    assert_eq!(Engine::open(&files.wal).unwrap().get(b"E", 1).unwrap(), Some(b"a".to_vec()));
}
