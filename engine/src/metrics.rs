//! Per-pattern latency telemetry (slice 16).
//!
//! This module is the *measurement substrate* for the layout experiment: it
//! records, per pattern and per window, the latency samples that the objective
//! scores as `p99(pattern, window)`
//! (`.scratch/15-objective-v0/provisional-objective.md` §1, §6), and - slice 26
//! - per **region** (`keys::region_prefix`, the `(key family, entity)` unit) the
//! access/shape counters a per-region layout learner needs. It is strictly
//! **observational**: nothing here is persisted, and no read or write decision
//! consults it. `Engine::set_metrics_enabled(false)` makes every instrumented
//! call site a no-op so the overhead can be measured by comparison and so that
//! engine behaviour provably does not depend on telemetry ([`crate::engine`]).
//!
//! ## PROVISIONAL pattern vocabulary
//!
//! The design is from `docs/5_PRODUCT.md` ("The telemetry store obeys its own
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
//! ## Per-region counters (slice 26)
//!
//! Alongside the latency histograms, the same window records per region - keyed
//! by the region prefix bytes ([`crate::keys::region_prefix`], `MARKER | entity |
//! 0`):
//!
//! | counter | meaning | source |
//! |---|---|---|
//! | `probes` | point lookups whose served region is this one | `get`, `unique_lookup`, `reverse_lookup` |
//! | `probe_misses` | of those, the ones that returned no row | same |
//! | `bytes_read` | data-block bytes read from tiles while serving this region | `TileCursor` via `scan_merged` |
//! | `bytes_written` | encoded WAL op bytes for this region's keys | `commit_block`/`commit_batch` append |
//! | `commit_blocks` | committed WAL records touching this region (once per block) | same |
//! | `commit_ops` | committed ops in this region | same |
//! | `compaction_bytes` | merged output tile bytes written by compaction for this region | `merge_region_run` (write side) |
//! | `compaction_merges` | compaction merges that included this region | same |
//! | `index_output_bytes` | derived secondary-access-path tile bytes built for this region | `build_order_no_index` (write side) |
//! | `count`/`mean_ns`/`p50_ns`/`p99_ns` | the same log-bucket latency histogram, per region | every instrumented read |
//!
//! `bytes_read` is attributed to the region being **served** (the probe/scan's
//! prefix), so it is correct for both Global and PerRegion tile layouts: a bloom
//! miss that still reads a block shows `bytes_read > 0`, a bloom reject shows 0.
//! `bytes_written` counts the exact per-op contribution to `encode_payload`
//! (tag + length-prefixed fields); the shared record header (magic, CSN, block
//! id, op count) is not attributed.
//!
//! **Region cap and folding rule.** At most [`MAX_REGIONS`] distinct region
//! prefixes are interned. Once the table is full a *new* region is not inserted:
//! its counters fold into a single `other` region row, reported with
//! `region = "other"`. Region names are rendered by [`crate::keys::region_name`]
//! (e.g. `P|Stock`); the ordering of the `regions` rows is first-interned order,
//! with `other` last. Region interning never allocates after first sight, and an
//! idle region is omitted from the snapshot rather than emitted zero-filled.
//!
//! **Compaction attribution.** The two compaction counters fire on the
//! per-region merge path only: each successful [`crate::engine::Engine`] region
//! merge adds the merged output tile's on-disk size to its region's
//! `compaction_bytes` and one to its `compaction_merges`. A Global-mode merge
//! (one tile over the whole keyspace) is *not* attributed, because its output is
//! not one region's. This is observability only: no decision, no format, and no
//! read/write/durability behaviour consults it, and disabled mode records
//! nothing.
//!
//! ## Per-region rewrite factor (the write side the learner prices)
//!
//! The **rewrite factor** of a region over a window (or over the summed windows
//! of a run) is defined as
//!
//! ```text
//! rewrite_factor(region) = compaction_bytes(region) / bytes_written(region)
//! ```
//!
//! i.e. the compaction output attributed to the region divided by that region's
//! *ingested* bytes over the same window(s) (`bytes_written` is the exact
//! encoded WAL op bytes for the region's keys). It is the per-region form of the
//! run-level `compaction_bytes / tile_bytes_before` ratio that
//! `flashdb-scale compact` reports and that slice 29 measured (14 merges /
//! 3,164,956,564 bytes / 1.932 eager versus 6 / 10,446,643 / 0.006 defer). The
//! learner, any report and this module must use *this* quantity; a window with no
//! ingest for the region leaves the factor undefined (division by zero) and it
//! must be reported as undefined, never as zero.
//!
//! ## Machine-readable dump
//!
//! [`WindowSnapshot::to_json`] freezes the field names a Python controller reads
//! (see the function docs). The load driver's `--metrics-json` mode wraps the
//! current and previous window in one JSON object per window line.
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

use crate::keys;

/// Hard cap on distinct interned patterns; beyond it new patterns fold to `other`.
pub const MAX_PATTERNS: usize = 256;
/// Hard cap on distinct interned regions; beyond it new regions fold to `other`.
pub const MAX_REGIONS: usize = 256;
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

/// A printable per-region summary for one window. One row per active region
/// (first-interned order), plus a trailing `region = "other"` row when any
/// region folded past [`MAX_REGIONS`]; an idle region is omitted.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionSummary {
    pub region: String,
    pub probes: u64,
    pub probe_misses: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub commit_blocks: u64,
    pub commit_ops: u64,
    /// Merged output tile bytes written by compaction for this region.
    pub compaction_bytes: u64,
    /// Compaction merges that included this region.
    pub compaction_merges: u64,
    /// Derived secondary-access-path tile bytes built for this region (WP-M).
    /// Distinct from `compaction_bytes`: it is a one-time off-path build, priced
    /// in the learner's reward but not in the merge-churn budget.
    pub index_output_bytes: u64,
    /// Latency samples recorded for this region (point lookups and scans), with
    /// the same bucket estimator as the per-pattern rows.
    pub count: u64,
    pub p50_ns: f64,
    pub p99_ns: f64,
    pub mean_ns: f64,
}

#[derive(Clone)]
struct RegionStats {
    probes: u64,
    probe_misses: u64,
    bytes_read: u64,
    bytes_written: u64,
    commit_blocks: u64,
    commit_ops: u64,
    compaction_bytes: u64,
    compaction_merges: u64,
    index_output_bytes: u64,
    count: u64,
    sum_ns: u128,
    buckets: [u32; BUCKETS],
}

impl Default for RegionStats {
    fn default() -> Self {
        RegionStats {
            probes: 0,
            probe_misses: 0,
            bytes_read: 0,
            bytes_written: 0,
            commit_blocks: 0,
            commit_ops: 0,
            compaction_bytes: 0,
            compaction_merges: 0,
            index_output_bytes: 0,
            count: 0,
            sum_ns: 0,
            buckets: [0; BUCKETS],
        }
    }
}

impl RegionStats {
    /// No counter fired this window, so the region is omitted from the snapshot.
    fn is_idle(&self) -> bool {
        self.probes == 0
            && self.bytes_read == 0
            && self.bytes_written == 0
            && self.commit_blocks == 0
            && self.compaction_bytes == 0
            && self.compaction_merges == 0
            && self.index_output_bytes == 0
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
/// order, then `other` last when any sample folded; followed by the active
/// regions in first-interned order, then `other` last when any region folded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WindowSnapshot {
    pub patterns: Vec<PatternSummary>,
    pub regions: Vec<RegionSummary>,
}

impl WindowSnapshot {
    /// The frozen machine-readable form of one window. Field names are part of
    /// the controller-facing schema and must not be renamed without a schema
    /// bump:
    ///
    /// ```json
    /// {
    ///   "patterns": [ {"pattern", "count", "p50_ns", "p99_ns", "mean_ns"} ],
    ///   "regions":  [ {"region", "probes", "probe_misses", "bytes_read",
    ///                  "bytes_written", "commit_blocks", "commit_ops",
    ///                  "compaction_bytes", "compaction_merges",
    ///                  "index_output_bytes"} ]
    /// }
    /// ```
    ///
    /// Object key order is serde_json's (a sorted map), so a dump is
    /// byte-deterministic for a deterministic window.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "patterns": self.patterns.iter().map(|p| serde_json::json!({
                "pattern": p.pattern,
                "count": p.count,
                "p50_ns": p.p50_ns,
                "p99_ns": p.p99_ns,
                "mean_ns": p.mean_ns,
            })).collect::<Vec<_>>(),
            "regions": self.regions.iter().map(|r| serde_json::json!({
                "region": r.region,
                "probes": r.probes,
                "probe_misses": r.probe_misses,
                "bytes_read": r.bytes_read,
                "bytes_written": r.bytes_written,
                "commit_blocks": r.commit_blocks,
                "commit_ops": r.commit_ops,
                "compaction_bytes": r.compaction_bytes,
                "compaction_merges": r.compaction_merges,
                "index_output_bytes": r.index_output_bytes,
                "count": r.count,
                "p50_ns": r.p50_ns,
                "p99_ns": r.p99_ns,
                "mean_ns": r.mean_ns,
            })).collect::<Vec<_>>(),
        })
    }
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
        for r in &self.regions {
            writeln!(
                f,
                "region={} probes={} probe_misses={} bytes_read={} bytes_written={} commit_blocks={} commit_ops={} compaction_bytes={} compaction_merges={} index_output_bytes={} count={} p50_ns={:.0} p99_ns={:.0} mean_ns={:.0}",
                r.region, r.probes, r.probe_misses, r.bytes_read, r.bytes_written, r.commit_blocks, r.commit_ops,
                r.compaction_bytes, r.compaction_merges, r.index_output_bytes, r.count, r.p50_ns, r.p99_ns, r.mean_ns
            )?;
        }
        Ok(())
    }
}

/// Fixed-shape, allocation-free-on-the-hot-path per-pattern and per-region
/// telemetry.
pub struct Metrics {
    enabled: bool,
    keys: Vec<PatternKey>,
    stats: Vec<PatternStats>,
    other: PatternStats,
    /// Interned region prefixes ([`crate::keys::region_prefix`] bytes) and their
    /// counters, `region_stats[i]` for `region_keys[i]`. Bounded by
    /// [`MAX_REGIONS`]; a new region past the cap folds into `region_other`.
    region_keys: Vec<Vec<u8>>,
    region_stats: Vec<RegionStats>,
    region_other: RegionStats,
    /// Reused buffer for building a `MARKER | entity | 0` prefix without
    /// allocating per op on the commit path.
    region_scratch: Vec<u8>,
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
            region_keys: Vec::new(),
            region_stats: Vec::new(),
            region_other: RegionStats::default(),
            region_scratch: Vec::new(),
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

    /// Number of distinct interned regions (excludes the `other` bin).
    pub fn region_count(&self) -> usize {
        self.region_keys.len()
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

    /// Resolve a region prefix ([`crate::keys::region_prefix`] bytes) to its
    /// interned index, interning on first sight. Returns [`DISABLED`] when
    /// observation is off and [`OTHER`] when the region table is full.
    pub fn resolve_region(&mut self, prefix: &[u8]) -> usize {
        if !self.enabled {
            return DISABLED;
        }
        match self.region_keys.iter().position(|k| k.as_slice() == prefix) {
            Some(i) => i,
            None if self.region_keys.len() < MAX_REGIONS => {
                self.region_keys.push(prefix.to_vec());
                self.region_stats.push(RegionStats::default());
                self.region_keys.len() - 1
            }
            None => OTHER,
        }
    }

    /// Resolve a `MARKER | entity | 0` region from its parts, building the
    /// prefix in the metrics' own scratch buffer so the commit path allocates
    /// nothing after first sight. Same table as [`Metrics::resolve_region`].
    pub fn resolve_region_parts(&mut self, marker: u8, entity: &[u8]) -> usize {
        if !self.enabled {
            return DISABLED;
        }
        let mut scratch = std::mem::take(&mut self.region_scratch);
        scratch.clear();
        scratch.push(marker);
        scratch.extend_from_slice(entity);
        scratch.push(0);
        let id = match self.region_keys.iter().position(|k| k.as_slice() == scratch.as_slice()) {
            Some(i) => i,
            None if self.region_keys.len() < MAX_REGIONS => {
                self.region_keys.push(scratch.clone());
                self.region_stats.push(RegionStats::default());
                self.region_keys.len() - 1
            }
            None => OTHER,
        };
        self.region_scratch = scratch;
        id
    }

    /// The `RegionStats` a region id refers to, folding unknown or `OTHER` ids
    /// into the `other` row rather than panicking on a stale id.
    fn region_stats_mut(&mut self, id: usize) -> &mut RegionStats {
        match id {
            OTHER => &mut self.region_other,
            i if i < self.region_stats.len() => &mut self.region_stats[i],
            _ => &mut self.region_other,
        }
    }

    /// Record one point lookup against its region; `hit = false` is a probe that
    /// returned no row (the signal a bloom would pay for itself).
    pub fn region_probe(&mut self, id: usize, hit: bool) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let s = self.region_stats_mut(id);
        s.probes = s.probes.saturating_add(1);
        if !hit {
            s.probe_misses = s.probe_misses.saturating_add(1);
        }
    }

    /// Add data-block bytes read from tiles while serving a region.
    pub fn region_bytes_read(&mut self, id: usize, bytes: u64) {
        if !self.enabled || id == DISABLED || bytes == 0 {
            return;
        }
        let s = self.region_stats_mut(id);
        s.bytes_read = s.bytes_read.saturating_add(bytes);
    }

    /// Add encoded WAL payload bytes for a region's keys.
    pub fn region_bytes_written(&mut self, id: usize, bytes: u64) {
        if !self.enabled || id == DISABLED || bytes == 0 {
            return;
        }
        let s = self.region_stats_mut(id);
        s.bytes_written = s.bytes_written.saturating_add(bytes);
    }

    /// Record one latency sample against a region, using the same log bucket
    /// estimator as the per-pattern rows. A no-op when disabled.
    pub fn record_region_latency(&mut self, id: usize, elapsed: Duration) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let b = bucket_index(&self.bounds, ns);
        let s = self.region_stats_mut(id);
        s.count = s.count.saturating_add(1);
        s.sum_ns = s.sum_ns.saturating_add(ns as u128);
        s.buckets[b] = s.buckets[b].saturating_add(1);
    }

    /// Count one committed op in a region.
    pub fn region_commit_op(&mut self, id: usize) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let s = self.region_stats_mut(id);
        s.commit_ops = s.commit_ops.saturating_add(1);
    }

    /// Count one committed WAL record (block) touching a region. Callers invoke
    /// this once per distinct region in a block.
    pub fn region_commit_block(&mut self, id: usize) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let s = self.region_stats_mut(id);
        s.commit_blocks = s.commit_blocks.saturating_add(1);
    }

    /// Count one successful compaction merge that included a region and add the
    /// merged output tile's bytes to it. Callers pass the output tile's on-disk
    /// size; a caller with no bytes to attribute still counts the merge by
    /// passing 0. See the module docs for the per-region rewrite factor this
    /// makes derivable. Observability only; a no-op when disabled.
    pub fn region_compaction(&mut self, id: usize, output_bytes: u64) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let s = self.region_stats_mut(id);
        s.compaction_merges = s.compaction_merges.saturating_add(1);
        s.compaction_bytes = s.compaction_bytes.saturating_add(output_bytes);
    }

    /// WP-M: attribute the derived secondary-access-path tile's output bytes to
    /// the region's own `index_output_bytes` counter. It is priced in the
    /// learner's reward (endurance) but kept OUT of `compaction_bytes` so the
    /// slice-39 merge-churn budget (which keys off `compaction_bytes`) does not
    /// treat a one-time index build as recurring reorganisation. It does NOT
    /// increment `compaction_merges` (an index build is not a merge).
    pub fn region_index_output(&mut self, id: usize, output_bytes: u64) {
        if !self.enabled || id == DISABLED {
            return;
        }
        let s = self.region_stats_mut(id);
        s.index_output_bytes = s.index_output_bytes.saturating_add(output_bytes);
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
        let mut regions = Vec::with_capacity(self.region_keys.len() + 1);
        for (i, k) in self.region_keys.iter().enumerate() {
            let s = &self.region_stats[i];
            if s.is_idle() {
                continue;
            }
            regions.push(region_summary(keys::region_name(k), s, &self.bounds));
        }
        if !self.region_other.is_idle() {
            regions.push(region_summary("other".to_string(), &self.region_other, &self.bounds));
        }
        WindowSnapshot { patterns, regions }
    }

    /// The plain, printable summary of the *current* window (no mutation).
    pub fn snapshot(&self) -> WindowSnapshot {
        self.build_snapshot()
    }

    /// Make the current window's snapshot available as the previous window and
    /// clear every counter, keeping at most one previous window. The intern
    /// tables are deliberately *not* cleared, so pattern/region indices stay
    /// stable and the next window re-resolves without reallocating. Returns the
    /// finished window.
    pub fn rotate_window(&mut self) -> &WindowSnapshot {
        self.previous = Some(self.build_snapshot());
        for s in &mut self.stats {
            *s = PatternStats::default();
        }
        self.other = PatternStats::default();
        for s in &mut self.region_stats {
            *s = RegionStats::default();
        }
        self.region_other = RegionStats::default();
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
        p50_ns: percentile(count, &s.buckets, bounds, 0.50),
        p99_ns: percentile(count, &s.buckets, bounds, 0.99),
        mean_ns: if count == 0 { 0.0 } else { s.sum_ns as f64 / count as f64 },
    }
}

/// Quantile estimator: find the bucket holding the `q`-th rank and interpolate
/// inside it (linear on the underflow bin, geometric elsewhere). Returns
/// nanoseconds. See the module docs for the 15% worst-case relative error.
fn percentile(count: u64, buckets: &[u32; BUCKETS], bounds: &[u64; BUCKETS], q: f64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let target = ((q * count as f64).ceil() as u64).clamp(1, count);
    let mut cum = 0u64;
    for i in 0..BUCKETS {
        let in_bucket = u64::from(buckets[i]);
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

fn region_summary(region: String, s: &RegionStats, bounds: &[u64; BUCKETS]) -> RegionSummary {
    RegionSummary {
        region,
        probes: s.probes,
        probe_misses: s.probe_misses,
        bytes_read: s.bytes_read,
        bytes_written: s.bytes_written,
        commit_blocks: s.commit_blocks,
        commit_ops: s.commit_ops,
        compaction_bytes: s.compaction_bytes,
        compaction_merges: s.compaction_merges,
        index_output_bytes: s.index_output_bytes,
        count: s.count,
        p50_ns: percentile(s.count, &s.buckets, bounds, 0.50),
        p99_ns: percentile(s.count, &s.buckets, bounds, 0.99),
        mean_ns: if s.count == 0 { 0.0 } else { s.sum_ns as f64 / s.count as f64 },
    }
}
