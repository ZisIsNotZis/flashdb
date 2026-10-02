"""Tests for the offered-load profile generator and its drift injection.

The scored property (objective v1) is a capacity curve over offered load, so the
things that must not silently break are: the trace is byte-identical for a seed,
the manifest describes exactly the trace that was emitted, drift measurably moves
the mix only where it is injected, the SLO ceiling arithmetic is right, and an
impossible segment (zero duration, non-positive rate) is rejected loudly.
"""

import hashlib
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import load_profile as lp  # noqa: E402
from flashdb_harness.generator import MIX, sku_name  # noqa: E402


def example() -> lp.LoadProfile:
    return lp.build_example_profile(seed=7)


# --- determinism -----------------------------------------------------------
def test_same_seed_is_byte_identical():
    assert lp.trace_bytes(example()) == lp.trace_bytes(example())


def test_different_seed_differs():
    assert lp.trace_bytes(lp.build_example_profile(seed=7)) != lp.trace_bytes(
        lp.build_example_profile(seed=8)
    )


def test_key_spike_is_deterministic_too():
    p = lp.LoadProfile(seed=3, segments=[
        lp.Segment(offered_rate=100, count=500,
                   drift=lp.Drift(kind="key", shape="stock_lookup", sku_rank=1, spike_pct=80)),
    ])
    data = lp.trace_bytes(p)
    assert data == lp.trace_bytes(p)
    assert data != lp.trace_bytes(lp.LoadProfile(seed=4, segments=p.segments))


# --- manifest matches the trace -------------------------------------------
def test_manifest_segments_sum_to_the_trace_and_hash_matches():
    profile = example()
    data, manifest = lp.generate(profile)
    assert manifest["schema"] == lp.MANIFEST_SCHEMA
    assert manifest["trace_sha256"] == hashlib.sha256(data).hexdigest()
    assert manifest["total_requests"] == data.count(b"\n")
    assert manifest["total_requests"] == profile.total_requests()

    start = 0
    for seg in manifest["segments"]:
        assert seg["start_request"] == start
        start += seg["count"]
    assert start == manifest["total_requests"]
    assert sum(s["count"] for s in manifest["segments"]) == data.count(b"\n")


def test_manifest_is_sorted_keys_and_lf():
    _data, manifest = lp.generate(example())
    text = lp.manifest_text(manifest)
    assert "\r" not in text and text.endswith("\n")
    top_keys = [ln for ln in text.splitlines() if ln.startswith('  "')]
    assert top_keys == sorted(top_keys), "manifest top-level keys must be sorted"
    assert top_keys[0] == '  "n_customers": 5000,'


# --- drift changes the mix, and only where injected ------------------------
def test_weight_drift_raises_the_injected_shape_share_only_in_its_segment():
    profile = example()
    _data, manifest = lp.generate(profile)
    shares = lp.segment_shares(profile)

    injected = next(s for s in manifest["segments"] if s["drift"] is not None)
    assert injected["index"] == 1
    assert injected["drift"]["kind"] == "weight"
    assert injected["drift"]["shape"] == "movements_scan"

    injected_share = shares[1]["movements_scan"]
    baseline_before = shares[0]["movements_scan"]
    baseline_after = shares[2]["movements_scan"]
    # 5/(70+15+10+5) ~= 0.05 baseline; 50/145 ~= 0.345 injected.
    assert injected_share > 3 * baseline_before, (injected_share, baseline_before)
    assert injected_share > 3 * baseline_after, (injected_share, baseline_after)
    assert baseline_after == pytest.approx(baseline_before, abs=0.02), "only the injected segment drifts"


def test_key_drift_spikes_one_sku_only_in_the_injected_segment():
    profile = lp.LoadProfile(seed=5, segments=[
        lp.Segment(offered_rate=100, count=3_000),
        lp.Segment(offered_rate=100, count=3_000,
                   drift=lp.Drift(kind="key", shape="stock_lookup", sku_rank=1, spike_pct=80)),
    ])
    hot = sku_name(1)
    totals = [0, 0]
    hits = [0, 0]
    for index, kind, req in lp.iter_segment_requests(profile):
        if kind != "stock_lookup":
            continue
        totals[index] += 1
        if req["blocks"]["q"]["get"]["Stock"]["where"]["sku"] == hot:
            hits[index] += 1
    before, after = hits[0] / totals[0], hits[1] / totals[1]
    assert before < 0.2, f"rank-1 sku should be ~Zipf share, got {before:.2f}"
    assert after > 0.5, f"spike should dominate the draws, got {after:.2f}"


def test_deadline_drift_adds_a_low_weight_shape_and_records_the_deadline():
    profile = lp.LoadProfile(seed=1, segments=[
        lp.Segment(offered_rate=100, count=500,
                   drift=lp.Drift(kind="deadline", shape="customer_lookup",
                                  weight=2, deadline_s=50)),
    ])
    _data, manifest = lp.generate(profile)
    drift = manifest["segments"][0]["drift"]
    assert drift["kind"] == "deadline" and drift["deadline_s"] == 50
    assert manifest["segments"][0]["mix"]["customer_lookup"] == MIX["customer_lookup"] + 2


# --- SLO ceiling helper (pure over the metrics table) ----------------------
def test_slo_ceilings_multiply_the_reference_p99_by_the_declared_inflation():
    rows = [{"pattern": "order_create", "p99": 100.0}, {"pattern": "stock_by_sku", "p99": 50.0}]
    assert lp.slo_ceilings(rows) == {"order_create": 200.0, "stock_by_sku": 100.0}
    assert lp.slo_ceilings(rows, inflation=1.5) == {"order_create": 150.0, "stock_by_sku": 75.0}
    assert lp.slo_ceilings([]) == {}
    assert lp.DEFAULT_SLO_INFLATION == 2.0
    with pytest.raises(ValueError, match="metrics row"):
        lp.slo_ceilings([{"pattern": "x"}])


def test_reference_segment_is_the_lowest_offered_rate():
    profile = lp.LoadProfile(seed=1, segments=[
        lp.Segment(offered_rate=4_000, count=100),
        lp.Segment(offered_rate=1_000, count=100),
        lp.Segment(offered_rate=2_000, count=100),
    ])
    assert lp.reference_segment_index(profile) == 1


# --- invalid segments are rejected clearly ---------------------------------
def test_zero_duration_or_count_is_rejected():
    with pytest.raises(ValueError, match="seconds"):
        lp.Segment(offered_rate=100, seconds=0)
    with pytest.raises(ValueError, match="count"):
        lp.Segment(offered_rate=100, count=0)


def test_non_positive_offered_rate_is_rejected():
    with pytest.raises(ValueError, match="offered_rate"):
        lp.Segment(offered_rate=-1, seconds=1)
    with pytest.raises(ValueError, match="offered_rate"):
        lp.Segment(offered_rate=0, seconds=1)


def test_ambiguous_duration_and_count_is_rejected():
    with pytest.raises(ValueError, match="exactly one"):
        lp.Segment(offered_rate=100)  # neither
    with pytest.raises(ValueError, match="exactly one"):
        lp.Segment(offered_rate=100, seconds=1, count=1)  # both
