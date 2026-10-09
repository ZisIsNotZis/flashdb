//! Offered-load pacer: issue a trace at a declared rate and measure whether the engine keeps up.
//!
//! Objective v1 (`.scratch/15-objective-v0/objective-v1.md`) scores a **capacity curve**: at each
//! offered-load level, how much of the offered work actually completes, with a pre-registered
//! latency ceiling as a constraint so throughput cannot be won by abandoning tail latency.
//!
//! This binary is deliberately simple: it reads a trace and a *rate schedule*, paces requests
//! open-loop (a request is issued even when it is already late, so a saturated engine shows up as
//! growing lateness rather than as a silently reduced rate), and prints one machine-readable line
//! per window. The harness maps a load profile onto the rate schedule; keeping the schema coupling
//! in Python means the pacer has no opinion about workload semantics.
//!
//! usage:
//!   flashdb-load <dir-or-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N]
//!                [--window-ms 1000] [--ceiling-us N] [--patterns] [--metrics-json]
//!                [--commit-batch B] [--group-requests B] [--no-tiles]
//!                [--policy-file PATH] [--policy-reload] [--peak-report]
//!                [--checkpoint-bytes N] [--rotate-bytes N] [--region-mode global|per_region]
//!                [--compact-bytes N]
//!
//! External policy control (slice 27): `--policy-file PATH` (or the
//! `FLASHDB_POLICY_FILE` env var) points the driver at the frozen JSON policy
//! file documented on [`flashdb_engine::engine::LayoutPolicy`]; it is read once
//! at open and installed on the engine. With `--policy-reload` the driver
//! re-reads and re-installs it at every window boundary, which is how an
//! external controller can retune the per-region merge trigger while a run is in
//! flight. A reload error is reported on stderr and the previously installed
//! policy is kept. The active policy is printed in the metrics JSON
//! (`"policy"`) so the controller can see what the engine actually holds.
//!
//! `--metrics-json` prints one JSON object per window (instead of the human
//! `window=` line) so a controller can consume per-region and per-pattern
//! counters without parsing text. Its frozen schema is documented on
//! `metrics_json_line`.
//!
//! WAL maintenance (Slice 32): `--checkpoint-bytes N` publishes the serving
//! memtable as tiles once the retained WAL exceeds N, and `--rotate-bytes N`
//! retires the published WAL onto a fresh segment once it exceeds N (it requires
//! `--checkpoint-bytes`). Both mirror `flashdb-scale seed-inventory`'s env knobs,
//! so a long write load leaves a bounded WAL suffix rather than one that grows
//! with the offered work. They are opt-in: without them the write load appends to
//! one growing WAL exactly as before, which is what made a many-GB instance open
//! by replaying a multi-tens-of-MB suffix. Off the read path: with neither flag,
//! no extra engine call is made.
//!
//! Compaction actuation (Slice 37): `--compact-bytes N` calls the engine's
//! EXISTING `maybe_compact_shape` (the shipped `CheapestPair` shape, merge budget
//! N bytes) once per metrics window, inside the same process that serves the load.
//! It is the missing half of the external-policy channel: `--policy-file` alone
//! installs a per-region policy, but until this slice the offered-load driver
//! never called compaction, so the policy's `merge_min_tiles` knob was inert on
//! this path. The driver passes the tile-count gate at its floor (2) so each
//! region's OWN `merge_min_tiles` decides eligibility, and it compacts at the
//! window boundary BEFORE the policy reload and the snapshot: the merge threshold
//! that governed the window being reported is the one that governs its compaction,
//! and the merged bytes land in that window's per-region `compaction_bytes`. The
//! window line carries the driver's own `compact_bytes` for the merge it just ran.
//! Off by default: with no `--compact-bytes`, no extra engine call is made.
//!
//! Opening: a **directory** is opened tile-aware with `Engine::open_discover`, so a published
//! tile is consulted before the memtable and the measured flow exercises the real tile read path;
//! a **WAL file** (or `--no-tiles`) keeps the historical `Engine::open` behaviour (full WAL replay
//! into the memtable, tiles ignored). `--no-tiles` on a directory is the controlled A/B: the same
//! data and trace, served from the replayed memtable instead of from tiles.
//!
//! Exit status is 0 even when the engine is saturated: saturation is data, not an error. A
//! trace/parse failure is an error (non-zero), because it means the measurement is invalid.

use std::fs::File;
use std::path::Path;
use std::io::{self, BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use flashdb_engine::engine::{CompactShape, Engine, LayoutPolicy, Op, Outcome, RegionMode, RequestOutcome};
use flashdb_engine::metrics::WindowSnapshot;
use flashdb_engine::replay::execute_requests_grouped;
use flashdb_engine::request::execute_request;
use serde_json::Value;

const MAX_LINE: u64 = 1 << 20;
/// A request issued more than this long after its scheduled time means the engine is not keeping
/// up with the offered rate. Well above the burst pacer's own <= 1 ms chunking floor.
const LAG_SATURATION_US: u64 = 10_000;
/// `--compact-bytes` passes the global active-tile gate at its floor, so under
/// [`RegionMode::PerRegion`] each region's OWN `merge_min_tiles` (the learner's knob)
/// is what decides eligibility rather than a driver-imposed cap.
const COMPACT_MIN_TILES: usize = 2;

struct Args {
    target: String,
    trace: String,
    rates: Vec<f64>,
    segment_requests: Option<u64>,
    window_ms: u64,
    ceiling_us: Option<u64>,
    patterns: bool,
    metrics_json: bool,
    commit_batch: Option<u64>,
    group_requests: Option<u64>,
    discover: bool,
    policy_file: Option<String>,
    policy_reload: bool,
    peak_report: bool,
    checkpoint_bytes: Option<u64>,
    rotate_bytes: Option<u64>,
    region_mode: Option<RegionMode>,
    compact_bytes: Option<u64>,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    if argv.len() < 3 {
        return Err("usage: flashdb-load <dir-or-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N] [--window-ms N] [--ceiling-us N] [--patterns] [--metrics-json] [--commit-batch B] [--group-requests B] [--no-tiles] [--policy-file PATH] [--policy-reload]".into());
    }
    let mut args = Args {
        target: argv[1].clone(),
        trace: argv[2].clone(),
        rates: Vec::new(),
        segment_requests: None,
        window_ms: 1000,
        ceiling_us: None,
        patterns: false,
        metrics_json: false,
        commit_batch: None,
        group_requests: None,
        discover: true,
        policy_file: None,
        policy_reload: false,
        peak_report: false,
        checkpoint_bytes: None,
        rotate_bytes: None,
        region_mode: None,
        compact_bytes: None,
    };
    let mut i = 3;
    while i < argv.len() {
        match argv[i].as_str() {
            "--rate-rps" => {
                let v = argv.get(i + 1).ok_or("--rate-rps needs a value")?;
                args.rates = v.split(',')
                    .map(|s| s.trim().parse::<f64>().map_err(|_| format!("bad rate {s:?}")))
                    .collect::<Result<Vec<_>, _>>()?;
                i += 2;
            }
            "--segment-requests" => {
                args.segment_requests = Some(argv.get(i + 1).ok_or("--segment-requests needs a value")?
                    .parse().map_err(|_| "--segment-requests must be an integer")?);
                i += 2;
            }
            "--window-ms" => {
                args.window_ms = argv.get(i + 1).ok_or("--window-ms needs a value")?
                    .parse().map_err(|_| "--window-ms must be an integer")?;
                i += 2;
            }
            "--ceiling-us" => {
                args.ceiling_us = Some(argv.get(i + 1).ok_or("--ceiling-us needs a value")?
                    .parse().map_err(|_| "--ceiling-us must be an integer")?);
                i += 2;
            }
            "--commit-batch" => {
                let n: u64 = argv.get(i + 1).ok_or("--commit-batch needs a value")?
                    .parse().map_err(|_| "--commit-batch must be a positive integer")?;
                if n == 0 { return Err("--commit-batch must be a positive integer".into()); }
                args.commit_batch = Some(n);
                i += 2;
            }
            "--group-requests" => {
                let n: u64 = argv.get(i + 1).ok_or("--group-requests needs a value")?
                    .parse().map_err(|_| "--group-requests must be a positive integer")?;
                if n == 0 { return Err("--group-requests must be a positive integer".into()); }
                args.group_requests = Some(n);
                i += 2;
            }
            "--patterns" => { args.patterns = true; i += 1; }
            "--metrics-json" => { args.metrics_json = true; i += 1; }
            "--policy-file" => {
                args.policy_file = Some(argv.get(i + 1).ok_or("--policy-file needs a value")?.clone());
                i += 2;
            }
            "--policy-reload" => { args.policy_reload = true; i += 1; }
            "--peak-report" => { args.peak_report = true; i += 1; }
            "--checkpoint-bytes" => {
                let n: u64 = argv.get(i + 1).ok_or("--checkpoint-bytes needs a value")?
                    .parse().map_err(|_| "--checkpoint-bytes must be a positive integer")?;
                if n == 0 { return Err("--checkpoint-bytes must be a positive integer".into()); }
                args.checkpoint_bytes = Some(n);
                i += 2;
            }
            "--rotate-bytes" => {
                let n: u64 = argv.get(i + 1).ok_or("--rotate-bytes needs a value")?
                    .parse().map_err(|_| "--rotate-bytes must be a positive integer")?;
                if n == 0 { return Err("--rotate-bytes must be a positive integer".into()); }
                args.rotate_bytes = Some(n);
                i += 2;
            }
            "--compact-bytes" => {
                let n: u64 = argv.get(i + 1).ok_or("--compact-bytes needs a value")?
                    .parse().map_err(|_| "--compact-bytes must be a positive integer")?;
                if n == 0 { return Err("--compact-bytes must be a positive integer".into()); }
                args.compact_bytes = Some(n);
                i += 2;
            }
            "--region-mode" => {
                let name = argv.get(i + 1).ok_or("--region-mode needs a value")?;
                args.region_mode = Some(RegionMode::from_name(name)
                    .ok_or_else(|| format!("unknown region mode {name:?} (global|per_region)"))?);
                i += 2;
            }
            "--no-tiles" => { args.discover = false; i += 1; }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if args.rates.is_empty() { return Err("--rate-rps is required".into()); }
    if args.rates.iter().any(|r| !(r.is_finite() && *r > 0.0)) {
        return Err("every rate must be finite and positive".into());
    }
    if args.window_ms == 0 { return Err("--window-ms must be positive".into()); }
    if args.commit_batch.is_some() && args.group_requests.is_some() {
        return Err("--commit-batch and --group-requests are different workloads; pick one".into());
    }
    if args.policy_reload && args.policy_file.is_none() {
        return Err("--policy-reload needs --policy-file (or FLASHDB_POLICY_FILE)".into());
    }
    // Rotation retires only a fully published, empty memtable (`Engine::rotate_wal`); with
    // `--checkpoint-bytes` the pair is a self-maintaining write load, and alone it only
    // succeeds on an instance that is already fully published (otherwise it fails closed).
    Ok(args)
}

/// Which rate applies to request `index` (0-based). Segments are equal-sized by request count
/// when `segment_requests` is given, otherwise the whole trace is one segment per rate.
fn rate_for(index: u64, total: u64, rates: &[f64], segment_requests: Option<u64>) -> f64 {
    if rates.len() == 1 { return rates[0]; }
    if let Some(size) = segment_requests {
        let seg = (index / size.max(1)) as usize;
        return rates[seg.min(rates.len() - 1)];
    }
    let seg = (index * rates.len() as u64 / total.max(1)) as usize;
    rates[seg.min(rates.len() - 1)]
}

/// Resolve the CLI target. A directory opens tile-aware with `Engine::open_discover` unless
/// `discover` is false, in which case (and for a WAL file path) the engine is opened by WAL
/// path with tiles ignored: `--no-tiles` is the memtable-only control for the same data.
fn open_target(target: &str, discover: bool) -> io::Result<Engine> {
    let path = Path::new(target);
    if path.is_dir() {
        if discover { Engine::open_discover(path) } else { Engine::open(path.join("data.wal")) }
    } else {
        Engine::open(path)
    }
}

/// Directory the in-session device baseline writes its scratch file into.
fn probe_dir(target: &str) -> std::path::PathBuf {
    let path = Path::new(target);
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."))
    }
}

/// Nearest-rank percentile of a sorted slice, in microseconds. `None` when empty.
fn percentile_us(sorted_us: &[u64], q: f64) -> Option<u64> {
    if sorted_us.is_empty() { return None; }
    let rank = ((sorted_us.len() as f64) * q).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted_us.len() - 1);
    Some(sorted_us[idx])
}

/// The policy file the driver should read: `--policy-file` wins over the
/// `FLASHDB_POLICY_FILE` env var; `None` means the engine keeps its default
/// policy (today's behaviour).
fn policy_file_path(cli: Option<&str>) -> Option<String> {
    cli.map(str::to_string)
        .or_else(|| std::env::var("FLASHDB_POLICY_FILE").ok().filter(|s| !s.is_empty()))
}

/// Read and parse the JSON policy file at `path`. The schema and its reject-on-
/// invalid error policy are documented on [`LayoutPolicy`].
fn read_policy(path: &str) -> io::Result<LayoutPolicy> {
    LayoutPolicy::from_json_file(Path::new(path))
}

/// Frozen machine-readable window line for `--metrics-json`. Top-level fields:
/// `window`, `rate_rps`, `requests` (this window), `completed`/`errors`
/// (cumulative), `p50_us`/`p99_us` (this window), `policy` (the active
/// per-region policy, see [`LayoutPolicy::to_json`]), then `current` and
/// `previous` window snapshots. A window snapshot is `{"patterns", "regions"}`
/// as frozen by [`WindowSnapshot::to_json`]; `previous` is `null` for the first
/// window. Object key order is serde_json's sorted map, so two identical runs
/// emit identical lines.
fn metrics_json_line(
    window: u64,
    rate: f64,
    requests: u64,
    completed: u64,
    errors: u64,
    compact_bytes: u64,
    p50_us: u64,
    p99_us: u64,
    policy: serde_json::Value,
    current: &WindowSnapshot,
    previous: Option<&WindowSnapshot>,
) -> String {
    serde_json::json!({
        "window": window,
        "rate_rps": rate,
        "requests": requests,
        "completed": completed,
        "errors": errors,
        "compact_bytes": compact_bytes,
        "p50_us": p50_us,
        "p99_us": p99_us,
        "policy": policy,
        "current": current.to_json(),
        "previous": previous.map(WindowSnapshot::to_json),
    })
    .to_string()
}

/// Time `n` fsyncs on a scratch file in the engine's directory and report the distribution.
/// 0/N is reported as "none" rather than 0, so a failed probe is never read as "infinitely fast".
fn probe_device(dir: &Path, n: usize) -> (Option<u64>, Option<u64>) {
    use std::fs::OpenOptions;
    let path = dir.join("device-probe.tmp");
    let Ok(file) = OpenOptions::new().create(true).write(true).truncate(true).open(&path) else {
        return (None, None);
    };
    let mut samples = Vec::with_capacity(n);
    for i in 0..n {
        // MUST write before syncing: an earlier version truncated and synced without writing,
        // which is a no-op and reported a 0 us device (i.e. it silently invalidated the baseline).
        if std::io::Write::write_all(&mut &file, format!("probe {i}
").as_bytes()).is_err() { break; }
        let t = Instant::now();
        if file.sync_data().is_err() { break; }
        samples.push(t.elapsed().as_micros() as u64);
    }
    let _ = std::fs::remove_file(&path);
    samples.sort_unstable();
    (percentile_us(&samples, 0.50), percentile_us(&samples, 0.99))
}

/// One pacer-side write block for the `--commit-batch` workload: the trace's Order write (a P
/// doc plus its U `order_no`) built without the flow's read/guard phase, so B requests can be
/// planned before one commit barrier. This is a measurement workload, not the adapter's order
/// flow: `order_no` is unique per generated request, so a batch never conflicts and the block
/// is the same shape (P+U together) the adapter produces. `handle` must be unique across the
/// batch; the caller reserves a contiguous run from `next_handle`.
fn batch_write_block(req: &Value, handle: u64) -> io::Result<(Vec<u8>, Vec<Op>)> {
    let id = req["id"].as_str().filter(|s| !s.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "request id must be a nonempty string"))?;
    let params = &req["params"];
    let order_no = params["order_no"].as_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the batch workload needs params.order_no"))?;
    let doc = serde_json::json!({
        "order_no": order_no,
        "sku": params["sku"],
        "loc": params["loc"],
        "qty": params["qty"],
        "status": "open",
    });
    let ops = vec![
        Op::PutDoc {
            entity: b"Order".to_vec(),
            handle,
            doc: serde_json::to_vec(&doc).map_err(io::Error::other)?,
        },
        Op::PutUnique {
            entity: b"Order".to_vec(),
            field: b"order_no".to_vec(),
            value: order_no.as_bytes().to_vec(),
            handle,
        },
    ];
    Ok((id.as_bytes().to_vec(), ops))
}

/// Read the kernel's recorded peak resident set (`VmHWM`) from `/proc/self/status`, in KiB.
/// Diagnostic only (see `--peak-report`): separates the open-time replay peak from the load's own.
fn peak_rss_kb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix("VmHWM:").map(str::trim))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

/// Reset the kernel's `VmHWM` high-water mark (`clear_refs=5`), so a later `peak_rss_kb()` read
/// measures only the load that follows. Diagnostic only; a no-op where the file is absent.
fn reset_peak_rss() {
    let _ = std::fs::write("/proc/self/clear_refs", "5\n");
}

/// Bounded-WAL maintenance for a sustained write load: publish the serving memtable once the
/// retained WAL exceeds `checkpoint_bytes`, then retire the published segment onto a fresh one
/// once it exceeds `rotate_bytes`. Both mirror `cmd_seed_inventory`'s cadence; the effect is that a
/// multi-hour (or multi-thousand-request) write load leaves a bounded suffix, so a later
/// `open_discover` replays only that suffix instead of everything written since the last rotation.
/// `rotate_bytes` requires `checkpoint_bytes` (checked at parse time) because rotation is only
/// legal on a fully published, empty memtable. No format, read, write or durability change: this
/// is the same tested `maybe_checkpoint`/`rotate_wal` protocol the seeder already uses.
fn maintain_wal(engine: &mut Engine, checkpoint_bytes: Option<u64>, rotate_bytes: Option<u64>)
    -> io::Result<()>
{
    if let Some(n) = checkpoint_bytes { engine.maybe_checkpoint(n)?; }
    if let Some(n) = rotate_bytes { engine.rotate_wal(n)?; }
    Ok(())
}

/// One window's compaction actuation: the engine's existing `maybe_compact_shape`
/// with the shipped shape and the driver's tile gate at its floor, so the ACTIVE
/// per-region policy's `merge_min_tiles` chooses which regions merge. Returns the
/// merged output tile's bytes (`None` when no region was eligible), exactly what
/// `maybe_compact` returns - no format, manifest, WAL or tile change.
fn compact_window(engine: &mut Engine, budget_bytes: u64) -> io::Result<Option<u64>> {
    engine.maybe_compact_shape(budget_bytes, COMPACT_MIN_TILES, CompactShape::CheapestPair)
}

/// Return already-freed heap pages to the OS. The load path streams large transient buffers (each
/// `movements_scan` result is hundreds of KB), and glibc keeps the freed arena pages, which showed
/// up as a ~243 MB process peak on a 760 MB working set - enough to OOM under the small memory caps
/// the cold-cache axis comparison needs. Purely allocator hygiene: no semantics, no format.
fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        malloc_trim(0);
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("flashdb-load: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().collect();
    let args = parse_args(argv).map_err(io::Error::other)?;

    let lines: Vec<String> = {
        let f = BufReader::new(File::open(&args.trace)?);
        let mut v = Vec::new();
        for line in f.lines() {
            let line = line?;
            if line.len() as u64 > MAX_LINE { return Err(io::Error::other("trace line too long").into()); }
            if !line.trim().is_empty() { v.push(line); }
        }
        v
    };
    let total = lines.len() as u64;
    if total == 0 { return Err(io::Error::other("empty trace").into()); }

    // Device context first. On this shared box a single fsync varies by two orders of magnitude
    // depending on what else is running, so a load run without a device baseline cannot be
    // interpreted: "saturation" may be the device, not the engine. Same session, same directory.
    let device = probe_device(&probe_dir(&args.target), 100);
    let mut engine = open_target(&args.target, args.discover)?;
    // External policy control: apply the JSON policy file (CLI wins over the
    // `FLASHDB_POLICY_FILE` env var) before any work. A malformed file is a hard
    // error at start: the controller asked for a policy it did not get.
    let policy_path = policy_file_path(args.policy_file.as_deref());
    if let Some(path) = &policy_path {
        let policy = read_policy(path)
            .map_err(|e| io::Error::new(e.kind(), format!("policy file {path}: {e}")))?;
        engine.set_layout_policy(policy);
    }
    // Tile write mode: not persisted, so a runner that builds a per-region arm must set it on the
    // write load too, or every publish is a whole-keyspace tile (slice 24/32).
    if let Some(mode) = args.region_mode {
        engine.set_region_mode(mode);
    }
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "device_fsync_p50_us={} device_fsync_p99_us={}",
        device.0.map(|v| v.to_string()).unwrap_or_else(|| "none".into()),
        device.1.map(|v| v.to_string()).unwrap_or_else(|| "none".into()))?;
    // Tile-touch context: an empty serving memtable with active tiles means reads must come
    // from tiles; with --no-tiles the same data is fully replayed into the memtable.
    writeln!(out, "engine_open target={} discover={} active_tiles={} serving_memtable_entries={}",
        args.target, args.discover, engine.active_tile_cutoffs().len(), engine.serving_memtable_entries())?;
    writeln!(out, "engine_policy policy={}", engine.layout_policy().to_json())?;
    // Memory attribution diagnostic (off by default): report the peak RSS reached by open, then
    // reset the kernel's high-water mark so the summary line measures the LOAD's own peak. This
    // separates the open-time WAL-replay footprint from anything the request path retains.
    if args.peak_report {
        writeln!(out, "load_peak stage=open hwm_kb={}",
            peak_rss_kb().map(|v| v.to_string()).unwrap_or_else(|| "none".into()))?;
        reset_peak_rss();
    }

    let t0 = Instant::now();
    // `due_us` is ALWAYS the due time of the next request to issue, so an unissued request never
    // advances the schedule (an earlier version mutated it on the skip path and raced away).
    let mut index: u64 = 0;
    let mut due_us: f64 = 1_000_000.0 / rate_for(0, total, &args.rates, args.segment_requests);
    let mut late_total: u64 = 0;
    let mut completed: u64 = 0;
    let mut errors: u64 = 0;
    let mut else_used: u64 = 0;
    let mut first_error: Option<String> = None;
    let mut latencies: Vec<u64> = Vec::new();
    // schedule lag = how late a request was issued relative to its due time. The burst pacer
    // allows a bounded chunk (<= 1 ms), so a small floor is expected and is NOT saturation;
    // saturation is when the lag distribution runs away, which is what the growth rule detects.
    let mut lags: Vec<u64> = Vec::new();
    let mut window_index: u64 = 0;
    let mut window_start = Instant::now();
    let mut window_late_start: u64 = 0;
    let mut window_rate: f64 = rate_for(0, total, &args.rates, args.segment_requests);
    let mut late_growth_windows: u32 = 0;
    let mut invalid_windows: u64 = 0;
    let mut last_valid_rate: f64 = 0.0;
    let mut saturated_at: Option<f64> = None;
    // Compaction actuation accounting (Slice 37): driver-side bytes and merges
    // produced by `--compact-bytes`. The engine also records each merge in the
    // per-region `compaction_bytes`/`compaction_merges` counters (metrics.rs);
    // these two are the driver's own total for the `load_summary` line.
    let mut compact_bytes_total: u64 = 0;
    let mut compact_merges: u64 = 0;

    while index < total {
        let errors_before = errors;
        let now_us = t0.elapsed().as_secs_f64() * 1e6;
        if due_us <= now_us {
            let size = args.group_requests.or(args.commit_batch).unwrap_or(1).max(1);
            let end = (index + size).min(total);
            window_rate = rate_for(index, total, &args.rates, args.segment_requests);
            let started = Instant::now();
            if args.group_requests.is_some() {
                // Grouped request mode: the real adapter flow (`cust -> take` with its stock
                // guard, plus the read shapes) executed in order inside ONE deferred-barrier
                // group, so guards see earlier requests' effects and the group shares one fsync.
                let mut requests: Vec<Value> = Vec::with_capacity((end - index) as usize);
                for i in index..end {
                    requests.push(serde_json::from_str(&lines[i as usize])?);
                }
                match execute_requests_grouped(&mut engine, &requests) {
                    Ok(results) => {
                        for result in results {
                            match result {
                                Ok(blocks) => {
                                    completed += 1;
                                    // `take: failed` is the stock guard's else branch: proof the
                                    // workload exercised the in-order guard semantics, not puts.
                                    if blocks.get("take").is_some_and(|b| b.status == "failed") {
                                        else_used += 1;
                                    }
                                }
                                Err(e) => {
                                    errors += 1;
                                    if first_error.is_none() {
                                        first_error = Some(format!("request group at {index}: {e}"));
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // The group's own failure (open or barrier): nothing in it is durable.
                        errors += requests.len() as u64;
                        if first_error.is_none() { first_error = Some(format!("request group at {index}: {e}")); }
                    }
                }
            } else if args.commit_batch.is_none() {
                let request: Value = serde_json::from_str(&lines[index as usize])?;
                match execute_request(&mut engine, &request) {
                    Ok(blocks) => {
                        completed += 1;
                        // `take: failed` is the stock guard's else branch. Count it
                        // here too (the grouped path already does), so a single-request
                        // run still proves the workload exercised the guard.
                        if blocks.get("take").is_some_and(|b| b.status == "failed") { else_used += 1; }
                    }
                    Err(e) => {
                        errors += 1;
                        if first_error.is_none() {
                            // Diagnosability matters: a load that rejects requests must never be
                            // reported as sustained throughput, and the reason must be visible.
                            first_error = Some(format!("request {}: {e}", index));
                        }
                    }
                }
            } else {
                // Batch mode: plan every write block in the group first (reads see pre-batch
                // state), then make the accepted requests durable with ONE barrier. Each write
                // request is its own commit group, so a unique conflict drops that request alone
                // instead of the whole batch. Read-shaped requests still execute immediately and
                // commit nothing.
                let base = engine.next_handle()?;
                let mut requests: Vec<Vec<(Vec<u8>, Vec<Op>)>> = Vec::new();
                let mut request_indices: Vec<u64> = Vec::new();
                for i in index..end {
                    let request: Value = serde_json::from_str(&lines[i as usize])?;
                    let order_flow = request.get("blocks").and_then(Value::as_object)
                        .is_some_and(|b| b.contains_key("take"));
                    if order_flow {
                        match batch_write_block(&request, base + requests.len() as u64) {
                            Ok(block) => {
                                requests.push(vec![block]);
                                request_indices.push(i);
                            }
                            Err(e) => {
                                errors += 1;
                                if first_error.is_none() { first_error = Some(format!("request {i}: {e}")); }
                            }
                        }
                    } else {
                        match execute_request(&mut engine, &request) {
                            Ok(_) => completed += 1,
                            Err(e) => {
                                errors += 1;
                                if first_error.is_none() { first_error = Some(format!("request {i}: {e}")); }
                            }
                        }
                    }
                }
                if !requests.is_empty() {
                    match engine.commit_batch(&requests) {
                        Ok(outcomes) => {
                            // One outcome per request, in input order; each batch request carries
                            // exactly one block, so a dropped request is its own failure, not a
                            // group failure.
                            for (outcome, &i) in outcomes.into_iter().zip(&request_indices) {
                                match outcome {
                                    RequestOutcome::Committed(blocks) => {
                                        for block in blocks {
                                            match block {
                                                Outcome::Committed { .. } | Outcome::AlreadyCommitted { .. } => completed += 1,
                                                Outcome::Conflict(_) => {
                                                    errors += 1;
                                                    if first_error.is_none() {
                                                        first_error = Some(format!("request {i}: commit_batch conflict"));
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    RequestOutcome::Conflict(conflict) => {
                                        errors += 1;
                                        if first_error.is_none() {
                                            first_error = Some(format!("request {i}: {} conflict", conflict.kind));
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            errors += requests.len() as u64;
                            if first_error.is_none() { first_error = Some(format!("request group at {index}: {e}")); }
                        }
                    }
                }
            }
            // Bounded-WAL maintenance (opt-in): publish then rotate once the write load has grown
            // the suffix past the configured thresholds, exactly as `seed-inventory` does. With
            // neither flag set this is not called, so read runs pay nothing.
            if args.checkpoint_bytes.is_some() || args.rotate_bytes.is_some() {
                maintain_wal(&mut engine, args.checkpoint_bytes, args.rotate_bytes)?;
            }
            // One latency sample per request of the group: in batch mode every request in the
            // group shares the single commit barrier, so their latencies are the same value.
            let group_us = started.elapsed().as_micros() as u64;
            let issue_us = started.duration_since(t0).as_secs_f64() * 1e6;
            let lag = (issue_us - due_us).max(0.0) as u64;
            for _ in index..end {
                latencies.push(group_us);
                lags.push(lag);
            }
            if lag > LAG_SATURATION_US { late_total += end - index; }
            // Advance the open-loop schedule by every request in the group (the group's first
            // request was due at `due_us`; the next group starts one interval later per request).
            let mut i = index;
            while i < end {
                if i + 1 < total {
                    due_us += 1_000_000.0 / rate_for(i + 1, total, &args.rates, args.segment_requests);
                }
                i += 1;
            }
            index = end;
        } else {
            // burst pacing: bounded chunks, never a per-request sleep whose granularity would
            // masquerade as engine saturation (measured: 41 us service time vs a ~1 ms timer)
            let chunk = (due_us - now_us).min(1000.0);
            std::thread::sleep(Duration::from_secs_f64(chunk / 1e6));
        }

        if window_start.elapsed() >= Duration::from_millis(args.window_ms) {
            // Compaction actuation (Slice 37), BEFORE the policy reload and the
            // snapshot: the merge threshold that governed this window is the one
            // that governs its compaction, and the merged bytes are attributed to
            // this window's per-region `compaction_bytes`. Off when unset.
            let mut compact_bytes_window: u64 = 0;
            if let Some(budget) = args.compact_bytes {
                if let Some(merged) = compact_window(&mut engine, budget)? {
                    compact_bytes_window = merged;
                    compact_bytes_total += merged;
                    compact_merges += 1;
                }
            }
            latencies.sort_unstable();
            let p50 = percentile_us(&latencies, 0.50).unwrap_or(0);
            let p99 = percentile_us(&latencies, 0.99).unwrap_or(0);
            let mut sorted_lags = lags.clone();
            sorted_lags.sort_unstable();
            let lag_p50 = percentile_us(&sorted_lags, 0.50).unwrap_or(0);
            let lag_p99 = percentile_us(&sorted_lags, 0.99).unwrap_or(0);
            let behind = lag_p99 > LAG_SATURATION_US;
            let window_late = late_total - window_late_start;
            let ceiling_breached = args.ceiling_us.is_some_and(|c| p99 > c);
            if behind || window_late > 0 { late_growth_windows += 1 } else { late_growth_windows = 0 }
            let saturated = late_growth_windows >= 3;
            let window_error_count = errors - errors_before;
            let rejected = window_error_count > 0;
            let invalid = ceiling_breached || saturated || rejected;
            if invalid { invalid_windows += 1 } else { last_valid_rate = last_valid_rate.max(window_rate) }
            if saturated && saturated_at.is_none() { saturated_at = Some(window_rate) }
            if args.metrics_json {
                // Machine-readable window line: the window just measured is
                // `current` (it has not been rotated yet) and the one before it
                // is `previous`. Emitting JSON instead of the human line keeps
                // stdout parseable by the controller; the header and summary
                // lines are not `{`-prefixed and are ignored by a JSONL reader.
                // With `--policy-reload` the file is re-read here, before the
                // snapshot, so the controller sees the policy it just set.
                if args.policy_reload {
                    if let Some(path) = &policy_path {
                        match read_policy(path) {
                            Ok(policy) => engine.set_layout_policy(policy),
                            Err(e) => eprintln!("flashdb-load: policy reload {path} failed; keeping previous policy: {e}"),
                        }
                    }
                }
                let current = engine.metrics_snapshot();
                let previous = engine.metrics_previous_window();
                writeln!(out, "{}", metrics_json_line(
                    window_index, window_rate, latencies.len() as u64, completed, errors,
                    compact_bytes_window, p50, p99, engine.layout_policy().to_json(), &current,
                    previous.as_ref()))?;
                engine.metrics_rotate_window();
            } else {
                writeln!(out, "window={window_index} rate_rps={:.0} requests={} completed={completed} \
                    errors={errors} lag_p50_us={lag_p50} lag_p99_us={lag_p99} behind={behind} \
                    late_total={late_total} errors_window={window_error_count} p50_us={p50} p99_us={p99} \
                    invalid={invalid} compact_bytes={compact_bytes_window} ceiling_us={}",
                    window_rate, latencies.len(),
                    args.ceiling_us.map(|c| c.to_string()).unwrap_or_else(|| "none".into()))?;
                if args.patterns {
                    let snap = engine.metrics_rotate_window();
                    write!(out, "{snap}")?;
                } else {
                    engine.metrics_rotate_window();
                }
            }
            out.flush()?;
            latencies.clear();
            lags.clear();
            // keep the process footprint flat across windows (scan results churn hundred-KB Vecs)
            trim_heap();
            window_index += 1;
            window_start = Instant::now();
            window_late_start = late_total;
        }
    }

    let elapsed = t0.elapsed();
    let device_end = probe_device(&probe_dir(&args.target), 50);
    writeln!(out, "device_fsync_end_p50_us={} device_fsync_end_p99_us={}",
        device_end.0.map(|v| v.to_string()).unwrap_or_else(|| "none".into()),
        device_end.1.map(|v| v.to_string()).unwrap_or_else(|| "none".into()))?;
    let elapsed_ms = elapsed.as_millis();
    if args.peak_report {
        writeln!(out, "load_peak stage=end hwm_kb={}",
            peak_rss_kb().map(|v| v.to_string()).unwrap_or_else(|| "none".into()))?;
    }
    let achieved_rps = if elapsed.as_secs_f64() > 0.0 { completed as f64 / elapsed.as_secs_f64() } else { 0.0 };
    writeln!(out, "load_summary requests={total} completed={completed} errors={errors} \
        else_used={else_used} late_total={late_total} windows={window_index} invalid_windows={invalid_windows} \
        last_valid_rate_rps={last_valid_rate:.0} saturated_at_rps={} commit_batch={} group_requests={} \
        elapsed_ms={elapsed_ms} achieved_rps={achieved_rps:.0} compact_bytes_total={compact_bytes_total} \
        compact_merges={compact_merges} first_error={:?}",
        saturated_at.map(|r| format!("{r:.0}")).unwrap_or_else(|| "none".into()),
        args.commit_batch.unwrap_or(1),
        args.group_requests.map(|g| g.to_string()).unwrap_or_else(|| "none".into()),
        first_error.unwrap_or_else(|| "none".into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_segments_split_by_request_count_and_clamp() {
        let rates = [10.0, 20.0];
        assert_eq!(rate_for(0, 100, &rates, Some(10)), 10.0);
        assert_eq!(rate_for(9, 100, &rates, Some(10)), 10.0);
        assert_eq!(rate_for(10, 100, &rates, Some(10)), 20.0);
        assert_eq!(rate_for(999, 100, &rates, Some(10)), 20.0, "clamped to the last segment");
    }

    #[test]
    fn rate_segments_split_evenly_without_explicit_size() {
        let rates = [10.0, 20.0, 40.0];
        assert_eq!(rate_for(0, 300, &rates, None), 10.0);
        assert_eq!(rate_for(150, 300, &rates, None), 20.0);
        assert_eq!(rate_for(299, 300, &rates, None), 40.0);
    }

    #[test]
    fn percentile_uses_nearest_rank_and_handles_tails() {
        let xs = [1u64, 2, 3, 4, 5, 6, 7, 8, 9, 1000];
        assert_eq!(percentile_us(&xs, 0.50), Some(5));
        assert_eq!(percentile_us(&xs, 0.99), Some(1000));
        assert_eq!(percentile_us(&xs, 1.0), Some(1000));
        assert_eq!(percentile_us(&[], 0.99), None);
    }

    #[test]
    fn metrics_json_flag_is_parsed_off_by_default() {
        let mut argv = vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into()];
        assert!(!parse_args(argv.clone()).expect("valid args").metrics_json);
        argv.push("--metrics-json".into());
        assert!(parse_args(argv).expect("valid args").metrics_json);
    }

    #[test]
    fn peak_report_flag_is_parsed_off_by_default() {
        let mut argv = vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into()];
        assert!(!parse_args(argv.clone()).expect("valid args").peak_report);
        argv.push("--peak-report".into());
        assert!(parse_args(argv).expect("valid args").peak_report);
    }

    #[test]
    fn wal_maintenance_flags_are_parsed() {
        let argv = vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                        "--checkpoint-bytes".into(), "8388608".into(),
                        "--rotate-bytes".into(), "8388608".into()];
        let args = parse_args(argv).expect("valid maintenance args");
        assert_eq!(args.checkpoint_bytes, Some(8_388_608));
        assert_eq!(args.rotate_bytes, Some(8_388_608));
        let plain = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                     "--rate-rps".into(), "10".into()]).expect("valid args");
        assert_eq!(plain.checkpoint_bytes, None);
        assert_eq!(plain.rotate_bytes, None);
        // Rotate alone is legal: it bounds an already-published instance.
        assert_eq!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                    "--rotate-bytes".into(), "8388608".into()]).expect("rotate-only args").rotate_bytes,
            Some(8_388_608));
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--checkpoint-bytes".into(), "0".into()]).is_err(),
            "a zero checkpoint threshold is rejected");
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--rotate-bytes".into(), "0".into()]).is_err(),
            "a zero rotate threshold is rejected");
    }

    #[test]
    fn compact_bytes_flag_enables_actuation_and_rejects_zero() {
        let plain = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                     "--rate-rps".into(), "10".into()]).expect("valid args");
        assert_eq!(plain.compact_bytes, None, "actuation is off unless requested");
        let on = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                  "--rate-rps".into(), "10".into(),
                                  "--compact-bytes".into(), "67108864".into()]).expect("valid actuation args");
        assert_eq!(on.compact_bytes, Some(67_108_864));
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                 "--rate-rps".into(), "10".into(),
                                 "--compact-bytes".into(), "0".into()]).is_err(),
            "a zero merge budget is rejected");
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                 "--rate-rps".into(), "10".into(),
                                 "--compact-bytes".into()]).is_err(),
            "--compact-bytes needs a value");
    }

    #[test]
    fn region_mode_flag_selects_and_rejects() {
        let per = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                  "--region-mode".into(), "per_region".into()]).expect("valid region mode");
        assert_eq!(per.region_mode, Some(RegionMode::PerRegion));
        let global = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                      "--region-mode".into(), "global".into()]).expect("valid region mode");
        assert_eq!(global.region_mode, Some(RegionMode::Global));
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--region-mode".into(), "nonsense".into()]).is_err(),
            "an unknown region mode is rejected");
    }

    #[test]
    fn policy_file_and_reload_flags_are_parsed() {
        let argv = vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                        "--policy-file".into(), "/tmp/p.json".into(), "--policy-reload".into()];
        let args = parse_args(argv).expect("valid policy args");
        assert_eq!(args.policy_file.as_deref(), Some("/tmp/p.json"));
        assert!(args.policy_reload);
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--policy-reload".into()]).is_err(),
            "--policy-reload without a policy source is rejected");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--policy-file".into()]).is_err(),
            "--policy-file needs a value");
    }

    /// Slice 34: the driver's JSON dump carries the per-region compaction
    /// counters through unchanged, so a controller can read the region's rewrite
    /// factor (`compaction_bytes / bytes_written`) straight from the window.
    #[test]
    fn metrics_json_line_carries_per_region_compaction_fields() {
        let snap = WindowSnapshot {
            patterns: Vec::new(),
            regions: vec![flashdb_engine::metrics::RegionSummary {
                region: "P|Stock".to_string(),
                probes: 0, probe_misses: 0, bytes_read: 0, bytes_written: 40,
                commit_blocks: 1, commit_ops: 1,
                compaction_bytes: 4288, compaction_merges: 1,
                count: 0, p50_ns: 0.0, p99_ns: 0.0, mean_ns: 0.0,
            }],
        };
        let line = metrics_json_line(1, 100.0, 1, 1, 0, 0, 10, 20,
            LayoutPolicy::declared_default().to_json(), &snap, None);
        assert!(line.contains(r#""compaction_bytes":4288,"compaction_merges":1"#), "got {line}");
        assert!(line.contains(r#""bytes_written":40"#), "got {line}");
    }

    #[test]
    fn metrics_json_line_freezes_the_wrapper_field_names() {
        let empty = WindowSnapshot::default();
        let policy = LayoutPolicy::declared_default().to_json();
        // `compact_bytes` is the slice-37 driver-side field: the bytes merged by the
        // `--compact-bytes` actuation call at this window's boundary (0 when unset).
        let line = metrics_json_line(3, 120.0, 10, 9, 1, 0, 500, 1500, policy.clone(), &empty, None);
        assert_eq!(
            line,
            r#"{"compact_bytes":0,"completed":9,"current":{"patterns":[],"regions":[]},"errors":1,"p50_us":500,"p99_us":1500,"policy":{"default":{"bloom_bits_per_key":10,"merge_min_tiles":2,"page_size":4096},"regions":{}},"previous":null,"rate_rps":120.0,"requests":10,"window":3}"#
        );
        // A previous window is embedded as an object, not a string.
        let with_prev = metrics_json_line(4, 120.0, 10, 19, 1, 4288, 500, 1500, policy, &empty, Some(&empty));
        assert!(with_prev.contains(r#""compact_bytes":4288"#), "got {with_prev}");
        assert!(with_prev.contains(r#""previous":{"patterns":[],"regions":[]}"#), "got {with_prev}");
    }

    #[test]
    fn argument_parsing_rejects_bad_input() {
        let ok = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(),
                                 "--rate-rps".into(), "10,20".into(), "--window-ms".into(), "500".into()]);
        let ok = ok.expect("valid args");
        assert_eq!(ok.rates, vec![10.0, 20.0]);
        assert_eq!(ok.window_ms, 500);
        assert!(ok.discover, "tile-aware discovery is the default");
        let memtable = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                        "--rate-rps".into(), "10".into(), "--no-tiles".into()])
            .expect("valid memtable-control args");
        assert!(!memtable.discover, "--no-tiles forces the memtable-only open");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into()]).is_err(), "rate is required");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "0".into()]).is_err(), "zero rate rejected");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(), "--nope".into()]).is_err());
        let batched = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(),
                                      "--rate-rps".into(), "10".into(), "--commit-batch".into(), "64".into()])
            .expect("valid batch args");
        assert_eq!(batched.commit_batch, Some(64));
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(),
                                "--rate-rps".into(), "10".into(), "--commit-batch".into(), "0".into()]).is_err(),
            "a zero batch size is rejected");
        let grouped = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(),
                                      "--rate-rps".into(), "10".into(), "--group-requests".into(), "64".into()])
            .expect("valid group args");
        assert_eq!(grouped.group_requests, Some(64));
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--commit-batch".into(), "8".into(), "--group-requests".into(), "8".into()]).is_err(),
            "the two grouping modes are mutually exclusive");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--group-requests".into(), "0".into()]).is_err(),
            "a zero group size is rejected");
    }
}
