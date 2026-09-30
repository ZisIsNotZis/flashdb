//! Micro-benchmark: cost of one unique probe against (a) the in-RAM memtable and
//! (b) a published tile, as a function of tile size. Not a correctness test.
//!
//! usage: cargo run --release --example probe_cost -- [DOCS ...]

use flashdb_engine::engine::{Engine, Op};
use std::time::Instant;

fn run(docs: u64, probes: usize) -> std::io::Result<()> {
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

    println!(
        "docs={docs} tile_bytes={tile_bytes} memtable_probe_us={mem_us:.1} tile_probe_us={tile_us:.1} \
         slowdown={:.0}x",
        tile_us / mem_us.max(0.01)
    );
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

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let docs: Vec<u64> = if args.is_empty() {
        vec![5_000, 20_000, 80_000, 320_000]
    } else {
        args.iter().map(|a| a.parse().unwrap()).collect()
    };
    for d in docs {
        run(d, 200)?;
    }
    Ok(())
}
