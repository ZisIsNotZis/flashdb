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
//! Arrivals model (latency trust). A request becomes READY at its scheduled due time. A group is
//! formed only from requests that are ALREADY READY (never future-scheduled ones), capped at
//! `--group-requests`, and is dispatched as soon as at least one request is ready - it is never
//! held open to fill the group. Each request's latency is measured PER REQUEST from ITS OWN due
//! time to the group's completion (`service_p50_us`/`service_p95_us`/`service_p99_us`), and its
//! schedule lag from due to issue (`lag_p50_us`/`lag_p95_us`/`lag_p99_us`); the group's own
//! elapsed time is reported only as the separate `group_elapsed_us` diagnostic. Two named ceilings
//! apply per window: `--ceiling-us` to the per-request service-latency p99 and `--lag-ceiling-us`
//! (default 10000, the pre-registration's "lag p99 <= 10 ms") to the per-request schedule-lag p99;
//! `reject_reason` says WHICH one rejected a window.
//!
//! usage:
//!   flashdb-load <dir-or-wal> <trace.jsonl> --rate-rps R[,R...] [--segment-requests N]
//!                [--window-ms 1000] [--ceiling-us N] [--lag-ceiling-us N] [--patterns]
//!                [--metrics-json] [--commit-batch B] [--group-requests B] [--no-tiles]
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

use std::collections::VecDeque;
use std::fs::File;
use std::path::Path;
use std::io::{self, BufRead, BufReader, Write};
use std::time::{Duration, Instant};

use flashdb_engine::engine::{build_compaction_job, BarrierCompletion, CompactShape, CompactionJob, CompactionOutcome, Engine, LayoutPolicy, Op, Outcome, RegionMode, RequestOutcome, MAX_PENDING_BARRIERS};
use flashdb_engine::keys::KeyEncoding;
use flashdb_engine::metrics::WindowSnapshot;
use flashdb_engine::replay::submit_requests_grouped;
use flashdb_engine::request::execute_request;
use serde_json::Value;

const MAX_LINE: u64 = 1 << 20;
/// Default schedule-lag ceiling in microseconds. This is the pre-registration's own number
/// (`.scratch/15-objective-v0/pre-registration.md`: "lag p99 <= 10 ms"); `--lag-ceiling-us`
/// overrides it. A window whose per-request schedule-lag p99 exceeds it is rejected and its rung
/// cannot be scored sustained.
const LAG_SATURATION_US: u64 = 10_000;
/// A request is "early" only if it was issued strictly before its due time by more than this
/// tolerance. The scheduler never forms a group from a not-yet-ready request, so the count is 0;
/// it is emitted so a regression (consuming future-scheduled requests) is visible in the output.
const EARLY_TOLERANCE_US: f64 = 0.0;
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
    lag_ceiling_us: u64,
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
    key_encoding: KeyEncoding,
    compact_bytes: Option<u64>,
    /// Host-stall control (slice 55): `--dry-run` replaces a request's engine
    /// execution with a bounded busy-wait (`--dry-run-busy-us`) after parsing
    /// the trace line, so the pacer/window bookkeeping is exercised with no
    /// engine I/O (no reads, writes, or fdatasync).
    dry_run: bool,
    dry_run_busy_us: u64,
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
        lag_ceiling_us: LAG_SATURATION_US,
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
        key_encoding: KeyEncoding::FamilyFirst,
        compact_bytes: None,
        dry_run: false,
        dry_run_busy_us: 0,
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
            "--lag-ceiling-us" => {
                let n: u64 = argv.get(i + 1).ok_or("--lag-ceiling-us needs a value")?
                    .parse().map_err(|_| "--lag-ceiling-us must be a positive integer")?;
                if n == 0 { return Err("--lag-ceiling-us must be a positive integer".into()); }
                args.lag_ceiling_us = n;
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
            "--dry-run" => { args.dry_run = true; i += 1; }
            "--dry-run-busy-us" => {
                let n: u64 = argv.get(i + 1).ok_or("--dry-run-busy-us needs a value")?
                    .parse().map_err(|_| "--dry-run-busy-us must be a non-negative integer")?;
                args.dry_run_busy_us = n;
                i += 2;
            }
            "--region-mode" => {
                let name = argv.get(i + 1).ok_or("--region-mode needs a value")?;
                args.region_mode = Some(RegionMode::from_name(name)
                    .ok_or_else(|| format!("unknown region mode {name:?} (global|per_region)"))?);
                i += 2;
            }
            "--key-encoding" => {
                let name = argv.get(i + 1).ok_or("--key-encoding needs a value")?;
                args.key_encoding = match name.as_str() {
                    "family_first" | "family-first" => KeyEncoding::FamilyFirst,
                    "entity_first" | "entity-first" => KeyEncoding::EntityFirst,
                    _ => return Err(format!("unknown key encoding {name:?} (family_first|entity_first)")),
                };
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
fn open_target(target: &str, discover: bool, encoding: KeyEncoding) -> io::Result<Engine> {
    let path = Path::new(target);
    if path.is_dir() {
        if discover {
            Engine::open_discover_with_encoding(path, encoding)
        } else {
            Engine::open_with_encoding(path.join("data.wal"), encoding)
        }
    } else {
        Engine::open_with_encoding(path, encoding)
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

/// One offered interval (us) for request `i`.
fn interval_us(i: u64, total: u64, rates: &[f64], segment_requests: Option<u64>) -> f64 {
    1_000_000.0 / rate_for(i, total, rates, segment_requests)
}

/// How many requests from `index` form the next group, plus the due time of the request after it.
///
/// The arrivals model: a request becomes READY at its scheduled due time. A group is formed only
/// from requests that are ALREADY READY (`due <= now_us`), never a future-scheduled one, capped at
/// `size` and never straddling an offered-rate boundary, and is dispatched as soon as at least one
/// request is ready. It is never held open to fill `size`; a whole group is only possible when the
/// engine has fallen behind and several requests are already past due. Returns `(end,
/// next_due_us)` with `index < end`, where `next_due_us` is the due time of request `end`.
fn ready_group(index: u64, total: u64, size: u64, due_us: f64, now_us: f64,
               rates: &[f64], segment_requests: Option<u64>) -> (u64, f64) {
    let window_rate = rate_for(index, total, rates, segment_requests);
    let cap = (index + size).min(total);
    let mut end = index;
    let mut d = due_us;
    while end < cap {
        // Never consume a future-scheduled request: only requests due at or before `now_us`.
        if d > now_us { break; }
        if end > index && rate_for(end, total, rates, segment_requests) != window_rate { break; }
        end += 1;
        if end < total { d += interval_us(end, total, rates, segment_requests); }
    }
    (end, d)
}

/// Which rule rejected a window, as a stable string, or `None` when the window is valid. The
/// service and lag ceilings are named separately so a report can say WHICH bound failed; a rung
/// cannot be scored sustained on either.
fn reject_reason(service_ceiling_breached: bool, lag_ceiling_breached: bool,
                 saturated: bool, rejected: bool) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if service_ceiling_breached { parts.push("service_ceiling"); }
    if lag_ceiling_breached { parts.push("lag_ceiling"); }
    // The lag-growth saturation rule is a distinct signal from a single lag-ceiling breach; keep
    // it visible even though the ceiling usually fires first.
    if saturated && !lag_ceiling_breached { parts.push("lag_saturation"); }
    if rejected { parts.push("errors"); }
    if parts.is_empty() { None } else { Some(parts.join("+")) }
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

/// One `--metrics-json` window line. A struct rather than a long positional argument list, so it
/// is obvious which measurement each field carries.
///
/// Latency model: a request arrives (becomes READY) at its scheduled due time and is issued in a
/// group at or after that time, so every measured latency includes its own waiting from arrival.
/// `service_p50_us`/`service_p95_us`/`service_p99_us` (explicit) and their legacy aliases
/// `p50_us`/`p95_us`/`p99_us` are PER-REQUEST service-latency percentiles (group completion minus
/// the request's OWN due time), and `lag_p50_us`/`lag_p95_us`/`lag_p99_us` are the PER-REQUEST
/// schedule-lag percentiles (issue time minus due time). `group_elapsed_us` is a separate
/// diagnostic (the largest group completion minus its first member's due time); it bounds the
/// per-request latencies and is NEVER presented as one.
///
/// Frozen top-level fields: `window`, `rate_rps`, `requests` (this window), `completed`/`errors`
/// (cumulative), the per-request service and lag percentiles above, `min_lag_us` (the window's
/// absolute minimum per-request lag), `group_elapsed_us`, `groups`/`mean_group_size`/
/// `max_group_size` (the window's group statistics; `mean_group_size == requests / groups`),
/// `groups_over_service_ceiling` (how many groups' elapsed exceeded `service_ceiling_us`),
/// `early_dispatches`, `window_us` (the window's actual wall time), `invalid` (the pacer's
/// rejection flag), `reject_reason` (WHICH rule rejected it, `null` when valid),
/// `service_ceiling_us`/`service_ceiling_breached` and `lag_ceiling_us`/`lag_ceiling_breached`,
/// `compact_bytes` (this window's actuation output), `policy` (the active per-region policy, see
/// [`LayoutPolicy::to_json`]), then `current` and `previous` window snapshots. A window snapshot
/// is `{"patterns", "regions"}` as frozen by [`WindowSnapshot::to_json`]; `previous` is `null` for
/// the first window. Object key order is serde_json's sorted map, so two identical runs emit
/// identical lines.
///
/// `invalid`, the reject reason and `window_us` must be on the JSON line, not only the human
/// `window=` line: a `--metrics-json` consumer builds the capacity curve from these lines, and
/// without the rejection flag it can score a saturated rung as sustained.
struct WindowLine<'a> {
    window: u64,
    rate: f64,
    requests: u64,
    completed: u64,
    errors: u64,
    compact_bytes: u64,
    /// Per-request service latency (completion minus the request's own due time) percentiles.
    /// Emitted under the explicit `service_pNN_us` names and the legacy `pNN_us` spellings.
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    /// Per-request schedule-lag (issue minus due) percentiles, plus the window's ABSOLUTE
    /// minimum lag. The percentiles are clamped at 0 so a negative (early) lag would be hidden;
    /// `min_lag_us` is the smallest per-request sample and is emitted so the floor is verifiable.
    lag_p50_us: u64,
    lag_p95_us: u64,
    lag_p99_us: u64,
    min_lag_us: u64,
    /// Largest group completion minus its first member's due time. Diagnostic, not a latency.
    group_elapsed_us: u64,
    /// Groups dispatched this window, their mean and max size. `mean_group_size` is
    /// `requests / groups` (0 when no group was dispatched), so a verifier can recompute it.
    groups: u64,
    mean_group_size: f64,
    max_group_size: u64,
    /// Groups whose elapsed (`group_elapsed_us` sample) exceeded `service_ceiling_us` this
    /// window; 0 when no `--ceiling-us` was given. Distinguishes a single slow group from the
    /// whole window stalling.
    groups_over_service_ceiling: u64,
    /// Requests issued before their due time by more than `EARLY_TOLERANCE_US` (always 0).
    early_dispatches: u64,
    window_us: u64,
    invalid: bool,
    reject_reason: Option<String>,
    service_ceiling_us: Option<u64>,
    service_ceiling_breached: bool,
    lag_ceiling_us: u64,
    lag_ceiling_breached: bool,
    policy: serde_json::Value,
    current: &'a WindowSnapshot,
    previous: Option<&'a WindowSnapshot>,
}

fn metrics_json_line(w: WindowLine) -> String {
    serde_json::json!({
        "window": w.window,
        "rate_rps": w.rate,
        "requests": w.requests,
        "completed": w.completed,
        "errors": w.errors,
        "compact_bytes": w.compact_bytes,
        "service_p50_us": w.p50_us,
        "service_p95_us": w.p95_us,
        "service_p99_us": w.p99_us,
        "p50_us": w.p50_us,
        "p95_us": w.p95_us,
        "p99_us": w.p99_us,
        "lag_p50_us": w.lag_p50_us,
        "lag_p95_us": w.lag_p95_us,
        "lag_p99_us": w.lag_p99_us,
        "min_lag_us": w.min_lag_us,
        "group_elapsed_us": w.group_elapsed_us,
        "groups": w.groups,
        "mean_group_size": w.mean_group_size,
        "max_group_size": w.max_group_size,
        "groups_over_service_ceiling": w.groups_over_service_ceiling,
        "early_dispatches": w.early_dispatches,
        "window_us": w.window_us,
        "invalid": w.invalid,
        "reject_reason": w.reject_reason,
        "service_ceiling_us": w.service_ceiling_us,
        "service_ceiling_breached": w.service_ceiling_breached,
        "lag_ceiling_us": w.lag_ceiling_us,
        "lag_ceiling_breached": w.lag_ceiling_breached,
        "policy": w.policy,
        "current": w.current.to_json(),
        "previous": w.previous.map(WindowSnapshot::to_json),
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

/// Cumulative kernel/allocator counters sampled once per window and differenced at each
/// boundary, so a window's stall can be attributed to the work that window actually did.
/// `/proc/self/stat` fields 10/12 are the minor/major page-fault counts; `/proc/self/io`'s
/// `read_bytes` is the bytes the block layer actually fetched from storage (a page-cache miss)
/// while `rchar` is every byte handed to `read`/`pread` regardless of cache, so the ratio
/// `read_bytes`/`rchar` separates a cold page-in from a cache-hit read without touching the
/// read path. A missing file (non-Linux) yields zeros and the per-window line is still emitted.
#[derive(Clone, Copy, Default)]
struct ProcCounters {
    minflt: u64,
    majflt: u64,
    rchar: u64,
    read_bytes: u64,
    wchar: u64,
    write_bytes: u64,
    /// Voluntary context switches (`/proc/self/status`), differenced per window.
    vol_csw: u64,
    /// Non-voluntary context switches (`/proc/self/status`), differenced per
    /// window: a jump here coincides with the process being descheduled.
    invol_csw: u64,
    /// User + system CPU time in clock ticks (`/proc/self/stat` fields 14/15),
    /// differenced per window so an off-CPU stall (wall time with no CPU
    /// advance) is separable from a busy (on-CPU) stall.
    utime_ticks: u64,
    stime_ticks: u64,
}

impl ProcCounters {
    fn since(self, prev: ProcCounters) -> ProcCounters {
        ProcCounters {
            minflt: self.minflt.saturating_sub(prev.minflt),
            majflt: self.majflt.saturating_sub(prev.majflt),
            rchar: self.rchar.saturating_sub(prev.rchar),
            read_bytes: self.read_bytes.saturating_sub(prev.read_bytes),
            wchar: self.wchar.saturating_sub(prev.wchar),
            write_bytes: self.write_bytes.saturating_sub(prev.write_bytes),
            vol_csw: self.vol_csw.saturating_sub(prev.vol_csw),
            invol_csw: self.invol_csw.saturating_sub(prev.invol_csw),
            utime_ticks: self.utime_ticks.saturating_sub(prev.utime_ticks),
            stime_ticks: self.stime_ticks.saturating_sub(prev.stime_ticks),
        }
    }
}

fn read_proc_counters() -> ProcCounters {
    let mut c = ProcCounters::default();
    if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
        // `pid (comm) state ...`: the comm field may itself contain spaces or parens, so parse
        // from the LAST ')' (the comm close). Tokens after it: state(0), ppid(1), pgrp(2),
        // session(3), tty_nr(4), tpgid(5), flags(6), minflt(7), cminflt(8), majflt(9), ...
        if let Some(rp) = stat.rfind(')') {
            let rest: Vec<&str> = stat[rp + 1..].split_whitespace().collect();
            c.minflt = rest.get(7).and_then(|v| v.parse().ok()).unwrap_or(0);
            c.majflt = rest.get(9).and_then(|v| v.parse().ok()).unwrap_or(0);
            c.utime_ticks = rest.get(11).and_then(|v| v.parse().ok()).unwrap_or(0);
            c.stime_ticks = rest.get(12).and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        for line in status.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            match k {
                "voluntary_ctxt_switches" => c.vol_csw = v.trim().parse().unwrap_or(0),
                "nonvoluntary_ctxt_switches" => c.invol_csw = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    if let Ok(io) = std::fs::read_to_string("/proc/self/io") {
        for line in io.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim().parse::<u64>().unwrap_or(0);
            match k.trim() {
                "rchar" => c.rchar = v,
                "read_bytes" => c.read_bytes = v,
                "wchar" => c.wchar = v,
                "write_bytes" => c.write_bytes = v,
                _ => {}
            }
        }
    }
    c
}

/// Per-group attribution (slice 55): the driver's view of one dispatched group,
/// so a single-request stall can be attributed to the wait before dispatch, the
/// engine's WAL append/apply/barrier, the request-path WAL maintenance, or time
/// off-CPU (completion minus the named phases).
#[derive(Clone, Copy, Default)]
struct GroupPhaseReport {
    /// Schedule lag of the group's first member (issue minus its due time): the
    /// pacer was this late observing the request as ready.
    due_dispatch_us: u64,
    /// Sum of the engine's per-block WAL `append` time inside this group.
    append_us: u64,
    /// Sum of the engine's memtable `apply` time inside this group.
    apply_us: u64,
    /// The group's single WAL `fdatasync` (0 for a read-only group).
    fsync_us: u64,
    /// Request-path WAL maintenance (publish/rotate) charged to this group.
    maintain_us: u64,
    /// Group completion minus its first member's due time (the `group_us`
    /// diagnostic, which bounds every member's service latency).
    completion_us: u64,
}

/// One overlapped write group whose WAL barrier is still in flight (slice 56).
/// The pacer records the group's identity, issue time and per-request outcome
/// summary at dispatch, then finalizes it (acks + per-request samples) only when
/// its covering barrier completes. The retained state is small: no parsed
/// request bodies, no per-request results, just the counts and due-time cursor
/// needed to recompute each member's lag and service latency.
#[derive(Clone)]
struct PendingGroup {
    /// Group sequence number within the window (for the `group_phase` line).
    seq: u64,
    /// First request index in the trace.
    index: u64,
    /// Number of requests in the group.
    count: u64,
    /// Dispatch time (us since run start), for per-request schedule lag.
    issue_us: f64,
    /// Due time of the first member (us), for per-request latency.
    first_due_us: f64,
    /// Dispatch clock, for the group's completion elapsed time.
    started: Instant,
    /// Engine WAL append elapsed (us), drained at submit.
    append_us: u64,
    /// Engine memtable apply elapsed (us), drained at submit.
    apply_us: u64,
    /// Requests whose block executed successfully (acked when the barrier
    /// completes).
    ok_count: u64,
    /// Requests that failed before append (counted immediately).
    err_count: u64,
    /// Guard `else` branches among the successful requests.
    else_delta: u64,
    /// First per-request error string, if any.
    first_error: Option<String>,
}

/// One `group_phase` line per dispatched group, so the phase breakdown is
/// available per group (not only as the window's maximum) for attribution.
/// Emitted to stderr so the frozen metrics-JSON schema on stdout is unchanged.
fn group_phase_print(window_index: u64, seq: u64, requests: u64, p: GroupPhaseReport) {
    eprintln!(
        "group_phase window={window_index} seq={seq} requests={requests} \
         due_dispatch_us={} append_us={} apply_us={} fsync_us={} maintain_us={} completion_us={}",
        p.due_dispatch_us, p.append_us, p.apply_us, p.fsync_us, p.maintain_us, p.completion_us);
}

/// Burn `us` microseconds of wall clock without I/O (the host-stall control's
/// stand-in for a request's on-CPU cost). A spin loop keeps the thread runnable
/// so the pacer keeps its normal scheduling profile; `us == 0` is a no-op.
fn busy_wait_us(us: u64) {
    let target = Instant::now() + Duration::from_micros(us);
    while Instant::now() < target {
        std::hint::spin_loop();
    }
}

/// Per-window residual-attribution line (slice 54, extended slice 55): the
/// kernel/allocator counters differenced over the window (including the OS
/// context-switch and CPU-tick view) plus the window's ABSOLUTE stall maxima and
/// the stall group's phase breakdown, so a stall can be attributed without a
/// rerun. Emitted to stderr so the frozen metrics-JSON schema on stdout is
/// unchanged.
fn window_stall_print(window_index: u64, c: ProcCounters, trim_us: u64, group_fsync_us: u64,
                      maintain_us: u64, stall: GroupPhaseReport, max_sleep_us: u64,
                      max_sleep_request_us: u64, max_latency_us: u64, max_lag_us: u64,
                      max_group_us: u64) {
    eprintln!(
        "window_stall window={window_index} minflt={} majflt={} rchar={} read_bytes={} \
         wchar={} write_bytes={} vol_csw={} invol_csw={} utime_ticks={} stime_ticks={} \
         trim_us={trim_us} group_fsync_us={group_fsync_us} maintain_us={maintain_us} \
         stall_due_dispatch_us={} stall_append_us={} stall_apply_us={} stall_fsync_us={} \
         stall_maintain_us={} stall_completion_us={} \
         max_sleep_us={max_sleep_us} max_sleep_request_us={max_sleep_request_us} \
         max_latency_us={max_latency_us} max_lag_us={max_lag_us} max_group_us={max_group_us}",
        c.minflt, c.majflt, c.rchar, c.read_bytes, c.wchar, c.write_bytes,
        c.vol_csw, c.invol_csw, c.utime_ticks, c.stime_ticks,
        stall.due_dispatch_us, stall.append_us, stall.apply_us, stall.fsync_us,
        stall.maintain_us, stall.completion_us);
}

/// Record one dispatched group's per-request schedule-lag and service-latency
/// samples plus its `group_elapsed_us` diagnostic. Shared by the synchronous
/// paths (called immediately) and the overlapped path (called when the group's
/// covering barrier completes), so the two paths emit identical per-request
/// semantics; only WHEN completion is known differs.
#[allow(clippy::too_many_arguments)]
/// Record one dispatched group's per-request schedule-lag samples (issue minus
/// each member's own due time). Lag is fully determined AT DISPATCH, so the
/// overlapped path records it here (in the dispatch window) rather than at
/// barrier completion; that is what lets a window boundary stay non-blocking
/// without mis-attributing a group's lag to the wrong window.
#[allow(clippy::too_many_arguments)]
fn record_group_lag(
    first_due_us: f64,
    index: u64,
    count: u64,
    issue_us: f64,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    lag_ceiling_us: u64,
    early_dispatches_total: &mut u64,
    early_dispatches_window: &mut u64,
    late_total: &mut u64,
    lags: &mut Vec<u64>,
) {
    let mut member_due_us = first_due_us;
    for i in index..index + count {
        let raw_lag = issue_us - member_due_us;
        if raw_lag < -EARLY_TOLERANCE_US {
            *early_dispatches_total += 1;
            *early_dispatches_window += 1;
        }
        let lag = raw_lag.max(0.0) as u64;
        lags.push(lag);
        if lag > lag_ceiling_us { *late_total += 1; }
        member_due_us += interval_us(i + 1, total, rates, segment_requests);
    }
}

/// Record one dispatched group's per-request service-latency samples (each
/// member's own due time to the group's completion) and the group's
/// `group_elapsed_us` diagnostic. Service latency is completion-based, so the
/// overlapped path records it when the covering barrier completes - which, for
/// a rare cross-boundary barrier, is the NEXT window rather than the dispatch
/// window. That is the intended attribution: schedule lag is dispatch-scoped,
/// service latency is completion-scoped.
#[allow(clippy::too_many_arguments)]
fn record_group_service(
    first_due_us: f64,
    index: u64,
    count: u64,
    completion_us: f64,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    latencies: &mut Vec<u64>,
    group_elapseds: &mut Vec<u64>,
) {
    let mut member_due_us = first_due_us;
    for i in index..index + count {
        let latency = (completion_us - member_due_us).max(0.0) as u64;
        latencies.push(latency);
        member_due_us += interval_us(i + 1, total, rates, segment_requests);
    }
    group_elapseds.push((completion_us - first_due_us).max(0.0) as u64);
}

/// Ack one completed overlapped group and record its per-request SERVICE
/// samples (the lag samples were already recorded at dispatch). `completion` is
/// either a barrier completion observed in FIFO order, or a failed `fdatasync`;
/// a failure counts the whole group as errors (nothing acknowledged) and is
/// returned so the caller stops dispatching on a poisoned engine.
#[allow(clippy::too_many_arguments)]
fn finalize_one_pending_group(
    pending: &mut VecDeque<PendingGroup>,
    completion: io::Result<BarrierCompletion>,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    window_index: u64,
    completed: &mut u64,
    errors: &mut u64,
    else_used: &mut u64,
    first_error: &mut Option<String>,
    latencies: &mut Vec<u64>,
    group_elapseds: &mut Vec<u64>,
    window_fsync_sum: &mut u64,
    stall_group: &mut GroupPhaseReport,
) -> io::Result<()> {
    let group = pending.pop_front().expect("a pending group matches an in-flight barrier");
    let completion = match completion {
        Ok(c) => c,
        Err(e) => {
            *errors += group.count;
            if first_error.is_none() {
                *first_error = Some(format!("request group at {}: {e}", group.index));
            }
            return Err(e);
        }
    };
    *completed += group.ok_count;
    *errors += group.err_count;
    *else_used += group.else_delta;
    if first_error.is_none() {
        *first_error = group.first_error.clone();
    }
    // The group's completion is the instant its barrier's `fdatasync` returned
    // on the worker, NOT this finalize call (the finalize may be deferred to a
    // later window). Service latency therefore includes the barrier and nothing
    // after it.
    let group_us = completion.completed_at.duration_since(group.started).as_micros() as u64;
    let completion_us = group.issue_us + group_us as f64;
    record_group_service(
        group.first_due_us, group.index, group.count, completion_us,
        total, rates, segment_requests, latencies, group_elapseds,
    );
    *window_fsync_sum = window_fsync_sum.saturating_add(completion.fsync_us);
    let report = GroupPhaseReport {
        due_dispatch_us: (group.issue_us - group.first_due_us).max(0.0) as u64,
        append_us: group.append_us,
        apply_us: group.apply_us,
        fsync_us: completion.fsync_us,
        maintain_us: 0,
        completion_us: group_us,
    };
    group_phase_print(window_index, group.seq, group.count, report);
    if report.completion_us > stall_group.completion_us {
        *stall_group = report;
    }
    Ok(())
}

/// Block until the OLDEST in-flight overlapped barrier completes, then ack its
/// group and record its service samples. Used at the in-flight cap (memory
/// bound), before WAL maintenance, and at the end of the run - never at the
/// ordinary window boundary, which now drains non-blockingly so a slow barrier
/// cannot delay the next window's first dispatch.
#[allow(clippy::too_many_arguments)]
fn drain_one_pending_group(
    engine: &mut Engine,
    pending: &mut VecDeque<PendingGroup>,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    window_index: u64,
    completed: &mut u64,
    errors: &mut u64,
    else_used: &mut u64,
    first_error: &mut Option<String>,
    latencies: &mut Vec<u64>,
    group_elapseds: &mut Vec<u64>,
    window_fsync_sum: &mut u64,
    stall_group: &mut GroupPhaseReport,
) -> io::Result<()> {
    let completion = engine.wait_group_barrier();
    finalize_one_pending_group(
        pending, completion, total, rates, segment_requests, window_index,
        completed, errors, else_used, first_error, latencies, group_elapseds,
        window_fsync_sum, stall_group,
    )
}

/// Drain every ALREADY-COMPLETED overlapped barrier (non-blocking), acking and
/// recording service samples without ever waiting for a still-in-flight
/// `fdatasync`. Called on the ordinary window boundary: lag samples are already
/// recorded at dispatch, so the window's schedule-lag statistics are complete
/// without blocking, and a cross-boundary barrier contributes only its service
/// samples to the window where it actually completes.
#[allow(clippy::too_many_arguments)]
fn drain_completed_groups(
    engine: &mut Engine,
    pending: &mut VecDeque<PendingGroup>,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    window_index: u64,
    completed: &mut u64,
    errors: &mut u64,
    else_used: &mut u64,
    first_error: &mut Option<String>,
    latencies: &mut Vec<u64>,
    group_elapseds: &mut Vec<u64>,
    window_fsync_sum: &mut u64,
    stall_group: &mut GroupPhaseReport,
) -> io::Result<()> {
    while !pending.is_empty() {
        match engine.try_take_group_barrier()? {
            Some(completion) => finalize_one_pending_group(
                pending, Ok(completion), total, rates, segment_requests, window_index,
                completed, errors, else_used, first_error, latencies, group_elapseds,
                window_fsync_sum, stall_group,
            )?,
            None => break,
        }
    }
    Ok(())
}

/// Block until every in-flight overlapped barrier has completed and been
/// recorded, preserving submission order. Used before WAL maintenance (which
/// rebinds the WAL) and at the end of the run (so the summary is complete).
#[allow(clippy::too_many_arguments)]
fn drain_all_pending_groups(
    engine: &mut Engine,
    pending: &mut VecDeque<PendingGroup>,
    total: u64,
    rates: &[f64],
    segment_requests: Option<u64>,
    window_index: u64,
    completed: &mut u64,
    errors: &mut u64,
    else_used: &mut u64,
    first_error: &mut Option<String>,
    latencies: &mut Vec<u64>,
    group_elapseds: &mut Vec<u64>,
    window_fsync_sum: &mut u64,
    stall_group: &mut GroupPhaseReport,
) -> io::Result<()> {
    while !pending.is_empty() {
        drain_one_pending_group(
            engine, pending, total, rates, segment_requests, window_index,
            completed, errors, else_used, first_error, latencies, group_elapseds,
            window_fsync_sum, stall_group,
        )?;
    }
    Ok(())
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

/// An off-request-path compaction worker. The driver sends it a
/// [`CompactionJob`] and it builds the replacement tile with
/// [`build_compaction_job`] while the main loop keeps dispatching and
/// committing; the main loop later applies the finished outcome with
/// [`Engine::apply_compaction_job`]. One job in flight at a time keeps the
/// worker's reads from racing the engine's own tile unlinks.
struct CompactionWorker {
    tx: std::sync::mpsc::Sender<CompactionJob>,
    rx: std::sync::mpsc::Receiver<io::Result<CompactionOutcome>>,
    in_flight: bool,
}

impl CompactionWorker {
    fn spawn(dir: std::path::PathBuf) -> Self {
        let (tx, job_rx) = std::sync::mpsc::channel::<CompactionJob>();
        let (out_tx, rx) = std::sync::mpsc::channel::<io::Result<CompactionOutcome>>();
        std::thread::spawn(move || {
            for job in job_rx {
                let result = build_compaction_job(&dir, &job);
                if out_tx.send(result).is_err() { break; }
            }
        });
        CompactionWorker { tx, rx, in_flight: false }
    }

    fn busy(&self) -> bool { self.in_flight }

    fn submit(&mut self, job: CompactionJob) -> io::Result<()> {
        self.tx.send(job).map_err(io::Error::other)?;
        self.in_flight = true;
        Ok(())
    }

    /// A finished outcome, when one is ready. A failed build is returned as the
    /// engine error (fail closed) and the in-flight slot is cleared.
    fn try_take_done(&mut self) -> io::Result<Option<CompactionOutcome>> {
        match self.rx.try_recv() {
            Ok(Ok(outcome)) => { self.in_flight = false; Ok(Some(outcome)) }
            Ok(Err(e)) => { self.in_flight = false; Err(e) }
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.in_flight = false;
                Err(io::Error::other("compaction worker disconnected"))
            }
        }
    }

    /// Blocking variant used only at the end of the run, to apply the last
    /// in-flight build before the summary. A disconnected sender at this point
    /// means the worker thread is gone (it panicked or exited), so it is
    /// propagated as an error rather than silently treated as "no outcome".
    fn take_done(&mut self) -> io::Result<Option<CompactionOutcome>> {
        match self.rx.recv() {
            Ok(Ok(outcome)) => { self.in_flight = false; Ok(Some(outcome)) }
            Ok(Err(e)) => { self.in_flight = false; Err(e) }
            Err(_) => { self.in_flight = false; Err(io::Error::other("compaction worker disconnected")) }
        }
    }
}

/// Per-applied-actuation observability (slice 53): one `compact_actuation` line
/// per merge, carrying the off-request-path build phase split plus the
/// request-path run-selection and lazy-apply times, so the remaining stall can
/// be attributed without a separate probe.
fn compact_actuation_print(window_index: u64, select_us: u64, apply_us: u64, outcome: &CompactionOutcome) {
    let s = &outcome.stats;
    eprintln!(
        "compact_actuation window={window_index} input_bytes={} output_bytes={} \
         select_us={select_us} apply_us={apply_us} build_elapsed_us={} \
         stream_us={} finish_us={} verify_us={} dir_sync_us={}",
        outcome.input_bytes, outcome.output_bytes, s.elapsed_us,
        s.build.stream_us, s.build.finish_us, s.build.verify_us, s.dir_sync_us);
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
    let mut engine = open_target(&args.target, args.discover, args.key_encoding)?;
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

    // Off-request-path compaction worker (slice 53): builds one replacement tile
    // at a time while the request path keeps committing. Only spawned when the
    // `--compact-bytes` actuator is on; its channel closes (and the worker exits)
    // when the driver drops it at the end of `run`.
    let mut compaction_worker = args.compact_bytes.map(|_| CompactionWorker::spawn(probe_dir(&args.target)));

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
    // schedule lag = how late a request was issued relative to its due time, PER REQUEST. The
    // pacer dispatches as soon as a request is ready, so a small floor is expected and is NOT
    // saturation; saturation is when the lag distribution runs away (the growth rule).
    let mut lags: Vec<u64> = Vec::new();
    // Per-group elapsed time (completion minus the group's first member's due time), a diagnostic
    // only: it bounds the window's per-request service latencies and is never one of them.
    let mut group_elapseds: Vec<u64> = Vec::new();
    let mut window_index: u64 = 0;
    let mut window_start = Instant::now();
    // Kernel/allocator counters sampled at the same instant as the window clock, so the first
    // window's delta covers only the load that follows (not open/replay).
    let mut proc_prev = read_proc_counters();
    // Per-window stall attribution (slice 55): the dispatched group with the
    // largest completion bounds the window's single-request stall; its phase
    // breakdown plus the pacer's worst sleep overshoot make the stall
    // attributable without a rerun.
    let mut stall_group: GroupPhaseReport = GroupPhaseReport::default();
    let mut window_fsync_sum: u64 = 0;
    // Overlapped-barrier state (slice 56): write groups whose barrier is still
    // in flight, in submission order, and the window's request-path WAL
    // maintenance time (drained from the group's own phases).
    let mut pending: VecDeque<PendingGroup> = VecDeque::new();
    let mut window_maintain_us: u64 = 0;
    let mut max_sleep_us: u64 = 0;
    let mut max_sleep_request_us: u64 = 0;
    let mut window_late_start: u64 = 0;
    // Boundary-vs-read lag-breach attribution (slice 57): the window-boundary
    // drain is now non-blocking, so `boundary_drain_prev_us` (the time the
    // PREVIOUS boundary's drain blocked the pacer) should be ~0; when it is
    // large enough to breach the lag ceiling by itself, every breach in the
    // first group of THIS window is attributed to the boundary, and everything
    // else to the request-path reads. `window_read_us` is the measured
    // request-path probe/read time (group dispatch elapsed minus append/apply).
    let mut boundary_drain_prev_us: u64 = 0;
    let mut window_boundary_drain_us: u64;
    let mut first_group_count: u64 = 0;
    let mut window_read_us: u64 = 0;
    let mut window_rate: f64 = rate_for(0, total, &args.rates, args.segment_requests);
    let mut late_growth_windows: u32 = 0;
    let mut invalid_windows: u64 = 0;
    // The ceilings' own rejection counts, separate from `invalid_windows` (which also folds in lag
    // and request errors), so the report can state WHICH ceiling rejected a window.
    let mut service_ceiling_rejected_windows: u64 = 0;
    let mut lag_ceiling_rejected_windows: u64 = 0;
    let mut ceil_rejected_windows: u64 = 0;
    // Requests dispatched strictly before their due time (must stay 0: the scheduler never
    // consumes a not-yet-ready request). Emitted so a regression is visible in the output.
    let mut early_dispatches_total: u64 = 0;
    let mut early_dispatches_window: u64 = 0;
    // Group formation statistics for the current window: how many groups were dispatched and the
    // size of the largest. `requests` (the per-window latency sample count) is the group-size sum,
    // so the window's mean group size is `requests / groups`.
    let mut groups_window: u64 = 0;
    let mut max_group_size_window: u64 = 0;
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
        // True when the group dispatched this iteration owes an overlapped WAL
        // barrier still in flight: its per-request service samples and acks are
        // deferred until the barrier completes, so the shared inline measurement
        // below is skipped for it. Schedule-lag samples are still recorded AT
        // DISPATCH (see `lag_recorded`), because lag is fully determined then.
        let mut deferred_group = false;
        // True when this group's per-request schedule-lag samples were already
        // recorded at dispatch (the grouped path), so the inline measurement
        // block records only service latency, never lag twice.
        let mut lag_recorded = false;
        let now_us = t0.elapsed().as_secs_f64() * 1e6;
        // Finalize any barriers that completed since the last iteration, without
        // blocking: this frees in-flight slots AND records each group's service
        // samples in the window where it actually completed, so the ordinary
        // window-boundary drain only has to account for the sliver that finished
        // since the previous iteration (and never waits on a slow fsync).
        drain_completed_groups(
            &mut engine, &mut pending, total, &args.rates, args.segment_requests,
            window_index,
            &mut completed, &mut errors, &mut else_used, &mut first_error,
            &mut latencies, &mut group_elapseds,
            &mut window_fsync_sum, &mut stall_group)?;
        if due_us <= now_us {
            let size = args.group_requests.or(args.commit_batch).unwrap_or(1).max(1);
            window_rate = rate_for(index, total, &args.rates, args.segment_requests);
            // Arrivals model: form a group only from requests ALREADY READY (due <= now_us), never
            // future-scheduled ones, capped at `size` and never straddling an offered-rate
            // boundary (so every completion is attributed to the rung it was OFFERED under).
            // Dispatch immediately once at least one request is ready; never hold the group open
            // to fill it. `end_due_us` is the due time of the next unissued request.
            let (end, end_due_us) = ready_group(
                index, total, size, due_us, now_us, &args.rates, args.segment_requests);
            debug_assert!(end > index);
            groups_window += 1;
            max_group_size_window = max_group_size_window.max(end - index);
            let group_seq = groups_window;
            let started = Instant::now();
            // `issue_us` is the dispatch instant shared by every per-request
            // schedule-lag sample; it is branch-independent, so it is computed
            // once here rather than re-derived in each dispatch path.
            let issue_us = started.duration_since(t0).as_secs_f64() * 1e6;
            if group_seq == 1 { first_group_count = end - index; }
            if args.dry_run {
                // Host-stall control (slice 55): pace and parse the trace but do
                // no engine I/O. A bounded busy-wait stands in for the request's
                // on-CPU cost so the pacer sees a comparable runnable load
                // without any reads, writes, or fdatasync.
                for i in index..end {
                    serde_json::from_str::<Value>(&lines[i as usize])?;
                    busy_wait_us(args.dry_run_busy_us);
                }
            } else if args.group_requests.is_some() {
                // Grouped request mode (overlapped, slice 56): the real adapter
                // flow (`cust -> take` with its stock guard, plus the read
                // shapes) is executed in order inside ONE group, so guards see
                // earlier requests' effects. The group's WAL barrier is then
                // SUBMITTED to the engine's background worker rather than taken
                // synchronously: the pacer can begin group N+1 while group N's
                // fdatasync is in flight, so a device fsync tail lands in group
                // N's own service latency, not the next group's schedule lag.
                //
                // Enforce the bounded in-flight-barrier cap BEFORE submitting.
                while engine.pending_barriers() >= MAX_PENDING_BARRIERS {
                    drain_one_pending_group(
                        &mut engine, &mut pending, total, &args.rates, args.segment_requests,
                        window_index,
                        &mut completed, &mut errors, &mut else_used, &mut first_error,
                        &mut latencies, &mut group_elapseds,
                        &mut window_fsync_sum, &mut stall_group)?;
                }
                let mut requests: Vec<Value> = Vec::with_capacity((end - index) as usize);
                for i in index..end {
                    requests.push(serde_json::from_str(&lines[i as usize])?);
                }
                let (barrier, results) = submit_requests_grouped(&mut engine, &requests)
                    .map_err(|e| io::Error::new(e.kind(), format!("request group at {index}: {e}")))?;
                let (append_us, apply_us) = engine.take_group_append_apply_us();
                // Request-path probe/read time: the dispatch elapsed minus the
                // engine's WAL append and memtable apply (the barrier is only
                // SUBMITTED on this path, never taken). The remainder is the
                // synchronous `cust`/stock/order probes - the cold-read stall
                // this work package attributes and reduces.
                let submit_elapsed_us = started.elapsed().as_micros() as u64;
                let read_us = submit_elapsed_us.saturating_sub(append_us.saturating_add(apply_us));
                window_read_us = window_read_us.saturating_add(read_us);
                let mut ok_count = 0u64;
                let mut err_count = 0u64;
                let mut else_delta = 0u64;
                let mut grp_first_error: Option<String> = None;
                for result in &results {
                    match result {
                        Ok(blocks) => {
                            ok_count += 1;
                            // `take: failed` is the stock guard's else branch: proof the
                            // workload exercised the in-order guard semantics, not puts.
                            if blocks.get("take").is_some_and(|b| b.status == "failed") {
                                else_delta += 1;
                            }
                        }
                        Err(e) => {
                            err_count += 1;
                            if grp_first_error.is_none() {
                                grp_first_error = Some(format!("request group at {index}: {e}"));
                            }
                        }
                    }
                }
                // Schedule-lag samples are dispatch-scoped: record them now, in
                // the dispatch window, so the window-boundary drain can stay
                // non-blocking without mis-attributing lag.
                record_group_lag(
                    due_us, index, end - index, issue_us, total, &args.rates, args.segment_requests,
                    args.lag_ceiling_us,
                    &mut early_dispatches_total, &mut early_dispatches_window, &mut late_total, &mut lags,
                );
                lag_recorded = true;
                if barrier {
                    pending.push_back(PendingGroup {
                        seq: group_seq,
                        index,
                        count: end - index,
                        issue_us,
                        first_due_us: due_us,
                        started,
                        append_us,
                        apply_us,
                        ok_count,
                        err_count,
                        else_delta,
                        first_error: grp_first_error,
                    });
                    deferred_group = true;
                } else {
                    // Empty/read-only group: no barrier owed, so ack it now; the
                    // shared measurement block below still records its per-request
                    // samples (completion is immediate, so service latency is just
                    // the dispatch/read work).
                    completed += ok_count;
                    errors += err_count;
                    else_used += else_delta;
                    if first_error.is_none() { first_error = grp_first_error; }
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
            // neither flag set this is not called, so read runs pay nothing. It runs on the
            // request path and is timed separately. With overlapped barriers in flight the
            // publish/rotation paths refuse to run, so when a threshold is crossed, drain every
            // in-flight barrier first; below the thresholds the maintenance call is a no-op and
            // the barriers stay untouched.
            let wal_len = engine.wal_valid_len();
            let needs_maintenance_drain = args.checkpoint_bytes.is_some_and(|n| wal_len > n)
                || args.rotate_bytes.is_some_and(|n| wal_len > n);
            if needs_maintenance_drain {
                drain_all_pending_groups(
                    &mut engine, &mut pending, total, &args.rates, args.segment_requests,
                    window_index,
                    &mut completed, &mut errors, &mut else_used, &mut first_error,
                    &mut latencies, &mut group_elapseds,
                    &mut window_fsync_sum, &mut stall_group)?;
            }
            let maintain_t0 = Instant::now();
            if !args.dry_run && (args.checkpoint_bytes.is_some() || args.rotate_bytes.is_some()) {
                maintain_wal(&mut engine, args.checkpoint_bytes, args.rotate_bytes)?;
            }
            let maintain_us = maintain_t0.elapsed().as_micros() as u64;
            window_maintain_us = window_maintain_us.saturating_add(maintain_us);

            // Per-request measurement. A request's latency runs from ITS OWN due time (arrival)
            // to the group's completion, so each member of the group gets its own sample rather
            // than the group's elapsed time. The group elapsed (completion minus the FIRST
            // member's due time) bounds every member's latency and is kept separately as a
            // diagnostic. A dispatch before a request's due time would make `raw_lag` negative;
            // the scheduler never does that, so `early_dispatches` stays 0. For an overlapped
            // write group this is deferred to `drain_one_pending_group` (its completion is its
            // covering barrier's completion), so the inline path is skipped.
            if !deferred_group {
                let group_us = started.elapsed().as_micros() as u64;
                let completion_us = issue_us + group_us as f64;
                let first_due_us = due_us;
                // Per-group phase attribution (slice 55): drain the engine's
                // append/apply/barrier accumulators once per dispatched group, so a
                // single-request stall is attributable to the wait before dispatch,
                // the WAL append/apply/barrier, the WAL maintenance, or off-CPU time.
                let phase = engine.take_group_phase_us();
                window_fsync_sum = window_fsync_sum.saturating_add(phase.fsync_us);
                let report = GroupPhaseReport {
                    due_dispatch_us: (issue_us - first_due_us).max(0.0) as u64,
                    append_us: phase.append_us,
                    apply_us: phase.apply_us,
                    fsync_us: phase.fsync_us,
                    maintain_us,
                    completion_us: group_us,
                };
                group_phase_print(window_index, group_seq, end - index, report);
                if report.completion_us > stall_group.completion_us {
                    stall_group = report;
                }
                // The grouped path already recorded schedule-lag at dispatch;
                // the synchronous/dry-run/batch paths record it here (their
                // completion is immediate, so dispatch and completion coincide).
                if !lag_recorded {
                    record_group_lag(
                        due_us, index, end - index, issue_us, total, &args.rates, args.segment_requests,
                        args.lag_ceiling_us,
                        &mut early_dispatches_total, &mut early_dispatches_window, &mut late_total, &mut lags,
                    );
                }
                record_group_service(
                    due_us, index, end - index, completion_us, total, &args.rates, args.segment_requests,
                    &mut latencies, &mut group_elapseds,
                );
            }
            // Advance the open-loop schedule to the due time of the next unissued request.
            due_us = end_due_us;
            index = end;
        } else {
            // burst pacing: bounded chunks, never a per-request sleep whose granularity would
            // masquerade as engine saturation (measured: 41 us service time vs a ~1 ms timer).
            // Time each chunk so a host-level wakeup delay (a <=1 ms sleep actually taking
            // hundreds of ms) is attributable instead of looking like an engine stall.
            let chunk = (due_us - now_us).min(1000.0);
            let sleep_t0 = Instant::now();
            std::thread::sleep(Duration::from_secs_f64(chunk / 1e6));
            let sleep_us = sleep_t0.elapsed().as_micros() as u64;
            if sleep_us > max_sleep_us {
                max_sleep_us = sleep_us;
                max_sleep_request_us = chunk as u64;
            }
        }

        // A window must not span two offered rates: with `--segment-requests` the ladder steps at
        // request-count boundaries, and a wall-clock window that straddles one would credit the
        // previous rung's completions to the new rung (and vice versa). Close early at the
        // boundary so every completion in a window was offered at that window's rate. The
        // `latencies` guard means the split happens on the group that ENDS the rung, not on every
        // idle iteration while waiting for the next rung's first request (which must not emit
        // empty windows).
        let next_rate = if index < total {
            rate_for(index, total, &args.rates, args.segment_requests)
        } else {
            window_rate
        };
        // `index >= total` flushes the trailing partial window: without it the last group's
        // completions were dropped from the curve (the summary counted them, the windows did not).
        if index >= total
            || window_start.elapsed() >= Duration::from_millis(args.window_ms)
            || (next_rate != window_rate && groups_window > 0)
        {
            // Drain only ALREADY-COMPLETED overlapped barriers (non-blocking):
            // schedule-lag samples were recorded at dispatch, so the window's lag
            // statistics are complete without waiting, and a still-in-flight
            // barrier contributes only its service samples to the window where it
            // actually completes. This removes the boundary-drain stall that the
            // WP-E residual attributed to a fraction of the rung-50 breaches.
            let boundary_drain_t0 = Instant::now();
            drain_completed_groups(
                &mut engine, &mut pending, total, &args.rates, args.segment_requests,
                window_index,
                &mut completed, &mut errors, &mut else_used, &mut first_error,
                &mut latencies, &mut group_elapseds,
                &mut window_fsync_sum, &mut stall_group)?;
            window_boundary_drain_us = boundary_drain_t0.elapsed().as_micros() as u64;
            // Compaction actuation (Slice 37/53), BEFORE the policy reload and
            // the snapshot: the merge threshold that governed this window is the
            // one that governs its compaction, and the merged bytes are attributed
            // to this window's per-region `compaction_bytes`. Off when unset.
            // Slice 53 makes the build off-path: run selection + the lazy apply
            // stay on the request path (both are cheap), the tile build runs on
            // the worker thread.
            let mut compact_bytes_window: u64 = 0;
            if let Some(budget) = args.compact_bytes {
                // Skip selection+apply while a barrier is still in flight: the
                // engine's compaction paths refuse to run then, and deferring by
                // one window changes only WHEN a merge runs, never WHETHER.
                if engine.pending_barriers() == 0 {
                    if let Some(worker) = compaction_worker.as_mut() {
                        let mut select_us: u64 = 0;
                        if !worker.busy() {
                            let select_t0 = Instant::now();
                            if let Some(job) = engine.compaction_job(budget, COMPACT_MIN_TILES, CompactShape::CheapestPair)? {
                                select_us = select_t0.elapsed().as_micros() as u64;
                                worker.submit(job)?;
                            }
                        }
                        if let Some(outcome) = worker.try_take_done()? {
                            let apply_t0 = Instant::now();
                            let merged = engine.apply_compaction_job(&outcome)?;
                            let apply_us = apply_t0.elapsed().as_micros() as u64;
                            compact_bytes_window = merged;
                            compact_bytes_total += merged;
                            compact_merges += 1;
                            compact_actuation_print(window_index, select_us, apply_us, &outcome);
                        }
                    }
                }
            }
            // Actual wall time the window covered, emitted so a consumer can compare it with the
            // window's offered schedule span. The pacer dispatches a group as soon as its first
            // request is ready (not a filled burst), so the per-window delta over a nominal
            // `--window-ms` need not be the offered rate; the harness divides a rung's completions
            // by the LONGER of its offered span and its summed wall time, which the offered rate
            // bounds.
            let window_us = window_start.elapsed().as_micros() as u64;
            latencies.sort_unstable();
            let p50 = percentile_us(&latencies, 0.50).unwrap_or(0);
            let p95 = percentile_us(&latencies, 0.95).unwrap_or(0);
            let p99 = percentile_us(&latencies, 0.99).unwrap_or(0);
            let mut sorted_lags = lags.clone();
            sorted_lags.sort_unstable();
            let lag_p50 = percentile_us(&sorted_lags, 0.50).unwrap_or(0);
            let lag_p95 = percentile_us(&sorted_lags, 0.95).unwrap_or(0);
            let lag_p99 = percentile_us(&sorted_lags, 0.99).unwrap_or(0);
            // The group elapsed is a diagnostic (the largest group completion minus its first
            // member's due time); it bounds the per-request service latencies and is never one.
            let group_elapsed_us = group_elapseds.iter().copied().max().unwrap_or(0);
            // Group statistics: the window's request count (`lags.len()`) is the
            // sum of its dispatched group sizes (schedule-lag is recorded at
            // dispatch), so `mean_group_size == requests / groups`.
            // `groups_over_service_ceiling` counts groups whose elapsed exceeded
            // the service ceiling, so a reader can tell whether the p99 tail is
            // one group's serial work or many independent stalls.
            let groups = groups_window;
            let requests_this_window = lags.len() as u64;
            let mean_group_size = if groups > 0 {
                requests_this_window as f64 / groups as f64
            } else {
                0.0
            };
            let max_group_size = max_group_size_window;
            // The absolute minimum per-request lag, kept separately from the clamped percentiles.
            let min_lag_us = lags.iter().copied().min().unwrap_or(0);
            // The absolute maximum per-request service latency and schedule lag (the window's
            // single-request stall), kept so the residual can be attributed to one request vs
            // many. `latencies` and `sorted_lags` are already sorted ascending, so the last
            // element is the maximum.
            let max_latency_us = latencies.last().copied().unwrap_or(0);
            let max_lag_us = sorted_lags.last().copied().unwrap_or(0);
            let groups_over_service_ceiling = group_elapseds.iter()
                .filter(|&&g| args.ceiling_us.is_some_and(|c| g > c))
                .count() as u64;
            let behind = lag_p99 > args.lag_ceiling_us;
            let window_late = late_total - window_late_start;
            // Boundary-vs-read breach attribution: a boundary drain can only
            // cause a lag breach if it alone blocked the pacer past the ceiling,
            // and then it delays exactly the next window's FIRST group. After the
            // non-blocking fix `boundary_drain_prev_us` is ~0, so this bucket is
            // ~0 and the residual breaches are the synchronous cold reads.
            let boundary_lag_breaches = if boundary_drain_prev_us > args.lag_ceiling_us {
                lags.iter().take(first_group_count as usize)
                    .filter(|&&lag| lag > args.lag_ceiling_us).count() as u64
            } else {
                0
            };
            let read_lag_breaches = window_late.saturating_sub(boundary_lag_breaches);
            let service_ceiling_breached = args.ceiling_us.is_some_and(|c| p99 > c);
            let lag_ceiling_breached = lag_p99 > args.lag_ceiling_us;
            if behind || window_late > 0 { late_growth_windows += 1 } else { late_growth_windows = 0 }
            let saturated = late_growth_windows >= 3;
            let window_error_count = errors - errors_before;
            let rejected = window_error_count > 0;
            let reason = reject_reason(service_ceiling_breached, lag_ceiling_breached, saturated, rejected);
            let invalid = reason.is_some();
            if invalid { invalid_windows += 1 } else { last_valid_rate = last_valid_rate.max(window_rate) }
            if service_ceiling_breached { service_ceiling_rejected_windows += 1 }
            if lag_ceiling_breached { lag_ceiling_rejected_windows += 1 }
            if service_ceiling_breached || lag_ceiling_breached { ceil_rejected_windows += 1 }
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
                writeln!(out, "{}", metrics_json_line(WindowLine {
                    window: window_index,
                    rate: window_rate,
                    requests: lags.len() as u64,
                    completed,
                    errors,
                    compact_bytes: compact_bytes_window,
                    p50_us: p50,
                    p95_us: p95,
                    p99_us: p99,
                    lag_p50_us: lag_p50,
                    lag_p95_us: lag_p95,
                    lag_p99_us: lag_p99,
                    min_lag_us,
                    group_elapsed_us,
                    groups,
                    mean_group_size,
                    max_group_size,
                    groups_over_service_ceiling,
                    early_dispatches: early_dispatches_window,
                    window_us,
                    invalid,
                    reject_reason: reason.clone(),
                    service_ceiling_us: args.ceiling_us,
                    service_ceiling_breached,
                    lag_ceiling_us: args.lag_ceiling_us,
                    lag_ceiling_breached,
                    policy: engine.layout_policy().to_json(),
                    current: &current,
                    previous: previous.as_ref(),
                }))?;
                engine.metrics_rotate_window();
            } else {
                writeln!(out, "window={window_index} rate_rps={:.0} requests={} completed={completed} \
                    errors={errors} service_p50_us={p50} service_p95_us={p95} service_p99_us={p99} \
                    lag_p50_us={lag_p50} lag_p95_us={lag_p95} lag_p99_us={lag_p99} \
                    min_lag_us={min_lag_us} group_elapsed_us={group_elapsed_us} groups={groups} \
                    mean_group_size={mean_group_size:.3} max_group_size={max_group_size} \
                    groups_over_service_ceiling={groups_over_service_ceiling} \
                    early_dispatches={early_dispatches_window} \
                    behind={behind} late_total={late_total} errors_window={window_error_count} \
                    invalid={invalid} reject_reason={} service_ceiling_us={} lag_ceiling_us={} \
                    compact_bytes={compact_bytes_window}",
                    window_rate, lags.len(),
                    reason.clone().unwrap_or_else(|| "none".into()),
                    args.ceiling_us.map(|c| c.to_string()).unwrap_or_else(|| "none".into()),
                    args.lag_ceiling_us)?;
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
            group_elapseds.clear();
            groups_window = 0;
            max_group_size_window = 0;
            early_dispatches_window = 0;
            // keep the process footprint flat across windows (scan results churn hundred-KB Vecs).
            // Timed and attributed here: `malloc_trim(0)` runs ON the request thread once per
            // window, so its stall lands in this window's boundary, not the next window's service
            // latencies; the elapsed time plus the differenced kernel counters are printed before
            // the index advances so `window_stall` and the window line share one index.
            let trim_t0 = Instant::now();
            trim_heap();
            let trim_us = trim_t0.elapsed().as_micros() as u64;
            let proc_now = read_proc_counters();
            window_stall_print(window_index, proc_now.since(proc_prev), trim_us, window_fsync_sum,
                               window_maintain_us, stall_group, max_sleep_us, max_sleep_request_us,
                               max_latency_us, max_lag_us, group_elapsed_us);
            eprintln!(
                "lag_breach_attribution window={window_index} total={window_late} \
                 boundary={boundary_lag_breaches} reads={read_lag_breaches} \
                 boundary_drain_us={boundary_drain_prev_us} read_us={window_read_us}");
            proc_prev = proc_now;
            // Reset the per-window stall attribution for the next window.
            stall_group = GroupPhaseReport::default();
            window_fsync_sum = 0;
            window_maintain_us = 0;
            max_sleep_us = 0;
            max_sleep_request_us = 0;
            boundary_drain_prev_us = window_boundary_drain_us;
            first_group_count = 0;
            window_read_us = 0;
            window_index += 1;
            window_start = Instant::now();
            window_late_start = late_total;
            // Deliberately do NOT relabel the window with the upcoming rung: an idle window while
            // the next rung's first group is still pending belongs to the rung that is still on
            // the offered schedule, so its wall time is not charged to the next rung.
        }
    }

    // Defensive: every dispatched group must be finalized before the summary,
    // and before any compaction apply (which refuses a barrier in flight). The
    // trailing-window flush is non-blocking, so a slow cross-boundary barrier
    // may still be in flight here; drain it now.
    drain_all_pending_groups(
        &mut engine, &mut pending, total, &args.rates, args.segment_requests,
        window_index,
        &mut completed, &mut errors, &mut else_used, &mut first_error,
        &mut latencies, &mut group_elapseds,
        &mut window_fsync_sum, &mut stall_group)?;

    // Drain the last in-flight compaction build so the final state and the
    // `compact_*` totals reflect it; the worker's channel closes when the driver
    // drops it, and any build that finished is applied before the summary.
    if let Some(worker) = compaction_worker.as_mut() {
        while worker.busy() {
            if let Some(outcome) = worker.take_done()? {
                let merged = engine.apply_compaction_job(&outcome)?;
                compact_bytes_total += merged;
                compact_merges += 1;
            }
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
        else_used={else_used} late_total={late_total} early_dispatches={early_dispatches_total} \
        windows={window_index} invalid_windows={invalid_windows} \
        service_ceiling_rejected_windows={service_ceiling_rejected_windows} \
        lag_ceiling_rejected_windows={lag_ceiling_rejected_windows} \
        ceiling_rejected_windows={ceil_rejected_windows} \
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
    fn proc_counters_since_differences_and_never_underflows() {
        let prev = ProcCounters { minflt: 1, majflt: 2, rchar: 3, read_bytes: 4, wchar: 5, write_bytes: 6,
            vol_csw: 7, invol_csw: 8, utime_ticks: 9, stime_ticks: 10 };
        let cur = ProcCounters { minflt: 11, majflt: 2, rchar: 103, read_bytes: 4, wchar: 5, write_bytes: 106,
            vol_csw: 17, invol_csw: 8, utime_ticks: 109, stime_ticks: 10 };
        let d = cur.since(prev);
        assert_eq!(d.minflt, 10);
        assert_eq!(d.majflt, 0, "equal counters differ to zero");
        assert_eq!(d.rchar, 100);
        assert_eq!(d.read_bytes, 0);
        assert_eq!(d.wchar, 0);
        assert_eq!(d.write_bytes, 100);
        assert_eq!(d.vol_csw, 10);
        assert_eq!(d.invol_csw, 0);
        assert_eq!(d.utime_ticks, 100);
        assert_eq!(d.stime_ticks, 0);
        // A counter that went backwards (e.g. a fresh /proc/self/io in a new
        // pid namespace) must saturate to 0, never panic.
        let back = prev.since(cur);
        assert_eq!(back.minflt, 0);
        assert_eq!(back.write_bytes, 0);
        assert_eq!(back.invol_csw, 0);
        assert_eq!(back.stime_ticks, 0);
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
    fn dry_run_flag_enables_the_host_stall_control() {
        let plain = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                     "--rate-rps".into(), "10".into()]).expect("valid args");
        assert!(!plain.dry_run, "dry-run is off unless requested");
        assert_eq!(plain.dry_run_busy_us, 0);
        let on = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                  "--rate-rps".into(), "10".into(),
                                  "--dry-run".into(), "--dry-run-busy-us".into(), "2000".into()])
            .expect("valid dry-run args");
        assert!(on.dry_run);
        assert_eq!(on.dry_run_busy_us, 2000);
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(),
                                 "--rate-rps".into(), "10".into(), "--dry-run-busy-us".into()]).is_err(),
            "--dry-run-busy-us needs a value");
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
    fn key_encoding_flag_selects_and_rejects() {
        let family = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into()])
            .expect("valid args");
        assert_eq!(family.key_encoding, KeyEncoding::FamilyFirst, "family-first is the default");
        let entity = parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                     "--key-encoding".into(), "entity_first".into()]).expect("valid encoding");
        assert_eq!(entity.key_encoding, KeyEncoding::EntityFirst);
        assert!(parse_args(vec!["b".into(), "dir".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--key-encoding".into(), "nonsense".into()]).is_err(),
            "an unknown key encoding is rejected");
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
        let line = metrics_json_line(WindowLine {
            window: 1, rate: 100.0, requests: 1, completed: 1, errors: 0, compact_bytes: 0,
            p50_us: 10, p95_us: 15, p99_us: 20,
            lag_p50_us: 0, lag_p95_us: 0, lag_p99_us: 0, min_lag_us: 0,
            group_elapsed_us: 0, groups: 1, mean_group_size: 1.0, max_group_size: 1,
            groups_over_service_ceiling: 0, early_dispatches: 0,
            window_us: 1_000_000, invalid: false, reject_reason: None,
            service_ceiling_us: None, service_ceiling_breached: false,
            lag_ceiling_us: 10_000, lag_ceiling_breached: false,
            policy: LayoutPolicy::declared_default().to_json(), current: &snap, previous: None,
        });
        assert!(line.contains(r#""compaction_bytes":4288,"compaction_merges":1"#), "got {line}");
        assert!(line.contains(r#""bytes_written":40"#), "got {line}");
    }

    /// The rejection inputs must be ON the JSON line: a `--metrics-json` consumer builds the
    /// capacity curve from these lines, and before this the `invalid`/`lag_p99_us` fields were
    /// only on the human `window=` line, so the consumer silently read them as `false`/`0` and
    /// could score a saturated rung as sustained.
    #[test]
    fn metrics_json_line_carries_the_rejection_and_wall_time_fields() {
        let empty = WindowSnapshot::default();
        let line = metrics_json_line(WindowLine {
            window: 7, rate: 50.0, requests: 64, completed: 128, errors: 0, compact_bytes: 0,
            p50_us: 100, p95_us: 500, p99_us: 999_999,
            lag_p50_us: 0, lag_p95_us: 0, lag_p99_us: 5_000_000, min_lag_us: 3,
            group_elapsed_us: 200, groups: 1, mean_group_size: 64.0, max_group_size: 64,
            groups_over_service_ceiling: 1, early_dispatches: 0,
            window_us: 1_280_000, invalid: true,
            reject_reason: Some("service_ceiling".into()),
            service_ceiling_us: Some(250_000), service_ceiling_breached: true,
            lag_ceiling_us: 10_000, lag_ceiling_breached: false,
            policy: LayoutPolicy::declared_default().to_json(), current: &empty, previous: None,
        });
        assert!(line.contains(r#""invalid":true"#), "got {line}");
        assert!(line.contains(r#""lag_p99_us":5000000"#), "got {line}");
        assert!(line.contains(r#""min_lag_us":3"#), "got {line}");
        assert!(line.contains(r#""groups_over_service_ceiling":1"#), "got {line}");
        assert!(line.contains(r#""window_us":1280000"#), "got {line}");
        assert!(line.contains(r#""service_ceiling_us":250000"#), "got {line}");
        assert!(line.contains(r#""service_ceiling_breached":true"#), "got {line}");
        assert!(line.contains(r#""reject_reason":"service_ceiling""#), "got {line}");
    }

    /// The per-window group statistics: `groups` counts the dispatched groups, `max_group_size`
    /// is the largest, and `mean_group_size == requests / groups`; `min_lag_us` is the absolute
    /// minimum per-request lag and `groups_over_service_ceiling` counts groups whose elapsed
    /// exceeded `service_ceiling_us`. None of these fields existed before the fix.
    #[test]
    fn metrics_json_line_carries_group_statistics_and_min_lag() {
        let empty = WindowSnapshot::default();
        let line = metrics_json_line(WindowLine {
            window: 2, rate: 100.0, requests: 96, completed: 96, errors: 0, compact_bytes: 0,
            p50_us: 100, p95_us: 200, p99_us: 300,
            lag_p50_us: 10, lag_p95_us: 20, lag_p99_us: 30, min_lag_us: 0,
            group_elapsed_us: 900, groups: 3, mean_group_size: 32.0, max_group_size: 64,
            groups_over_service_ceiling: 2, early_dispatches: 0,
            window_us: 1_000_000, invalid: false, reject_reason: None,
            service_ceiling_us: Some(500), service_ceiling_breached: false,
            lag_ceiling_us: 10_000, lag_ceiling_breached: false,
            policy: LayoutPolicy::declared_default().to_json(), current: &empty, previous: None,
        });
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid json");
        assert_eq!(v["groups"], 3);
        assert_eq!(v["max_group_size"], 64);
        assert_eq!(v["min_lag_us"], 0);
        assert_eq!(v["groups_over_service_ceiling"], 2);
        let mean = v["mean_group_size"].as_f64().expect("mean is numeric");
        let derived = v["requests"].as_f64().unwrap() / v["groups"].as_f64().unwrap();
        assert!((mean - derived).abs() < 1e-9, "mean {mean} must equal requests/groups {derived}");
    }

    #[test]
    fn metrics_json_line_freezes_the_wrapper_field_names() {
        let empty = WindowSnapshot::default();
        let policy = LayoutPolicy::declared_default().to_json();
        // `compact_bytes` is the slice-37 driver-side field: the bytes merged by the
        // `--compact-bytes` actuation call at this window's boundary (0 when unset).
        let line = metrics_json_line(WindowLine {
            window: 3, rate: 120.0, requests: 10, completed: 9, errors: 1, compact_bytes: 0,
            p50_us: 500, p95_us: 1000, p99_us: 1500,
            lag_p50_us: 0, lag_p95_us: 0, lag_p99_us: 0, min_lag_us: 0,
            group_elapsed_us: 0, groups: 2, mean_group_size: 5.0, max_group_size: 8,
            groups_over_service_ceiling: 0, early_dispatches: 0,
            window_us: 1_000_000, invalid: false, reject_reason: None,
            service_ceiling_us: None, service_ceiling_breached: false,
            lag_ceiling_us: 10_000, lag_ceiling_breached: false,
            policy: policy.clone(), current: &empty, previous: None,
        });
        assert_eq!(
            line,
            r#"{"compact_bytes":0,"completed":9,"current":{"patterns":[],"regions":[]},"early_dispatches":0,"errors":1,"group_elapsed_us":0,"groups":2,"groups_over_service_ceiling":0,"invalid":false,"lag_ceiling_breached":false,"lag_ceiling_us":10000,"lag_p50_us":0,"lag_p95_us":0,"lag_p99_us":0,"max_group_size":8,"mean_group_size":5.0,"min_lag_us":0,"p50_us":500,"p95_us":1000,"p99_us":1500,"policy":{"default":{"bloom_bits_per_key":10,"merge_min_tiles":2,"page_size":4096},"regions":{}},"previous":null,"rate_rps":120.0,"reject_reason":null,"requests":10,"service_ceiling_breached":false,"service_ceiling_us":null,"service_p50_us":500,"service_p95_us":1000,"service_p99_us":1500,"window":3,"window_us":1000000}"#
        );
        // A previous window is embedded as an object, not a string.
        let with_prev = metrics_json_line(WindowLine {
            window: 4, rate: 120.0, requests: 10, completed: 19, errors: 1, compact_bytes: 4288,
            p50_us: 500, p95_us: 1000, p99_us: 1500,
            lag_p50_us: 0, lag_p95_us: 0, lag_p99_us: 0, min_lag_us: 0,
            group_elapsed_us: 0, groups: 2, mean_group_size: 5.0, max_group_size: 8,
            groups_over_service_ceiling: 0, early_dispatches: 0,
            window_us: 1_000_000, invalid: false, reject_reason: None,
            service_ceiling_us: None, service_ceiling_breached: false,
            lag_ceiling_us: 10_000, lag_ceiling_breached: false,
            policy, current: &empty, previous: Some(&empty),
        });
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

    /// Arrivals model: a group is formed only from requests ALREADY READY at dispatch, so it never
    /// consumes a future-scheduled request; a whole `size` group forms only once enough requests
    /// are past due (the driver ran ahead before this fix).
    #[test]
    fn ready_group_never_consumes_a_future_request() {
        let rates = [1.0]; // one request per 1_000_000 us
        // now == the first due time: only request 0 is ready, even with a group size of 64.
        let (end, next_due) = ready_group(0, 100, 64, 1_000_000.0, 1_000_000.0, &rates, None);
        assert_eq!(end, 1, "a not-yet-due request must never join the group");
        assert_eq!(next_due, 2_000_000.0);
        // The engine is four intervals behind: requests 0..=4 are ready -> a 5-request group.
        let (end, _) = ready_group(0, 100, 64, 1_000_000.0, 5_000_000.0, &rates, None);
        assert_eq!(end, 5);
        // The cap still applies when far behind.
        let (end, _) = ready_group(0, 100, 3, 1_000_000.0, 100_000_000.0, &rates, None);
        assert_eq!(end, 3, "the group is capped at --group-requests");
    }

    /// A group never straddles an offered-rate boundary, so every completion is attributed to the
    /// rung it was OFFERED under.
    #[test]
    fn ready_group_stops_at_the_offered_rate_boundary() {
        let rates = [1.0, 1_000_000.0];
        // Segment boundary at index 10 (`--segment-requests 10`): a group at index 8 may only
        // reach 10 even though every request is past due.
        let (end, _) = ready_group(8, 20, 64, 1_000_000.0, 1e12, &rates, Some(10));
        assert_eq!(end, 10);
    }

    #[test]
    fn reject_reason_names_which_ceiling_failed() {
        assert_eq!(reject_reason(false, false, false, false), None);
        assert_eq!(reject_reason(true, false, false, false).as_deref(), Some("service_ceiling"));
        assert_eq!(reject_reason(false, true, false, false).as_deref(), Some("lag_ceiling"));
        assert_eq!(reject_reason(true, true, false, false).as_deref(), Some("service_ceiling+lag_ceiling"));
        assert_eq!(reject_reason(false, false, false, true).as_deref(), Some("errors"));
    }

    #[test]
    fn lag_ceiling_flag_parses_and_defaults() {
        let plain = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into()])
            .expect("valid args");
        assert_eq!(plain.lag_ceiling_us, 10_000, "the pre-registration lag bound is the default");
        let set = parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                  "--lag-ceiling-us".into(), "25000".into()]).expect("valid lag ceiling");
        assert_eq!(set.lag_ceiling_us, 25_000);
        assert!(parse_args(vec!["b".into(), "wal".into(), "t.jsonl".into(), "--rate-rps".into(), "10".into(),
                                "--lag-ceiling-us".into(), "0".into()]).is_err(),
            "a zero lag ceiling is rejected");
    }

    /// Schedule-lag samples are dispatch-scoped and service-latency samples are
    /// completion-scoped: the split recorders must produce the same per-request
    /// lag (issue minus due) and service latency (completion minus due) the
    /// single combined recorder used to, without cross-contaminating the two
    /// vectors. This is what lets the window-boundary drain stay non-blocking
    /// while a cross-boundary group still lands its lag in the dispatch window.
    #[test]
    fn record_group_lag_and_service_split_dispatch_and_completion_scopes() {
        let rates = [10.0]; // 100,000 us per request interval
        let mut early_total = 0u64;
        let mut early_window = 0u64;
        let mut late_total = 0u64;
        let mut lags: Vec<u64> = Vec::new();
        let mut latencies: Vec<u64> = Vec::new();
        let mut group_elapseds: Vec<u64> = Vec::new();
        // Two requests due at 100,000 and 200,000 us, dispatched at 250,000 us
        // (both late), completed at 400,000 us.
        record_group_lag(100_000.0, 0, 2, 250_000.0, 100, &rates, None, 10_000,
                         &mut early_total, &mut early_window, &mut late_total, &mut lags);
        assert_eq!(lags, vec![150_000, 50_000], "lag = issue - each member's own due time");
        assert_eq!(late_total, 2, "both requests are over the 10 ms ceiling");
        assert_eq!(early_total, 0);
        record_group_service(100_000.0, 0, 2, 400_000.0, 100, &rates, None,
                             &mut latencies, &mut group_elapseds);
        assert_eq!(latencies, vec![300_000, 200_000], "latency = completion - each member's own due time");
        assert_eq!(group_elapseds, vec![300_000], "group elapsed = completion - first member's due time");
        assert_eq!(lags, vec![150_000, 50_000], "the service recorder must not touch lags");
        assert!(latencies.iter().all(|&l| l > 10_000), "service latency is not bounded by the lag ceiling");
    }
}
