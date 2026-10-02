//! End-to-end round-trip of the `flashdb-scale` benchmark binary: seed a few
//! thousand deterministic docs into a temp directory, then require `verify` to
//! reproduce the exact CSN, per-entity doc counts and unique-handle probes.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn scale_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flashdb-scale-bin-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    dir
}

fn run_scale(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_flashdb-scale"))
        .args(args)
        .output()
        .expect("flashdb-scale binary runs");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn seed_then_verify_roundtrip() {
    let dir = scale_dir();
    let dir_s = dir.to_str().unwrap().to_string();

    // 3000 docs, 500 per commit -> exactly 6 blocks; 64-byte docs.
    let (status, stdout, stderr) =
        run_scale(&["seed", &dir_s, "3000", "500", "64"]);
    assert!(
        status.success(),
        "seed failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("phase=seed ok=true docs=3000"), "seed summary line missing: {stdout}");
    assert!(stdout.contains("csn=6"), "seed must report 6 committed blocks: {stdout}");

    let (status, stdout, stderr) = run_scale(&["verify", &dir_s, "3000"]);
    assert!(
        status.success(),
        "verify failed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("phase=verify ok=true csn=6 docs_a=1500 docs_b=1500"),
        "verify must reproduce the seeded shape: {stdout}"
    );

    // A mismatched doc total must fail loudly and nonzero.
    let (status, stdout, _stderr) = run_scale(&["verify", &dir_s, "2999"]);
    assert!(!status.success(), "verify with wrong total must fail: {stdout}");
    assert!(stdout.contains("phase=verify ok=false"), "failure must be machine-visible: {stdout}");

    let (status, stdout, stderr) = run_scale(&["scanbench", &dir_s, "1"]);
    assert!(
        status.success(),
        "scanbench failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("phase=scanbench ok=true passes=1"), "scanbench summary line missing: {stdout}");

    fs::remove_dir_all(&dir).unwrap();
}

/// Slice 25: `seed-inventory` with the checkpoint knob on publishes the seeded
/// dimensions as tiles through the tested publication protocol, and the resulting
/// directory's serving memtable is empty -- i.e. a subsequent read must consult a
/// tile. `tileinfo` reads each tile's superblock so the page/bloom state is
/// observable without opening it as an engine tile.
#[test]
fn seed_inventory_checkpoint_publishes_tiles_and_empties_memtable() {
    let dir = scale_dir();
    let dir_s = dir.to_str().unwrap().to_string();
    let trace = dir.join("seed.jsonl");
    let line = r#"{"block":"seed-00000000","docs":[{"entity":"Customer","doc":{"email":"c000000@example.com"},"unique":{"field":"email","value":"c000000@example.com"}},{"entity":"Stock","doc":{"sku":"S000001","loc":"L000","on_hand":5},"unique":{"field":"sku_loc","value":["S000001","L000"]}}]}"#;
    fs::write(&trace, format!("{line}\n")).unwrap();

    let seeded = Command::new(env!("CARGO_BIN_EXE_flashdb-scale"))
        .args(["seed-inventory", &dir_s, trace.to_str().unwrap()])
        .env("FLASHDB_SEED_CHECKPOINT_BYTES", "0")
        .env("FLASHDB_REGION_MODE", "per_region")
        .output()
        .expect("flashdb-scale binary runs");
    let seeded_out = String::from_utf8_lossy(&seeded.stdout);
    assert!(seeded.status.success(), "seed-inventory failed: {seeded_out}");
    assert!(seeded_out.contains("publishes=1"), "the final block must publish: {seeded_out}");
    assert!(seeded_out.contains("serving_memtable_entries=0"), "the published tile must evict the memtable: {seeded_out}");

    let info = Command::new(env!("CARGO_BIN_EXE_flashdb-scale"))
        .args(["tileinfo", &dir_s])
        .output()
        .expect("flashdb-scale binary runs");
    let info_out = String::from_utf8_lossy(&info.stdout);
    assert!(info.status.success(), "tileinfo failed: {info_out}");
    assert!(!info_out.contains("active_tiles=0"), "at least one tile must be active: {info_out}");
    assert!(info_out.contains("serving_memtable_entries=0"), "reads must come from tiles: {info_out}");
    assert!(info_out.contains("region=P|Customer") && info_out.contains("region=P|Stock"),
        "per-region tiles must be reported: {info_out}");
    assert!(info_out.contains("page_size=4096"), "the default page size must be reported: {info_out}");

    fs::remove_dir_all(&dir).unwrap();
}
