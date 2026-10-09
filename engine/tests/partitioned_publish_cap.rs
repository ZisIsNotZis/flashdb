//! Pins a verified engine-level incompatibility: a partitioned publish whose
//! window spans more `(entity, key family)` regions than the manifest's
//! remaining tile capacity fails, and that failure is indistinguishable from a
//! hard cap unless the caller can rotate the cadence.
//!
//! **This test documents CURRENT behaviour, not desired behaviour.** It is not a
//! specification of what the engine should do; it is a deterministic, minimal
//! reproduction of the abort that a scored cold run hits when it drives the
//! runner's shipped 16 KiB checkpoint/rotate cadence on a per-region instance
//! (`.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/`). If a
//! later change (a raised `manifest::MAX_TILES`, or a cadence that keeps the
//! per-region publish count below the cap) makes this test fail, that is the
//! intended signal that the recorded incompatibility was addressed — update the
//! pin, do not "fix" the engine to keep the old text.
//!
//! Mechanism. `manifest::MAX_TILES` is 1024 (`engine/src/manifest.rs:118`). A
//! `RegionMode::PerRegion` publish emits one tile per region present in the
//! window; the per-region loop checks `emitted.len() >= capacity`
//! (`engine/src/engine.rs:1062-1063`) BEFORE opening each new region's file,
//! where `capacity = MAX_TILES - active_tiles` (`engine/src/engine.rs:1003`). So a
//! publish near the cap refuses to publish its later regions and returns
//! `InvalidInput` with the pinned text below instead of silently dropping
//! regions or panicking. The 336-region width is the region count the slice-44
//! run actually reached (`active tiles 336` in the preserved curve).

use flashdb_engine::engine::{Engine, Op, RegionMode};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// One region per entity, matching the shipped runner's 336-region instance so
/// the arithmetic below reproduces the run's tile count exactly.
const REGIONS: usize = 336;

/// The exact error the scored run aborted with, preserved verbatim in
/// `.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/adversarial-declared.stderr`.
const PINNED_ERROR: &str =
    "active tile list is full for this publish: a partitioned publish emits one tile per region and the remaining capacity is smaller";

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
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn entity(i: usize) -> Vec<u8> {
    format!("E{i:03}").into_bytes()
}

/// One runner-style cadence cycle: commit a block that touches every region,
/// then checkpoint with a deliberately tiny WAL threshold (the shipped cadence
/// is 16 KiB; 1 byte forces a publish on every cycle, which is the same defect
/// class at minimum size). Returns `maybe_checkpoint`'s result.
fn cadence_cycle(e: &mut Engine, cycle: u64) -> io::Result<bool> {
    let ops: Vec<Op> = (0..REGIONS)
        .map(|i| Op::PutDoc {
            entity: entity(i),
            handle: cycle,
            doc: vec![b'x'],
        })
        .collect();
    e.commit_block(format!("cycle-{cycle}").as_bytes(), &ops)?;
    e.maybe_checkpoint(1)
}

#[test]
fn per_region_cadence_exhausts_the_tile_cap_and_the_publish_fails() {
    let files = Files::new();
    let mut e = Engine::create(&files.wal).unwrap();
    e.set_region_mode(RegionMode::PerRegion);

    // 1024 / 336 = 3 whole cadence publishes (1008 tiles) with capacity to
    // spare; the fourth starts with capacity 16 < 336 regions and hits the cap
    // partway through. Assert the successes explicitly so a change to the cap or
    // the region width surfaces here, not as a mystery panic.
    for cycle in 1..=3u64 {
        assert!(
            cadence_cycle(&mut e, cycle).unwrap(),
            "cycle {cycle} is under the cap and must checkpoint"
        );
        assert_eq!(
            e.active_tile_cutoffs().len(),
            REGIONS * cycle as usize,
            "a per-region publish emits one tile per region"
        );
    }

    let err = cadence_cycle(&mut e, 4)
        .expect_err("the fourth cadence publish exceeds the 1024-tile cap and must fail, not succeed or panic");
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "refused as InvalidInput");
    assert_eq!(err.to_string(), PINNED_ERROR, "the pinned current-behaviour error text");

    // The refusal happens before the manifest step, so the durable active list
    // is still exactly the three successful cadence publishes: the failure is
    // reported, never a silent partial publish. (The engine is left usable; the
    // region tiles already written for the aborted publish are unreferenced
    // orphans, which is why the run must abort rather than keep going.)
    assert_eq!(
        e.active_tile_cutoffs().len(),
        REGIONS * 3,
        "the failed publish must not have appended any active tiles"
    );
}
