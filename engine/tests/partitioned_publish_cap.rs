//! Pins the fixed behaviour of the per-region publish tile cap (STATUS SS7.4).
//!
//! A `RegionMode::PerRegion` publish emits one tile per `(entity, key family)`
//! region present in the window. The active-tile cap is now a **configurable
//! policy bound** (default `manifest::MAX_TILES = 4096`, see
//! `engine/src/manifest.rs`), and the engine refuses the whole publish up front
//! — before writing any region file — when the window's distinct regions exceed
//! the remaining capacity. The refusal is `InvalidInput` with the pinned text
//! below: fail closed, never a silent partial publish and never orphan region
//! files from a refused publish.
//!
//! Two pins:
//! - `per_region_publish_over_the_cap_fails_closed`: a genuine over-cap publish
//!   still fails closed, exactly at the cap boundary.
//! - `shipped_default_cadence_keeps_a_per_region_run_under_the_cap`: the scored
//!   runner's shipped 8 MiB checkpoint/rotate cadence keeps a per-region run
//!   under the cap (below the publish threshold, then one whole-window publish
//!   fits).
//!
//! The 336-region width is the region count the slice-44 run actually reached
//! (`active tiles 336` in the preserved curve), so the arithmetic reproduces the
//! run's tile count exactly.

use flashdb_engine::engine::{Engine, Op, RegionMode, MAX_ACTIVE_TILES};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// One region per entity, matching the shipped runner's 336-region instance.
const REGIONS: usize = 336;

/// The exact error the scored run aborted with, preserved verbatim in
/// `.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/adversarial-declared.stderr`.
const PINNED_ERROR: &str =
    "active tile list is full for this publish: a partitioned publish emits one tile per region and the remaining capacity is smaller";

/// The scored runner's shipped default checkpoint/rotate cadence
/// (`harness/flashdb_harness/cold_0b.py`), matching the build harness's
/// known-compatible 8 MiB cadence.
const SHIPPED_CADENCE: u64 = 8 << 20;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn test_root() -> PathBuf {
    let shm = PathBuf::from("/dev/shm");
    if shm.is_dir() {
        shm
    } else {
        std::env::temp_dir()
    }
}

struct Files {
    dir: PathBuf,
    wal: PathBuf,
}

impl Files {
    fn new() -> Self {
        let dir = test_root().join(format!(
            "flashdb-partcap-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self {
            wal: dir.join("data.wal"),
            dir,
        }
    }

    fn tile_file_count(&self) -> usize {
        fs::read_dir(&self.dir).unwrap()
            .filter_map(|d| d.ok())
            .filter(|d| d.file_name().to_string_lossy().ends_with(".tile"))
            .count()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn entity(i: usize) -> Vec<u8> {
    format!("E{i:03}").into_bytes()
}

/// Commit one block that touches every region (one `PutDoc` per entity), the
/// runner's per-cycle shape.
fn commit_all_regions(e: &mut Engine, cycle: u64) -> io::Result<()> {
    let ops: Vec<Op> = (0..REGIONS)
        .map(|i| Op::PutDoc {
            entity: entity(i),
            handle: cycle,
            doc: vec![b'x'],
        })
        .collect();
    e.commit_block(format!("cycle-{cycle}").as_bytes(), &ops)?;
    Ok(())
}

/// One runner-style cadence cycle: commit a block that touches every region,
/// then checkpoint at `threshold`. A 1-byte threshold forces a publish on every
/// cycle (the minimum-size instance of the shipped cadence's defect class).
fn cadence_cycle(e: &mut Engine, cycle: u64, threshold: u64) -> io::Result<bool> {
    commit_all_regions(e, cycle)?;
    e.maybe_checkpoint(threshold)
}

#[test]
fn per_region_publish_over_the_cap_fails_closed() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    assert_eq!(e.max_tiles(), MAX_ACTIVE_TILES, "the raised default cap is the public ceiling");

    // 4096 / 336 = 12 whole-region publishes (4032 tiles) with capacity to
    // spare; the 13th starts with capacity 64 < 336 regions and is refused up
    // front. Assert the successes explicitly so a change to the cap or the
    // region width surfaces here, not as a mystery panic.
    let full_cycles = (e.max_tiles() / REGIONS) as u64;
    for cycle in 1..=full_cycles {
        assert!(
            cadence_cycle(&mut e, cycle, 1).unwrap(),
            "cycle {cycle} is under the cap and must checkpoint"
        );
        assert_eq!(
            e.active_tile_cutoffs().len(),
            REGIONS * cycle as usize,
            "a per-region publish emits one tile per region"
        );
    }

    let err = cadence_cycle(&mut e, full_cycles + 1, 1)
        .expect_err("the next cadence publish exceeds the cap and must fail, not succeed or panic");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "refused as InvalidInput");
    assert_eq!(err.to_string(), PINNED_ERROR, "the pinned fail-closed error text");

    // The refusal happens before any region file is written and before the
    // manifest step, so the durable active list is unchanged AND no orphan
    // region tiles were left behind.
    assert_eq!(
        e.active_tile_cutoffs().len(),
        REGIONS * full_cycles as usize,
        "the failed publish must not have appended any active tiles"
    );
    assert_eq!(
        files.tile_file_count(),
        REGIONS * full_cycles as usize,
        "the refused publish must not have written any orphan region tiles"
    );
}

#[test]
fn shipped_default_cadence_keeps_a_per_region_run_under_the_cap() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);

    // The shipped 8 MiB cadence keeps the run below the publish threshold for a
    // representative number of write cycles: no tiles accumulate, so the cap is
    // never approached by the cadence itself.
    for cycle in 1..=5u64 {
        commit_all_regions(&mut e, cycle).unwrap();
        assert!(
            !e.maybe_checkpoint(SHIPPED_CADENCE).unwrap(),
            "cycle {cycle} stays below the shipped cadence and must not publish"
        );
        assert_eq!(e.active_tile_cutoffs().len(), 0, "no tiles accumulate under the cadence");
    }

    // The eventual whole-window publish (when the run's maintenance finally
    // crosses the threshold) emits exactly one tile per region and fits the cap
    // in a single publish.
    assert!(e.maybe_checkpoint(1).unwrap(), "the forced final publish must checkpoint");
    assert_eq!(
        e.active_tile_cutoffs().len(),
        REGIONS,
        "one whole-window publish emits one tile per region, far under the cap"
    );
    assert_eq!(files.tile_file_count(), REGIONS, "exactly one tile per region was written");
}
