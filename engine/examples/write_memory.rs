//! Slice 10a — MEASUREMENT ONLY. Quantify where write-path memory goes so the
//! 10b fix targets measured bytes.
//!
//! This file adds no engine behavior. Every number is read from this process's
//! `/proc/self/status` (`VmRSS`/`VmHWM`) or from its own cgroup, and every ratio
//! is computed here and printed as `key=value`, so the report quotes the binary
//! instead of re-deriving numbers.
//!
//! usage: cargo run --release --example write_memory -- <mode> [args]
//!   memtable   <value_bytes> <total_bytes>   exp1: RAM bytes per payload byte,
//!                                            blocks of 1000 PutDocs, no tiles,
//!                                            no rotation
//!   window     <w_mb> <cycles>               exp2: peak RSS vs publish/rotate
//!                                            window (maybe_checkpoint/rotate_wal)
//!   breakdown  <w_mb>                        exp3: per-contributor RSS at one
//!                                            window (replay / expected memtable /
//!                                            newest projection / live memtable)
//!   compaction <w_mb> <tiles>                exp4: compaction peak vs tile bytes
//!
//! One process per invocation: `VmHWM` is monotonic, and `clear_refs` value 5
//! resets it to the current RSS so each phase's peak is measured from a clean
//! floor. `malloc_trim(0)` returns freed arena memory before a baseline is read.
//!
//! Workload is PutDoc-only (one doc per op) so the engine's per-op uniqueness
//! scan never runs: the measured bytes are memtable + verification state, not
//! the O(n) unique probe.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::keys;
use flashdb_engine::memtable::Memtable;
use flashdb_engine::wal;

const ENTITY: &[u8] = b"E";
const BLOCK_DOCS: u64 = 1000;
/// Doc size for window/breakdown/compaction: matches the 1 KiB scaled run.
const DOC_V: usize = 1024;

fn emit(line: &str) {
    println!("{line}");
}

/// One numeric field from `/proc/self/status` (`VmRSS`, `VmHWM`, ...), in KiB.
fn status_field(name: &str) -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else { return 0 };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(name) {
            if let Some(rest) = rest.strip_prefix(':') {
                return rest.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

fn rss_kb() -> u64 { status_field("VmRSS") }
fn hwm_kb() -> u64 { status_field("VmHWM") }

/// Reset `VmHWM` to the current RSS (Linux `clear_refs` value 5), so a phase's
/// peak starts from the memory already held when the phase begins.
fn reset_hwm() {
    let _ = fs::write("/proc/self/clear_refs", "5");
}

/// glibc `malloc_trim(0)`: return free main-arena memory to the OS so a later
/// baseline is the live set, not a retained arena. No-op off glibc/Linux.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn malloc_trim() {
    extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn malloc_trim() {}

/// This process's cgroup v2 directory, if it is namespaced under a cgroup.
fn cgroup_dir() -> Option<PathBuf> {
    let text = fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = text.lines().last()?.splitn(3, ':').nth(2)?;
    if path.is_empty() {
        return None;
    }
    Some(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn fresh_dir(tag: &str) -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("flashdb-write-memory-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Sum of file lengths with the given extension in `dir` (0 when none).
fn ext_bytes(dir: &Path, ext: &str) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            total += fs::metadata(&path)?.len();
        }
    }
    Ok(total)
}

/// A doc body of exactly `n` bytes (engine rejects empty docs; content is inert).
fn doc(n: usize) -> Vec<u8> {
    vec![b'x'; n]
}

/// Commit `docs` PutDocs in blocks of [`BLOCK_DOCS`], handles allocated from
/// `first_handle`. Returns the next unused handle and the number of blocks.
fn commit_docs(
    e: &mut Engine,
    first_handle: u64,
    docs: u64,
    value_bytes: usize,
    block_base: &str,
) -> io::Result<(u64, u64)> {
    let mut handle = first_handle;
    let mut remaining = docs;
    let mut block = 0u64;
    while remaining > 0 {
        let n = remaining.min(BLOCK_DOCS);
        let mut ops = Vec::with_capacity(n as usize);
        for _ in 0..n {
            ops.push(Op::PutDoc { entity: ENTITY.to_vec(), handle, doc: doc(value_bytes) });
            handle += 1;
        }
        e.commit_block(format!("{block_base}-{block}").as_bytes(), &ops)?;
        remaining -= n;
        block += 1;
    }
    Ok((handle, block))
}

/// Total bytes on disk that make up the engine directory (manifest included).
fn dir_bytes(dir: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let meta = entry?.metadata()?;
        if meta.is_file() {
            total += meta.len();
        }
    }
    Ok(total)
}

// ---------- exp 1: memtable bytes per payload byte ----------

fn run_memtable(value_bytes: usize, total_bytes: u64) -> io::Result<()> {
    let dir = fresh_dir("memtable")?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    malloc_trim();
    let base = rss_kb();
    reset_hwm();
    let docs = total_bytes / value_bytes as u64;
    let first = e.next_handle()?;
    let (_, blocks) = commit_docs(&mut e, first, docs, value_bytes, "m")?;
    let hwm = hwm_kb();
    let payload = docs * value_bytes as u64;
    let wal_bytes = ext_bytes(&dir, "wal")?;
    let delta = hwm.saturating_sub(base);
    emit(&format!(
        "mode=memtable value_bytes={value_bytes} docs={docs} blocks={blocks} payload_bytes={payload} \
         wal_bytes={wal_bytes} memtable_entries={} ram_base_kb={base} ram_hwm_kb={hwm} ram_delta_kb={delta} \
         factor={:.4} factor_wal={:.4}",
        e.serving_memtable_entries(),
        delta as f64 * 1024.0 / payload as f64,
        delta as f64 * 1024.0 / wal_bytes as f64,
    ));
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

// ---------- exp 2: working set vs window size ----------

fn run_window(w_mb: u64, cycles: u64) -> io::Result<()> {
    let w = w_mb << 20;
    let dir = fresh_dir("window")?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    malloc_trim();
    let base = rss_kb();
    reset_hwm();
    // Below the window a checkpoint must not fire; cap the loop so a wrong
    // threshold can never hang the measurement.
    let blocks_per_window = (w / (BLOCK_DOCS * DOC_V as u64)).max(1);
    let cap = cycles * (blocks_per_window * 3 + 10) + 100;
    let mut handle = e.next_handle()?;
    let mut blocks = 0u64;
    let mut payload = 0u64;
    let mut sampled_peak = base;
    let mut done = 0u64;
    let started = Instant::now();
    while done < cycles {
        loop {
            if blocks > cap {
                return Err(io::Error::new(io::ErrorKind::Other, "window loop cap hit: checkpoint threshold never crossed"));
            }
            let mut ops = Vec::with_capacity(BLOCK_DOCS as usize);
            for _ in 0..BLOCK_DOCS {
                ops.push(Op::PutDoc { entity: ENTITY.to_vec(), handle, doc: doc(DOC_V) });
                handle += 1;
            }
            e.commit_block(format!("w{blocks}").as_bytes(), &ops)?;
            blocks += 1;
            payload += BLOCK_DOCS * DOC_V as u64;
            sampled_peak = sampled_peak.max(rss_kb());
            if e.maybe_checkpoint(w)? {
                break;
            }
        }
        if !e.rotate_wal(w)? {
            return Err(io::Error::new(io::ErrorKind::Other, "rotate_wal refused right after a checkpoint"));
        }
        done += 1;
        sampled_peak = sampled_peak.max(rss_kb());
    }
    let hwm = hwm_kb();
    let wal_bytes = ext_bytes(&dir, "wal")?;
    let tile_bytes = ext_bytes(&dir, "tile")?;
    let disk_bytes = dir_bytes(&dir)?;
    let delta = hwm.saturating_sub(base);
    emit(&format!(
        "mode=window window_mb={w_mb} cycles={done} blocks={blocks} payload_bytes={payload} \
         tile_bytes={tile_bytes} wal_bytes={wal_bytes} disk_bytes={disk_bytes} \
         ram_base_kb={base} ram_peak_sampled_kb={sampled_peak} ram_hwm_kb={hwm} ram_delta_kb={delta} \
         delta_over_window={:.3} delta_over_payload={:.4} delta_over_disk={:.4} elapsed_s={:.3}",
        delta as f64 * 1024.0 / w as f64,
        delta as f64 * 1024.0 / payload as f64,
        delta as f64 * 1024.0 / disk_bytes.max(1) as f64,
        started.elapsed().as_secs_f64(),
    ));
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

// ---------- exp 3: where the peak comes from ----------

/// `newest_projection` is private; replicate it exactly over the public merged
/// scan (`Engine::scan_all`, which with no published tiles yields precisely the
/// live memtable entries in key order). Same algorithm: keep the first CSN-desc
/// version per logical key (full key minus its trailing 8-byte `~csn`) within
/// `(lower, cutoff]`. Faithful because it consumes the same ordered, same-filtered
/// entry stream that `Memtable::entries()` feeds the original. Streaming one entry
/// at a time (not collected) so the measurement does not add a hidden copy.
fn reduce_step(
    out: &mut Memtable,
    last_logical: &mut Option<Vec<u8>>,
    key: &[u8],
    value: &[u8],
    lower: u64,
    cutoff: u64,
) {
    let Some(csn) = keys::key_csn(key) else { return };
    if csn <= lower || csn > cutoff {
        return;
    }
    let logical = &key[..key.len() - 8];
    if last_logical.as_deref() == Some(logical) {
        return;
    }
    *last_logical = Some(logical.to_vec());
    out.apply(key.to_vec(), value.to_vec());
}

fn run_breakdown(w_mb: u64) -> io::Result<()> {
    let w = w_mb << 20;
    let dir = fresh_dir("breakdown")?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    malloc_trim();
    let rss0 = rss_kb();
    let docs = w / DOC_V as u64;
    let first = e.next_handle()?;
    let (next_handle, _) = commit_docs(&mut e, first, docs, DOC_V, "b")?;
    malloc_trim();
    let rss_live = rss_kb();
    let live_entries = e.serving_memtable_entries();
    let wal_path = dir.join("data.wal");

    // (d) live memtable itself for one window: RSS the live engine added.
    emit(&format!(
        "mode=breakdown step=live_memtable w_mb={w_mb} entries={live_entries} rss_before_kb={rss0} rss_after_kb={rss_live} delta_kb={}",
        rss_live.saturating_sub(rss0),
    ));

    // (c) newest projection of the live memtable alone.
    let cutoff = e.csn();
    let mut proj = Memtable::new();
    let mut last_logical: Option<Vec<u8>> = None;
    e.scan_all(|key, value| {
        reduce_step(&mut proj, &mut last_logical, key, value, 0, cutoff);
        Ok(())
    })?;
    let rss_proj = rss_kb();
    emit(&format!(
        "mode=breakdown step=newest_projection entries={} rss_before_kb={rss_live} rss_after_kb={rss_proj} delta_kb={}",
        proj.len(),
        rss_proj.saturating_sub(rss_live),
    ));
    drop(proj);
    malloc_trim();
    let rss_after_proj = rss_kb();

    // (a) wal::replay of the suffix alone.
    let (records, valid_len) = wal::replay(&wal_path)?;
    let records_bytes: u64 = records.iter().map(|r| r.len() as u64).sum();
    let rss_records = rss_kb();
    emit(&format!(
        "mode=breakdown step=wal_replay records={} records_bytes={records_bytes} wal_valid_len={valid_len} \
         rss_before_kb={rss_after_proj} rss_after_kb={rss_records} delta_kb={}",
        records.len(),
        rss_records.saturating_sub(rss_after_proj),
    ));

    // (b) expected Memtable built from those records, while they are held (the
    // write_verified_tile shape). The workload is PutDoc-only, so applying the
    // same primary key/value each record's single op would apply is byte-identical
    // to decode_payload + apply_op; no private codec is duplicated.
    let mut expected = Memtable::new();
    for handle in first..next_handle {
        let csn = (handle - first) / BLOCK_DOCS + 1;
        expected.apply(keys::primary_key(ENTITY, handle, csn).unwrap(), doc(DOC_V));
    }
    let rss_built = rss_kb();
    let built_pairs: u64 = expected.scan_iter(b"").map(|(k, v)| (k.len() + v.len()) as u64).sum();
    emit(&format!(
        "mode=breakdown step=expected_memtable entries={} key_value_bytes={built_pairs} \
         rss_before_kb={rss_records} rss_after_kb={rss_built} delta_kb={}",
        expected.len(),
        rss_built.saturating_sub(rss_records),
    ));

    let hwm = hwm_kb();
    emit(&format!(
        "mode=breakdown peak step=all_live rss_hwm_kb={hwm} rss_base_kb={rss_live} peak_delta_kb={}",
        hwm.saturating_sub(rss_live),
    ));
    drop(expected);
    drop(records);
    malloc_trim();
    let rss_end = rss_kb();
    emit(&format!("mode=breakdown step=freed_all rss_end_kb={rss_end} rss_live_kb={rss_live}"));
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

// ---------- exp 4: compaction peak vs tile bytes ----------

fn run_compaction(w_mb: u64, tiles: u64) -> io::Result<()> {
    let w = w_mb << 20;
    let dir = fresh_dir("compaction")?;
    let mut e = Engine::create(dir.join("data.wal"))?;
    let mut handle = e.next_handle()?;
    let mut blocks = 0u64;
    let blocks_per_window = (w / (BLOCK_DOCS * DOC_V as u64)).max(1);
    let cap = tiles * (blocks_per_window * 3 + 10) + 100;
    for _ in 0..tiles {
        loop {
            if blocks > cap {
                return Err(io::Error::new(io::ErrorKind::Other, "tile loop cap hit: checkpoint threshold never crossed"));
            }
            let mut ops = Vec::with_capacity(BLOCK_DOCS as usize);
            for _ in 0..BLOCK_DOCS {
                ops.push(Op::PutDoc { entity: ENTITY.to_vec(), handle, doc: doc(DOC_V) });
                handle += 1;
            }
            e.commit_block(format!("c{blocks}").as_bytes(), &ops)?;
            blocks += 1;
            if e.maybe_checkpoint(w)? {
                break;
            }
        }
        if !e.rotate_wal(w)? {
            return Err(io::Error::new(io::ErrorKind::Other, "rotate_wal refused right after a checkpoint"));
        }
    }
    malloc_trim();
    let base = rss_kb();
    reset_hwm();
    let tile_bytes = ext_bytes(&dir, "tile")?;
    let wal_bytes = ext_bytes(&dir, "wal")?;
    let started = Instant::now();
    e.compact()?;
    let elapsed = started.elapsed().as_secs_f64();
    let rss_post = rss_kb();
    let hwm = hwm_kb();
    let tile_bytes_after = ext_bytes(&dir, "tile")?;
    let delta = hwm.saturating_sub(base);
    emit(&format!(
        "mode=compaction window_mb={w_mb} tiles={tiles} tile_bytes_before={tile_bytes} tile_bytes_after={tile_bytes_after} \
         wal_bytes={wal_bytes} ram_base_kb={base} ram_post_kb={rss_post} ram_hwm_kb={hwm} peak_delta_kb={delta} \
         peak_over_tile_bytes={:.3} elapsed_s={elapsed:.3}",
        delta as f64 * 1024.0 / tile_bytes.max(1) as f64,
    ));
    let _ = fs::remove_dir_all(&dir);
    Ok(())
}

// ---------- main ----------

const USAGE: &str = "usage: write_memory memtable <value_bytes> <total_bytes>\n\
                     usage: write_memory window <w_mb> <cycles>\n\
                     usage: write_memory breakdown <w_mb>\n\
                     usage: write_memory compaction <w_mb> <tiles>";

fn parse(s: Option<&String>, name: &str) -> io::Result<u64> {
    s.and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("missing/invalid {name}")))
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // Report the cgroup the process runs in: memory.current/peak are readable on
    // this host, but outside a dedicated scope they cover the whole terminal
    // scope (page cache included), so the process VmRSS/VmHWM are the attributable
    // numbers and the cgroup figures are context only.
    let path = cgroup_dir();
    emit(&format!(
        "mode=cgroup path={} memory_current_bytes={:?} memory_peak_bytes={:?} memory_max={:?}",
        path.as_deref().map(|p| p.display().to_string()).unwrap_or_else(|| "unavailable".into()),
        path.as_deref().and_then(|p| read_u64(&p.join("memory.current"))),
        path.as_deref().and_then(|p| read_u64(&p.join("memory.peak"))),
        path.as_deref().and_then(|p| fs::read_to_string(p.join("memory.max")).ok()).map(|s| s.trim().to_string()),
    ));
    match args.get(1).map(String::as_str) {
        Some("memtable") => run_memtable(parse(args.get(2), "value_bytes")? as usize, parse(args.get(3), "total_bytes")?),
        Some("window") => run_window(parse(args.get(2), "w_mb")?, parse(args.get(3), "cycles")?),
        Some("breakdown") => run_breakdown(parse(args.get(2), "w_mb")?),
        Some("compaction") => run_compaction(parse(args.get(2), "w_mb")?, parse(args.get(3), "tiles")?),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
