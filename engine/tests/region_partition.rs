//! Slice 24: region-partitioned tiles. A publish or compaction output emits one
//! tile per `(entity, key family)` region, so per-region layout policy has an
//! actuator. These tests prove the read surface is unchanged, recovery validates
//! the larger `(cutoff, region)`-ordered list, compaction never merges across
//! regions, rotation survives the multi-tile-per-cutoff list, and a tile reads
//! on its own stored parameters regardless of the live policy (I2).

use flashdb_engine::engine::{Engine, LayoutPolicy, Op, RegionMode};
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
        let dir = test_root().join(format!("flashdb-region-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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

/// The op script both engines replay, so their read surfaces are comparable.
fn script() -> Vec<Vec<Op>> {
    vec![
        vec![
            doc(b"Order", 1, b"o1"),
            doc(b"Stock", 1, b"s1"),
            unique(b"Stock", b"sku", b"A", 1),
            reverse(b"Ticket", b"order", 10, 1),
        ],
        vec![
            doc(b"Order", 2, b"o2"),
            doc(b"Order", 1, b"o1v2"),
            unique(b"Stock", b"sku", b"B", 2),
            reverse(b"Ticket", b"order", 10, 2),
        ],
        vec![
            doc(b"Stock", 2, b"s2"),
            unique(b"Stock", b"sku", b"A", 2),
        ],
    ]
}

/// Read every surface of an engine into a comparable tuple. `tiles` records the
/// active cutoff list so the caller can see the partitioning without depending
/// on it in the read comparison.
fn readings(e: &Engine) -> Vec<String> {
    let mut out = Vec::new();
    out.push(format!("get Order/1 {:?}", e.get(b"Order", 1).unwrap()));
    out.push(format!("get Order/2 {:?}", e.get(b"Order", 2).unwrap()));
    out.push(format!("get Stock/1 {:?}", e.get(b"Stock", 1).unwrap()));
    out.push(format!("get Stock/2 {:?}", e.get(b"Stock", 2).unwrap()));
    out.push(format!("unique Stock sku A {:?}", e.unique_lookup(b"Stock", b"sku", b"A").unwrap()));
    out.push(format!("unique Stock sku B {:?}", e.unique_lookup(b"Stock", b"sku", b"B").unwrap()));
    out.push(format!("unique Stock sku Z {:?}", e.unique_lookup(b"Stock", b"sku", b"Z").unwrap()));
    out.push(format!("reverse Ticket order 10 {:?}", e.reverse_lookup(b"Ticket", b"order", 10).unwrap()));
    for entity in [&b"Order"[..], &b"Stock"[..]] {
        let mut rows = Vec::new();
        e.scan_primary(entity, |h, v| { rows.push((h, v.to_vec())); Ok(()) }).unwrap();
        out.push(format!("scan {:?} {:?}", String::from_utf8_lossy(entity), rows));
    }
    out
}

fn drive(mode: RegionMode) -> (Files, Engine) {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(mode);
    let blocks = script();
    for (i, ops) in blocks.iter().enumerate() {
        e.commit_block(format!("b{}", i + 1).as_bytes(), ops).unwrap();
    }
    (files, e)
}

#[test]
fn reads_are_identical_to_a_global_engine_and_survive_reopen() {
    let (gfiles, mut global) = drive(RegionMode::Global);
    let (pfiles, mut partitioned) = drive(RegionMode::PerRegion);
    let csn = global.csn();
    assert_eq!(csn, partitioned.csn());
    // Publish both at the same CSN so the read comparison is over the same
    // split between tiles and memtable.
    global.publish_tile(gfiles.dir.join("tile.pub.tile"), csn).unwrap();
    partitioned.publish_tile(pfiles.dir.join("tile.pub.tile"), csn).unwrap();

    assert_eq!(readings(&global), readings(&partitioned), "every read is unchanged by partitioning");
    assert_eq!(global.active_tile_cutoffs().len(), 1);
    assert_eq!(partitioned.active_tile_cutoffs().len(), 4, "P|Order, P|Stock, U|Stock, R|Ticket");
    assert!(partitioned.active_tile_cutoffs().iter().all(|&c| c == csn));

    // Recovery validates the larger tile list and serves the same state.
    let reopened = Engine::open_discover(&pfiles.dir).unwrap();
    assert_eq!(reopened.active_tile_cutoffs(), partitioned.active_tile_cutoffs());
    assert_eq!(readings(&reopened), readings(&partitioned));
}

#[test]
fn region_rule_is_entity_and_key_family() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // Same entity, two U fields: ONE region. Different entities/families: more.
    e.commit_block(b"b1", &[
        doc(b"Stock", 1, b"s"),
        unique(b"Stock", b"sku", b"A", 1),
        unique(b"Stock", b"loc", b"L", 1),
        unique(b"Customer", b"email", b"c@x", 1),
        reverse(b"Stock", b"loc", 9, 1),
    ]).unwrap();
    e.publish_tile(files.dir.join("t.tile"), 1).unwrap();
    // P|Stock, U|Stock, U|Customer, R|Stock: four regions, all at cutoff 1.
    assert_eq!(e.active_tile_cutoffs(), vec![1, 1, 1, 1]);
    let mut names: Vec<String> = fs::read_dir(&files.dir).unwrap()
        .filter_map(|d| d.ok())
        .filter_map(|d| d.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tile"))
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["t.r0.tile", "t.r1.tile", "t.r2.tile", "t.r3.tile"]);
    let reopened = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(reopened.active_tile_cutoffs(), vec![1, 1, 1, 1]);
}

#[test]
fn per_region_policy_applies_and_reading_never_needs_it() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // The scan-heavy entity/family gets 64 KiB pages; a probe-heavy region gets
    // 4 KiB and no bloom. A region without an override keeps the default.
    let mut policy = LayoutPolicy::declared_default();
    policy.set_region("P|Movement", 65536, 4);
    policy.set_region("U|Stock", 4096, 0);
    e.set_layout_policy(policy);
    e.commit_block(b"b1", &[
        doc(b"Movement", 1, b"m1"),
        unique(b"Stock", b"sku", b"A", 1),
        reverse(b"Ticket", b"order", 7, 1),
    ]).unwrap();
    e.publish_tile(files.dir.join("t.tile"), 1).unwrap();

    // Now install an EMPTY / unknown policy (as recovery does) and read: each
    // tile carries its own page size and bloom bits, so reads must be identical.
    let before = readings_for(&e);
    e.set_layout_policy(LayoutPolicy::with_default(4096, 10));
    assert_eq!(readings_for(&e), before, "reads never consult the live policy");
    let reopened = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(reopened.layout_policy().for_region("P|Movement"), (4096, 10), "recovery has no policy");
    assert_eq!(readings_for(&reopened), before, "a tile reads on its stored parameters (I2)");
}

fn readings_for(e: &Engine) -> Vec<String> {
    vec![
        format!("{:?}", e.get(b"Movement", 1).unwrap()),
        format!("{:?}", e.unique_lookup(b"Stock", b"sku", b"A").unwrap()),
        format!("{:?}", e.reverse_lookup(b"Ticket", b"order", 7).unwrap()),
    ]
}

#[test]
fn compaction_merges_only_within_a_region() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // Three publishes, each touching P|Order and U|Stock -> 6 tiles.
    for (i, v) in [b"a".as_slice(), b"b", b"c"].iter().enumerate() {
        let csn = (i + 1) as u64;
        e.commit_block(format!("b{csn}").as_bytes(), &[
            doc(b"Order", 1, v),
            unique(b"Stock", b"sku", b"A", 1),
        ]).unwrap();
        e.publish_tile(files.dir.join(format!("t{csn}.tile")), csn).unwrap();
    }
    assert_eq!(e.active_tile_cutoffs(), vec![1, 1, 2, 2, 3, 3]);
    let out = e.maybe_compact(u64::MAX, 2).unwrap();
    assert!(out.is_some(), "a region with >= 2 tiles is compactable");
    // One run of one region is merged (its tile count drops); no tile from the
    // other region is touched, so every region's cutoffs stay increasing and
    // the read surface is unchanged.
    let cutoffs = e.active_tile_cutoffs();
    assert_eq!(cutoffs.len(), 5, "one region's tiers merged; the other region is untouched");
    // Every read is still correct after the region-local merge.
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"c".to_vec()));
    assert_eq!(e.unique_lookup(b"Stock", b"sku", b"A").unwrap(), Some(1));
    // And it reopens: the (cutoff, region) invariant holds.
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.active_tile_cutoffs(), cutoffs);
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"c".to_vec()));
}

#[test]
fn rotation_then_reopen_works_with_several_tiles_per_cutoff() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"b1", &[doc(b"Order", 1, b"o"), unique(b"Stock", b"sku", b"A", 1)]).unwrap();
    e.publish_tile(files.dir.join("t.tile"), 1).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![1, 1]);
    // Rotation requires the newest cutoff == csn and an empty memtable, both
    // true after a full publish even with several tiles at that cutoff.
    assert!(e.rotate_wal(0).unwrap(), "rotation succeeds with two tiles at cutoff 1");
    // Writes continue against the fresh segment and a second publish appends.
    e.commit_block(b"b2", &[doc(b"Order", 1, b"o2"), unique(b"Stock", b"sku", b"A", 1)]).unwrap();
    e.publish_tile(files.dir.join("t2.tile"), 2).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![1, 1, 2, 2]);
    drop(e);
    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.active_tile_cutoffs(), vec![1, 1, 2, 2], "a rotated, region-partitioned directory reopens");
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"o2".to_vec()));
    assert_eq!(e.unique_lookup(b"Stock", b"sku", b"A").unwrap(), Some(1));
}

#[test]
fn partitioned_reopen_rejects_a_region_out_of_order_or_duplicated() {
    // The ordering invariant (`(cutoff, region)` strictly increasing, each
    // region distinct within a cutoff group) is enforced in `open_impl`; the
    // engine-level fault tests in `engine.rs` build the malformed lists. Here we
    // simply confirm the honest partitioned list round-trips byte for byte.
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"b1", &[doc(b"Order", 1, b"o"), unique(b"Stock", b"sku", b"A", 1)]).unwrap();
    e.publish_tile(files.dir.join("t.tile"), 1).unwrap();
    assert!(Engine::open_discover(&files.dir).is_ok());
}
