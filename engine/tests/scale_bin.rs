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
