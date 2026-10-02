//! Slice 28: the two region-compaction defects slice 27 surfaced.
//!
//! `write_compacted_tile` merged a region run with the GLOBAL WAL suffix in
//! `(lower, cutoff]`, so when the run started at a region's oldest tile
//! (`lower = 0`) other regions' keys landed in the output; and `merge_region_run`
//! never unlinked its retired inputs. These tests prove a region merge now holds
//! exactly its region (every read surface unchanged before/after), that a
//! floor-spanning region merge still reopens, and that the retired inputs are
//! unlinked after the durable flip.

use flashdb_engine::engine::{Engine, Op, RegionMode};
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
        let dir = test_root().join(format!("flashdb-regioncompact-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

fn doc(entity: &[u8], h: u64, v: &[u8]) -> Op { Op::PutDoc { entity: entity.to_vec(), handle: h, doc: v.to_vec() } }
fn unique(entity: &[u8], field: &[u8], value: &[u8], h: u64) -> Op {
    Op::PutUnique { entity: entity.to_vec(), field: field.to_vec(), value: value.to_vec(), handle: h }
}
fn reverse(entity: &[u8], field: &[u8], target: u64, source: u64) -> Op {
    Op::PutReverse { entity: entity.to_vec(), field: field.to_vec(), target, source }
}

/// Every read surface, so a compaction that changed any of them is visible.
fn readings(e: &Engine) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("get Order/1 {:?}", e.get(b"Order", 1).unwrap()));
    for sku in [&b"A"[..], &b"B"[..], &b"Z"[..]] {
        out.push(format!("unique Stock/sku/{:?} {:?}", String::from_utf8_lossy(sku), e.unique_lookup(b"Stock", b"sku", sku).unwrap()));
    }
    out.push(format!("reverse Ticket/order/10 {:?}", e.reverse_lookup(b"Ticket", b"order", 10).unwrap()));
    for entity in [&b"Order"[..], &b"Stock"[..]] {
        let mut rows = Vec::new();
        e.scan_primary(entity, |h, v| { rows.push((h, v.to_vec())); Ok(()) }).unwrap();
        out.push(format!("scan {:?} {:?}", String::from_utf8_lossy(entity), rows));
    }
    out
}

fn tile_file_count(dir: &std::path::Path) -> usize {
    fs::read_dir(dir).unwrap()
        .filter_map(|d| d.ok())
        .filter(|d| d.file_name().to_string_lossy().ends_with(".tile"))
        .count()
}

/// A region's run must be restricted to that region even when it starts at the
/// region's oldest tile (`lower = 0`) and the WAL window also carries another
/// region's ops. `P|Order` is written at cutoffs 1, 2 and 3 while `U|Stock` and
/// `R|Ticket` land at 1 and 2, so the P|Order merge's global `(0, 3]` window
/// contains both foreign regions.
#[test]
fn region_compaction_leaves_every_read_surface_identical() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    let blocks: &[&[Op]] = &[
        &[doc(b"Order", 1, b"o1"), unique(b"Stock", b"sku", b"A", 1), reverse(b"Ticket", b"order", 10, 1)],
        &[doc(b"Order", 1, b"o2"), unique(b"Stock", b"sku", b"B", 2), reverse(b"Ticket", b"order", 10, 2)],
        &[doc(b"Order", 1, b"o3")],
    ];
    for (i, ops) in blocks.iter().enumerate() {
        let csn = (i + 1) as u64;
        e.commit_block(format!("b{csn}").as_bytes(), ops).unwrap();
        e.publish_tile(files.dir.join(format!("t{csn}.tile")), csn).unwrap();
    }
    let before = readings(&e);
    let cutoffs_before = e.active_tile_cutoffs();
    assert!(cutoffs_before.len() >= 6, "P|Order x3 plus U|Stock/R|Ticket");

    let mut merged = 0;
    while e.maybe_compact(u64::MAX, 2).unwrap().is_some() { merged += 1; }
    assert!(merged > 0, "the scenario must exercise a region merge");
    assert!(e.active_tile_cutoffs().len() < cutoffs_before.len(), "a region's tiers collapsed");

    assert_eq!(readings(&e), before, "every read is unchanged by a region merge");
    drop(e);
    let reopened = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(readings(&reopened), before, "the merged directory reopens with the same reads");
}

/// The rotate-then-compact-then-reopen regression, per region: a region merge
/// must span the segment floor (its oldest tile is below the floor, so `lower =
/// 0` and the below-floor entry is carried by the run tile, not the retired WAL)
/// and reopen.
#[test]
fn floor_spanning_region_compaction_reopens() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"b1", &[doc(b"Order", 1, b"pre")]).unwrap();
    e.publish_tile(&files.dir.join("tile-1.tile"), 1).unwrap();
    // Rotate at cutoff 1: floor = 1, the pre-rotation WAL is retired.
    assert!(e.rotate_wal(0).unwrap(), "rotation succeeds with one region tile at cutoff 1");
    e.commit_block(b"b2", &[doc(b"Order", 1, b"post")]).unwrap();
    e.publish_tile(&files.dir.join("tile-2.tile"), 2).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![1, 2]);

    // region ``compact`` collapses P|Order across the floor into one tile at 2.
    e.compact().unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![2], "the below-floor and suffix tiles merged");
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"post".to_vec()));
    // DEFECT 2: both retired inputs are unlinked after the durable flip.
    assert!(!files.dir.join("tile-1.tile").exists() && !files.dir.join("tile-2.tile").exists(),
        "the region merge's retired inputs are unlinked");
    drop(e);

    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.csn(), 2);
    assert_eq!(e.serving_memtable_entries(), 0);
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"post".to_vec()),
        "a floor-spanning region merge reopens and serves the below-floor and suffix state");
}

/// DEFECT 2: after a per-region compaction the directory holds exactly the active
/// tiles. Only `*.tile` files are counted - `data.wal`/`data-N.wal` and `manifest`
/// are expected non-tile files - and no crash or partial-compaction path ran, so
/// no orphan `.tile` is allowed.
#[test]
fn region_compaction_leaves_no_orphan_tile_files() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    for csn in 1..=3u64 {
        e.commit_block(format!("b{csn}").as_bytes(), &[doc(b"Order", 1, format!("v{csn}").as_bytes())]).unwrap();
        e.publish_tile(files.dir.join(format!("t{csn}.tile")), csn).unwrap();
    }
    assert_eq!(tile_file_count(&files.dir), 3);
    let retired = ["t1.tile", "t2.tile"];
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "P|Order merges");
    assert_eq!(tile_file_count(&files.dir), e.active_tile_cutoffs().len(),
        "every .tile on disk is manifest-referenced; the retired inputs were unlinked");
    for name in retired {
        assert!(!files.dir.join(name).exists(), "{name} was unlinked after the flip");
    }
}
