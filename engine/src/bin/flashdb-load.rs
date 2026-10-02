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
//!
//! `--metrics-json` prints one JSON object per window (instead of the human
//! `window=` line) so a controller can consume per-region and per-pattern
//! counters without parsing text. Its frozen schema is documented on
//! `metrics_json_line`.
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

use flashdb_engine::engine::{BatchOutcome, Engine, Op, Outcome};
use flashdb_engine::metrics::WindowSnapshot;
use flashdb_engine::replay::execute_requests_grouped;
use flashdb_engine::request::execute_request;
use serde_json::Value;

const MAX_LINE: u64 = 1 << 20;
/// A request issued more than this long after its scheduled time means the engine is not keeping
/// up with the offered rate. Well above the burst pacer's own <= 1 ms chunking floor.
const LAG_SATURATION_US: u64 = 10_000;

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
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    if argv.len() < 3 {
        return Err("usage: flashdb-load <dir-or-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N] [--window-ms N] [--ceiling-us N] [--patterns] [--metrics-json] [--commit-batch B] [--group-requests B] [--no-tiles]".into());
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

/// Frozen machine-readable window line for `--metrics-json`. Top-level fields:
/// `window`, `rate_rps`, `requests` (this window), `completed`/`errors`
/// (cumulative), `p50_us`/`p99_us` (this window), then `current` and `previous`
/// window snapshots. A window snapshot is `{"patterns", "regions"}` as frozen by
/// [`WindowSnapshot::to_json`]; `previous` is `null` for the first window. Object
/// key order is serde_json's sorted map, so two identical runs emit identical
/// lines.
fn metrics_json_line(
    window: u64,
    rate: f64,
    requests: u64,
    completed: u64,
    errors: u64,
    p50_us: u64,
    p99_us: u64,
    current: &WindowSnapshot,
    previous: Option<&WindowSnapshot>,
) -> String {
    serde_json::json!({
        "window": window,
        "rate_rps": rate,
        "requests": requests,
        "completed": completed,
        "errors": errors,
        "p50_us": p50_us,
        "p99_us": p99_us,
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
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "device_fsync_p50_us={} device_fsync_p99_us={}",
        device.0.map(|v| v.to_string()).unwrap_or_else(|| "none".into()),
        device.1.map(|v| v.to_string()).unwrap_or_else(|| "none".into()))?;
    // Tile-touch context: an empty serving memtable with active tiles means reads must come
    // from tiles; with --no-tiles the same data is fully replayed into the memtable.
    writeln!(out, "engine_open target={} discover={} active_tiles={} serving_memtable_entries={}",
        args.target, args.discover, engine.active_tile_cutoffs().len(), engine.serving_memtable_entries())?;

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
                // state), then make the whole group durable with ONE barrier. Read-shaped
                // requests still execute immediately and commit nothing.
                let base = engine.next_handle()?;
                let mut blocks: Vec<(Vec<u8>, Vec<Op>)> = Vec::new();
                for i in index..end {
                    let request: Value = serde_json::from_str(&lines[i as usize])?;
                    let order_flow = request.get("blocks").and_then(Value::as_object)
                        .is_some_and(|b| b.contains_key("take"));
                    if order_flow {
                        match batch_write_block(&request, base + blocks.len() as u64) {
                            Ok(block) => blocks.push(block),
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
                if !blocks.is_empty() {
                    match engine.commit_batch(&blocks) {
                        Ok(BatchOutcome::Committed(outcomes)) => {
                            for outcome in outcomes {
                                match outcome {
                                    Outcome::Committed { .. } | Outcome::AlreadyCommitted { .. } => completed += 1,
                                    Outcome::Conflict(_) => {
                                        errors += 1;
                                        if first_error.is_none() {
                                            first_error = Some(format!("request group at {index}: commit_batch conflict"));
                                        }
                                    }
                                }
                            }
                        }
                        Ok(BatchOutcome::Conflict { index: bad, conflict }) => {
                            errors += blocks.len() as u64;
                            if first_error.is_none() {
                                first_error = Some(format!("request group at {index}: block {bad} {} conflict", conflict.kind));
                            }
                        }
                        Err(e) => {
                            errors += blocks.len() as u64;
                            if first_error.is_none() { first_error = Some(format!("request group at {index}: {e}")); }
                        }
                    }
                }
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
                let current = engine.metrics_snapshot();
                let previous = engine.metrics_previous_window();
                writeln!(out, "{}", metrics_json_line(
                    window_index, window_rate, latencies.len() as u64, completed, errors,
                    p50, p99, &current, previous.as_ref()))?;
                engine.metrics_rotate_window();
            } else {
                writeln!(out, "window={window_index} rate_rps={:.0} requests={} completed={completed} \
                    errors={errors} lag_p50_us={lag_p50} lag_p99_us={lag_p99} behind={behind} \
                    late_total={late_total} errors_window={window_error_count} p50_us={p50} p99_us={p99} \
                    invalid={invalid} ceiling_us={}",
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
    let achieved_rps = if elapsed.as_secs_f64() > 0.0 { completed as f64 / elapsed.as_secs_f64() } else { 0.0 };
    writeln!(out, "load_summary requests={total} completed={completed} errors={errors} \
        else_used={else_used} late_total={late_total} windows={window_index} invalid_windows={invalid_windows} \
        last_valid_rate_rps={last_valid_rate:.0} saturated_at_rps={} commit_batch={} group_requests={} \
        elapsed_ms={elapsed_ms} achieved_rps={achieved_rps:.0} first_error={:?}",
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
    fn metrics_json_line_freezes_the_wrapper_field_names() {
        let empty = WindowSnapshot::default();
        let line = metrics_json_line(3, 120.0, 10, 9, 1, 500, 1500, &empty, None);
        assert_eq!(
            line,
            r#"{"completed":9,"current":{"patterns":[],"regions":[]},"errors":1,"p50_us":500,"p99_us":1500,"previous":null,"rate_rps":120.0,"requests":10,"window":3}"#
        );
        // A previous window is embedded as an object, not a string.
        let with_prev = metrics_json_line(4, 120.0, 10, 19, 1, 500, 1500, &empty, Some(&empty));
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
