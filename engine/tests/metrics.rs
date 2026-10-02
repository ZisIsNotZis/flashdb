//! Slice 16: per-pattern latency telemetry — estimator accuracy, the pattern
//! cap's folding rule, window rollup, disabled mode, determinism, and the
//! engine call-site instrumentation.

use flashdb_engine::engine::{Engine, Op, RegionMode};
use flashdb_engine::metrics::{Metrics, RegionSummary, WindowSnapshot, DISABLED, MAX_PATTERNS, MAX_REGIONS, OTHER};
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

// ---------- slice 26: per-region counters ----------

fn region_of<'a>(snap: &'a WindowSnapshot, name: &str) -> Option<&'a RegionSummary> {
    snap.regions.iter().find(|r| r.region == name)
}

#[test]
fn region_counters_attribute_probes_misses_reads_writes_and_commits() {
    let d = dir();
    let mut e = Engine::create(d.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    // Two docs in P|E at handles 1 and 5, published so the reads are tile-resident.
    e.commit_block(b"b1", &[
        Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"a".to_vec() },
        Op::PutDoc { entity: b"E".to_vec(), handle: 5, doc: b"b".to_vec() },
    ]).unwrap();
    e.build_tile(d.join("data.tile"), 1).unwrap();
    assert_eq!(e.serving_memtable_entries(), 0, "reads must be tile-resident");

    assert_eq!(e.get(b"E", 1).unwrap().as_deref(), Some(&b"a"[..]), "hit");
    assert_eq!(e.get(b"E", 3).unwrap(), None, "in-range miss (handle 3 sits between 1 and 5)");
    assert_eq!(e.get(b"F", 1).unwrap(), None, "a region no tile covers");

    let snap = e.metrics_snapshot();
    let pe = region_of(&snap, "P|E").expect("P|E region row");
    assert_eq!(pe.probes, 2, "the two P|E gets; the P|F get is its own region");
    assert_eq!(pe.probe_misses, 1, "handle 1 is a hit, handle 3 is a miss");
    assert!(pe.bytes_read > 0, "the in-range miss still read a tile block");
    assert_eq!(pe.commit_ops, 2);
    assert_eq!(pe.commit_blocks, 1, "both ops were in one block");
    assert_eq!(pe.count, 2, "two latency samples: the hit and the in-range miss");
    assert!(pe.mean_ns > 0.0, "a tile read takes measurable time");
    // entity "E" (1 byte), doc 1 byte: each op = tag 1 + (4+1) + 8 + (4+1) = 19.
    assert_eq!(pe.bytes_written, 38, "two 19-byte PutDoc ops");

    let pf = region_of(&snap, "P|F").expect("P|F region row");
    assert_eq!(pf.probes, 1);
    assert_eq!(pf.probe_misses, 1);
    assert_eq!(pf.bytes_read, 0, "no tile covers P|F, so no block was read");
    assert_eq!(pf.bytes_written, 0, "P|F was only read");
    assert!(region_of(&snap, "U|E").is_none(), "an idle region must not appear");

    // A unique-index commit lands in U|E, not P|E: same entity, different family.
    e.commit_block(b"b2", &[Op::PutUnique {
        entity: b"E".to_vec(), field: b"f".to_vec(), value: b"v".to_vec(), handle: 1,
    }]).unwrap();
    let snap = e.metrics_snapshot();
    let ue = region_of(&snap, "U|E").expect("U|E region row");
    assert_eq!(ue.commit_ops, 1);
    assert_eq!(ue.bytes_written, 1 + (4 + 1) + (4 + 1) + (4 + 1) + 8, "one 24-byte PutUnique op");
    assert_eq!(region_of(&snap, "P|E").unwrap().commit_blocks, 1, "the unique op is not a P|E block");

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn region_cap_folds_extra_regions_into_other() {
    let mut m = Metrics::new();
    for i in 0..MAX_REGIONS {
        let name = format!("P{i}\x00");
        let id = m.resolve_region(name.as_bytes());
        assert!(id < MAX_REGIONS);
        m.region_probe(id, true);
    }
    assert_eq!(m.region_count(), MAX_REGIONS);

    const EXTRA: u64 = 20;
    for i in 0..EXTRA {
        let name = format!("X{i}\x00");
        let id = m.resolve_region(name.as_bytes());
        assert_eq!(id, OTHER, "a region past the cap must fold, not intern");
        m.region_probe(id, false);
    }
    assert_eq!(m.region_count(), MAX_REGIONS, "the region table must not grow past the cap");
    let snap = m.snapshot();
    assert_eq!(snap.regions.len(), MAX_REGIONS + 1, "named regions plus the other bin");
    let other = region_of(&snap, "other").expect("other region row");
    assert_eq!(other.probes, EXTRA);
    assert_eq!(other.probe_misses, EXTRA);
    assert_eq!(region_of(&snap, "P|0").unwrap().probes, 1);
}

#[test]
fn disabled_mode_records_no_regions() {
    let mut m = Metrics::new();
    m.set_enabled(false);
    let id = m.resolve_region(b"PE\x00");
    assert_eq!(id, DISABLED);
    m.region_probe(id, false);
    m.region_bytes_read(id, 4096);
    m.region_bytes_written(id, 100);
    m.region_commit_op(id);
    m.region_commit_block(id);
    m.region_compaction(id, 2048);
    m.record_region_latency(id, Duration::from_nanos(1_000));
    m.region_probe(OTHER, false); // even a stale id is a no-op when off
    assert!(m.snapshot().regions.is_empty());
    assert!(m.snapshot().patterns.is_empty());
    assert_eq!(m.region_count(), 0);
    assert_eq!(m.resolve_region_parts(b'P', b"E"), DISABLED);
}

#[test]
fn window_rollup_moves_region_counters_and_omits_idle_regions() {
    let mut m = Metrics::new();
    let a = m.resolve_region(b"PA\x00");
    let b = m.resolve_region(b"PB\x00");
    m.region_probe(a, true);
    m.region_probe(a, false);
    m.region_probe(b, true);
    m.region_bytes_read(a, 4096);

    let finished = m.rotate_window().clone();
    assert_eq!(region_of(&finished, "P|A").unwrap().probes, 2);
    assert_eq!(region_of(&finished, "P|A").unwrap().probe_misses, 1);
    assert_eq!(region_of(&finished, "P|A").unwrap().bytes_read, 4096);
    assert_eq!(region_of(&finished, "P|B").unwrap().probes, 1);
    assert!(m.snapshot().regions.is_empty(), "the current window must be cleared");
    assert_eq!(m.previous_window().map(|w| w.regions.len()), Some(2));

    // Second window touches only P|A: P|B is interned but idle, so omitted.
    let a2 = m.resolve_region(b"PA\x00");
    assert_eq!(a, a2, "region indices stay stable across a rollup");
    m.region_probe(a, true);
    let cur = m.snapshot();
    assert_eq!(region_of(&cur, "P|A").unwrap().probes, 1);
    assert!(region_of(&cur, "P|B").is_none(), "an idle region is omitted, not zero-filled");
    assert_eq!(region_of(m.previous_window().unwrap(), "P|A").unwrap().probes, 2, "the previous window is immutable");
}

#[test]
fn region_json_dump_is_deterministic_and_uses_frozen_field_names() {
    fn dump() -> String {
        let mut m = Metrics::new();
        let r = m.resolve_region(b"PStock\x00");
        let s = m.resolve_region(b"UStock\x00");
        m.region_probe(r, true);
        m.region_probe(r, false);
        m.region_bytes_read(r, 4096);
        m.region_bytes_written(r, 100);
        m.region_commit_op(r);
        m.region_commit_block(r);
        m.region_compaction(r, 2048);
        m.region_bytes_written(s, 7);
        m.snapshot().to_json().to_string()
    }
    let a = dump();
    assert_eq!(a, dump(), "same input, same dump");
    // The whole document is pinned, so a renamed field is a failing diff here.
    assert_eq!(
        a,
        r#"{"patterns":[],"regions":[{"bytes_read":4096,"bytes_written":100,"commit_blocks":1,"commit_ops":1,"compaction_bytes":2048,"compaction_merges":1,"count":0,"mean_ns":0.0,"p50_ns":0.0,"p99_ns":0.0,"probe_misses":1,"probes":2,"region":"P|Stock"},{"bytes_read":0,"bytes_written":7,"commit_blocks":0,"commit_ops":0,"compaction_bytes":0,"compaction_merges":0,"count":0,"mean_ns":0.0,"p50_ns":0.0,"p99_ns":0.0,"probe_misses":0,"probes":0,"region":"U|Stock"}]}"#
    );
}

// ---------- slice 34: per-region compaction attribution ----------

/// A small deterministic engine (no long load): `P|E` gets two tiles and is the
/// only region eligible under the default `merge_min_tiles = 2`, while `P|F`
/// holds one tile and is not. The merge's output byte count must land on `P|E`
/// only. The dumped JSON is printed under `--nocapture`, which is the real
/// measurement for the slice report.
#[test]
fn region_compaction_counters_attribute_the_merged_output_to_its_region() {
    let d = dir();
    let mut e = Engine::create(d.join("data.wal")).unwrap();
    e.set_region_mode(RegionMode::PerRegion);
    e.commit_block(b"b1", &[Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"a".to_vec() }]).unwrap();
    e.publish_tile(d.join("t1.tile"), 1).unwrap();
    e.commit_block(b"b2", &[Op::PutDoc { entity: b"E".to_vec(), handle: 2, doc: b"b".to_vec() }]).unwrap();
    e.publish_tile(d.join("t2.tile"), 2).unwrap();
    e.commit_block(b"b3", &[Op::PutDoc { entity: b"F".to_vec(), handle: 3, doc: b"c".to_vec() }]).unwrap();
    e.publish_tile(d.join("t3.tile"), 3).unwrap();
    assert_eq!(e.active_tile_cutoffs().len(), 3, "two P|E tiles and one P|F tile");

    let out_bytes = e.maybe_compact(u64::MAX, 2).unwrap().expect("P|E has two tiles and merges");
    let snap = e.metrics_snapshot();
    println!("region_compaction_dump={}", snap.to_json());
    let pe = region_of(&snap, "P|E").expect("P|E region row");
    assert_eq!(pe.compaction_merges, 1, "exactly one region merge included P|E");
    assert_eq!(pe.compaction_bytes, out_bytes, "the merged output tile's size is attributed to P|E");
    assert!(pe.compaction_bytes > 0);
    let pf = region_of(&snap, "P|F").expect("P|F saw a commit and is not idle");
    assert_eq!(pf.compaction_merges, 0, "P|F had one tile and was not eligible");
    assert_eq!(pf.compaction_bytes, 0, "no compaction bytes are attributed to a region that did not merge");
    assert!(pe.compaction_bytes > pf.compaction_bytes);

    // Disabled mode records nothing, including on the compaction hook.
    e.set_metrics_enabled(false);
    let mut m = Metrics::new();
    m.set_enabled(false);
    let id = m.resolve_region(b"PE\x00");
    m.region_compaction(id, 4096);
    assert!(m.snapshot().regions.is_empty(), "disabled region_compaction records nothing");

    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn identical_engine_runs_produce_the_same_region_dump() {
    fn run() -> String {
        let d = dir();
        let mut e = Engine::create(d.join("data.wal")).unwrap();
        e.commit_block(b"b1", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"a".to_vec() },
            Op::PutDoc { entity: b"F".to_vec(), handle: 2, doc: b"b".to_vec() },
        ]).unwrap();
        e.build_tile(d.join("data.tile"), 1).unwrap();
        e.get(b"E", 1).unwrap();
        e.get(b"F", 9).unwrap();
        let mut json = e.metrics_snapshot().to_json();
        let _ = std::fs::remove_dir_all(&d);
        // Latency is wall-clock and therefore not byte-reproducible; the counters
        // are. (The metrics-level dump test pins the full document with fixed
        // durations.)
        if let Some(rows) = json["regions"].as_array_mut() {
            for row in rows {
                let obj = row.as_object_mut().unwrap();
                for key in ["count", "mean_ns", "p50_ns", "p99_ns"] {
                    obj.remove(key);
                }
            }
        }
        json["regions"].to_string()
    }
    assert_eq!(run(), run(), "same input, same region dump");
}
