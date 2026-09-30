//! FDBTILE3 page-structured tile format through the public Engine API: a
//! multi-block tile round-trips with its directory index and its bloom section,
//! and directory corruption is rejected at open (before any data block is read).
//! The bloom filter itself is unit-tested in `src/bloom.rs`; since slice 08d-2b
//! the engine builds one over each tile's unique prefixes, so every tile written
//! here has a nonempty bloom section.

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
        "bloom_offset" => u64::from_le_bytes(bytes[76..84].try_into().unwrap()),
        "bloom_len" => u64::from_le_bytes(bytes[84..92].try_into().unwrap()),
        "bloom_crc32c" => u32::from_le_bytes(bytes[92..96].try_into().unwrap()) as u64,
        "bits_per_key" => u32::from_le_bytes(bytes[96..100].try_into().unwrap()) as u64,
        "hash_count" => bytes[100] as u64,
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
    assert_eq!(&bytes[..8], b"FDBTILE3");
    assert_eq!(superblock_field(&bytes, "count"), 1200);
    let dir_offset = superblock_field(&bytes, "dir_offset") as usize;
    let dir_len = superblock_field(&bytes, "dir_len") as usize;
    assert!(dir_offset >= 4096 && dir_len > 0, "directory is present after the superblock page");
    let fence_count = u16::from_le_bytes(bytes[dir_offset + 8..dir_offset + 10].try_into().unwrap());
    assert!(fence_count >= 8, "directory lists one fence per data block, got {fence_count}");
    // The engine wires a per-tile bloom in 08d-2b: the section sits between the
    // data blocks and the directory, and both its own header and the superblock
    // record 10 bits/key (7 probes).
    let bloom_offset = superblock_field(&bytes, "bloom_offset") as usize;
    let bloom_len = superblock_field(&bytes, "bloom_len") as usize;
    assert!(bloom_len > 0, "the engine writes a bloom");
    assert!(bloom_offset >= 4096, "the bloom starts after the superblock page");
    assert!(bloom_offset + bloom_len <= dir_offset, "the bloom ends at or before the directory");
    assert_eq!(superblock_field(&bytes, "bits_per_key"), 10);
    assert_eq!(superblock_field(&bytes, "hash_count"), 7);
    let bit_count = u64::from_le_bytes(bytes[bloom_offset..bloom_offset + 8].try_into().unwrap());
    let header_bpk = u32::from_le_bytes(bytes[bloom_offset + 16..bloom_offset + 20].try_into().unwrap());
    assert_eq!(header_bpk, 10, "the bloom header agrees with the superblock copy");
    assert_eq!(bytes[bloom_offset + 20], 7);
    assert_eq!(bloom_len, 24 + bit_count.div_ceil(8) as usize, "the section length matches its bit count");

    let reopened = Engine::open_with_tile(&files.wal, &files.tile).unwrap();
    assert_eq!(reopened.serving_memtable_entries(), 0);
    assert_eq!(reopened.get(b"E", 600).unwrap(), Some(b"v".to_vec()));
}

/// The enlarged superblock CRC covers the new FDBTILE3 bloom fields: corrupting
/// one of them (without touching any other structure) is rejected at open.
#[test]
fn new_superblock_fields_are_covered_by_the_superblock_crc() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1, b"a")]).unwrap();
    e.build_tile(&files.tile, 1).unwrap();
    drop(e);
    let mut bytes = fs::read(&files.tile).unwrap();
    bytes[96] ^= 1; // bits_per_key, one of the added fields
    fs::write(&files.tile, &bytes).unwrap();
    assert_eq!(Engine::open_with_tile(&files.wal, &files.tile).err().unwrap().kind(), ErrorKind::InvalidData);
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
