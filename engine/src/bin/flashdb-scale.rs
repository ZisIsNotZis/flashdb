//! Scale benchmark driver for the flashdb v0 engine
//! (seed / seed-inventory / verify / scanbench).
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
//! `Engine::maybe_compact_shape(COMPACT_MERGE_BUDGET, MAX_ACTIVE_TILES, shape)`:
//! the trigger is the engine's active-tile cap (`MAX_ACTIVE_TILES`, a policy
//! constant of 1024 backed by a chained multi-page manifest — not a hard-coded
//! 8/12 and no longer one 4 KiB page), the shape is the runtime
//! `FLASHDB_COMPACT_SHAPE` policy, and the driver asks the merge to cap each
//! merge at eight 64 MiB rotation windows of tile bytes. Whether a merge
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
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::time::Instant;

use flashdb_engine::engine::{CompactShape, Engine, LayoutPolicy, Op, Outcome, RegionMode, MAX_ACTIVE_TILES};
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
/// Merge budget in rotation windows. Overridable with `FLASHDB_COMPACT_WINDOWS` so a
/// budget sweep needs no rebuild; the default is the shipped policy.
const COMPACT_MERGE_WINDOWS: u64 = 8;

fn compact_merge_budget() -> u64 {
    std::env::var("FLASHDB_COMPACT_WINDOWS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(COMPACT_MERGE_WINDOWS)
        * ROTATE_BYTES
}

/// Shipped merge shape. Overridable with `FLASHDB_COMPACT_SHAPE` (one of
/// `cheapest_pair`, `ratio_group`, `budget_prefix`) so a shape sweep needs no
/// rebuild; the default is the measured winner.
///
/// Default: `cheapest_pair`. Measured at 2,000,000 x 1 KiB (budget 512 MiB,
/// cap 12): ... (filled in by the 12d measurement).
/// Shipped default after the shape sweep (deterministic metrics, same payload).
/// At 5 M docs x 1 KiB: BudgetPrefix write_amp 3.920 / 21 compactions /
/// 9.14 GB compaction output, RatioGroup 4.137 / 25 / 10.26 GB, CheapestPair
/// 5.868 / 65 / 19.20 GB. At 2 M docs BudgetPrefix and RatioGroup both reach
/// 2.998; CheapestPair is 3.827. BudgetPrefix is therefore the default, and the
/// other shapes stay reachable via FLASHDB_COMPACT_SHAPE for re-measurement.
const COMPACT_SHAPE: CompactShape = CompactShape::BudgetPrefix;

fn compact_shape() -> CompactShape {
    match std::env::var("FLASHDB_COMPACT_SHAPE") {
        Ok(name) => CompactShape::from_name(&name).unwrap_or_else(|| {
            eprintln!(
                "unknown FLASHDB_COMPACT_SHAPE={name:?}; using {}",
                COMPACT_SHAPE.name()
            );
            COMPACT_SHAPE
        }),
        Err(_) => COMPACT_SHAPE,
    }
}

/// Provisional layout override for measurement: `FLASHDB_PAGE_SIZE` (bytes,
/// 4096..=65536) and `FLASHDB_BLOOM_BITS` (0 disables the bloom) are read by
/// [`LayoutPolicy::from_env`]; an unset or invalid value keeps the declared
/// default, which reproduces today's behaviour exactly. The parameters land in
/// each tile's superblock, so a directory written here reads back without the
/// env set. Nothing about this is durable.
fn layout_policy() -> LayoutPolicy { LayoutPolicy::from_env() }

/// `FLASHDB_REGION_MODE=global|per_region` selects the tile write mode (slice
/// 24). Unset or unknown keeps the shipped default, [`RegionMode::Global`],
/// which reproduces the pre-partitioning single-tile stream.
fn region_mode() -> RegionMode {
    match std::env::var("FLASHDB_REGION_MODE") {
        Ok(name) => RegionMode::from_name(&name).unwrap_or_else(|| {
            eprintln!("unknown FLASHDB_REGION_MODE={name:?}; using global");
            RegionMode::Global
        }),
        Err(_) => RegionMode::Global,
    }
}

/// Per-region layout overrides for measurement:
/// `FLASHDB_REGION_LAYOUT="P|Movement=65536:4,U|Stock=4096:0"` (comma-separated
/// `region=page_size:bloom_bits`, the region spelled as `LayoutPolicy` expects).
/// Applied on top of [`layout_policy`]; each value lands in the emitted tile's
/// superblock, so a directory written here reads back without the env set.
///
/// `FLASHDB_POLICY_FILE=<path>` (slice 27) is the external-controller channel and
/// is **authoritative**: when it is set and parses, it REPLACES the env-derived
/// policy above, so a controller declares the whole per-region policy (page size,
/// bloom bits and merge threshold) in one file. The schema and its reject-on-
/// invalid error policy are documented on [`LayoutPolicy`]. A file that fails to
/// parse is a hard error (a controller must never silently measure a policy it
/// did not set).
fn region_layout_policy() -> LayoutPolicy {
    if let Ok(path) = std::env::var("FLASHDB_POLICY_FILE") {
        if !path.is_empty() {
            return match LayoutPolicy::from_json_file(Path::new(&path)) {
                Ok(policy) => policy,
                Err(e) => {
                    eprintln!("FLASHDB_POLICY_FILE={path:?} rejected: {e}");
                    std::process::exit(1);
                }
            };
        }
    }
    let mut policy = layout_policy();
    let Ok(spec) = std::env::var("FLASHDB_REGION_LAYOUT") else { return policy };
    for entry in spec.split(',').filter(|s| !s.is_empty()) {
        let Some((region, values)) = entry.split_once('=') else {
            eprintln!("FLASHDB_REGION_LAYOUT entry {entry:?} is not region=page:bits; ignored");
            continue;
        };
        let (page, bits) = values.split_once(':').unwrap_or((values, "10"));
        match (page.parse::<u32>(), bits.parse::<u8>()) {
            (Ok(page), Ok(bits)) => { policy.set_region(region, page, bits); }
            _ => eprintln!("FLASHDB_REGION_LAYOUT entry {entry:?} has a bad page/bits; ignored"),
        }
    }
    policy
}

/// Emit the active policy as one machine-readable line so a run is
/// self-describing and a controller can confirm what the engine holds.
fn emit_policy(phase: &str, policy: &LayoutPolicy) {
    emit(&format!("phase={phase} policy={}", policy.to_json()));
}

/// `seed-inventory` checkpoint cadence (measurement knob). When set, the seeded
/// inventory publishes the current memtable as tiles through the tested
/// `maybe_checkpoint`/`publish_tile` protocol once the retained WAL exceeds this
/// many bytes, and always publishes the unpublished tail at the end (so the
/// caller is guaranteed at least one tile). Unset (the default) keeps the
/// pre-slice behaviour: the seed stays in the WAL-backed memtable and the flow
/// reads it from RAM, never from tiles.
fn seed_checkpoint_bytes() -> Option<u64> {
    std::env::var("FLASHDB_SEED_CHECKPOINT_BYTES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
}

/// Every `*.tile` file in the engine directory as `(plain name, bytes)`, sorted.
/// Used by the seed/checkpoint/tileinfo reporting; unreferenced leftovers from a
/// failed publish would be included, so a caller that needs the active set must
/// cross-check against `Engine::active_tile_cutoffs`.
fn tile_files(dir: &Path) -> io::Result<Vec<(String, u64)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if path.extension().and_then(|e| e.to_str()) == Some("tile") {
            out.push((name.to_string(), fs::metadata(&path)?.len()));
        }
    }
    out.sort();
    Ok(out)
}

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

/// Telemetry is on by default in this measurement driver; `FLASHDB_METRICS=0`
/// disables it, which is how the overhead of the instrumented call sites is
/// measured by same-configuration comparison.
fn metrics_enabled() -> bool {
    !matches!(std::env::var("FLASHDB_METRICS").as_deref(), Ok("0") | Ok("off") | Ok("false"))
}

/// Print the finished observation window as one `metrics` line per pattern and
/// roll it; `metrics_rotate_window` is the driver's one-window-per-line primitive.
fn print_metrics(phase: &str, e: &Engine) {
    let snap = e.metrics_rotate_window();
    if snap.patterns.is_empty() {
        emit(&format!("phase={phase} metrics=none"));
        return;
    }
    for p in &snap.patterns {
        emit(&format!(
            "phase={phase} metrics pattern={} count={} p50_ns={:.0} p99_ns={:.0} mean_ns={:.0}",
            p.pattern, p.count, p.p50_ns, p.p99_ns, p.mean_ns
        ));
    }
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
) -> io::Result<usize> {
    let mut fresh = 0;
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
        fresh += 1;
    }
    Ok(fresh)
}

fn parse_num(text: &str, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    text.parse::<u64>()
        .map_err(|_| format!("invalid {name}: {text:?}").into())
}

const USAGE: &str = "usage: flashdb-scale seed <dir> <total_docs> <docs_per_commit> <value_bytes>\n\
                     usage: flashdb-scale seed-inventory <dir> <seed-trace.jsonl>\n\
                     usage: flashdb-scale checkpoint <dir>\n\
                     usage: flashdb-scale compact <dir>\n\
                     usage: flashdb-scale tileinfo <dir>\n\
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
        Some("seed-inventory") if args.len() == 4 => {
            cmd_seed_inventory(Path::new(&args[2]), Path::new(&args[3]))
        }
        Some("checkpoint") if args.len() == 3 => cmd_checkpoint(Path::new(&args[2])),
        Some("compact") if args.len() == 3 => cmd_compact(Path::new(&args[2])),
        Some("tileinfo") if args.len() == 3 => cmd_tileinfo(Path::new(&args[2])),
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
    let layout = region_layout_policy();
    e.set_layout_policy(layout.clone());
    e.set_region_mode(region_mode());
    let (layout_page_size, layout_bloom_bits) = layout.default_layout();
    e.set_metrics_enabled(metrics_enabled());
    emit_policy("seed", &layout);
    let docs_a = total_docs / 2;
    let docs_b = total_docs - docs_a;
    let total_blocks = total_docs.div_ceil(docs_per_commit);
    let interval = (total_blocks / 10).clamp(1, 50);
    let shape = compact_shape();
    emit(&format!(
        "phase=seed start shape={} merge_budget_bytes={} max_tiles={MAX_ACTIVE_TILES} region_mode={} layout_page_size={layout_page_size} layout_bloom_bits={layout_bloom_bits}",
        shape.name(),
        compact_merge_budget(),
        region_mode().name(),
    ));
    let mut next_handle = e.next_handle()?;
    let mut rss_peak = rss_kb();
    let started = Instant::now();
    // Cumulative, measured accounting (definitions in the module header).
    let mut payload_bytes: u64 = 0;
    let mut wal_written: u64 = 0;
    let mut tile_bytes_publish: u64 = 0;
    let mut tile_bytes_compact: u64 = 0;
    let mut compactions: u64 = 0;
    let mut publishes: u64 = 0;
    let mut tiles_published: u64 = 0;
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
            publishes += 1;
            tiles_published += count_published_tiles(dir, &mut seen_publish_tiles, &mut tile_bytes_publish)? as u64;
            if let Some(merged) = e.maybe_compact_shape(compact_merge_budget(), MAX_ACTIVE_TILES, shape)? {
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
                "phase=seed docs={end} csn={} shape={} payload_bytes={payload_bytes} wal_bytes={} wal_written_bytes={wal_appended} tile_bytes_publish={tile_bytes_publish} tile_bytes_compact={tile_bytes_compact} tile_bytes_total={tile_bytes} compactions={compactions} write_amplification={write_amp:.3} docs_s_interval={docs_s_interval:.0} docs_s_cumulative={docs_s_cumulative:.0} rss_kb={} rss_peak_kb={rss_peak}",
                e.csn(),
                shape.name(),
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
    print_metrics("seed", &e);
    emit(&format!(
        "phase=seed ok=true docs={total_docs} csn={} shape={} max_tiles={MAX_ACTIVE_TILES} region_mode={} blocks={total_blocks} payload_bytes={payload_bytes} wal_bytes={} wal_written_bytes={wal_appended} tile_bytes_publish={tile_bytes_publish} tile_bytes_compact={tile_bytes_compact} tile_bytes_total={tile_bytes} compactions={compactions} publishes={publishes} tiles_published={tiles_published} tiles_per_publish={:.2} write_amplification={write_amp:.3} docs_s_cumulative={:.0} rss_kb={} rss_peak_kb={rss_peak} elapsed_s={elapsed:.3} layout_page_size={layout_page_size} layout_bloom_bits={layout_bloom_bits}",
        e.csn(),
        shape.name(),
        region_mode().name(),
        wal_bytes(dir)?,
        tiles_published as f64 / publishes.max(1) as f64,
        total_docs as f64 / elapsed.max(1e-9),
        rss_kb(),
    ));
    Ok(())
}

// ---------- seed-inventory ----------
// Apply a harness-produced dimension seeding trace (see
// `harness/flashdb_harness/seed.py`) as raw engine P/U ops. The request adapter
// has no create shape, so this is the established bootstrap path (the engine
// tests and `seed` both use `Op::PutDoc` / `Op::PutUnique`).

/// One declared unique per entity, mirroring `engine/src/request.rs`'s catalog.
fn seed_unique_field(entity: &str) -> Option<&'static str> {
    match entity {
        "Customer" => Some("email"),
        "Stock" => Some("sku_loc"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct SeedUnique {
    field: String,
    value: serde_json::Value,
}

#[derive(Deserialize)]
struct SeedDoc {
    entity: String,
    doc: serde_json::Value,
    unique: SeedUnique,
}

#[derive(Deserialize)]
struct SeedBlock {
    block: String,
    docs: Vec<SeedDoc>,
}

/// Unique-value bytes for `entity`. Must equal the bytes the adapter's lookups
/// probe: Customer.email is the raw UTF-8 email, Stock.sku_loc is
/// `serde_json::to_vec(&[sku, loc])` (`request.rs::stock_key`). Also checks that
/// the document carries the same fields, so P and U can never disagree.
fn seed_unique_value(
    entity: &str,
    unique: &SeedUnique,
    doc: &serde_json::Value,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let field = seed_unique_field(entity).expect("caller checked the entity");
    if unique.field != field {
        return Err(format!(
            "{entity} declares unique field {field:?}, trace gave {:?}",
            unique.field
        )
        .into());
    }
    let object = doc
        .as_object()
        .ok_or_else(|| format!("{entity} doc must be a JSON object"))?;
    match (entity, &unique.value) {
        ("Customer", serde_json::Value::String(email)) => {
            if object.get("email").and_then(serde_json::Value::as_str) != Some(email.as_str()) {
                return Err("Customer doc email disagrees with its unique value".into());
            }
            Ok(email.as_bytes().to_vec())
        }
        ("Stock", serde_json::Value::Array(pair)) if pair.len() == 2 => {
            let sku = pair[0].as_str();
            let loc = pair[1].as_str();
            if sku.is_none() || loc.is_none() {
                return Err("Stock sku_loc entries must be strings".into());
            }
            if object.get("sku").and_then(serde_json::Value::as_str) != sku
                || object.get("loc").and_then(serde_json::Value::as_str) != loc
            {
                return Err("Stock doc sku/loc disagree with the unique value".into());
            }
            if object.get("on_hand").and_then(serde_json::Value::as_i64).is_none() {
                return Err("Stock doc on_hand must be an i64".into());
            }
            Ok(serde_json::to_vec(&unique.value)?)
        }
        ("Customer", _) => Err("Customer unique value must be the email string".into()),
        ("Stock", _) => Err("Stock unique value must be a [sku, loc] array".into()),
        _ => unreachable!("seed_unique_field rejects unknown entities"),
    }
}

fn cmd_seed_inventory(dir: &Path, trace: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let wal = dir.join("data.wal");
    let mut e = if wal.exists() {
        Engine::open(&wal)?
    } else {
        fs::create_dir_all(dir)?;
        Engine::create(&wal)?
    };
    // Same region/layout knobs as `seed`, so a per-region page assignment can be
    // measured through this bootstrap too; the parameters land in each tile's
    // own superblock, so the directory reads back without the env set.
    let layout = region_layout_policy();
    e.set_layout_policy(layout.clone());
    e.set_region_mode(region_mode());
    e.set_metrics_enabled(metrics_enabled());
    emit_policy("seed-inventory", &layout);
    let (layout_page_size, layout_bloom_bits) = layout.default_layout();
    let checkpoint_bytes = seed_checkpoint_bytes();
    let file = fs::File::open(trace)
        .map_err(|e| format!("seed trace {} unreadable ({e})", trace.display()))?;
    let started = Instant::now();
    let mut blocks = 0u64;
    let mut docs = 0u64;
    let mut customers = 0u64;
    let mut stocks = 0u64;
    let mut publishes = 0u64;
    let mut last_published_csn = 0u64;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("seed trace line {}: {e}", index + 1))?;
        let parsed: SeedBlock = serde_json::from_str(&line)
            .map_err(|e| format!("seed trace line {}: {e}", index + 1))?;
        if parsed.block.is_empty() {
            return Err(format!("seed trace line {}: empty block id", index + 1).into());
        }
        let mut ops = Vec::with_capacity(parsed.docs.len() * 2);
        // `next_handle()` only advances when a block commits, so within one block
        // the handles must be reserved with a local counter (as `cmd_seed` does).
        let mut next_handle = e.next_handle()?;
        for d in &parsed.docs {
            if seed_unique_field(&d.entity).is_none() {
                return Err(format!(
                    "seed trace line {}: unsupported entity {:?}",
                    index + 1,
                    d.entity
                )
                .into());
            }
            let value = seed_unique_value(&d.entity, &d.unique, &d.doc)
                .map_err(|e| format!("seed trace line {}: {e}", index + 1))?;
            let handle = next_handle;
            next_handle += 1;
            let doc = serde_json::to_vec(&d.doc)?;
            ops.push(Op::PutDoc { entity: d.entity.clone().into_bytes(), handle, doc });
            ops.push(Op::PutUnique {
                entity: d.entity.clone().into_bytes(),
                field: d.unique.field.clone().into_bytes(),
                value,
                handle,
            });
            match d.entity.as_str() {
                "Customer" => customers += 1,
                "Stock" => stocks += 1,
                _ => unreachable!(),
            }
            docs += 1;
        }
        match e.commit_block(parsed.block.as_bytes(), &ops)? {
            Outcome::Committed { .. } | Outcome::AlreadyCommitted { .. } => {}
            other => return Err(format!("block {blocks}: unexpected outcome {other:?}").into()),
        }
        if e.next_handle()? != next_handle {
            return Err(format!("block {blocks}: handle watermark drifted").into());
        }
        blocks += 1;
        if let Some(max_wal_bytes) = checkpoint_bytes {
            if e.maybe_checkpoint(max_wal_bytes)? {
                publishes += 1;
                last_published_csn = e.csn();
            }
        }
    }
    // Guarantee at least one tile when checkpointing is on, even if the trace is
    // smaller than the cadence: publish the unpublished tail through the same
    // machinery (`maybe_checkpoint(0)` publishes whenever anything is uncovered).
    if checkpoint_bytes.is_some() && last_published_csn < e.csn() {
        e.maybe_checkpoint(0)?;
        publishes += 1;
    }
    emit(&format!(
        "phase=seed-inventory ok=true blocks={blocks} docs={docs} customers={customers} stocks={stocks} csn={} publishes={publishes} tiles={} serving_memtable_entries={} region_mode={} layout_page_size={layout_page_size} layout_bloom_bits={layout_bloom_bits} dir={} elapsed_s={:.3}",
        e.csn(),
        tile_files(dir)?.len(),
        e.serving_memtable_entries(),
        region_mode().name(),
        dir.display(),
        started.elapsed().as_secs_f64(),
    ));
    Ok(())
}

// ---------- checkpoint ----------

/// Publish everything the engine's WAL-backed memtable currently holds as tiles
/// through the tested `maybe_checkpoint` protocol, so a later read consults
/// tiles instead of the memtable. The engine must already exist; layout/region
/// policy come from the same env knobs as `seed`. A no-op (published=false) when
/// the newest active tile already covers the current CSN.
fn cmd_checkpoint(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::open_discover(dir)?;
    let policy = region_layout_policy();
    e.set_layout_policy(policy.clone());
    e.set_region_mode(region_mode());
    e.set_metrics_enabled(metrics_enabled());
    emit_policy("checkpoint", &policy);
    let csn_before = e.csn();
    let newest = e.active_tile_cutoffs().last().copied().unwrap_or(0);
    let published = if newest < csn_before { e.maybe_checkpoint(0)? } else { false };
    emit(&format!(
        "phase=checkpoint ok=true csn_before={csn_before} published={published} csn={} tiles={} serving_memtable_entries={} region_mode={} dir={}",
        e.csn(),
        tile_files(dir)?.len(),
        e.serving_memtable_entries(),
        region_mode().name(),
        dir.display(),
    ));
    Ok(())
}

// ---------- compact ----------

/// Active tiles per region, in first-seen list order, as `(region, count)`.
/// A whole-keyspace tile is reported as `(global)`.
fn tiles_per_region(e: &Engine) -> Vec<(String, usize)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for region in e.active_tile_regions() {
        let name = region.unwrap_or_else(|| "(global)".to_string());
        if !counts.contains_key(&name) { order.push(name.clone()); }
        *counts.entry(name).or_default() += 1;
    }
    order.into_iter().map(|name| { let count = counts[&name]; (name, count) }).collect()
}

/// `compact <dir>` (slice 27): apply the active per-region policy's merge
/// thresholds to an already tile-resident directory the inventory flow
/// published, compacting every eligible region to a fixed point, and report the
/// per-region tile counts and merge accounting. `max_tiles` is set to the
/// current active count on each call so the engine's global gate is always open
/// and the per-region threshold is the only thing deciding which regions merge -
/// exactly the actuator under test. Compaction output bytes are measured; the
/// rewrite factor is `compaction_bytes / tile_bytes_before`.
fn cmd_compact(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut e = Engine::open_discover(dir)?;
    let policy = region_layout_policy();
    e.set_layout_policy(policy.clone());
    e.set_region_mode(region_mode());
    e.set_metrics_enabled(metrics_enabled());
    let shape = compact_shape();
    emit_policy("compact", &policy);
    let before = tiles_per_region(&e);
    let before_render = before.iter().map(|(r, c)| format!("{r}:{c}")).collect::<Vec<_>>().join(",");
    let tile_bytes_before = e.active_tile_bytes()?;
    let mut merges = 0u64;
    let mut compaction_bytes = 0u64;
    loop {
        let active = e.active_tile_cutoffs().len();
        if active < 2 { break; }
        match e.maybe_compact_shape(compact_merge_budget(), active, shape)? {
            Some(merged) => { merges += 1; compaction_bytes += merged; }
            None => break,
        }
    }
    let after = tiles_per_region(&e);
    let after_render = after.iter().map(|(r, c)| format!("{r}:{c}")).collect::<Vec<_>>().join(",");
    let tile_bytes_after = e.active_tile_bytes()?;
    let rewrite_factor = compaction_bytes as f64 / tile_bytes_before.max(1) as f64;
    emit(&format!(
        "phase=compact ok=true shape={} region_mode={} merges={merges} compaction_bytes={compaction_bytes} tile_bytes_before={tile_bytes_before} tile_bytes_after={tile_bytes_after} rewrite_factor={rewrite_factor:.3} active_tiles_before={} active_tiles_after={} tiles_per_region_before={before_render} tiles_per_region_after={after_render} dir={}",
        shape.name(),
        region_mode().name(),
        before.iter().map(|(_, c)| c).sum::<usize>(),
        after.iter().map(|(_, c)| c).sum::<usize>(),
        dir.display(),
    ));
    Ok(())
}

// ---------- tileinfo ----------

/// One active tile's self-described layout, parsed read-only from its
/// superblock and first directory fence (never opened as an engine tile, so no
/// tile/WAL/index RAM is paid). This is the evidence that a flow's data is
/// tile-resident and with which page/bloom parameters.
struct TileMeta {
    name: String,
    bytes: u64,
    cutoff: u64,
    entries: u64,
    page_size: u32,
    bloom_len: u64,
    bits_per_key: u32,
    hash_count: u8,
    dir_len: u64,
    fences: u16,
    region: String,
}

fn read_tile_meta(path: &Path) -> Result<TileMeta, Box<dyn std::error::Error>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = fs::File::open(path)?;
    let bytes = f.metadata()?.len();
    let mut sb = [0u8; 4096];
    f.read_exact(&mut sb).map_err(|e| format!("{}: superblock unreadable ({e})", path.display()))?;
    if &sb[0..8] != b"FDBTILE3" {
        return Err(format!("{}: superblock magic is not FDBTILE3", path.display()).into());
    }
    let u64at = |b: &[u8], i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    let u32at = |b: &[u8], i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let dir_offset = u64at(&sb, 56);
    let dir_len = u64at(&sb, 64);
    // First fence record: [block_len u32][crc u32][fence_count u16]
    // [first_key_len u32][block_offset u64][block_len u32][first_key bytes].
    let (region, fences) = if dir_len == 0 {
        ("?".to_string(), 0)
    } else {
        f.seek(SeekFrom::Start(dir_offset + 8))?;
        let mut fencesb = [0u8; 2];
        f.read_exact(&mut fencesb)?;
        let fences = u16::from_le_bytes(fencesb);
        f.seek(SeekFrom::Start(dir_offset + 10))?;
        let mut lenb = [0u8; 4];
        f.read_exact(&mut lenb)?;
        let first_key_len = u32::from_le_bytes(lenb) as usize;
        f.seek(SeekFrom::Start(dir_offset + 10 + 4 + 8 + 4))?;
        let mut key = vec![0u8; first_key_len];
        f.read_exact(&mut key)?;
        let region = flashdb_engine::keys::region_prefix(&key)
            .map(flashdb_engine::keys::region_name)
            .unwrap_or_else(|| "?".to_string());
        (region, fences)
    };
    Ok(TileMeta {
        name: path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string(),
        bytes,
        cutoff: u64at(&sb, 8),
        entries: u64at(&sb, 48),
        page_size: u32at(&sb, 72),
        bloom_len: u64at(&sb, 84),
        bits_per_key: u32at(&sb, 96),
        hash_count: sb[100],
        dir_len,
        fences,
        region,
    })
}

/// Report every tile file's page size, bloom state and region, plus the active
/// tile count and the serving-memtable entry count. A serving memtable of 0
/// entries with a non-empty active tile list is the direct proof that reads
/// cannot be served from RAM: the data is only readable through tiles.
fn cmd_tileinfo(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let e = Engine::open_discover(dir)?;
    let active = e.active_tile_cutoffs();
    // The write-time region mode is not persisted; each tile's region is derived
    // from its own first key, which is what the per-tile lines report below.
    emit(&format!(
        "phase=tileinfo ok=true dir={} active_tiles={} serving_memtable_entries={}",
        dir.display(),
        active.len(),
        e.serving_memtable_entries(),
    ));
    let mut metas = Vec::new();
    for (name, bytes) in tile_files(dir)? {
        match read_tile_meta(&dir.join(&name)) {
            Ok(mut m) => {
                m.bytes = bytes;
                metas.push(m);
            }
            Err(err) => emit(&format!("phase=tileinfo file={name} parse_error={err}")),
        }
    }
    metas.sort_by(|a, b| (a.cutoff, &a.region).cmp(&(b.cutoff, &b.region)));
    for m in &metas {
        emit(&format!(
            "phase=tileinfo tile={} bytes={} cutoff={} entries={} page_size={} bloom_len={} bits_per_key={} hash_count={} dir_len={} fences={} region={}",
            m.name, m.bytes, m.cutoff, m.entries, m.page_size, m.bloom_len, m.bits_per_key, m.hash_count, m.dir_len,
            m.fences, m.region,
        ));
    }
    let page_sizes: std::collections::BTreeSet<u32> = metas.iter().map(|m| m.page_size).collect();
    emit(&format!(
        "phase=tileinfo ok=true tiles={} tile_bytes_total={} page_sizes={:?} bloom_tiles={}",
        metas.len(),
        metas.iter().map(|m| m.bytes).sum::<u64>(),
        page_sizes,
        metas.iter().filter(|m| m.bloom_len > 0).count(),
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
    let mut e = Engine::open_discover(dir)?;
    e.set_metrics_enabled(metrics_enabled());
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
    // Point reads of the same handles the unique probes resolved to, exercising
    // the P pattern; a missing document is a real verify failure.
    let mut gets = 0u64;
    for k in 0..PROBES_PER_ENTITY {
        if meta.docs_a > 0 {
            let i = k * meta.docs_a / PROBES_PER_ENTITY;
            if e.get(ENTITY_A, i + 1)?.is_none() {
                return Err(format!("point get EntityA handle {} returned none", i + 1).into());
            }
            gets += 1;
        }
        if meta.docs_b > 0 {
            let i = k * meta.docs_b / PROBES_PER_ENTITY;
            if e.get(ENTITY_B, meta.docs_a + i + 1)?.is_none() {
                return Err(format!("point get EntityB handle {} returned none", meta.docs_a + i + 1).into());
            }
            gets += 1;
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
    print_metrics("verify", &e);
    emit(&format!(
        "phase=verify ok=true csn={} docs_a={} docs_b={} probes={probes} gets={gets} scans={} rss_kb={}",
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
    let mut e = Engine::open_discover(dir)?;
    e.set_metrics_enabled(metrics_enabled());
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
    print_metrics("scanbench", &e);
    emit(&format!(
        "phase=scanbench ok=true passes={passes} docs={docs_total} bytes={bytes_total} value_bytes={} elapsed_s={:.3} mib_s={:.2} rss_kb={}",
        meta.value_bytes,
        started.elapsed().as_secs_f64(),
        bytes_total as f64 / (1u64 << 20) as f64 / elapsed,
        rss_kb(),
    ));
    Ok(())
}
