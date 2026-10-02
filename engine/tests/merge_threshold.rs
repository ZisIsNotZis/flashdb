//! Slice 27: the per-region compaction-trigger actuator.
//!
//! `LayoutPolicy` now carries a per-region `merge_min_tiles`: an automatic merge
//! for a region is only considered once that region's own tile count reaches its
//! threshold. The default is 2, today's effective threshold (selection already
//! required a run of two tiles), so nothing changes unless a policy asks. These
//! tests prove the defer/allow boundary, that regions are gated independently,
//! that a lazy neighbour cannot hold back an eager one, that the explicit
//! `compact()` still collapses a region regardless, that the default reproduces
//! today's output byte-for-byte, and that the JSON policy file is accepted or
//! rejected exactly as documented.

use flashdb_engine::engine::{Engine, LayoutPolicy, Op, RegionMode, DEFAULT_MERGE_MIN_TILES};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn test_root() -> PathBuf {
    let shm = PathBuf::from("/dev/shm");
    if shm.is_dir() { shm } else { std::env::temp_dir() }
}
struct Files { dir: PathBuf, wal: PathBuf }
impl Files {
    fn new() -> Self {
        let dir = test_root().join(format!("flashdb-merge-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&dir).unwrap();
        Self { wal: dir.join("data.wal"), dir }
    }
}
impl Drop for Files { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.dir); } }

/// Publish one tile for `entity` at each of `n` successive cutoffs, so the
/// region accumulates `n` tiles at distinct cutoffs.
fn publish_region(e: &mut Engine, dir: &Path, entity: &[u8], n: u64) {
    for _ in 0..n {
        let csn = e.csn() + 1;
        e.commit_block(format!("b{csn}").as_bytes(), &[
            Op::PutDoc { entity: entity.to_vec(), handle: csn, doc: format!("v{csn}").into_bytes() },
        ]).unwrap();
        e.publish_tile(dir.join(format!("t{csn}.tile")), csn).unwrap();
    }
}

fn count_region(e: &Engine, region: &str) -> usize {
    e.active_tile_regions().iter().filter(|r| r.as_deref() == Some(region)).count()
}

fn per_region_engine(files: &Files) -> Engine {
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e
}

#[test]
fn a_region_below_its_threshold_defers_and_merges_once_it_reaches_it() {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    publish_region(&mut e, &files.dir, b"Order", 3);
    assert_eq!(count_region(&e, "P|Order"), 3);

    let mut policy = LayoutPolicy::declared_default();
    assert_eq!(policy.set_region_merge("P|Order", 5), None);
    e.set_layout_policy(policy);

    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_none(), "3 tiles < threshold 5: no merge");
    assert_eq!(count_region(&e, "P|Order"), 3, "the region is untouched below its threshold");

    publish_region(&mut e, &files.dir, b"Order", 2);
    assert_eq!(count_region(&e, "P|Order"), 5, "the region is at its threshold");
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "at the threshold the region merges");
    assert!(count_region(&e, "P|Order") < 5, "the merge retired at least one tile");
}

#[test]
fn two_regions_with_different_thresholds_compact_independently() {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    // P|Order is eager (default 2), P|Stock is lazy (5); each region starts with
    // 3 tiles. P|Order is written first, so merging it pulls no P|Stock keys in
    // from the WAL suffix and its tile is identified by its own region.
    publish_region(&mut e, &files.dir, b"Order", 3);
    publish_region(&mut e, &files.dir, b"Stock", 3);
    assert_eq!(count_region(&e, "P|Order"), 3);
    assert_eq!(count_region(&e, "P|Stock"), 3);

    let mut policy = LayoutPolicy::declared_default();
    policy.set_region_merge("P|Stock", 5);
    e.set_layout_policy(policy);

    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "the eager region merges");
    assert!(count_region(&e, "P|Order") < 3, "the eager region compacted");
    assert_eq!(count_region(&e, "P|Stock"), 3, "the lazy region is held back below its threshold");

    // Bring P|Stock to its own threshold; now it too becomes eligible and merges.
    publish_region(&mut e, &files.dir, b"Stock", 2);
    assert_eq!(count_region(&e, "P|Stock"), 5);
    let mut lazy_merged = false;
    while e.maybe_compact(u64::MAX, 2).unwrap().is_some() { lazy_merged = true; }
    assert!(lazy_merged, "the lazy region merges once it reaches its threshold");
    assert!(count_region(&e, "P|Stock") < 5, "the lazy region's own tiers were merged");
}

#[test]
fn an_eager_region_is_not_held_back_by_a_lazy_neighbour() {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    publish_region(&mut e, &files.dir, b"Order", 3); // eager (default 2)
    publish_region(&mut e, &files.dir, b"Stock", 8); // lazy, far from its threshold
    let mut policy = LayoutPolicy::declared_default();
    policy.set_region_merge("P|Stock", 100);
    e.set_layout_policy(policy);
    // The eager region keeps compacting across calls while the lazy one is inert.
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some());
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some());
    assert!(count_region(&e, "P|Order") < 3, "the eager region compacted");
    assert_eq!(count_region(&e, "P|Stock"), 8, "the lazy neighbour never moved");
}

#[test]
fn explicit_compact_ignores_the_threshold_and_collapses_a_region() {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    publish_region(&mut e, &files.dir, b"Order", 3);
    let mut policy = LayoutPolicy::declared_default();
    policy.set_region_merge("P|Order", 200);
    e.set_layout_policy(policy);
    // The automatic path refuses...
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_none());
    // ...but an explicit collapse-on-request still merges the region to one tile.
    e.compact().unwrap();
    assert_eq!(count_region(&e, "P|Order"), 1, "compact() collapses the region on request");
    assert_eq!(e.get(b"Order", 3).unwrap(), Some(b"v3".to_vec()));
}

/// Read the exact tile-file bytes of a directory, keyed by name, for a
/// byte-for-byte comparison between two runs.
fn tile_bytes(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("tile") {
            out.insert(path.file_name().unwrap().to_string_lossy().into_owned(), fs::read(&path).unwrap());
        }
    }
    out
}

/// The existing slice-24 region-compaction scenario, returning the on-disk tile
/// bytes plus the read surface, so two policies can be compared exactly.
fn region_compaction_scenario(policy: LayoutPolicy) -> (Files, BTreeMap<String, Vec<u8>>, Vec<String>) {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    e.set_layout_policy(policy);
    for (i, v) in [b"a".as_slice(), b"b", b"c"].iter().enumerate() {
        let csn = (i + 1) as u64;
        e.commit_block(format!("b{csn}").as_bytes(), &[
            Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: v.to_vec() },
            Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku".to_vec(), value: b"A".to_vec(), handle: 1 },
        ]).unwrap();
        e.publish_tile(files.dir.join(format!("t{csn}.tile")), csn).unwrap();
    }
    let mut merged = 0;
    while e.maybe_compact(u64::MAX, 2).unwrap().is_some() { merged += 1; }
    assert!(merged > 0, "the scenario must exercise region compaction");
    let reads = vec![
        format!("{:?}", e.get(b"Order", 1).unwrap()),
        format!("{:?}", e.unique_lookup(b"Stock", b"sku", b"A").unwrap()),
    ];
    let bytes = tile_bytes(&files.dir);
    (files, bytes, reads)
}

#[test]
fn default_policy_reproduces_todays_compaction_byte_for_byte() {
    // "Today" is the default policy (every region's effective threshold is 2).
    assert_eq!(LayoutPolicy::declared_default().default_merge_min_tiles(), DEFAULT_MERGE_MIN_TILES);
    assert_eq!(DEFAULT_MERGE_MIN_TILES, 2, "the default is the pre-slice-27 effective threshold");
    let (files_default, bytes_default, reads_default) = region_compaction_scenario(LayoutPolicy::declared_default());
    // The same run with the threshold spelled out explicitly (2 everywhere) must
    // produce byte-identical tiles and reads: the default is exactly eager.
    let mut explicit = LayoutPolicy::declared_default();
    explicit.set_region_merge("P|Order", 2);
    explicit.set_region_merge("U|Stock", 2);
    let (files_explicit, bytes_explicit, reads_explicit) = region_compaction_scenario(explicit);
    assert_eq!(bytes_default, bytes_explicit, "the default policy's tile output is byte-for-byte today's");
    assert_eq!(reads_default, reads_explicit);
    // Sanity: the scenario really produced compacted output, so the comparison is
    // not vacuous.
    assert!(bytes_default.keys().any(|n| n.starts_with("compact-")), "a compaction output exists");
    drop((files_default, files_explicit));
}

#[test]
fn recovery_after_threshold_compaction_reopens_with_the_same_data() {
    let files = Files::new();
    let mut e = per_region_engine(&files);
    publish_region(&mut e, &files.dir, b"Order", 4);
    publish_region(&mut e, &files.dir, b"Stock", 2);
    let mut policy = LayoutPolicy::declared_default();
    policy.set_region_merge("P|Order", 4); // eligible exactly at 4
    policy.set_region_merge("P|Stock", 100); // never eligible here
    e.set_layout_policy(policy.clone());
    assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "the Order region is at its threshold");
    let cutoffs_before = e.active_tile_cutoffs();
    let order_before = e.get(b"Order", 4).unwrap();
    let stock_before = e.get(b"Stock", 6).unwrap();
    drop(e);

    let e = Engine::open_discover(&files.dir).unwrap();
    assert_eq!(e.active_tile_cutoffs(), cutoffs_before, "the (cutoff, region) list reopens unchanged");
    assert_eq!(e.get(b"Order", 4).unwrap(), order_before);
    assert_eq!(e.get(b"Stock", 6).unwrap(), stock_before);
}

#[test]
fn policy_file_schema_round_trips_and_rejects_invalid_input() {
    // A well-formed file: defaults plus two region rows.
    let good = br#"{
        "default": {"merge_min_tiles": 2},
        "regions": {
            "P|Stock": {"merge_min_tiles": 2},
            "P|Order": {"merge_min_tiles": 64, "bloom_bits_per_key": 0}
        }
    }"#;
    let policy = LayoutPolicy::from_json_slice(good).expect("a well-formed policy file parses");
    assert_eq!(policy.merge_min_tiles_for_region("P|Stock"), 2);
    assert_eq!(policy.merge_min_tiles_for_region("P|Order"), 64);
    assert_eq!(policy.for_region("P|Order"), (4096, 0), "page falls back to the default, bits are set");
    assert_eq!(policy.merge_min_tiles_for_region("U|Unlisted"), 2, "an unlisted region uses the default");
    // The JSON dump round-trips through the parser unchanged.
    let dumped = policy.to_json().to_string();
    let reparsed = LayoutPolicy::from_json_slice(dumped.as_bytes()).expect("its own dump reparses");
    assert_eq!(reparsed, policy);

    // A well-formed region that names no tile is accepted (the policy is a
    // lookup table); it is not an error.
    assert!(LayoutPolicy::from_json_slice(br#"{"regions": {"P|NoSuchData": {"merge_min_tiles": 8}}}"#).is_ok());

    // Everything below is a hard rejection of the whole document.
    let rejected: &[&[u8]] = &[
        br#"{"regions": {"Nope": {"merge_min_tiles": 2}}}"#,      // not a region spelling
        br#"{"regions": {"P|": {"merge_min_tiles": 2}}}"#,         // empty entity
        br#"{"regions": {"X|Foo": {"merge_min_tiles": 2}}}"#,      // unknown family marker
        br#"{"regions": {"P|Order": {"merge_min_tiles": 1}}}"#,    // below the merge minimum
        br#"{"regions": {"P|Order": {"merge_min_tiles": 300}}}"#,  // above a u8
        br#"{"regions": {"P|Order": {"page_size": 123}}}"#,        // page size out of range
        br#"{"regions": {"P|Order": {"bloom_bits_per_key": 200}}}"#, // bits above a u8
        br#"{"regions": {"P|Order": {"merge_min_tiles": 2, "typo": 1}}}"#, // unknown field
        br#"{"defalut": {}}"#,                                     // unknown top-level field
        b"not json",                                               // not JSON
    ];
    for bytes in rejected {
        assert!(
            LayoutPolicy::from_json_slice(bytes).is_err(),
            "must reject: {}",
            String::from_utf8_lossy(bytes)
        );
    }
}
