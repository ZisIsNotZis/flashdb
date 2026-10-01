//! Per-pattern latency telemetry (slice 16).
//!
//! This module is the *measurement substrate* for the layout experiment: it
//! records, per pattern and per window, the latency samples that the objective
//! scores as `p99(pattern, window)`
//! (`.scratch/15-objective-v0/provisional-objective.md` §1, §6). It is strictly
//! **observational**: nothing here is persisted, and no read or write decision
//! consults it. `Engine::set_metrics_enabled(false)` makes every instrumented
//! call site a no-op so the overhead can be measured by comparison and so that
//! engine behaviour provably does not depend on telemetry ([`crate::engine`]).
//!
//! ## PROVISIONAL pattern vocabulary
//!
//! The design is from `docs/learning.md` ("The telemetry store obeys its own
//! design ... sketch-based counters ... percentiles never means ... a hard cap").
//! The key spelling below is **PROVISIONAL** and is fixed only so the driver,
//! the learner and the simulator agree on one string; it is not an author
//! decision (see `.scratch/15-objective-v0/provisional-objective.md` §8):
//!
//! | key | engine call |
//! |---|---|
//! | `P:<entity>` | primary-key point lookup ([`crate::engine::Engine::get`]) |
//! | `U:<entity>.<field>` | unique-index probe ([`crate::engine::Engine::unique_lookup`]) |
//! | `R:<entity>.<field>` | reverse-index lookup ([`crate::engine::Engine::reverse_lookup`]) |
//! | `scan:<entity>` | range / full scan of one entity ([`crate::engine::Engine::scan_primary`]) |
//! | `commit` | block commit ([`crate::engine::Engine::commit_block`]) |
//!
//! Resolution is done once per call by a linear scan over an interned `Vec`;
//! every comparison is `&[u8]`/`Kind` equality, so the steady path performs **no
//! heap allocation and no string formatting**. The printable key is rendered
//! only by [`Metrics::snapshot`] / [`Metrics::rotate_window`], which are
//! off-path.
//!
//! ## Memory bound and folding rule
//!
//! At most [`MAX_PATTERNS`] distinct patterns are interned. Once the intern table
//! is full a *new* pattern is not inserted: its samples are folded into a single
//! `other` bin, which is reported as the pattern key `other`. The table never
//! grows past the cap and resolution never panics.
//!
//! ## Histogram and p99 estimator
//!
//! Each pattern keeps a fixed [`BUCKETS`]-bucket log-spaced latency histogram
//! (bucket `i` spans `BUCKET_BASE_NS * BUCKET_RATIO^(i-1) .. BUCKET_BASE_NS *
//! BUCKET_RATIO^i` for `i >= 1`; bucket 0 is the underflow bin `[0, base)` and
//! bucket `BUCKETS-1` is the overflow bin `[base * ratio^126, ∞)`). This covers
//! roughly 100 ns to 4.4 s.
//!
//! The quantile estimator interpolates *inside* the bucket the target rank lands
//! in: bucket 0 linearly on `[0, base)`, every later bucket geometrically
//! (`lower * ratio^frac`). The estimator's resolution is therefore the bucket
//! width ratio, and its **worst-case relative error is `BUCKET_RATIO - 1` = 15%
//! for bins 1..=126** (the true value and the estimate both lie in one bucket of
//! span `lower*(ratio-1)`). Samples below `base` are reported at ≤ `base`; samples
//! at or above the top boundary are clamped into the overflow bin and may be
//! underestimated without bound — stated, not hidden.
//!
//! The ratio/bucket-count trade-off is forced: a 5% ratio would need ~330 buckets
//! to span the engine's real latency range (sub-µs memtable reads to multi-second
//! scans), so 128 buckets at 15% is chosen to *cover* the range rather than clip
//! it. `BUCKETS` and `BUCKET_RATIO` are constants a future slice may retune.

use std::fmt;
use std::time::Duration;

/// Hard cap on distinct interned patterns; beyond it new patterns fold to `other`.
pub const MAX_PATTERNS: usize = 256;
/// Number of log-spaced latency buckets per pattern.
pub const BUCKETS: usize = 128;
/// Bucket width ratio (successive upper bound / lower bound).
pub const BUCKET_RATIO: f64 = 1.15;
/// Lower bound of bucket 1, in nanoseconds; bucket 0 is the underflow bin below it.
pub const BUCKET_BASE_NS: f64 = 100.0;

/// Sentinel: metrics disabled, so no resolution work was performed.
pub const DISABLED: usize = usize::MAX - 1;
/// Sentinel: the pattern exceeded [`MAX_PATTERNS`] and folds into `other`.
pub const OTHER: usize = usize::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Primary,
    Unique,
    Reverse,
    Scan,
    Commit,
}

/// One interned pattern: the identity it was resolved from, compared structurally
/// (never concatenated) so resolution needs no allocation.
struct PatternKey {
    kind: Kind,
    entity: Vec<u8>,
    field: Vec<u8>,
}

#[derive(Clone)]
struct PatternStats {
    count: u64,
    sum_ns: u128,
    /// Per-bucket counts; `u32` bounds one bucket's share of one window and is
    /// saturated on overflow rather than wrapping.
    buckets: [u32; BUCKETS],
}

impl Default for PatternStats {
    fn default() -> Self {
        PatternStats { count: 0, sum_ns: 0, buckets: [0; BUCKETS] }
    }
}

/// A printable per-pattern summary for one window.
#[derive(Clone, Debug, PartialEq)]
pub struct PatternSummary {
    pub pattern: String,
    pub count: u64,
    pub p50_ns: f64,
    pub p99_ns: f64,
    pub mean_ns: f64,
}

/// A printable snapshot of one window: the active patterns in first-resolved
/// order, then `other` last when any sample folded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowSnapshot {
    pub patterns: Vec<PatternSummary>,
}

impl fmt::Display for WindowSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for p in &self.patterns {
            writeln!(
                f,
                "pattern={} count={} p50_ns={:.0} p99_ns={:.0} mean_ns={:.0}",
                p.pattern, p.count, p.p50_ns, p.p99_ns, p.mean_ns
            )?;
        }
        Ok(())
    }
}

/// Fixed-shape, allocation-free-on-the-hot-path per-pattern telemetry.
pub struct Metrics {
    enabled: bool,
    keys: Vec<PatternKey>,
    stats: Vec<PatternStats>,
    other: PatternStats,
    /// Lower nanoseconds boundary of each bucket, precomputed so recording is a
    /// branchless partition point instead of a per-sample `ln`.
    bounds: [u64; BUCKETS],
    previous: Option<WindowSnapshot>,
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let mut bounds = [0u64; BUCKETS];
        // bounds[0] is the underflow bin's lower bound (0 ns); bounds[i] for
        // i >= 1 is the lower bound of bucket i.
        for (i, b) in bounds.iter_mut().enumerate().skip(1) {
            *b = (BUCKET_BASE_NS * BUCKET_RATIO.powi(i as i32 - 1)).round() as u64;
        }
        Metrics {
            enabled: true,
            keys: Vec::new(),
            stats: Vec::new(),
            other: PatternStats::default(),
            bounds,
            previous: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turn observation on or off. Off makes every instrumented call site a
    /// no-op; it never changes engine behaviour either way.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// Number of distinct interned patterns (excludes the `other` bin).
    pub fn pattern_count(&self) -> usize {
        self.keys.len()
    }

    pub fn resolve_primary(&mut self, entity: &[u8]) -> usize {
        self.resolve(Kind::Primary, entity, &[])
    }

    pub fn resolve_unique(&mut self, entity: &[u8], field: &[u8]) -> usize {
        self.resolve(Kind::Unique, entity, field)
    }

    pub fn resolve_reverse(&mut self, entity: &[u8], field: &[u8]) -> usize {
        self.resolve(Kind::Reverse, entity, field)
    }

    pub fn resolve_scan(&mut self, entity: &[u8]) -> usize {
        self.resolve(Kind::Scan, entity, &[])
    }

    pub fn resolve_commit(&mut self) -> usize {
        self.resolve(Kind::Commit, &[], &[])
    }

    /// Resolve a call to its interned index, interning on first sight. Returns
    /// [`DISABLED`] when observation is off (doing no work) and [`OTHER`] when
    /// the intern table is full (samples fold into `other`).
    fn resolve(&mut self, kind: Kind, entity: &[u8], field: &[u8]) -> usize {
        if !self.enabled {
            return DISABLED;
        }
        for (i, k) in self.keys.iter().enumerate() {
            if k.kind == kind && k.entity == entity && k.field == field {
                return i;
            }
        }
        if self.keys.len() >= MAX_PATTERNS {
            return OTHER;
        }
        self.keys.push(PatternKey { kind, entity: entity.to_vec(), field: field.to_vec() });
        self.stats.push(PatternStats::default());
        self.keys.len() - 1
    }

    /// Record one latency sample against a resolved pattern. A no-op when
    /// disabled; `DISABLED`/`OTHER` are handled without panicking.
    pub fn record(&mut self, id: usize, elapsed: Duration) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let stats = match id {
            OTHER => &mut self.other,
            i if i < self.stats.len() => &mut self.stats[i],
            // A stale/foreign id cannot panic the engine; fold it.
            _ => &mut self.other,
        };
        stats.count = stats.count.saturating_add(1);
        stats.sum_ns = stats.sum_ns.saturating_add(ns as u128);
        let b = bucket_index(&self.bounds, ns);
        stats.buckets[b] = stats.buckets[b].saturating_add(1);
    }

    fn build_snapshot(&self) -> WindowSnapshot {
        let mut patterns = Vec::with_capacity(self.keys.len() + 1);
        for (i, k) in self.keys.iter().enumerate() {
            let s = &self.stats[i];
            if s.count == 0 {
                continue;
            }
            patterns.push(summarize(render_key(k), s, &self.bounds));
        }
        if self.other.count > 0 {
            patterns.push(summarize("other".to_string(), &self.other, &self.bounds));
        }
        WindowSnapshot { patterns }
    }

    /// The plain, printable summary of the *current* window (no mutation).
    pub fn snapshot(&self) -> WindowSnapshot {
        self.build_snapshot()
    }

    /// Make the current window's snapshot available as the previous window and
    /// clear every counter, keeping at most one previous window. The intern table
    /// is deliberately *not* cleared, so pattern indices stay stable and the next
    /// window re-resolves without reallocating. Returns the finished window.
    pub fn rotate_window(&mut self) -> &WindowSnapshot {
        self.previous = Some(self.build_snapshot());
        for s in &mut self.stats {
            *s = PatternStats::default();
        }
        self.other = PatternStats::default();
        self.previous.as_ref().unwrap()
    }

    /// The snapshot from the most recent [`Metrics::rotate_window`], if any.
    pub fn previous_window(&self) -> Option<&WindowSnapshot> {
        self.previous.as_ref()
    }
}

/// Bucket index for a nanosecond latency: the last boundary at or below `ns`,
/// clamped into range. `bounds[0] == 0`, so `ns == 0` maps to bucket 0.
fn bucket_index(bounds: &[u64; BUCKETS], ns: u64) -> usize {
    let i = bounds.partition_point(|&b| b <= ns).saturating_sub(1);
    i.min(BUCKETS - 1)
}

fn summarize(pattern: String, s: &PatternStats, bounds: &[u64; BUCKETS]) -> PatternSummary {
    let count = s.count;
    PatternSummary {
        pattern,
        count,
        p50_ns: percentile(s, bounds, 0.50),
        p99_ns: percentile(s, bounds, 0.99),
        mean_ns: if count == 0 { 0.0 } else { s.sum_ns as f64 / count as f64 },
    }
}

/// Quantile estimator: find the bucket holding the `q`-th rank and interpolate
/// inside it (linear on the underflow bin, geometric elsewhere). Returns
/// nanoseconds. See the module docs for the 15% worst-case relative error.
fn percentile(s: &PatternStats, bounds: &[u64; BUCKETS], q: f64) -> f64 {
    if s.count == 0 {
        return 0.0;
    }
    let target = ((q * s.count as f64).ceil() as u64).clamp(1, s.count);
    let mut cum = 0u64;
    for i in 0..BUCKETS {
        let in_bucket = u64::from(s.buckets[i]);
        if in_bucket == 0 {
            continue;
        }
        let before = cum;
        cum += in_bucket;
        if cum >= target {
            let frac = ((target - before) as f64 / in_bucket as f64).clamp(0.0, 1.0);
            if i == 0 {
                return frac * BUCKET_BASE_NS;
            }
            let lower = (bounds[i] as f64).max(1.0);
            return lower * BUCKET_RATIO.powf(frac);
        }
    }
    // Unreachable while count > 0 (some bucket holds a sample); fall back to the
    // overflow bin's lower bound rather than returning a misleading zero.
    (bounds[BUCKETS - 1] as f64).max(1.0)
}

fn render_key(k: &PatternKey) -> String {
    let entity = String::from_utf8_lossy(&k.entity);
    match k.kind {
        Kind::Primary => format!("P:{entity}"),
        Kind::Unique => format!("U:{entity}.{}", String::from_utf8_lossy(&k.field)),
        Kind::Reverse => format!("R:{entity}.{}", String::from_utf8_lossy(&k.field)),
        Kind::Scan => format!("scan:{entity}"),
        Kind::Commit => "commit".to_string(),
    }
}
