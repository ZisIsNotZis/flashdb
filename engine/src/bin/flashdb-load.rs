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
//!   flashdb-load <existing-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N]
//!                [--window-ms 1000] [--ceiling-us N] [--patterns]
//!
//! Exit status is 0 even when the engine is saturated: saturation is data, not an error. A
//! trace/parse failure is an error (non-zero), because it means the measurement is invalid.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use flashdb_engine::engine::Engine;
use flashdb_engine::request::execute_request;
use serde_json::Value;

const MAX_LINE: u64 = 1 << 20;

struct Args {
    wal: String,
    trace: String,
    rates: Vec<f64>,
    segment_requests: Option<u64>,
    window_ms: u64,
    ceiling_us: Option<u64>,
    patterns: bool,
}

fn parse_args(argv: Vec<String>) -> Result<Args, String> {
    if argv.len() < 3 {
        return Err("usage: flashdb-load <existing-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N] [--window-ms N] [--ceiling-us N] [--patterns]".into());
    }
    let mut args = Args {
        wal: argv[1].clone(),
        trace: argv[2].clone(),
        rates: Vec::new(),
        segment_requests: None,
        window_ms: 1000,
        ceiling_us: None,
        patterns: false,
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
            "--patterns" => { args.patterns = true; i += 1; }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if args.rates.is_empty() { return Err("--rate-rps is required".into()); }
    if args.rates.iter().any(|r| !(r.is_finite() && *r > 0.0)) {
        return Err("every rate must be finite and positive".into());
    }
    if args.window_ms == 0 { return Err("--window-ms must be positive".into()); }
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

/// Nearest-rank percentile of a sorted slice, in microseconds. `None` when empty.
fn percentile_us(sorted_us: &[u64], q: f64) -> Option<u64> {
    if sorted_us.is_empty() { return None; }
    let rank = ((sorted_us.len() as f64) * q).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted_us.len() - 1);
    Some(sorted_us[idx])
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

    let mut engine = Engine::open(&args.wal)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let t0 = Instant::now();
    let mut due_us: f64 = 0.0;               // cumulative schedule, open-loop
    let mut late_total: u64 = 0;
    let mut completed: u64 = 0;
    let mut errors: u64 = 0;
    let mut latencies: Vec<u64> = Vec::new();
    let mut window_index: u64 = 0;
    let mut window_start = Instant::now();
    let mut window_late_start: u64 = 0;
    let mut late_growth_windows: u32 = 0;
    let mut invalid_windows: u64 = 0;
    // the highest rate at which a window stayed valid, and the rate that first saturated
    let mut last_valid_rate: f64 = 0.0;
    let mut saturated_at: Option<f64> = None;

    for (index, line) in lines.iter().enumerate() {
        let rate = rate_for(index as u64, total, &args.rates, args.segment_requests);
        due_us += 1_000_000.0 / rate;
        let now_us = t0.elapsed().as_secs_f64() * 1e6;
        if now_us < due_us {
            std::thread::sleep(Duration::from_secs_f64((due_us - now_us) / 1e6));
        } else {
            late_total += 1;
        }
        let request: Value = serde_json::from_str(line)?;
        let started = Instant::now();
        match execute_request(&mut engine, &request) {
            Ok(_) => completed += 1,
            Err(_) => errors += 1,
        }
        latencies.push(started.elapsed().as_micros() as u64);

        if window_start.elapsed() >= Duration::from_millis(args.window_ms) {
            latencies.sort_unstable();
            let p50 = percentile_us(&latencies, 0.50).unwrap_or(0);
            let p99 = percentile_us(&latencies, 0.99).unwrap_or(0);
            let window_late = late_total - window_late_start;
            let overdue = p99;
            let ceiling_breached = args.ceiling_us.is_some_and(|c| overdue > c);
            let lag_grew = window_late > 0;
            if lag_grew { late_growth_windows += 1 } else { late_growth_windows = 0 }
            let saturated = late_growth_windows >= 3;
            let invalid = ceiling_breached || saturated;
            if invalid { invalid_windows += 1 } else { last_valid_rate = last_valid_rate.max(rate) }
            if saturated && saturated_at.is_none() { saturated_at = Some(rate) }
            writeln!(out, "window={window_index} rate_rps={rate:.0} requests={} completed={completed} \
                errors={errors} late_window={window_late} late_total={late_total} p50_us={p50} \
                p99_us={p99} invalid={invalid} ceiling_us={}",
                latencies.len(),
                args.ceiling_us.map(|c| c.to_string()).unwrap_or_else(|| "none".into()))?;
            if args.patterns {
                let snap = engine.metrics_rotate_window();
                write!(out, "{snap}")?;
                out.flush()?;
            } else {
                engine.metrics_rotate_window();
            }
            latencies.clear();
            window_index += 1;
            window_start = Instant::now();
            window_late_start = late_total;
        }
    }

    writeln!(out, "load_summary requests={total} completed={completed} errors={errors} \
        late_total={late_total} windows={window_index} invalid_windows={invalid_windows} \
        last_valid_rate_rps={last_valid_rate:.0} saturated_at_rps={}",
        saturated_at.map(|r| format!("{r:.0}")).unwrap_or_else(|| "none".into()))?;
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
    fn argument_parsing_rejects_bad_input() {
        let ok = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(),
                                 "--rate-rps".into(), "10,20".into(), "--window-ms".into(), "500".into()]);
        let ok = ok.expect("valid args");
        assert_eq!(ok.rates, vec![10.0, 20.0]);
        assert_eq!(ok.window_ms, 500);
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into()]).is_err(), "rate is required");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "0".into()]).is_err(), "zero rate rejected");
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(), "--nope".into()]).is_err());
    }
}
