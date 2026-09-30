//! Scale benchmark driver for the flashdb v0 engine (seed / verify / scanbench).
//!
//! All progress is emitted as machine-readable `key=value` lines on stdout so a
//! supervisor can monitor a run. Failures print `phase=<cmd> ok=false
//! error=<message>` (plus the error on stderr) and exit nonzero. The binary is
//! honest instrumentation: every number is measured from the engine or the
//! process, never extrapolated progress.
//!
//! Data model: two entities with one declared unique field each, written via
//! raw engine Ops (PutDoc + PutUnique):
//!   EntityA: unique field `ua`, value `a<i>`, doc index i in [0, docs_a)
//!   EntityB: unique field `ub`, value `b<i>`, doc index i in [0, docs_b)
//! The deterministic std-only doc stream (SplitMix64) splits total_docs evenly:
//! global doc g with g < docs_a is EntityA doc g, otherwise EntityB doc
//! g - docs_a. Handles are allocated sequentially in stream order on a fresh
//! database (first handle 1), so global doc g has handle g + 1; `seed` asserts
//! the engine's handle watermark stays in lockstep with that allocation.
//!
//! Durability cadence per committed block: `maybe_checkpoint(64 MiB)` then
//! `rotate_wal(64 MiB)`. `rotate_wal` only fires once every committed block is
//! published as a tile (newest tile cutoff == csn, empty memtable) AND the
//! active WAL file exceeds the threshold — exactly what a just-fired checkpoint
//! provides.
//!
//! Compaction is bounded and policy-driven, not "merge everything every N
//! checkpoints". After each successful checkpoint the driver calls
//! `Engine::maybe_compact(COMPACT_MERGE_BUDGET, MAX_ACTIVE_TILES)`: the trigger
//! is the engine's real active-tile cap (`MAX_ACTIVE_TILES`, the manifest-page
//! bound — not a hard-coded 8/12) and the driver asks `maybe_compact` to cap
//! each merge at eight 64 MiB rotation windows of tile bytes. Whether a merge
//! actually stays bounded — and therefore whether write amplification stays
//! flat as the dataset grows — is exactly what the accounting below measures:
//! `tile_bytes_compact` against `payload_bytes`. `publish_tile` still refuses
//! once the active list is full, and compaction happens right after the
//! checkpoint that reached the cap, before the next publish, so it never blocks
//! a seed.
//!
//! `seed` reports measured, never extrapolated, accounting on every progress
//! line and at the end:
//!   `payload_bytes`        document bodies + unique-key values handed to commit_block
//!   `wal_written_bytes`    WAL bytes appended (sum of every rotated segment + live segment)
//!   `tile_bytes_publish`   bytes of ordinary `tile-<csn>.tile` files as they appear
//!   `tile_bytes_compact`   bytes of `compact-<cutoff>.tile` outputs (maybe_compact's result)
//!   `tile_bytes_total`     publish + compact
//!   `compactions`          maybe_compact calls that actually merged a run
//!   `write_amplification`  (wal_written_bytes + tile_bytes_total) / payload_bytes
//!   `docs_s_interval`      throughput since the previous progress line
//!   `docs_s_cumulative`    throughput since seed started

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::Instant;

use flashdb_engine::engine::{Engine, Op, Outcome, MAX_ACTIVE_TILES};
use serde::{Deserialize, Serialize};

const ENTITY_A: &[u8] = b"EntityA";
const ENTITY_B: &[u8] = b"EntityB";
const FIELD_A: &[u8] = b"ua";
const FIELD_B: &[u8] = b"ub";
const CHECKPOINT_BYTES: u64 = 64 << 20;
const ROTATE_BYTES: u64 = 64 << 20;
/// Compaction merge budget as a multiple of the 64 MiB rotation window: 8 * 64
/// MiB = 512 MiB of tile bytes. This is the size cap the driver asks
/// `maybe_compact` to respect, so one merge is meant to rewrite only the oldest
/// run under that cap rather than the whole dataset. The `seed` accounting is
/// the check on that intent, not an assumption of it.
const COMPACT_MERGE_WINDOWS: u64 = 8;
const COMPACT_MERGE_BUDGET: u64 = COMPACT_MERGE_WINDOWS * ROTATE_BYTES;
/// unique_lookup probes per entity in `verify`. Deliberately small: each probe
/// is a full prefix scan over tiles + memtable (O(data)).
const PROBES_PER_ENTITY: u64 = 20;
const SCAN_PASSES: u64 = 3;
const META_NAME: &str = "scale-meta.json";
const META_FORMAT: &str = "flashdb-scale-meta-1";

/// Written by `seed` next to the engine directory contents so `verify` and
/// `scanbench` can assert the exact seeded shape (block count, per-entity doc
/// counts). The engine ignores unreferenced files in its directory.
#[derive(Serialize, Deserialize)]
struct Meta {
    format: String,
    total_docs: u64,
    docs_per_commit: u64,
    value_bytes: u64,
    docs_a: u64,
    docs_b: u64,
}

// ---------- process plumbing ----------

fn emit(line: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

/// Current resident set in KiB from /proc/self/status; 0 when unavailable
/// (non-Linux). Sampled, not a guaranteed peak — hence the separate peak
/// tracking across sample points.
fn rss_kb() -> u64 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else { return 0 };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest.trim().trim_end_matches("kB").trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Sum of `*.wal` file sizes in the engine directory (the live WAL plus any
/// not-yet-unlinked segment): the honest "WAL bytes on disk" figure.
fn wal_bytes(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) == Some("wal") {
            total += fs::metadata(&path)?.len();
        }
    }
    Ok(total)
}

/// Sum the on-disk size of every ordinary `tile-<csn>.tile` file not yet
/// counted in `seen`, adding the newly appeared bytes to `total`. Compaction
/// outputs (`compact-*.tile`) are excluded here: their size is `maybe_compact`'s
/// return value, so they are counted exactly once and separately. Counting a
/// file the first time it appears makes the total cumulative even though
/// compaction later unlinks superseded ordinary tiles.
fn count_published_tiles(
    dir: &Path,
    seen: &mut HashMap<String, u64>,
    total: &mut u64,
) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.starts_with("tile-") || path.extension().and_then(|e| e.to_str()) != Some("tile") {
            continue;
        }
        if seen.contains_key(name) {
            continue;
        }
        let len = fs::metadata(&path)?.len();
        seen.insert(name.to_string(), len);
        *total += len;
    }
    Ok(())
}

fn parse_num(text: &str, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    text.parse::<u64>()
        .map_err(|_| format!("invalid {name}: {text:?}").into())
}

const USAGE: &str = "usage: flashdb-scale seed <dir> <total_docs> <docs_per_commit> <value_bytes>\n\
                     usage: flashdb-scale verify <dir> <total_docs>\n\
                     usage: flashdb-scale scanbench <dir> <passes>";

fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    match args.get(1).map(String::as_str) {
        Some("seed") if args.len() == 6 => cmd_seed(
            Path::new(&args[2]),
            parse_num(&args[3], "total_docs")?,
            parse_num(&args[4], "docs_per_commit")?,
            parse_num(&args[5], "value_bytes")?,
        ),
        Some("verify") if args.len() == 4 => {
            cmd_verify(Path::new(&args[2]), parse_num(&args[3], "total_docs")?)
        }
        Some("scanbench") if args.len() == 4 => {
            cmd_scanbench(Path::new(&args[2]), parse_num(&args[3], "passes")?)
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn fail(phase: &str, e: Box<dyn std::error::Error>) -> ! {
    emit(&format!("phase={phase} ok=false error={e}"));
    eprintln!("{phase} failed: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Err(e) = run(&args) {
        fail(args.get(1).map(String::as_str).unwrap_or("unknown"), e);
    }
}

// ---------- deterministic doc stream ----------

/// SplitMix64 (std-only): mixed output stream from a per-doc seed.
struct SplitMix64(u64);

impl SplitMix64 {
    fn new(tag: u8, index: u64) -> Self {
        SplitMix64(
            0x9E37_79B9_7F4A_7C15
                ^ u64::from(tag).wrapping_mul(0xA076_1D64_78BD_642F)
                ^ index.wrapping_mul(0xE703_7ED1_A0B4_28DB),
        )
    }
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// One document body: `{"e":"A","i":<i>,"r":"<hex pad>"}` sized to exactly
/// `value_bytes` (grows instead of truncating only below the JSON overhead).
/// The pad is deterministic pseudo-random hex from SplitMix64 keyed by
/// (entity tag, doc index), so every run seeds byte-identical content.
fn doc_bytes(tag: u8, index: u64, value_bytes: u64) -> Vec<u8> {
    let head = format!("{{\"e\":\"{}\",\"i\":{index},\"r\":\"", tag as char);
    const TAIL: &str = "\"}";
    let target = usize::try_from(value_bytes).unwrap_or(usize::MAX);
    let pad = target.saturating_sub(head.len() + TAIL.len()).max(2);
    let mut rng = SplitMix64::new(tag, index);
    let mut doc = Vec::with_capacity(head.len() + pad + TAIL.len());
    doc.extend_from_slice(head.as_bytes());
    while doc.len() < head.len() + pad {
        for byte in rng.next().to_le_bytes() {
            doc.push(HEX[(byte >> 4) as usize]);
            doc.push(HEX[(byte & 0xF) as usize]);
        }
    }
    doc.truncate(head.len() + pad);
    doc.extend_from_slice(TAIL.as_bytes());
    doc
}

// ---------- seed ----------

fn cmd_seed(
    dir: &Path,
    total_docs: u64,
    docs_per_commit: u64,
    value_bytes: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    if docs_per_commit == 0 {
        return Err("docs_per_commit must be >= 1".into());
    }
    let wal = dir.join("data.wal");
    if wal.exists() {
        // Wal::create never truncates; a rerun without removing the directory
        // must fail loudly rather than silently diverge from the doc stream.
        return Err(format!("refusing to overwrite existing {}", wal.display()).into());
    }
    fs::create_dir_all(dir)?;
    let mut e = Engine::create(&wal)?;
    let docs_a = total_docs / 2;
    let docs_b = total_docs - docs_a;
    let total_blocks = total_docs.div_ceil(docs_per_commit);
    let interval = (total_blocks / 10).clamp(1, 50);
    let mut next_handle = e.next_handle()?;
    let mut rss_peak = rss_kb();
    let started = Instant::now();
    // Cumulative, measured accounting (definitions in the module header).
    let mut payload_bytes: u64 = 0;
    let mut wal_written: u64 = 0;
    let mut tile_bytes_publish: u64 = 0;
    let mut tile_bytes_compact: u64 = 0;
    let mut compactions: u64 = 0;
    let mut seen_publish_tiles: HashMap<String, u64> = HashMap::new();
    let mut last_progress = started;
    let mut last_progress_docs: u64 = 0;

    for block in 0..total_blocks {
        let begin = block * docs_per_commit;
        let end = (begin + docs_per_commit).min(total_docs);
        let mut ops = Vec::with_capacity(((end - begin) * 2) as usize);
        for g in begin..end {
            let handle = next_handle;
            next_handle += 1;
            if g < docs_a {
                let i = g;
                let doc = doc_bytes(b'A', i, value_bytes);
                let value = format!("a{i}").into_bytes();
                payload_bytes += (doc.len() + value.len()) as u64;
                ops.push(Op::PutDoc { entity: ENTITY_A.to_vec(), handle, doc });
                ops.push(Op::PutUnique {
                    entity: ENTITY_A.to_vec(),
                    field: FIELD_A.to_vec(),
                    value,
                    handle,
                });
            } else {
                let i = g - docs_a;
                let doc = doc_bytes(b'B', i, value_bytes);
                let value = format!("b{i}").into_bytes();
                payload_bytes += (doc.len() + value.len()) as u64;
                ops.push(Op::PutDoc { entity: ENTITY_B.to_vec(), handle, doc });
                ops.push(Op::PutUnique {
                    entity: ENTITY_B.to_vec(),
                    field: FIELD_B.to_vec(),
                    value,
                    handle,
                });
            }
        }
        let expected_csn = block + 1;
        match e.commit_block(format!("scale-seed-block-{block}").as_bytes(), &ops)? {
            Outcome::Committed { csn } if csn == expected_csn => {}
            other => {
                return Err(format!(
                    "block {block}: unexpected commit outcome {other:?}, expected Committed csn={expected_csn}"
                )
                .into())
            }
        }
        if e.next_handle()? != next_handle {
            return Err(format!("handle watermark drifted at block {block}").into());
        }
        // Checkpoint first (publishes everything through csn when the WAL file
        // exceeds the threshold); a successful checkpoint is the only event that
        // grows the active tile list. Then compact if the engine's real cap is
        // reached -- right here, before the next publish can hit the refusal.
        // `maybe_compact` is a no-op below the cap, so most blocks do no merge
        // work. Rotate last (only legal once a tile covers csn with an empty
        // memtable). All three are per-block calls, each a no-op below threshold.
        if e.maybe_checkpoint(CHECKPOINT_BYTES)? {
            count_published_tiles(dir, &mut seen_publish_tiles, &mut tile_bytes_publish)?;
            if let Some(merged) = e.maybe_compact(COMPACT_MERGE_BUDGET, MAX_ACTIVE_TILES)? {
                compactions += 1;
                tile_bytes_compact += merged;
            }
        }
        // Account the live segment's bytes toward WAL-written only when a
        // rotation is about to unlink it; the remainder is added by size at
        // report time, so every appended byte is counted exactly once.
        let segment_bytes = wal_bytes(dir)?;
        if e.rotate_wal(ROTATE_BYTES)? {
            wal_written += segment_bytes;
        }
        rss_peak = rss_peak.max(rss_kb());
        let done = block + 1;
        if done % interval == 0 || done == total_blocks {
            let now = Instant::now();
            let wal_appended = wal_written + wal_bytes(dir)?;
            let tile_bytes = tile_bytes_publish + tile_bytes_compact;
            let docs_s_interval =
                (end - last_progress_docs) as f64 / now.duration_since(last_progress).as_secs_f64().max(1e-9);
            let docs_s_cumulative =
                end as f64 / now.duration_since(started).as_secs_f64().max(1e-9);
            let write_amp =
                (wal_appended + tile_bytes) as f64 / payload_bytes.max(1) as f64;
            emit(&format!(
                "phase=seed docs={end} csn={} payload_bytes={payload_bytes} wal_bytes={} wal_written_bytes={wal_appended} tile_bytes_publish={tile_bytes_publish} tile_bytes_compact={tile_bytes_compact} tile_bytes_total={tile_bytes} compactions={compactions} write_amplification={write_amp:.3} docs_s_interval={docs_s_interval:.0} docs_s_cumulative={docs_s_cumulative:.0} rss_kb={} rss_peak_kb={rss_peak}",
                e.csn(),
                wal_bytes(dir)?,
                rss_kb(),
            ));
            last_progress = now;
            last_progress_docs = end;
        }
    }

    let meta = Meta {
        format: META_FORMAT.to_string(),
        total_docs,
        docs_per_commit,
        value_bytes,
        docs_a,
        docs_b,
    };
    let meta_file = fs::File::create(dir.join(META_NAME))?;
    serde_json::to_writer_pretty(io::BufWriter::new(meta_file), &meta)?;
    let elapsed = started.elapsed().as_secs_f64();
    let wal_appended = wal_written + wal_bytes(dir)?;
    let tile_bytes = tile_bytes_publish + tile_bytes_compact;
    let write_amp = (wal_appended + tile_bytes) as f64 / payload_bytes.max(1) as f64;
    emit(&format!(
        "phase=seed ok=true docs={total_docs} csn={} blocks={total_blocks} payload_bytes={payload_bytes} wal_bytes={} wal_written_bytes={wal_appended} tile_bytes_publish={tile_bytes_publish} tile_bytes_compact={tile_bytes_compact} tile_bytes_total={tile_bytes} compactions={compactions} write_amplification={write_amp:.3} docs_s_cumulative={:.0} rss_kb={} rss_peak_kb={rss_peak} elapsed_s={elapsed:.3}",
        e.csn(),
        wal_bytes(dir)?,
        total_docs as f64 / elapsed.max(1e-9),
        rss_kb(),
    ));
    Ok(())
}

// ---------- verify ----------

fn read_meta(dir: &Path, expect_total_docs: Option<u64>) -> Result<Meta, Box<dyn std::error::Error>> {
    let path = dir.join(META_NAME);
    let raw = fs::read(&path)
        .map_err(|e| format!("seed metadata {} unreadable ({e}); run `seed` first", path.display()))?;
    let meta: Meta = serde_json::from_slice(&raw)
        .map_err(|e| format!("{} is not valid scale metadata: {e}", path.display()))?;
    if meta.format != META_FORMAT {
        return Err(format!("{}: unknown metadata format {:?}", path.display(), meta.format).into());
    }
    if let Some(expect) = expect_total_docs {
        if meta.total_docs != expect {
            return Err(format!(
                "total_docs={expect} disagrees with seeded total_docs={}",
                meta.total_docs
            )
            .into());
        }
    }
    if meta.docs_per_commit == 0 {
        return Err(format!("{}: docs_per_commit=0", path.display()).into());
    }
    Ok(meta)
}

fn count_entity(e: &Engine, entity: &[u8]) -> io::Result<u64> {
    let mut count = 0u64;
    e.scan_primary(entity, |_, _| {
        count += 1;
        Ok(())
    })?;
    Ok(count)
}

fn cmd_verify(dir: &Path, total_docs: u64) -> Result<(), Box<dyn std::error::Error>> {
    let meta = read_meta(dir, Some(total_docs))?;
    let e = Engine::open_discover(dir)?;
    let expected_csn = meta.total_docs.div_ceil(meta.docs_per_commit);
    if e.csn() != expected_csn {
        return Err(format!(
            "csn={} but the seeded stream committed {expected_csn} blocks (total_docs={}, docs_per_commit={})",
            e.csn(),
            meta.total_docs,
            meta.docs_per_commit
        )
        .into());
    }
    // Handles were allocated sequentially in stream order from a fresh database,
    // so global doc g has handle g + 1: EntityA doc i -> i + 1, EntityB doc
    // i -> docs_a + i + 1.
    let mut probes = 0u64;
    for k in 0..PROBES_PER_ENTITY {
        if meta.docs_a > 0 {
            let i = k * meta.docs_a / PROBES_PER_ENTITY;
            let got = e.unique_lookup(ENTITY_A, FIELD_A, format!("a{i}").as_bytes())?;
            if got != Some(i + 1) {
                return Err(format!("unique probe EntityA/ua/a{i} resolved to {got:?}, expected Some({})", i + 1).into());
            }
            probes += 1;
        }
        if meta.docs_b > 0 {
            let i = k * meta.docs_b / PROBES_PER_ENTITY;
            let got = e.unique_lookup(ENTITY_B, FIELD_B, format!("b{i}").as_bytes())?;
            if got != Some(meta.docs_a + i + 1) {
                return Err(format!(
                    "unique probe EntityB/ub/b{i} resolved to {got:?}, expected Some({})",
                    meta.docs_a + i + 1
                )
                .into());
            }
            probes += 1;
        }
    }
    // Full scan passes are the prototype's strong path; three per entity must
    // each observe exactly the seeded live doc count.
    for pass in 1..=SCAN_PASSES {
        let count_a = count_entity(&e, ENTITY_A)?;
        if count_a != meta.docs_a {
            return Err(format!("scan pass {pass}: EntityA has {count_a} docs, expected {}", meta.docs_a).into());
        }
        let count_b = count_entity(&e, ENTITY_B)?;
        if count_b != meta.docs_b {
            return Err(format!("scan pass {pass}: EntityB has {count_b} docs, expected {}", meta.docs_b).into());
        }
    }
    emit(&format!(
        "phase=verify ok=true csn={} docs_a={} docs_b={} probes={probes} scans={} rss_kb={}",
        e.csn(),
        meta.docs_a,
        meta.docs_b,
        SCAN_PASSES * 2,
        rss_kb(),
    ));
    Ok(())
}

// ---------- scanbench ----------

fn cmd_scanbench(dir: &Path, passes: u64) -> Result<(), Box<dyn std::error::Error>> {
    if passes == 0 {
        return Err("passes must be >= 1".into());
    }
    let meta = read_meta(dir, None)?;
    let e = Engine::open_discover(dir)?;
    let started = Instant::now();
    let mut docs_total = 0u64;
    let mut bytes_total = 0u64;
    for pass in 1..=passes {
        let mut docs = 0u64;
        let mut bytes = 0u64;
        for entity in [ENTITY_A, ENTITY_B] {
            e.scan_primary(entity, |_, value| {
                docs += 1;
                bytes += value.len() as u64;
                Ok(())
            })?;
        }
        docs_total += docs;
        bytes_total += bytes;
        emit(&format!(
            "phase=scanbench pass={pass}/{passes} docs={docs} bytes={bytes} elapsed_s={:.3}",
            started.elapsed().as_secs_f64(),
        ));
    }
    let elapsed = started.elapsed().as_secs_f64().max(1e-9);
    emit(&format!(
        "phase=scanbench ok=true passes={passes} docs={docs_total} bytes={bytes_total} value_bytes={} elapsed_s={:.3} mib_s={:.2} rss_kb={}",
        meta.value_bytes,
        started.elapsed().as_secs_f64(),
        bytes_total as f64 / (1u64 << 20) as f64 / elapsed,
        rss_kb(),
    ));
    Ok(())
}
