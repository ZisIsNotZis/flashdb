//! Micro-benchmark: cost of one unique probe against (a) the in-RAM memtable and
//! (b) a published tile, as a function of tile size. Not a correctness test.
//!
//! usage: cargo run --release --example probe_cost -- [scan] [DOCS ...]
//!
//! `scan` additionally reports the full sequential tile scan throughput (MiB/s);
//! without it only the probe cost is printed.

use flashdb_engine::engine::{Engine, Op};
use std::time::Instant;

const SCAN_ITERS: usize = 3;

fn run(docs: u64, probes: usize, measure_scan: bool) -> std::io::Result<()> {
    let dir = std::env::temp_dir().join(format!("flashdb-probe-cost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut e = Engine::create(dir.join("data.wal"))?;

    let payload = vec![b'x'; 200];
    let mut csn_line = 0u64;
    for start in (1..=docs).step_by(1000) {
        let end = (start + 999).min(docs);
        let mut ops = Vec::new();
        for h in start..=end {
            ops.push(Op::PutDoc { entity: b"P".to_vec(), handle: h, doc: payload.clone() });
            ops.push(Op::PutUnique {
                entity: b"P".to_vec(),
                field: b"no".to_vec(),
                value: format!("u{h}").into_bytes(),
                handle: h,
            });
        }
        e.commit_block(format!("b{csn_line}").as_bytes(), &ops)?;
        csn_line += 1;
    }

    // (a) memtable-only probes (all data still in the BTreeMap)
    let mem_us = time_probes(&e, docs, probes)?;

    // (b) publish one tile covering everything, then probe the tiles only
    e.publish_tile(dir.join("tile-1.tile"), csn_line)?;
    let tile_bytes = std::fs::metadata(dir.join("tile-1.tile"))?.len();
    let tile_us = time_probes(&e, docs, probes)?;

    if measure_scan {
        let scan_mib_s = time_scan(&e, tile_bytes)?;
        println!(
            "docs={docs} tile_bytes={tile_bytes} memtable_probe_us={mem_us:.1} tile_probe_us={tile_us:.1} \
             scan_mib_s={scan_mib_s:.0}"
        );
    } else {
        println!(
            "docs={docs} tile_bytes={tile_bytes} memtable_probe_us={mem_us:.1} tile_probe_us={tile_us:.1} \
             slowdown={:.0}x",
            tile_us / mem_us.max(0.01)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn time_probes(e: &Engine, docs: u64, probes: usize) -> std::io::Result<f64> {
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut total = 0f64;
    for _ in 0..probes {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let h = 1 + (state >> 33) % docs;
        let value = format!("u{h}");
        let t = Instant::now();
        let got = e.unique_lookup(b"P", b"no", value.as_bytes())?;
        total += t.elapsed().as_secs_f64() * 1e6;
        assert_eq!(got, Some(h));
    }
    Ok(total / probes as f64)
}

/// Best-of-`SCAN_ITERS` throughput of a full sequential pass over every tile
/// block, expressed as tile bytes / elapsed time (so it counts blocks read, not
/// only live payload).
fn time_scan(e: &Engine, tile_bytes: u64) -> std::io::Result<f64> {
    let mut best = 0f64;
    for _ in 0..SCAN_ITERS {
        let mut entries = 0u64;
        let t = Instant::now();
        e.scan_all(|key, value| {
            entries += 1;
            std::hint::black_box((key.len(), value.len()));
            Ok(())
        })?;
        let secs = t.elapsed().as_secs_f64();
        assert!(entries > 0);
        best = best.max((tile_bytes as f64 / 1_048_576.0) / secs.max(1e-9));
    }
    Ok(best)
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let scan = args.first().is_some_and(|a| a == "scan");
    let rest = if scan { &args[1..] } else { &args[..] };
    let docs: Vec<u64> = if rest.is_empty() {
        vec![5_000, 20_000, 80_000, 320_000]
    } else {
        rest.iter().map(|a| a.parse().unwrap()).collect()
    };
    for d in docs {
        run(d, 200, scan)?;
    }
    Ok(())
}
