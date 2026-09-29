use flashdb_engine::engine::{Engine, Op, Outcome};
use flashdb_engine::wal::crc32c;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Files { dir: PathBuf, wal: PathBuf, tile: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("flashdb-manifest-e2e-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), tile: dir.join("data.tile"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { fs::remove_dir_all(&self.dir).unwrap(); } }
fn doc(h: u64, v: &[u8]) -> Op { Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() } }

#[test]
fn publish_survives_reopen_via_manifest_and_ignores_unreferenced_tiles() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"covered"), doc(2, b"later")]).unwrap();
    e.commit_block(b"two", &[doc(1, b"newer")]).unwrap();
    e.publish_tile(&files.tile, 1).unwrap();
    assert_eq!(e.serving_memtable_entries(), 1, "only CSN>cutoff versions stay in memory");
    assert_eq!(e.get(b"E", 1, 2).unwrap(), Some(b"newer".to_vec()));
    drop(e);

    // Reopen discovers manifest+tile without any explicit tile argument.
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 2);
    assert_eq!(e.next_handle().unwrap(), 3);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"covered".to_vec()));
    assert_eq!(e.serving_memtable_entries(), 1, "verified covered prefix not rematerialized");
    // A second tile is refused in this slice.
    let second = files.dir.join("second.tile");
    assert_eq!(e.publish_tile(&second, 2).unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
    // New writes still commit over the published state.
    assert!(matches!(e.commit_block(b"three", &[doc(5, b"five")]).unwrap(), Outcome::Committed { csn: 3 }));
    drop(e);

    // Crash after tile sync but before manifest publish: the leftover file is
    // unreferenced and ignored on discovery; WAL alone still recovers.
    let mut e = Engine::open_discover(&files.dir).unwrap();
    let leftover = files.dir.join("leftover.tile");
    fs::write(&leftover, b"half-written candidate").unwrap();
    drop(e);
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.get(b"E", 5, 3).unwrap(), Some(b"five".to_vec()));
    assert!(e.publish_tile(&leftover, 3).is_err(), "leftover candidate cannot be silently reused");
    assert_eq!(e.get(b"E", 5, 3).unwrap(), Some(b"five".to_vec()), "failed publish changed nothing");
}

#[test]
fn torn_or_corrupt_manifest_fails_closed_and_wal_only_open_still_works() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"covered")]).unwrap();
    e.publish_tile(&files.tile, 1).unwrap();
    drop(e);
    let manifest = files.dir.join("manifest");
    let pristine = fs::read(&manifest).unwrap();

    // Truncate slot B's tail: the seq-1 page is torn, so A/B selection falls back
    // to the older but fully valid seq-0 empty root. Data stays safe because the
    // full WAL is retained and replayed; the tile becomes unreferenced.
    fs::write(&manifest, &pristine[..pristine.len() - 100]).unwrap();
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 1);
    assert_eq!(e.serving_memtable_entries(), 1, "no tile root: full WAL replay serves everything");
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"covered".to_vec()));
    drop(e);
    fs::write(&manifest, &pristine).unwrap();

    // CRC corruption of BOTH pages is structural ambiguity: fail closed rather
    // than guessing, and WAL-only recovery remains available and authoritative.
    let mut bytes = pristine.clone();
    bytes[20] ^= 0x80; // slot A (seq 0) payload byte
    bytes[4096 + 20] ^= 0x80; // slot B (seq 1) payload byte
    fs::write(&manifest, &bytes).unwrap();
    let err = Engine::open_discover(&files.dir).map(|_| ()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let e = Engine::open(&files.wal).unwrap();
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"covered".to_vec()));
}

#[test]
fn adoption_refuses_manifest_with_active_tile_and_accepts_empty_root() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"covered")]).unwrap();
    e.publish_tile(&files.tile, 1).unwrap();
    drop(e);
    // An engine opened WITHOUT discovery must not clobber the existing root.
    let mut plain = Engine::open(&files.wal).unwrap();
    assert_eq!(plain.publish_tile(&files.dir.join("again.tile"), 1).map(|_| ()).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData);
    drop(plain);
    assert!(Engine::open_discover(&files.dir).is_ok(), "refused publish left the root intact");

    // Empty seq-0 root (e.g. torn first publish) can be adopted.
    let other = Files::new();
    let mut e = Engine::create(&other.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"x")]).unwrap();
    // Fabricate an empty-root manifest exactly as Manifest::create would.
    let mut page = vec![0u8; 4096];
    page[..8].copy_from_slice(&0u64.to_le_bytes());
    let crc = crc32c(&page[12..]);
    page[8..12].copy_from_slice(&crc.to_le_bytes());
    fs::write(other.dir.join("manifest"), &page).unwrap();
    e.publish_tile(&other.tile, 1).unwrap();
    drop(e);
    let e = Engine::open_discover(&other.dir).unwrap();
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"x".to_vec()));
}

#[test]
fn manifest_tile_reference_disagreeing_with_tile_header_fails_closed() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"covered")]).unwrap();
    e.publish_tile(&files.tile, 1).unwrap();
    drop(e);
    // Swap the recorded cutoff without touching the tile or WAL digest path.
    let mut bytes = fs::read(&files.dir.join("manifest")).unwrap();
    let seq1_slot = 4096; // single publish -> active root in slot B
    bytes[seq1_slot + 12 + 4 + 4 + 9] ^= 0x01; // first cutoff byte inside payload
    // Recompute the page CRC so only the root/header disagreement is exercised.
    let payload = &bytes[seq1_slot + 12..seq1_slot + 4096];
    let crc = crc32c(payload);
    bytes[seq1_slot + 8..seq1_slot + 12].copy_from_slice(&crc.to_le_bytes());
    fs::write(files.dir.join("manifest"), &bytes).unwrap();
    let err = Engine::open_discover(&files.dir).map(|_| ()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn torn_manifest_create_is_reported_with_remediation_hint() {
    let files = Files::new();
    fs::write(files.dir.join("manifest"), b"short").unwrap();
    let err = Engine::open_discover(&files.dir).map(|_| ()).unwrap_err();
    assert!(err.to_string().contains("move it aside"));
}

#[test]
fn manifest_durable_but_engine_crashed_before_flip_recovers_new_root() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.commit_block(b"one", &[doc(1, b"covered")]).unwrap();
    e.publish_tile(&files.tile, 1).unwrap();
    drop(e);
    // Simulate "manifest durable, ack lost": reopen must select the new root and
    // serve from the tile; the block's dedup id still comes from the retained WAL.
    let mut e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.committed_block_csn(b"one"), Some(1));
    assert_eq!(e.commit_block(b"one", &[doc(9, b"ignored")]).unwrap(), Outcome::AlreadyCommitted { csn: 1 });
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"E", 1, 1).unwrap(), Some(b"covered".to_vec()));
}
