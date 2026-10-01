//! Slice 16: per-pattern latency telemetry — estimator accuracy, the pattern
//! cap's folding rule, window rollup, disabled mode, determinism, and the
//! engine call-site instrumentation.

use flashdb_engine::engine::{Engine, Op};
use flashdb_engine::metrics::{Metrics, WindowSnapshot, DISABLED, MAX_PATTERNS, OTHER};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);

fn dir() -> PathBuf {
    let p = std::env::temp_dir().join(format!("flashdb-metrics-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// Nearest-rank percentile of a sample, the convention the estimator uses: the
/// `ceil(q*n)`-th smallest value.
fn direct_percentile(mut sample: Vec<u64>, q: f64) -> f64 {
    sample.sort_unstable();
    let n = sample.len();
    let rank = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
    sample[rank] as f64
}

fn count_of(snap: &WindowSnapshot, pattern: &str) -> u64 {
    snap.patterns.iter().find(|p| p.pattern == pattern).map(|p| p.count).unwrap_or(0)
}

/// SplitMix64, so the synthetic samples are deterministic without a dependency.
fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

const N: usize = 100_000;

/// A uniform sample over `[lo, hi]` ns.
fn uniform_sample(seed: u64, lo: u64, hi: u64) -> Vec<u64> {
    let mut s = seed;
    (0..N).map(|_| lo + splitmix(&mut s) % (hi - lo)).collect()
}

/// The estimator's worst case is the bucket ratio minus one (15%, see
/// `metrics.rs`); the observed error on a smooth distribution is a few percent,
/// so 15% absolute-relative tolerance is generous and still meaningful.
const TOLERANCE: f64 = 0.15;

fn check_estimate(sample: &[u64], label: &str) {
    let mut m = Metrics::new();
    let id = m.resolve_primary(b"E");
    for &x in sample {
        m.record(id, Duration::from_nanos(x));
    }
    let snap = m.snapshot();
    let p = &snap.patterns[0];
    let direct = direct_percentile(sample.to_vec(), 0.99);
    let rel = (p.p99_ns - direct).abs() / direct.max(1.0);
    assert!(rel <= TOLERANCE, "{label}: estimator p99={:.0} direct={direct:.0} rel_err={rel:.3}", p.p99_ns);
    assert_eq!(p.count, sample.len() as u64);
    // The mean is exact: it is a sum, not a bucket reconstruction.
    let mean = sample.iter().map(|&x| x as f64).sum::<f64>() / sample.len() as f64;
    assert!((p.mean_ns - mean).abs() / mean.max(1.0) < 1e-9, "{label}: mean {:.1} vs {mean:.1}", p.mean_ns);
}

#[test]
fn histogram_p99_matches_direct_percentile_within_tolerance() {
    // Uniform over 1 µs .. 5 ms: spans tens of buckets and puts p99 (~4.95 ms)
    // near the top but inside the histogram's covered range.
    let sample = uniform_sample(0x5EED_0001, 1_000, 5_000_000);
    check_estimate(&sample, "uniform");
}

#[test]
fn p99_estimator_error_on_known_exponential_distribution() {
    // Exponential, mean 1 ms: p99 = mean * ln 100 ≈ 4.605 ms, known in closed form.
    let mut s = 0x5EED_0002u64;
    let mean = 1_000_000.0f64;
    let mut sample = Vec::with_capacity(N);
    for _ in 0..N {
        let u = (splitmix(&mut s) as f64 + 1.0) / (u64::MAX as f64 + 1.0);
        sample.push((-u.ln() * mean).round() as u64 + 1);
    }
    let theoretical = mean * 100f64.ln();
    let direct = direct_percentile(sample.clone(), 0.99);
    // The sample's own p99 matches the closed form well enough to anchor it.
    assert!((direct - theoretical).abs() / theoretical < 0.05, "sample p99 {direct:.0} vs theory {theoretical:.0}");
    check_estimate(&sample, "exponential");
}

#[test]
fn pattern_cap_folds_extra_patterns_into_other() {
    let mut m = Metrics::new();
    for i in 0..MAX_PATTERNS {
        let id = m.resolve_primary(format!("E{i}").as_bytes());
        assert!(id < MAX_PATTERNS);
        m.record(id, Duration::from_nanos(1_000));
    }
    assert_eq!(m.pattern_count(), MAX_PATTERNS);

    const EXTRA: u64 = 20;
    for i in 0..EXTRA {
        let id = m.resolve_primary(format!("X{i}").as_bytes());
        assert_eq!(id, OTHER, "a pattern past the cap must fold, not be interned");
        m.record(id, Duration::from_nanos(2_000));
    }
    assert_eq!(m.pattern_count(), MAX_PATTERNS, "the intern table must not grow past the cap");
    let snap = m.snapshot();
    assert_eq!(snap.patterns.len(), MAX_PATTERNS + 1, "named patterns plus the other bin");
    assert_eq!(count_of(&snap, "other"), EXTRA);
    assert_eq!(count_of(&snap, "P:E0"), 1);
}

#[test]
fn window_rollup_moves_counts_to_previous_and_clears_current() {
    let mut m = Metrics::new();
    let a = m.resolve_primary(b"A");
    let u = m.resolve_unique(b"A", b"f");
    m.record(a, Duration::from_nanos(1_000));
    m.record(a, Duration::from_nanos(2_000));
    m.record(u, Duration::from_nanos(3_000));

    let finished = m.rotate_window().clone();
    assert_eq!(count_of(&finished, "P:A"), 2);
    assert_eq!(count_of(&finished, "U:A.f"), 1);

    // Current window is cleared; the finished window is retained exactly once.
    assert!(m.snapshot().patterns.is_empty(), "the current window must be cleared");
    assert_eq!(m.previous_window().map(|w| w.patterns.len()), Some(2));

    // A second window records independently; indices stay stable after rotation.
    let a2 = m.resolve_primary(b"A");
    assert_eq!(a, a2);
    m.record(a, Duration::from_nanos(4_000));
    let cur = m.snapshot();
    assert_eq!(count_of(&cur, "P:A"), 1);
    assert_eq!(count_of(&cur, "U:A.f"), 0, "an idle pattern is omitted, not zero-counted");
    assert_eq!(count_of(m.previous_window().unwrap(), "P:A"), 2, "the previous window is immutable");
}

#[test]
fn disabled_mode_resolves_and_records_nothing() {
    let mut m = Metrics::new();
    m.set_enabled(false);
    let id = m.resolve_primary(b"A");
    assert_eq!(id, DISABLED);
    m.record(id, Duration::from_nanos(1_000));
    m.record(0, Duration::from_nanos(1_000)); // even a stale id is a no-op when off
    assert!(m.snapshot().patterns.is_empty());
    assert_eq!(m.pattern_count(), 0);
    assert!(!m.enabled());
}

#[test]
fn same_inputs_produce_the_same_snapshot() {
    fn run() -> WindowSnapshot {
        let mut m = Metrics::new();
        for i in 0..2_000u64 {
            let id = match i % 4 {
                0 => m.resolve_primary(b"A"),
                1 => m.resolve_unique(b"A", b"f"),
                2 => m.resolve_scan(b"B"),
                _ => m.resolve_commit(),
            };
            m.record(id, Duration::from_nanos(100 + (i * 7919) % 50_000));
        }
        m.snapshot()
    }
    assert_eq!(run(), run());
}

#[test]
fn engine_instruments_each_read_path_and_commit() {
    let d = dir();
    let mut e = Engine::create(d.join("data.wal")).unwrap();
    let ops = vec![
        Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"payload".to_vec() },
        Op::PutUnique { entity: b"E".to_vec(), field: b"f".to_vec(), value: b"v".to_vec(), handle: 1 },
        Op::PutReverse { entity: b"E".to_vec(), field: b"g".to_vec(), target: 7, source: 1 },
    ];
    e.commit_block(b"b0", &ops).unwrap();
    assert_eq!(e.get(b"E", 1).unwrap().as_deref(), Some(&b"payload"[..]));
    assert_eq!(e.unique_lookup(b"E", b"f", b"v").unwrap(), Some(1));
    assert_eq!(e.reverse_lookup(b"E", b"g", 7).unwrap(), vec![1]);
    let mut scanned = 0;
    e.scan_primary(b"E", |_, _| { scanned += 1; Ok(()) }).unwrap();
    assert_eq!(scanned, 1);

    let snap = e.metrics_snapshot();
    for key in ["commit", "P:E", "U:E.f", "R:E.g", "scan:E"] {
        assert!(count_of(&snap, key) >= 1, "missing pattern {key}: {:?}", snap.patterns.iter().map(|p| &p.pattern).collect::<Vec<_>>());
    }
    let commit_before = count_of(&snap, "commit");

    // Disabled mode: the same calls record nothing new.
    e.set_metrics_enabled(false);
    e.commit_block(b"b1", &ops).unwrap();
    e.get(b"E", 1).unwrap();
    e.unique_lookup(b"E", b"f", b"v").unwrap();
    e.scan_primary(b"E", |_, _| Ok(())).unwrap();
    assert!(!e.metrics_enabled());
    let after = e.metrics_snapshot();
    assert_eq!(count_of(&after, "commit"), commit_before);
    assert_eq!(after, snap, "disabled calls must not change any counter");

    let _ = std::fs::remove_dir_all(&d);
}
