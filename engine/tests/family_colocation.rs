//! Slice 50: entity-first key encoding (`keys::KeyEncoding::EntityFirst`) makes
//! one entity's `P|X`, `U|X` and `R|X` entries sort adjacently, so under
//! `RegionMode::PerRegion` they land in the SAME tile. These tests assert the
//! tile fences (not merely the key order), prove the default family-first
//! spelling is unchanged (each family in its own tile), and show the read
//! surface (unique/reverse lookups, duplicate-owner invariant) and a reopen
//! under the same encoding all still hold.

use flashdb_engine::engine::{Engine, Op, Outcome, RegionMode};
use flashdb_engine::keys::{self, KeyEncoding};
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
        let dir = test_root().join(format!("flashdb-coloc-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
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

/// A mix of short and long entities (entity-major order additionally has to keep
/// `"A"`/`"AB"` apart from `"ABC"`, exercised by construction).
fn entities() -> Vec<Vec<u8>> {
    vec![b"A".to_vec(), b"AB".to_vec(), b"ABC".to_vec(), b"Customer".to_vec(), b"Stock".to_vec()]
}

/// The three encoded keys of entity `i`'s single row under `enc`, all at csn 1.
fn family_keys(enc: KeyEncoding, entity: &[u8], h: u64) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    (
        keys::primary_key_with(enc, entity, h, 1).unwrap(),
        keys::unique_key_with(enc, entity, b"u", b"val", h, 1).unwrap(),
        keys::reverse_key_with(enc, entity, b"r", 100 + h, h, 1).unwrap(),
    )
}

/// Index of the single active tile whose stored-key fence range contains `key`.
fn tile_of(ranges: &[(Option<Vec<u8>>, Option<Vec<u8>>)], key: &[u8]) -> Option<usize> {
    ranges.iter().position(|(first, last)| {
        first.as_deref().is_some_and(|f| f <= key) && last.as_deref().is_some_and(|l| key <= l)
    })
}

fn drive(enc: KeyEncoding) -> (Files, Engine) {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_key_encoding(enc);
    e.set_region_mode(RegionMode::PerRegion);
    let mut ops = Vec::new();
    for (i, ent) in entities().iter().enumerate() {
        let h = i as u64 + 1;
        ops.push(doc(ent, h, b"v"));
        ops.push(unique(ent, b"u", b"val", h));
        ops.push(reverse(ent, b"r", 100 + h, h));
    }
    e.commit_block(b"b1", &ops).unwrap();
    e.publish_tile(files.dir.join("t.tile"), 1).unwrap();
    (files, e)
}

#[test]
fn entity_first_puts_each_entitys_three_families_in_one_tile() {
    let (_files, e) = drive(KeyEncoding::EntityFirst);
    let ranges = e.active_tile_key_ranges();
    let regions = e.active_tile_regions();
    let list = entities();
    assert_eq!(ranges.len(), list.len(), "one tile per entity, not one per (family, entity)");
    assert!(ranges.iter().all(|(f, l)| f.is_some() && l.is_some()), "every small tile has trusted fences");

    for (i, ent) in list.iter().enumerate() {
        let h = i as u64 + 1;
        let (p, u, r) = family_keys(KeyEncoding::EntityFirst, ent, h);
        let tp = tile_of(&ranges, &p).unwrap_or_else(|| panic!("no tile fence covers P|{ent:?}"));
        let tu = tile_of(&ranges, &u).unwrap_or_else(|| panic!("no tile fence covers U|{ent:?}"));
        let tr = tile_of(&ranges, &r).unwrap_or_else(|| panic!("no tile fence covers R|{ent:?}"));
        assert_eq!(tu, tp, "U|{ent:?} must share a tile with P|{ent:?}");
        assert_eq!(tr, tp, "R|{ent:?} must share a tile with P|{ent:?}");
        // The shared tile's fence range really brackets all three keys, and the
        // tile is exactly this entity's region (so no other entity shares it).
        let (first, last) = &ranges[tp];
        let (first, last) = (first.as_deref().unwrap(), last.as_deref().unwrap());
        assert!(first <= p.as_slice() && p.as_slice() <= last);
        assert!(first <= u.as_slice() && u.as_slice() <= last);
        assert!(first <= r.as_slice() && r.as_slice() <= last);
        assert_eq!(regions[tp].as_deref(), Some(format!("C|{}", String::from_utf8_lossy(ent)).as_str()));
    }
}

#[test]
fn family_first_keeps_each_family_in_its_own_tile() {
    // The default spelling is unchanged: with three families per entity and five
    // entities a PerRegion publish emits one tile per (family, entity).
    let (_files, e) = drive(KeyEncoding::FamilyFirst);
    let ranges = e.active_tile_key_ranges();
    assert_eq!(ranges.len(), entities().len() * 3, "P/U/R of an entity are three regions");
    for (i, ent) in entities().iter().enumerate() {
        let h = i as u64 + 1;
        let (p, u, r) = family_keys(KeyEncoding::FamilyFirst, ent, h);
        let tp = tile_of(&ranges, &p).unwrap();
        let tu = tile_of(&ranges, &u).unwrap();
        let tr = tile_of(&ranges, &r).unwrap();
        assert!(tp != tu && tu != tr && tp != tr, "families of {ent:?} stay in distinct tiles");
    }
}

#[test]
fn entity_first_reads_duplicate_owner_invariant_and_reopen() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_key_encoding(KeyEncoding::EntityFirst);
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"b1", &[
        doc(b"Order", 1, b"o1"),
        unique(b"Order", b"num", b"N1", 1),
        reverse(b"Order", b"line", 10, 1),
        reverse(b"Order", b"line", 10, 2),
    ]).unwrap();

    // Unique and reverse lookups resolve to the right handles under the new key
    // spelling.
    assert_eq!(e.unique_lookup(b"Order", b"num", b"N1").unwrap(), Some(1));
    assert_eq!(e.reverse_lookup(b"Order", b"line", 10).unwrap(), vec![1, 2]);
    assert_eq!(e.get(b"Order", 1).unwrap(), Some(b"o1".to_vec()));

    // Duplicate-owner invariant: a second live owner for the same unique value is
    // a conflict and leaves the committed state unchanged.
    let out = e.commit_block(b"b2", &[unique(b"Order", b"num", b"N1", 2)]).unwrap();
    assert!(matches!(out, Outcome::Conflict(_)), "two live owners for one unique value must conflict");
    assert_eq!(e.unique_lookup(b"Order", b"num", b"N1").unwrap(), Some(1), "conflict changed nothing");

    let csn = e.csn();
    e.publish_tile(files.dir.join("t.tile"), csn).unwrap();
    let ranges_before = e.active_tile_key_ranges();
    assert_eq!(e.active_tile_regions(), vec![Some("C|Order".to_string())], "P/U/R of Order co-located");

    // Reopen with the same encoding: WAL replay re-encodes the ops, so the
    // recovered state and tile fences match, and every read still resolves.
    let reopened = Engine::open_discover_with_encoding(&files.dir, KeyEncoding::EntityFirst).unwrap();
    assert_eq!(reopened.active_tile_key_ranges(), ranges_before);
    assert_eq!(reopened.active_tile_regions(), vec![Some("C|Order".to_string())]);
    assert_eq!(reopened.get(b"Order", 1).unwrap(), Some(b"o1".to_vec()));
    assert_eq!(reopened.unique_lookup(b"Order", b"num", b"N1").unwrap(), Some(1));
    assert_eq!(reopened.reverse_lookup(b"Order", b"line", 10).unwrap(), vec![1, 2]);
}
