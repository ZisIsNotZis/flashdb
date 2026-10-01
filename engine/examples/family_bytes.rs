//! Inspection hook: per-key-family entry counts and byte shares of the merged view.
//!
//! Motivation (`.scratch/14-layout-harness/layout-headroom`): the next layout axis that stays
//! correctness-preserving is keeping different key families in different tiles, so a slowly
//! changing family is not rewritten when a hot family compacts. Its headroom reaches the
//! provisional 15% threshold only if the slow family is at least ~20% of written bytes, so this
//! measures the family composition of a real engine directory instead of assuming it.
//!
//! usage: cargo run --release --example family_bytes -- <engine-dir>

use flashdb_engine::engine::Engine;
use std::collections::BTreeMap;

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().unwrap_or_else(|| {
        eprintln!("usage: family_bytes <engine-dir>");
        std::process::exit(2);
    });
    let engine = Engine::open_discover(std::path::Path::new(dir))?;
    // key bytes + value bytes per family marker byte
    let mut bytes: BTreeMap<u8, (u64, u64)> = BTreeMap::new(); // marker -> (entries, bytes)
    engine.scan_all(|key, value| {
        let marker = key.first().copied().unwrap_or(b'?');
        let e = bytes.entry(marker).or_insert((0, 0));
        e.0 += 1;
        e.1 += key.len() as u64 + value.len() as u64;
        Ok(())
    })?;
    let total: u64 = bytes.values().map(|(_, b)| *b).sum();
    println!("dir={dir} total_bytes={total}");
    for (marker, (entries, b)) in &bytes {
        let name = match *marker {
            b'P' => "P primary",
            b'U' => "U unique",
            b'R' => "R reverse",
            other => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("unknown key family marker {other:?}"))),
        };
        println!("family={marker} ({name}) entries={entries} bytes={b} share={:.4}",
                 *b as f64 / total.max(1) as f64);
    }
    Ok(())
}
