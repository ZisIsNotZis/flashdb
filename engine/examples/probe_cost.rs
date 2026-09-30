//! Micro-benchmark: cost of one unique probe against (a) the in-RAM memtable and
//! (b) a published tile, as a function of tile size. Not a correctness test.
//!
//! usage: cargo run --release --example probe_cost -- [scan|crc] [DOCS ...]
//!        cargo run --release --example probe_cost -- tiles [TILES ...]
//!        cargo run --release --example probe_cost -- spread [TILES ...]
//!
//! `scan` additionally reports the full sequential tile scan throughput (MiB/s);
//! without it only the probe cost is printed. `crc` reports one-shot CRC-32C
//! throughput (GB/s) on a buffer sized like a tile (default 64 MiB, optional
//! second argument overrides it in bytes). `tiles` builds N independent,
//! disjoint-range tiles (each `DOCS_PER_TILE` docs) and reports µs per probe for
//! an existing value and for a not-yet-stored value above every tile (the append
//! case): the tile-count axis of the probe cost, i.e. exactly what the key-range
//! fast reject targets. `spread` builds the same N tiles but spreads the stored
//! unique values across the whole keyspace so an absent probe falls INSIDE every
//! tile's range (the case the key-range fast reject cannot help) and reports µs
//! per abort probe: the tile-count axis the per-tile bloom (08d-2b) targets. Run
//! `spread` at both this revision and its pre-bloom parent to get the
//! with/without-bloom comparison.

use flashdb_engine::engine::{Engine, Op, Outcome};
use std::time::Instant;

const SCAN_ITERS: usize = 3;
const DOCS_PER_TILE: u64 = 5_000;
const TILE_PROBES: usize = 200;

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

/// Probe cost against exactly `tiles` disjoint-range published tiles: one random
/// existing value (probes fall inside every tile's range, so the filter can only
/// reject the tiles below it) and one not-yet-stored value above every tile (the
/// append case, where the filter rejects all existing tiles in RAM).
fn run_multi_tile(tiles: usize, docs_per_tile: u64, probes: usize) -> std::io::Result<()> {
    let dir = std::env::temp_dir().join(format!("flashdb-probe-cost-{}-t{tiles}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    let payload = vec![b'x'; 200];
    let mut docs = 0u64;
    for tile in 0..tiles {
        let start = tile as u64 * docs_per_tile + 1;
        let end = start + docs_per_tile - 1;
        let mut ops = Vec::with_capacity((docs_per_tile * 2) as usize);
        for h in start..=end {
            ops.push(Op::PutDoc { entity: b"P".to_vec(), handle: h, doc: payload.clone() });
            ops.push(Op::PutUnique {
                entity: b"P".to_vec(),
                field: b"no".to_vec(),
                value: format!("u{h}").into_bytes(),
                handle: h,
            });
        }
        let cutoff = match e.commit_block(format!("tile-block-{tile}").as_bytes(), &ops)? {
            Outcome::Committed { csn } => csn,
            other => return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("unexpected outcome {other:?}"))),
        };
        e.publish_tile(dir.join(format!("tile-{tile}.tile")), cutoff)?;
        docs = end;
    }
    // Two probe shapes: an existing value (random tile, so probes fall inside
    // every tile's range and the filter can only reject the tiles *below* it),
    // and a not-yet-stored value above every tile (the append case, where the
    // filter rejects all existing tiles in RAM).
    let existing_us = time_probes(&e, docs, probes)?;
    let append_us = time_lookup(&e, format!("u{}", docs + 1).as_bytes(), probes)?;
    println!("tiles={tiles} docs={docs} probe_existing_us={existing_us:.2} probe_append_us={append_us:.2}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Probe cost when the stored unique values are SPREAD across the whole keyspace,
/// so an absent probe falls INSIDE every tile's key range: the key-range fast
/// reject cannot drop any tile, and with no bloom every probe pays one clipped
/// cursor (directory block + data block + 4 KiB buffer) per active tile. With the
/// per-tile bloom (08d-2b) an absent prefix is rejected in RAM for every tile.
/// Each tile stores even slots `s<slot:06>-v<tile>`; a probe uses an odd slot, so
/// it is absent everywhere yet sorts between every tile's first and last unique
/// value. Compare against the same mode run at the pre-bloom parent revision.
fn run_spread(tiles: usize, docs_per_tile: u64, probes: usize) -> std::io::Result<()> {
    let dir = std::env::temp_dir().join(format!("flashdb-probe-cost-spread-{}-t{tiles}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    let payload = vec![b'x'; 200];
    let mut handles = 0u64;
    for tile in 0..tiles {
        let mut ops = Vec::with_capacity((docs_per_tile * 2) as usize);
        for slot in 0..docs_per_tile {
            handles += 1;
            ops.push(Op::PutDoc { entity: b"P".to_vec(), handle: handles, doc: payload.clone() });
            ops.push(Op::PutUnique {
                entity: b"P".to_vec(),
                field: b"no".to_vec(),
                value: format!("s{:06}-v{tile}", slot * 2).into_bytes(),
                handle: handles,
            });
        }
        let cutoff = match e.commit_block(format!("spread-block-{tile}").as_bytes(), &ops)? {
            Outcome::Committed { csn } => csn,
            other => return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("unexpected outcome {other:?}"))),
        };
        e.publish_tile(dir.join(format!("tile-{tile}.tile")), cutoff)?;
    }
    // Absent, in-range probes: odd slots with each tile's suffix sort inside every
    // tile's range and are stored by no tile.
    let mut total = 0f64;
    for p in 0..probes {
        let slot = (p as u64 % 64) * 2 + 1;
        let tile = (p * 7) % tiles;
        let value = format!("s{:06}-v{tile}", slot);
        let t = Instant::now();
        let got = e.unique_lookup(b"P", b"no", value.as_bytes())?;
        total += t.elapsed().as_secs_f64() * 1e6;
        debug_assert!(got.is_none(), "spread probes are absent by construction");
    }
    let us = total / probes.max(1) as f64;
    println!("mode=spread tiles={tiles} docs_per_tile={docs_per_tile} probe_absent_us={us:.2}");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Average µs of `unique_lookup` over `probes` calls for one fixed value; the
/// result is not asserted, so an absent value is allowed.
fn time_lookup(e: &Engine, value: &[u8], probes: usize) -> std::io::Result<f64> {
    let mut total = 0f64;
    for _ in 0..probes {
        let t = Instant::now();
        let got = e.unique_lookup(b"P", b"no", value)?;
        total += t.elapsed().as_secs_f64() * 1e6;
        std::hint::black_box(got);
    }
    Ok(total / probes as f64)
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

/// Best-of-`SCAN_ITERS` one-shot CRC-32C throughput over a large buffer, in GB/s.
fn time_crc(bytes: usize) -> f64 {
    let data: Vec<u8> = (0..bytes).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let mut best = 0f64;
    for _ in 0..SCAN_ITERS {
        let t = Instant::now();
        let sum = flashdb_engine::crc::crc32c(&data);
        let secs = t.elapsed().as_secs_f64();
        std::hint::black_box(sum);
        best = best.max((bytes as f64 / 1e9) / secs.max(1e-9));
    }
    best
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "crc") {
        let bytes = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(64 << 20);
        println!("crc_bytes={bytes} crc_gb_s={:.2}", time_crc(bytes));
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "tiles") {
        let counts: Vec<usize> = if args.len() > 1 {
            args[1..].iter().map(|a| a.parse().unwrap()).collect()
        } else {
            vec![1, 4, 12]
        };
        for tiles in counts {
            run_multi_tile(tiles, DOCS_PER_TILE, TILE_PROBES)?;
        }
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "spread") {
        let counts: Vec<usize> = if args.len() > 1 {
            args[1..].iter().map(|a| a.parse().unwrap()).collect()
        } else {
            vec![1, 4, 12]
        };
        for tiles in counts {
            run_spread(tiles, DOCS_PER_TILE, TILE_PROBES)?;
        }
        return Ok(());
    }
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
