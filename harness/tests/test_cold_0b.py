"""Tests for the 0b cold-arm runner (Slice 35): the pure parsing, curve, sweep and verdict logic.

No engine and no long run: every test drives a pure function with a small synthetic window or a
hand-built arm record. The long run itself is a nohup experiment; its logic must be provable here.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import cold_0b as m  # noqa: E402


def _window_line(window: int, rate: float, requests: int, completed: int, *,
                 errors: int = 0, p50: int = 1000, p99: int = 2000,
                 regions: list[dict] | None = None, policy: dict | None = None) -> str:
    return json.dumps({
        "window": window, "rate_rps": rate, "requests": requests,
        "completed": completed, "errors": errors, "p50_us": p50, "p99_us": p99,
        "policy": policy or {"default": {"page_size": 4096, "bloom_bits_per_key": 10,
                                         "merge_min_tiles": 2}, "regions": {}},
        "current": {"patterns": [], "regions": regions or []},
    }, sort_keys=True)


# ---------------------------------------------------------------------------
# Workload plans and traces
# ---------------------------------------------------------------------------
def test_plan_workload_stepping_and_drift_scaling() -> None:
    win = m.plan_workload("win", 50, 1000)
    assert win.rates == [50, 100, 200, 400]
    assert all(p == m.WIN_MIX for p in win.mixes)
    adv = m.plan_workload("adversarial", 50, 1000)
    assert all(p["scan"] == 0 for p in adv.mixes), "adversarial must have no scan locality"
    drift = m.plan_workload("drift", 50, 1000, drift_rate=150)
    assert drift.rates == [150] * 6, "drift runs at one offered rate"
    assert drift.mixes[0]["scan"] == m.WIN_MIX["scan"]
    assert drift.mixes[2]["scan"] == m.WIN_MIX["scan"] * 10, "drift multiplies the shape weight"


def test_build_trace_is_deterministic_and_has_the_expected_shapes(tmp_path) -> None:
    class A:
        seed, customers, skus, locs, on_hand, write_requests = 7, 500, 200, 2, 1000, 300

    plan = m.plan_workload("win", 50, 200)
    first = tmp_path / "a.jsonl"
    second = tmp_path / "b.jsonl"
    meta_a = m.build_trace(first, plan, A)
    meta_b = m.build_trace(second, plan, A)
    assert first.read_bytes() == second.read_bytes(), "same seed and plan must emit identical bytes"
    assert meta_a["total"] == 800 and meta_a["counts"] == meta_b["counts"]
    # Every emitted line is a request the adapter can parse, and the scan keeps the adapter's
    # `find` layout (the entity plus sibling where/order/limit) - the bug a naive nesting hides.
    scans = [json.loads(line) for line in first.read_text().splitlines()]
    scan = next(o for o in scans
                if "q" in o["blocks"] and "StockMovement" in o["blocks"]["q"].get("find", {}))
    find = scan["blocks"]["q"]["find"]
    assert "where" in find["StockMovement"] and find["limit"] == 100 and find["order"] == ["movement_no"]
    writes = [o for o in scans if "take" in o["blocks"]]
    assert writes, "the win trace must include order_flow writes"
    assert any(w["params"]["qty"] > A.on_hand for w in writes), "some writes force the else branch"


# ---------------------------------------------------------------------------
# Window parsing and the capacity curve
# ---------------------------------------------------------------------------
def test_window_records_compute_per_window_throughput_and_region_totals() -> None:
    lines = [
        "device_fsync_p50_us=5000 device_fsync_p99_us=9000",
        _window_line(0, 50, 50, 50, regions=[
            {"region": "P|Stock", "count": 50, "mean_ns": 100.0, "p50_ns": 90.0, "p99_ns": 200.0,
             "bytes_read": 10, "bytes_written": 0, "compaction_bytes": 0, "compaction_merges": 0}]),
        _window_line(1, 50, 50, 100, regions=[
            {"region": "P|Stock", "count": 50, "mean_ns": 100.0, "p50_ns": 90.0, "p99_ns": 200.0,
             "bytes_read": 20, "bytes_written": 0, "compaction_bytes": 30, "compaction_merges": 1}]),
    ]
    recs = m.window_records(lines, window_ms=1000)
    assert len(recs) == 2
    assert recs[0]["achieved_rps"] == 50.0 and recs[1]["achieved_rps"] == 50.0
    totals = m.region_compaction_totals(recs)
    assert totals["P|Stock"]["bytes_read"] == 30
    assert totals["P|Stock"]["compaction_bytes"] == 30
    assert totals["P|Stock"]["compaction_merges"] == 1


def test_summarize_curve_marks_sustained_and_invalid() -> None:
    windows = [
        {"rate_rps": 50.0, "achieved_rps": 49.0, "p50_us": 10, "p99_us": 20, "errors": 0},
        {"rate_rps": 50.0, "achieved_rps": 50.0, "p50_us": 10, "p99_us": 20, "errors": 0},
        {"rate_rps": 100.0, "achieved_rps": 60.0, "p50_us": 10, "p99_us": 20, "errors": 0},
        {"rate_rps": 200.0, "achieved_rps": 199.0, "p50_us": 10, "p99_us": 20, "errors": 1},
    ]
    curve = m.summarize_curve(windows)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[50.0]["sustained"] is True
    assert by_rate[100.0]["sustained"] is False
    assert by_rate[200.0]["sustained"] is False, "a rejected window invalidates the level"
    assert m.highest_sustained_rate(curve) == 50.0


def test_ceiling_breach_invalidates_the_level() -> None:
    windows = [{"rate_rps": 50.0, "achieved_rps": 50.0, "p50_us": 10, "p99_us": 9000, "errors": 0},
               {"rate_rps": 100.0, "achieved_rps": 100.0, "p50_us": 10, "p99_us": 100, "errors": 0}]
    curve = m.summarize_curve(windows, ceiling_us=5000)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[50.0]["sustained"] is False
    assert by_rate[100.0]["sustained"] is True


# ---------------------------------------------------------------------------
# Policy echo, sweep, argmin
# ---------------------------------------------------------------------------
def test_policy_active_map_merges_default_and_region_entry() -> None:
    policy = {"default": {"page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2},
              "regions": {"P|Stock": {"page_size": 65536, "merge_min_tiles": 8}}}
    active = m.policy_active_map(policy)
    assert active["P|Stock"].page_size == 65536 and active["P|Stock"].merge_min_tiles == 8
    assert m.policy_active_map({}) == {}


def test_build_sweep_attributes_window_cost_to_the_echoed_candidate() -> None:
    policy_a = {"default": {"page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2},
                "regions": {"P|Stock": {"page_size": 4096, "merge_min_tiles": 2}}}
    policy_b = {"default": {"page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2},
                "regions": {"P|Stock": {"page_size": 65536, "merge_min_tiles": 2}}}
    lines = [
        _window_line(0, 50, 10, 10, policy=policy_a, regions=[
            {"region": "P|Stock", "count": 10, "mean_ns": 1000.0, "p50_ns": 1, "p99_ns": 2,
             "bytes_read": 0, "bytes_written": 0, "compaction_bytes": 0, "compaction_merges": 0}]),
        _window_line(1, 100, 10, 20, policy=policy_b, regions=[
            {"region": "P|Stock", "count": 10, "mean_ns": 100.0, "p50_ns": 1, "p99_ns": 2,
             "bytes_read": 0, "bytes_written": 0, "compaction_bytes": 0, "compaction_merges": 0}]),
    ]
    sweep = m.build_sweep(m.window_records(lines, 1000))
    picks = m.argmin_from_sweep(sweep)
    assert picks["P|Stock"].page_size == 65536, "the lower-cost echoed candidate wins the argmin"
    # Restricting to the pre-drift (lowest) rate keeps only the first window's evidence.
    sweep_pre = m.build_sweep(m.window_records(lines, 1000), max_rate=50.0)
    assert len(sweep_pre["P|Stock"]) == 1
    assert next(iter(sweep_pre["P|Stock"])).startswith("LayoutCandidate(page_size=4096")


# ---------------------------------------------------------------------------
# Threshold evaluation
# ---------------------------------------------------------------------------
def _record(rates: list[tuple[float, float, bool]], reorg: int = 0,
            recovery: dict | None = None, workload: str | None = None) -> dict:
    return {
        "workload": workload,
        "curve": [{"offered_rps": r, "achieved_rps": a, "sustained": s,
                   "p50_us": 1, "p99_us": 2, "invalid_windows": 0} for r, a, s in rates],
        "regions": {"P|Stock": {"compaction_bytes": reorg}} if reorg else {},
        "recovery_windows": recovery,
        "windows": [],
    }


def test_threshold1_passes_when_learned_sustains_a_higher_level() -> None:
    records = {
        "win/declared": _record([(50, 50, True), (100, 60, False)]),
        "win/learned@1.0": _record([(50, 50, True), (100, 98, True)]),
    }
    verdicts = m.evaluate_thresholds(records, lambdas=(1.0,))
    t1 = verdicts[0]
    assert t1["status"] == "PASS"
    assert "one load level higher" in t1["detail"]


def test_threshold1_passes_on_fifteen_percent_at_the_common_level() -> None:
    records = {
        "win/declared": _record([(50, 50, True), (100, 100, True)]),
        "win/learned@1.0": _record([(50, 50, True), (100, 120, True)]),
    }
    t1 = m.evaluate_thresholds(records, lambdas=(1.0,))[0]
    assert t1["status"] == "PASS" and t1["gain"] > 0.15


def test_threshold1_fails_inside_the_measured_noise_band() -> None:
    records = {
        "win/declared": _record([(50, 100, True), (100, 100, True)]),
        "win/learned@1.0": _record([(50, 102, True), (100, 102, True)]),
    }
    t1 = m.evaluate_thresholds(records, lambdas=(1.0,), noise_band=0.05)[0]
    assert t1["status"] == "NOT-MEASURABLE", "a 2% gap inside a 5% band is not a win"


def test_threshold2_fails_when_learned_loses_more_than_five_percent() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
        "adversarial/declared": _record([(50, 100, True)]),
        "adversarial/learned@1.0": _record([(50, 80, True)]),
    }
    t2 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "adversarial" in v["threshold"]][0]
    assert t2["status"] == "FAIL" and t2["loss"] > 0.05


def test_threshold2_passes_on_a_tie() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
        "adversarial/declared": _record([(50, 100, True)]),
        "adversarial/learned@1.0": _record([(50, 100, True)]),
    }
    t2 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "adversarial" in v["threshold"]][0]
    assert t2["status"] == "PASS" and t2["loss"] == 0.0


def test_threshold3_is_not_measurable_when_the_driver_never_compacts() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
    }
    t3 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "reorganisation" in v["threshold"]][0]
    assert t3["status"] == "NOT-MEASURABLE"
    assert "never calls maybe_compact_shape" in t3["detail"]


def test_threshold3_fails_when_learned_writes_four_times_more() -> None:
    records = {
        "win/declared": _record([(50, 50, True)], reorg=1000),
        "win/learned@1.0": _record([(50, 50, True)], reorg=4000),
    }
    t3 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "reorganisation" in v["threshold"]][0]
    assert t3["status"] == "FAIL"


def test_threshold4_marks_the_two_window_target_unreachable_at_defaults() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
        "drift/learned@1.0": _record([(150, 150, True)], recovery={"P|StockMovement": 6}),
    }
    t4 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "drift" in v["threshold"]][0]
    assert t4["status"] == "NOT MEETABLE WITH CURRENT PARAMETERS"
    assert t4["recovery_windows"] == {"P|StockMovement": 6}


def test_threshold4_passes_on_a_two_window_recovery() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
        "drift/learned@1.0": _record([(150, 150, True)], recovery={"P|StockMovement": 2}),
    }
    t4 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,)) if "drift" in v["threshold"]][0]
    assert t4["status"] == "PASS"


# ---------------------------------------------------------------------------
# Header / summary parsing
# ---------------------------------------------------------------------------
def test_parse_engine_open_and_summary() -> None:
    text = "\n".join([
        "engine_open target=/tmp/x discover=true active_tiles=324 serving_memtable_entries=4000",
        "device_fsync_p50_us=5000 device_fsync_p99_us=9000",
        'load_summary requests=8000 completed=8000 errors=0 else_used=600 late_total=0 windows=60 '
        'invalid_windows=2 last_valid_rate_rps=200 saturated_at_rps=200 commit_batch=1 '
        'group_requests=64 elapsed_ms=60000 achieved_rps=133 first_error="none"',
        "cgroup_peak_bytes=150000000",
    ])
    openinfo = m.parse_engine_open(text)
    assert openinfo["active_tiles"] == 324 and openinfo["serving_memtable_entries"] == 4000
    summary = m.parse_load_summary(text)
    assert summary["commit_rate"] == 1.0
    assert summary["else_used"] == 600
    assert summary["invalid_windows"] == 2
    assert summary["saturated_at_rps"] == 200.0
    assert summary["cgroup_peak_bytes"] == 150000000


# ---------------------------------------------------------------------------
# Ladder under-power guard (Slice 36)
# ---------------------------------------------------------------------------
def test_ladder_underpowered_when_every_arm_sustains_the_top_step() -> None:
    records = {
        "win/declared": _record([(50, 50, True), (100, 100, True), (200, 200, True)], workload="win"),
        "win/learned@1.0": _record([(50, 50, True), (100, 100, True), (200, 200, True)], workload="win"),
        "win/static-derived": _record([(50, 50, True), (100, 100, True), (200, 200, True)], workload="win"),
    }
    assert m.ladder_underpowered(records, "win") is True
    assert m.ladder_underpowered(records, "adversarial") is False, "no arm for that workload"


def test_ladder_not_underpowered_when_one_arm_fails_the_top() -> None:
    records = {
        "win/declared": _record([(50, 50, True), (100, 60, False)], workload="win"),
        "win/learned@1.0": _record([(50, 50, True), (100, 98, True)], workload="win"),
    }
    assert m.ladder_underpowered(records, "win") is False


def test_threshold1_underpowered_ladder_is_not_measurable_not_a_tie() -> None:
    # Every level sustained with equal throughput: the highest-sustained-level metric ties by
    # construction, so the verdict is NOT-MEASURABLE, never a tie PASS/FAIL artefact.
    records = {
        "win/declared": _record([(50, 50, True), (100, 100, True), (200, 200, True), (400, 400, True)]),
        "win/learned@1.0": _record([(50, 50, True), (100, 100, True), (200, 200, True), (400, 400, True)]),
    }
    t1 = m.evaluate_thresholds(records, lambdas=(1.0,))[0]
    assert t1["status"] == "NOT-MEASURABLE"
    assert "ladder under-powered" in t1["detail"]
    assert "top step (400 rps)" in t1["detail"], "the observed top rate must be named"


def test_threshold1_sustained_top_with_a_fifteen_percent_win_still_passes() -> None:
    # A fully-sustained ladder whose top level shows a > 15% throughput gap is decidable (the
    # pre-registration's alternative basis); the guard must not hide a real win.
    records = {
        "win/declared": _record([(50, 50, True), (100, 100, True)]),
        "win/learned@1.0": _record([(50, 50, True), (100, 120, True)]),
    }
    t1 = m.evaluate_thresholds(records, lambdas=(1.0,))[0]
    assert t1["status"] == "PASS" and t1["gain"] > 0.15


def test_extend_ladder_appends_a_rate_and_a_matching_mix() -> None:
    plan = m.plan_workload("win", 50, 100)
    bigger = m.extend_ladder(plan)
    assert bigger.rates == [50, 100, 200, 400, 800]
    assert len(bigger.mixes) == len(bigger.rates), "a rate without a mix would truncate the trace"
    assert bigger.mixes[-1] == plan.mixes[-1]
    assert m.is_ladder(plan) is True
    assert m.is_ladder(m.plan_workload("drift", 50, 100, drift_rate=150)) is False


# ---------------------------------------------------------------------------
# Coldness evidence (Slice 36)
# ---------------------------------------------------------------------------
def test_coldness_evidence_flags_cache_evicting_at_the_cap() -> None:
    cold = m.coldness_evidence({"cgroup_peak_bytes": 190 << 20, "load_hwm_kb": 4096},
                               192 << 20, 1_645_273_432)
    assert cold["cgroup_peak_bytes"] == 190 << 20
    assert cold["cache_evicting"] is True
    assert abs(cold["data_cap_ratio"] - 1_645_273_432 / (192 << 20)) < 1e-9
    assert cold["load_hwm_bytes"] == 4096 * 1024
    warm = m.coldness_evidence({"cgroup_peak_bytes": 8 << 20}, 192 << 20, 1_645_273_432)
    assert warm["cache_evicting"] is False
    assert m.coldness_evidence({}, 192 << 20, 1)["cache_evicting"] is False


def test_summarize_curve_excludes_invalid_windows_from_sustained():
    """Regression for the bug that made a saturated arm score as sustained at every rung.

    The first window of a ladder step can carry completions drained from the previous step, so it
    can look like it achieved the new offered rate. The pacer marks such a window invalid (its
    schedule lag ran away); that flag was not parsed, so the step was scored sustained anyway.
    """
    from flashdb_harness.cold_0b import summarize_curve

    windows = [
        # a genuinely sustained rung: two valid windows at 100 rps
        {"rate_rps": 100.0, "achieved_rps": 100.0, "p50_us": 100, "p99_us": 200,
         "errors": 0, "invalid": False, "lag_p99_us": 1000},
        {"rate_rps": 100.0, "achieved_rps": 99.0, "p50_us": 100, "p99_us": 200,
         "errors": 0, "invalid": False, "lag_p99_us": 1000},
        # a saturated rung: the draining window "achieves" the offered rate but is invalid, while
        # the other window is still valid (the lag rule needs consecutive windows) and shows the
        # engine honestly far behind
        {"rate_rps": 400.0, "achieved_rps": 400.0, "p50_us": 100, "p99_us": 200,
         "errors": 0, "invalid": True, "lag_p99_us": 5_000_000},
        {"rate_rps": 400.0, "achieved_rps": 120.0, "p50_us": 100, "p99_us": 200,
         "errors": 0, "invalid": False, "lag_p99_us": 2_000_000},
    ]
    curve = summarize_curve(windows)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[100.0]["sustained"] is True
    assert by_rate[400.0]["sustained"] is False, (
        "a rung whose windows the pacer marked invalid must never be scored sustained, even if one "
        "of them appears to achieve the offered rate by draining a previous step's backlog")
    assert by_rate[400.0]["invalid_windows"] == 1
    assert by_rate[400.0]["achieved_rps"] == 120.0, "the invalid windows are excluded from the median"

    from flashdb_harness.cold_0b import highest_sustained_rate
    assert highest_sustained_rate(curve) == 100.0
