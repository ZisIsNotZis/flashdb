//! Per-tile page size and bloom bits/key through the public Engine API: a
//! directory of mixed-layout tiles reads correctly after reopen (primary,
//! unique and scan paths), an out-of-range stored page size is rejected at open
//! with `InvalidData`, and a stored page-size field of 0 (absent/unknown) is read
//! as the default rather than failing closed. The per-tile parameters are
//! self-describing in each tile's superblock; nothing durable outside the tile
//! records them (the durable layout policy is a later slice).

use flashdb_engine::engine::{Engine, LayoutPolicy, Op};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Files {
    dir: PathBuf,
    wal: PathBuf,
}
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "flashdb-tile-layout-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files {
    fn drop(&mut self) { fs::remove_dir_all(&self.dir).unwrap(); }
}

fn doc(h: u64) -> Op {
    Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: format!("d{h}").into_bytes() }
}
fn uniq(value: &[u8], h: u64) -> Op {
    Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: value.to_vec(), handle: h }
}

fn superblock_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn superblock_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}
/// Recompute the superblock CRC after a test edits the 108-byte header in place,
/// so only the rule under test can reject the file (CRC covers bytes 0..104).
fn repair_superblock_crc(bytes: &mut [u8]) {
    let crc = flashdb_engine::crc::crc32c(&bytes[..104]);
    bytes[104..108].copy_from_slice(&crc.to_le_bytes());
}

/// Three tiles written under three different policies (default 4096/bloom 10,
/// 65536/no bloom, 8192/bloom 14) in one directory. After reopen every read path
/// must resolve across the mix, and each tile keeps the parameters it was written
/// with.
#[test]
fn mixed_layout_directory_reads_after_reopen() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();

    let publish = |e: &mut Engine, name: &str, cutoff: u64, lo: u64, hi: u64| {
        let mut ops = Vec::new();
        for h in lo..=hi {
            ops.push(doc(h));
            ops.push(uniq(format!("a{h}").as_bytes(), h));
        }
        e.commit_block(format!("b{cutoff}").as_bytes(), &ops).unwrap();
        e.publish_tile(files.dir.join(name), cutoff).unwrap();
    };

    // Tile 1 keeps the declared default (4096 pages, bloom 10 bits/key).
    publish(&mut e, "tile-1.tile", 1, 1, 200);
    assert_eq!(superblock_u32(&fs::read(files.dir.join("tile-1.tile")).unwrap(), 72), 4096);

    // Tile 2: 64 KiB pages, no bloom.
    e.set_layout_policy(LayoutPolicy::with_default(65536, 0));
    publish(&mut e, "tile-2.tile", 2, 201, 400);
    let t2 = fs::read(files.dir.join("tile-2.tile")).unwrap();
    assert_eq!(superblock_u32(&t2, 72), 65536, "the 64 KiB page lands in the superblock");
    assert_eq!(superblock_u64(&t2, 84), 0, "0 bloom bits writes no bloom section");
    assert_eq!(superblock_u32(&t2, 96), 0, "0 bloom bits stores a zero bits/key");

    // Tile 3: 8192 pages, bloom 14.
    e.set_layout_policy(LayoutPolicy::with_default(8192, 14));
    publish(&mut e, "tile-3.tile", 3, 401, 600);
    let t3 = fs::read(files.dir.join("tile-3.tile")).unwrap();
    assert_eq!(superblock_u32(&t3, 72), 8192);
    assert_eq!(superblock_u32(&t3, 96), 14, "the bloom bits/key is the policy value");
    drop(e);

    // Reopen from the manifest and exercise primary, unique and both scan paths.
    let e = Engine::open_discover(&files.dir).unwrap();
    for h in [1u64, 200, 201, 400, 401, 600] {
        assert_eq!(e.get(b"E", h).unwrap(), Some(format!("d{h}").into_bytes()), "primary get handle {h}");
        assert_eq!(e.unique_lookup(b"E", b"u", format!("a{h}").as_bytes()).unwrap(), Some(h), "unique probe handle {h}");
    }
    // An absent probe on the no-bloom tile must not be rejected.
    assert_eq!(e.unique_lookup(b"E", b"u", b"absent").unwrap(), None);

    let mut scanned = Vec::new();
    e.scan_primary(b"E", |handle, _| { scanned.push(handle); Ok(()) }).unwrap();
    assert_eq!(scanned, (1..=600).collect::<Vec<u64>>(), "scan sees every live doc once, in handle order");

    let mut all = 0u64;
    e.scan_all(|_, _| { all += 1; Ok(()) }).unwrap();
    assert_eq!(all, 1200, "scan_all sees the P and U keys of all three tiles");
}

/// A page size outside 4096..=65536 stored in the superblock fails closed at open.
#[test]
fn out_of_range_stored_page_size_rejected_at_open() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1)]).unwrap();
    e.build_tile(files.dir.join("data.tile"), 1).unwrap();
    drop(e);

    for bad in [4095u32, 65537] {
        let mut bytes = fs::read(files.dir.join("data.tile")).unwrap();
        bytes[72..76].copy_from_slice(&bad.to_le_bytes());
        repair_superblock_crc(&mut bytes);
        fs::write(files.dir.join("data.tile"), &bytes).unwrap();
        let err = Engine::open_with_tile(&files.wal, &files.dir.join("data.tile"))
            .err().expect("out-of-range page size must fail closed");
        assert_eq!(err.kind(), ErrorKind::InvalidData, "page size {bad} must fail closed");
    }
}

/// A stored page-size field of 0 is \"absent/unknown\": it is read as the default
/// (4096), and the tile still serves its data.
#[test]
fn absent_page_size_field_is_read_as_default() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"b1", &[doc(1)]).unwrap();
    e.build_tile(files.dir.join("data.tile"), 1).unwrap();
    drop(e);

    let mut bytes = fs::read(files.dir.join("data.tile")).unwrap();
    bytes[72..76].copy_from_slice(&0u32.to_le_bytes());
    repair_superblock_crc(&mut bytes);
    fs::write(files.dir.join("data.tile"), &bytes).unwrap();

    let e = Engine::open_with_tile(&files.wal, &files.dir.join("data.tile")).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap(), Some(b"d1".to_vec()));
}
