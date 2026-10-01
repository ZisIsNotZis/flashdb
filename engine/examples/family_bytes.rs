//! Inspection hook: per-key-family entry counts and byte shares of the merged view.
//!
//! Motivation (`.scratch/14-layout-harness/layout-headroom`): the next layout axis that stays
//! correctness-preserving is keeping different key families in different tiles, so a slowly
//! changing family is not rewritten when a hot family compacts. Its headroom reaches the
//! provisional 15% threshold only if the slow family is at least ~20% of written bytes, so this
//! measures the family composition of a real engine directory instead of assuming it.
//!
//! Two views are reported per family (`P` primary, `U` unique, `R` reverse):
//!
//!   written  every version present in the engine. On a directory that was only replayed (no
//!            checkpoint, so every committed version still lives in the WAL-backed memtable) this
//!            is the true **written** byte share. After compaction the older versions are gone, so
//!            the written share can only be reconstructed from the trace, not from the directory.
//!   live     the newest version per logical key (the stored view a read observes). Keys end in
//!            `~csn`, and ascending key order puts the newest version first, so a key whose
//!            8-byte `~csn` suffix changed is a rewrite and only its first occurrence counts.
//!
//! For write-once families (this workload's `U` and `R`) written == live. For a rewritten family
//! (here `P`, whose Stock documents are patched on every order) written > live, so the live share
//! is a *lower* bound for the write share -- exactly the relation the headroom decision needs.
//!
//! usage: cargo run --release --example family_bytes -- <engine-dir>

use flashdb_engine::engine::Engine;
use std::collections::BTreeMap;

/// The trailing 8 bytes are `~csn` (all keys end in it, see `keys::key_csn`); the rest is the
/// logical key shared by every version.
fn logical_prefix(key: &[u8]) -> &[u8] {
    &key[..key.len().saturating_sub(8)]
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args.first().unwrap_or_else(|| {
        eprintln!("usage: family_bytes <engine-dir>");
        std::process::exit(2);
    });
    let engine = Engine::open_discover(std::path::Path::new(dir))?;
    // marker -> (entries, bytes); `written` counts every version, `live` only the newest.
    let mut written: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
    let mut live: BTreeMap<u8, (u64, u64)> = BTreeMap::new();
    let mut last_prefix: Vec<u8> = Vec::new();
    engine.scan_all(|key, value| {
        let marker = key.first().copied().unwrap_or(b'?');
        let width = (key.len() + value.len()) as u64;
        let w = written.entry(marker).or_insert((0, 0));
        w.0 += 1;
        w.1 += width;
        let prefix = logical_prefix(key);
        if prefix != last_prefix.as_slice() {
            last_prefix.clear();
            last_prefix.extend_from_slice(prefix);
            let l = live.entry(marker).or_insert((0, 0));
            l.0 += 1;
            l.1 += width;
        }
        Ok(())
    })?;
    let written_total: u64 = written.values().map(|(_, b)| *b).sum();
    let live_total: u64 = live.values().map(|(_, b)| *b).sum();
    println!("dir={dir} written_bytes={written_total} live_bytes={live_total}");
    for marker in [b'P', b'U', b'R'] {
        let name = match marker {
            b'P' => "P primary",
            b'U' => "U unique",
            b'R' => "R reverse",
            _ => unreachable!(),
        };
        let (we, wb) = written.get(&marker).copied().unwrap_or((0, 0));
        let (le, lb) = live.get(&marker).copied().unwrap_or((0, 0));
        println!(
            "family={} ({name}) written_entries={we} written_bytes={wb} written_share={:.4} \
             live_entries={le} live_bytes={lb} live_share={:.4}",
            marker as char,
            wb as f64 / written_total.max(1) as f64,
            lb as f64 / live_total.max(1) as f64,
        );
    }
    if let Some((marker, _)) = written.iter().find(|(m, _)| !matches!(m, b'P' | b'U' | b'R')) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown key family marker {marker:?}"),
        ));
    }
    Ok(())
}
