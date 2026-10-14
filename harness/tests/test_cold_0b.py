"""Tests for the 0b cold-arm runner (Slice 35): the pure parsing, curve, sweep and verdict logic.

No engine and no long run: every test drives a pure function with a small synthetic window or a
hand-built arm record. The long run itself is a nohup experiment; its logic must be provable here.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import cold_0b as m  # noqa: E402


def _curve_window(rate: float, completed_delta: int, *, requests: int | None = None,
                  window_s: float = 1.0, errors: int = 0, invalid: bool = False,
                  p50: int = 10, p99: int = 20, lag_p99: int = 0,
                  ceiling_breached: bool = False) -> dict:
    """A window record shaped like `window_records` output, for `summarize_curve` tests."""
    offered = completed_delta if requests is None else requests
    return {"rate_rps": rate, "requests": offered, "completed": completed_delta,
            "completed_delta": completed_delta, "window_s": window_s, "errors": errors,
            "invalid": invalid, "p50_us": p50, "p99_us": p99, "lag_p99_us": lag_p99,
            "ceiling_breached": ceiling_breached, "ceiling_us": None}


def _window_line(window: int, rate: float, requests: int, completed: int, *,
                 errors: int = 0, p50: int = 1000, p99: int = 2000,
                 regions: list[dict] | None = None, policy: dict | None = None,
                 compact: int = 0) -> str:
    return json.dumps({
        "window": window, "rate_rps": rate, "requests": requests,
        "completed": completed, "errors": errors, "p50_us": p50, "p99_us": p99,
        "compact_bytes": compact,
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


def test_plan_workload_write_drift_multiplies_the_write_weight_only() -> None:
    """WP-W: the write-spike drift is selectable and keeps every other WIN_MIX weight."""
    drift = m.plan_workload("drift", 50, 1000, drift_shape="write", drift_factor=10, drift_rate=150)
    assert drift.rates == [150] * 6
    assert drift.mixes[0] == m.WIN_MIX and drift.mixes[1] == m.WIN_MIX
    drifted = drift.mixes[2]
    assert drifted["write"] == m.WIN_MIX["write"] * 10
    for shape in ("stock", "customer", "scan"):
        assert drifted[shape] == m.WIN_MIX[shape], "only the write weight is multiplied"
    # The default remains the ticket-73 scan spike.
    default = m.plan_workload("drift", 50, 1000, drift_rate=150)
    assert default.mixes[2]["scan"] == m.WIN_MIX["scan"] * 10
    assert default.mixes[2]["write"] == m.WIN_MIX["write"]


def test_plan_workload_drift_pre_segments_extends_the_clean_pre_drift_baseline() -> None:
    """WP-W: a longer pre-drift keeps the drift boundary after the learner's burn-in (ticket 73 c)."""
    drift = m.plan_workload("drift", 50, 1000, drift_rate=150, drift_pre_segments=6)
    assert drift.rates == [150] * 10, "6 pre-drift + 4 post-drift segments"
    assert all(mix == m.WIN_MIX for mix in drift.mixes[:6]), "the first 6 segments are pre-drift win mix"
    assert drift.mixes[6]["scan"] == m.WIN_MIX["scan"] * 10
    # The default keeps the ticket-73 trace shape (2 pre-drift + 4 post-drift = 6 total).
    assert m.plan_workload("drift", 50, 1000, drift_rate=150).rates == [150] * 6


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
        _curve_window(50.0, 49.0),
        _curve_window(50.0, 50.0),
        _curve_window(100.0, 60.0),
        _curve_window(200.0, 199.0, errors=1),
    ]
    curve = m.summarize_curve(windows)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[50.0]["sustained"] is True
    assert by_rate[100.0]["sustained"] is False
    assert by_rate[200.0]["sustained"] is False, "a rejected window invalidates the level"
    assert m.highest_sustained_rate(curve) == 50.0


def test_ceiling_breach_invalidates_the_level() -> None:
    windows = [_curve_window(50.0, 50.0, p99=9000),
               _curve_window(100.0, 100.0, p99=100)]
    curve = m.summarize_curve(windows, ceiling_us=5000)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[50.0]["sustained"] is False
    assert by_rate[100.0]["sustained"] is True
    assert by_rate[50.0]["ceiling_rejected_windows"] == 1


# ---------------------------------------------------------------------------
# Policy echo, sweep, argmin
# ---------------------------------------------------------------------------
def test_policy_active_map_merges_default_and_region_entry() -> None:
    policy = {"default": {"page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2},
              "regions": {"P|Stock": {"page_size": 65536, "merge_min_tiles": 8}}}
    active = m.policy_active_map(policy)
    assert active["P|Stock"].page_size == 65536 and active["P|Stock"].merge_min_tiles == 8
    assert m.policy_active_map({}) == {}


def test_policy_active_map_and_argmin_preserve_the_order_no_index_axis() -> None:
    policy = {"default": {"page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2},
              "regions": {"P|StockMovement": {"page_size": 65536, "merge_min_tiles": 2,
                                              "index_order_no": True}}}
    active = m.policy_active_map(policy)
    assert active["P|StockMovement"].index_order_no is True
    # argmin over a sweep that only saw the indexed candidate keeps the flag.
    sweep = {"P|StockMovement": {
        str(active["P|StockMovement"]): {"n": 1, "sum_cost": 1.0, "page_size": 65536,
                                         "merge_min_tiles": 2, "index_order_no": True}}}
    picks = m.argmin_from_sweep(sweep)
    assert picks["P|StockMovement"].index_order_no is True


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


def test_threshold3_is_not_measurable_when_compaction_is_not_actuated() -> None:
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
    }
    t3 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,), compaction_actuated=False)
          if "reorganisation" in v["threshold"]][0]
    assert t3["status"] == "NOT-MEASURABLE"
    assert "not actuated" in t3["detail"]


def test_threshold3_zero_bytes_with_actuation_is_flagged_as_a_defect() -> None:
    # Slice 37: the driver DOES compact now, so 0 vs 0 is no longer an inert-knob non-result; it
    # means the actuation did not fire and must be reported as a defect, not repeated as the old
    # NOT-MEASURABLE.
    records = {
        "win/declared": _record([(50, 50, True)]),
        "win/learned@1.0": _record([(50, 50, True)]),
    }
    t3 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,), compaction_actuated=True)
          if "reorganisation" in v["threshold"]][0]
    assert t3["status"] == "NOT-MEASURABLE"
    assert "DEFECT" in t3["detail"]


def test_threshold3_is_measurable_and_passes_when_declared_compacts() -> None:
    # Compaction actuated: declared (eager merge) writes output, learned (deferred) writes 0.
    records = {
        "win/declared": _record([(50, 50, True)], reorg=2_000_000),
        "win/learned@1.0": _record([(50, 50, True)], reorg=0),
    }
    t3 = [v for v in m.evaluate_thresholds(records, lambdas=(1.0,), compaction_actuated=True)
          if "reorganisation" in v["threshold"]][0]
    assert t3["status"] == "PASS"
    assert t3["declared_bytes"] == 2_000_000 and t3["learned_bytes"] == 0


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
def test_window_records_and_curve_carry_driver_compact_bytes() -> None:
    lines = [_window_line(0, 50, 50, 50, compact=4096),
             _window_line(1, 50, 50, 100, compact=0)]
    recs = m.window_records(lines, window_ms=1000)
    assert [r["compact_bytes"] for r in recs] == [4096, 0]
    curve = m.summarize_curve(recs)
    assert curve[0]["compact_bytes"] == 4096


def test_flashdb_load_command_passes_compact_bytes_only_when_actuated() -> None:
    bins = {"load": "flashdb-load"}
    plan = m.plan_workload("win", 50, 100)
    cmd = m.flashdb_load_command(bins, "dir", "t.jsonl", plan, "p.json", reload=True,
                                 checkpoint_bytes=16384, rotate_bytes=16384, group_requests=64,
                                 window_ms=1000, ceiling_us=None, lag_ceiling_us=10_000,
                                 compact_bytes=64 << 20, max_tiles=4096)
    assert "--compact-bytes 67108864" in cmd
    assert "--lag-ceiling-us 10000" in cmd
    assert "--max-tiles 4096" in cmd
    off = m.flashdb_load_command(bins, "dir", "t.jsonl", plan, "p.json", reload=False,
                                 checkpoint_bytes=16384, rotate_bytes=16384, group_requests=64,
                                 window_ms=1000, ceiling_us=None, lag_ceiling_us=10_000,
                                 compact_bytes=0, max_tiles=4096)
    assert "--compact-bytes" not in off, "0 means the pre-slice inert path, so no flag"


def _cadence_args(checkpoint: int, rotate: int, max_tiles: int = 4096) -> argparse.Namespace:
    return argparse.Namespace(checkpoint_bytes=checkpoint, rotate_bytes=rotate, max_tiles=max_tiles)


def test_shipped_default_cadence_is_the_build_harness_cadence() -> None:
    assert m.SHIPPED_CADENCE_BYTES == 8 << 20, "the scored runner ships the build harness's 8 MiB cadence"
    assert m.MAX_TILES_DEFAULT == 4096, "the shipped cap is the raised engine ceiling"


def test_cadence_refusal_allows_the_shipped_default_and_refuses_16k(monkeypatch) -> None:
    monkeypatch.setattr(m, "_tileinfo_active_tiles", lambda bins, base, env: 336)
    # The shipped default (8 MiB cadence, 4096 cap) over a 336-region / 12000-write run: fits.
    assert m.cadence_refusal(_cadence_args(8 << 20, 8 << 20), {"scale": "x"}, Path("d"), {}, 12000) is None
    # The old shipped 16 KiB cadence would exhaust the cap and is refused up front.
    refusal = m.cadence_refusal(_cadence_args(16384, 16384), {"scale": "x"}, Path("d"), {}, 12000)
    assert refusal is not None and "incompatible per-region cadence" in refusal


def test_cadence_refusal_refuses_an_already_full_list(monkeypatch) -> None:
    monkeypatch.setattr(m, "_tileinfo_active_tiles", lambda bins, base, env: 5000)
    refusal = m.cadence_refusal(_cadence_args(8 << 20, 8 << 20, max_tiles=4096), {"scale": "x"}, Path("d"), {}, 0)
    assert refusal is not None and "no publish can fit" in refusal


def test_cadence_refusal_refuses_unequal_checkpoint_and_rotate(monkeypatch) -> None:
    monkeypatch.setattr(m, "_tileinfo_active_tiles", lambda bins, base, env: 336)
    refusal = m.cadence_refusal(_cadence_args(8 << 20, 16 << 20), {"scale": "x"}, Path("d"), {}, 12000)
    assert refusal is not None and "must match" in refusal


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

    A window the pacer marked invalid (its schedule lag ran away) must make the step unsustained.
    The flag is now on the `--metrics-json` line and parsed into the window records; before, the
    JSON line omitted it, so a JSONL reader defaulted it to false and scored the step sustained.
    """
    windows = [
        # a genuinely sustained rung: two valid windows at 100 rps
        _curve_window(100.0, 100.0, p50=100, p99=200, lag_p99=1000),
        _curve_window(100.0, 99.0, p50=100, p99=200, lag_p99=1000),
        # a saturated rung: the draining window "achieves" the offered rate but is invalid, while
        # the other window is still valid (the lag rule needs consecutive windows) and shows the
        # engine honestly far behind.
        _curve_window(400.0, 400.0, p50=100, p99=200, invalid=True, lag_p99=5_000_000),
        _curve_window(400.0, 120.0, p50=100, p99=200, lag_p99=2_000_000),
    ]
    curve = m.summarize_curve(windows)
    by_rate = {row["offered_rps"]: row for row in curve}
    assert by_rate[100.0]["sustained"] is True
    assert by_rate[400.0]["sustained"] is False, (
        "a rung whose windows the pacer marked invalid must never be scored sustained, even if one "
        "of them appears to achieve the offered rate by draining a previous step's backlog")
    assert by_rate[400.0]["invalid_windows"] == 1
    # The rung's achieved rate is its total completions over its total wall time (both windows are
    # real time): (400 + 120) / 2 s, never a >offered figure. The invalid flag is what fails it.
    assert by_rate[400.0]["achieved_rps"] == 260.0
    assert m.highest_sustained_rate(curve) == 100.0


def test_group_bursts_cannot_inflate_a_rung_above_its_offered_rate():
    """Regression for the 128% per-rung figures in .scratch/39-0b-measurable/adversarial.md.

    `--group-requests 64` issues a whole group when its FIRST request is due, so a 50 rps rung's
    windows carry a 64-request burst (64 rps) or nothing. The old estimator took the MEDIAN of the
    per-window delta over the nominal window and reported 64 rps = 128% of the offered 50. The fix
    attributes completions to the rung and divides by its elapsed wall time instead.
    """
    lines = []
    completed = 0
    # A 2000-request 50 rps rung: 31 bursts of 64 + one of 16, spread over 40 one-second windows.
    for i, n in enumerate([64] * 31 + [16] + [0] * 8):
        completed += n
        lines.append(json.dumps({
            "window": i, "rate_rps": 50.0, "requests": n, "completed": completed,
            "errors": 0, "p50_us": 100, "p99_us": 200, "window_us": 1_000_000,
            "invalid": False, "lag_p99_us": 0, "ceiling_breached": False,
            "policy": {"default": {"page_size": 4096, "merge_min_tiles": 2}, "regions": {}},
            "current": {"patterns": [], "regions": []},
        }, sort_keys=True))
    recs = m.window_records(lines, window_ms=1000)
    # The burst really is in the input: the OLD median-of-window-rates estimator returns 64 rps.
    assert statistics.median(r["achieved_rps"] for r in recs) == 64.0, (
        "the synthetic rung must reproduce the 128% burst the old estimator reported")

    row = {r["offered_rps"]: r for r in m.summarize_curve(recs)}[50.0]
    assert row["achieved_rps"] == 50.0, "completions over the rung's wall time is the offered rate"
    assert row["achieved_fraction"] <= 1.0 + m.RUNG_RATE_TOLERANCE
    assert row["rate_exceeded"] is False
    assert row["sustained"] is True


def test_a_rung_that_exceeds_its_offered_rate_fails_closed():
    # 60 completions attributed to a rung that offered only 30 requests is physically impossible
    # (a drain or a bookkeeping bug). The guard must refuse to quote it rather than score it.
    windows = [_curve_window(50.0, 60, requests=30)]
    row = {r["offered_rps"]: r for r in m.summarize_curve(windows)}[50.0]
    assert row["achieved_rps"] == 60.0
    assert row["rate_exceeded"] is True
    assert row["sustained"] is False


def test_json_window_invalid_flag_reaches_the_curve():
    # The driver must put `invalid` on the --metrics-json line; a JSONL reader that defaults it to
    # false would score a saturated rung as sustained (the pre-fix behaviour).
    lines = [
        json.dumps({"window": 0, "rate_rps": 400.0, "requests": 400, "completed": 400,
                    "errors": 0, "p50_us": 10, "p99_us": 20, "window_us": 1_000_000,
                    "invalid": False, "lag_p99_us": 0, "ceiling_breached": False,
                    "policy": {}, "current": {"patterns": [], "regions": []}}, sort_keys=True),
        json.dumps({"window": 1, "rate_rps": 400.0, "requests": 0, "completed": 400,
                    "errors": 0, "p50_us": 10, "p99_us": 20, "window_us": 1_000_000,
                    "invalid": True, "lag_p99_us": 5_000_000, "ceiling_breached": False,
                    "policy": {}, "current": {"patterns": [], "regions": []}}, sort_keys=True),
    ]
    row = {r["offered_rps"]: r for r in m.summarize_curve(m.window_records(lines, 1000))}[400.0]
    assert row["invalid_windows"] == 1
    assert row["sustained"] is False


def test_a_scored_run_requires_a_latency_ceiling():
    # The pre-registration validity rule needs a ceiling; the scored runner must fail closed when
    # one is not supplied rather than silently defaulting to something permissive.
    with pytest.raises(SystemExit):
        m.main([])


# ---------------------------------------------------------------------------
# Slice 39: drift-rate auto, the reward terms, and the churn budget
# ---------------------------------------------------------------------------
def test_resolve_drift_rate_auto_is_a_fraction_of_the_pre_drift_capacity() -> None:
    rate, info = m.resolve_drift_rate("auto", pre_drift_highest_sustained_rps=100.0,
                                      reference_rate=50, fraction=0.70)
    assert rate == 70 and info["mode"] == "auto_from_pre_drift_capacity"
    assert info["pre_drift_highest_sustained_rps"] == 100.0
    # an explicit integer passes through unchanged
    rate, info = m.resolve_drift_rate("150", pre_drift_highest_sustained_rps=100.0,
                                      reference_rate=50)
    assert rate == 150 and info["mode"] == "explicit"
    # no pre-flight result falls back to the reference rate rather than inventing a capacity
    rate, info = m.resolve_drift_rate("auto", pre_drift_highest_sustained_rps=0.0,
                                      reference_rate=50)
    assert rate == 50 and info["mode"] == "auto_fallback_reference_rate"


def test_preflight_plan_is_a_win_mix_ladder() -> None:
    plan = m.preflight_plan(50, 1000)
    assert plan.rates == [50, 100, 200, 400]
    assert plan.segment_requests == 1000
    assert all(mix == m.WIN_MIX for mix in plan.mixes)


def test_declared_reorg_rate_is_bytes_per_window() -> None:
    rec = {
        "curve": [{"offered_rps": 70, "windows": 153}],
        "regions": {"P|Stock": {"compaction_bytes": 1_530_000, "windows": 153}},
    }
    assert m.run_window_count(rec) == 153
    assert m.total_reorg_bytes(rec) == 1_530_000
    assert m.declared_reorg_rate_bytes_per_window(rec) == 10_000.0
    # a record with no curve rows falls back to the region window count, never a division by zero
    empty = {"curve": [], "regions": {"P|Stock": {"compaction_bytes": 500, "windows": 5}}}
    assert m.declared_reorg_rate_bytes_per_window(empty) == 100.0
    assert m.run_window_count({"curve": [], "regions": {}}) == 1


def test_reward_term_totals_split_ingest_from_reorganisation() -> None:
    lambda_ns_per_byte = 1.0
    rec = {"regions": {
        "P|Stock": {"service_ns": 10_000.0, "bytes_written": 100, "compaction_bytes": 900},
        "U|Stock": {"service_ns": 10_000.0, "bytes_written": 0, "compaction_bytes": 0},
    }}
    terms = m.reward_term_totals(rec, lambda_ns_per_byte)
    assert terms["service_ns"] == 20_000.0
    assert terms["ingest_bytes"] == 100 and terms["reorg_bytes"] == 900
    assert terms["ingest_write_term_ns"] == 100 and terms["reorg_write_term_ns"] == 900
    assert terms["ingest_to_service_ratio"] == 100 / 20_000.0
    assert terms["reorg_to_service_ratio"] == 900 / 20_000.0


def test_region_compaction_totals_accumulate_service_time() -> None:
    lines = [_window_line(0, 50, 50, 50, regions=[
        {"region": "P|Stock", "count": 10, "mean_ns": 100.0, "p50_ns": 90.0, "p99_ns": 200.0,
         "bytes_read": 10, "bytes_written": 0, "compaction_bytes": 30, "compaction_merges": 1}])]
    totals = m.region_compaction_totals(m.window_records(lines, window_ms=1000))
    assert totals["P|Stock"]["service_ns"] == 1000.0
    assert totals["P|Stock"]["count"] == 10


def test_thresholds_two_and_four_are_emitted_without_the_win_workload() -> None:
    """Slice 39 regression: the required adversarial+drift run has no win arms, so T2/T4 must
    still be computed (the old loop skipped every threshold when win was absent)."""
    records = {
        "adversarial/declared": _record([(50, 100, True)], workload="adversarial"),
        "adversarial/learned@1.0": _record([(50, 100, True)], workload="adversarial"),
        "drift/declared": _record([(70, 70, True)], reorg=1000, workload="drift"),
        "drift/learned@1.0": _record([(70, 70, True)], reorg=2000, recovery={"P|Stock": 8},
                                     workload="drift"),
        "drift/static-derived": _record([(70, 70, True)], reorg=9_000, workload="drift"),
    }
    verdicts = m.evaluate_thresholds(records, lambdas=(1.0,))
    kinds = {v["threshold"].split()[0]: v for v in verdicts}
    assert "T1" not in kinds, "no win arms means no T1 verdict"
    assert kinds["T2"]["status"] == "PASS"
    assert kinds["T4"]["status"] == "NOT MEETABLE WITH CURRENT PARAMETERS"
    assert "drift reorg: declared 1000 B, learned 2000 B (ratio 2.00)" in kinds["T4"]["detail"]
    assert "static-derived 9000 B" in kinds["T4"]["detail"]


# ---------------------------------------------------------------------------
# Latency model (t1/t2/t3): per-request latency, schedule lag and named ceilings
# ---------------------------------------------------------------------------
def _latency_window(rate: float, completed_delta: int, *, requests: int,
                    group_elapsed_us: int, service_p50: int, service_p95: int, service_p99: int,
                    lag_p50: int = 0, lag_p95: int = 0, lag_p99: int = 0,
                    invalid: bool = False, reject_reason: str | None = None,
                    service_breached: bool = False, lag_breached: bool = False,
                    early: int = 0, window_s: float = 1.0,
                    min_lag: int = 0, groups: int = 1, max_group_size: int | None = None,
                    groups_over_service_ceiling: int = 0) -> str:
    """A ``--metrics-json`` window from the fixed driver (per-request latencies + named ceilings).

    ``p50_us``/``p99_us`` are deliberately set to the GROUP elapsed to reproduce the pre-fix
    driver, while the explicit ``service_pNN_us`` fields carry the per-request values: a consumer
    that reads the retired group-inflated ``p50_us`` reports the wrong number. `min_lag`, `groups`,
    `max_group_size` and `groups_over_service_ceiling` are the slice-48 additions; the emitted
    ``mean_group_size`` is derived as ``requests / groups`` so it is always consistent.
    """
    return json.dumps({
        "window": 0, "rate_rps": rate, "requests": requests, "completed": completed_delta,
        "errors": 0, "compact_bytes": 0,
        "service_p50_us": service_p50, "service_p95_us": service_p95, "service_p99_us": service_p99,
        "p50_us": group_elapsed_us, "p95_us": group_elapsed_us, "p99_us": group_elapsed_us,
        "lag_p50_us": lag_p50, "lag_p95_us": lag_p95, "lag_p99_us": lag_p99,
        "min_lag_us": min_lag,
        "group_elapsed_us": group_elapsed_us, "early_dispatches": early,
        "groups": groups, "mean_group_size": (requests / groups) if groups else 0.0,
        "max_group_size": max_group_size if max_group_size is not None else requests,
        "groups_over_service_ceiling": groups_over_service_ceiling,
        "window_us": int(window_s * 1e6), "invalid": invalid, "reject_reason": reject_reason,
        "service_ceiling_us": 10_000, "service_ceiling_breached": service_breached,
        "lag_ceiling_us": 10_000, "lag_ceiling_breached": lag_breached,
        "policy": {}, "current": {"patterns": [], "regions": []},
    }, sort_keys=True)


def test_t1_per_request_latency_is_not_the_group_elapsed_time() -> None:
    """--group-requests 64: the group shares one barrier, so its own elapsed is ~108 ms while each
    of its 64 members is served in ~1.6 ms. The old driver wrote the group elapsed into ``p50_us``
    (inflating the per-request bound by ~N); the fixed one reports the PER-REQUEST service p50 and
    keeps the group elapsed only as the separate ``group_elapsed_us`` diagnostic."""
    line = _latency_window(50.0, 64, requests=64, group_elapsed_us=108_282,
                           service_p50=1_600, service_p95=2_100, service_p99=2_500)
    rec = m.window_records([line], window_ms=1000)[0]
    assert rec["group_elapsed_us"] == 108_282, "the group elapsed is a separate diagnostic"
    assert rec["service_p50_us"] == 1_600
    row = {r["offered_rps"]: r for r in m.summarize_curve(m.window_records([line], 1000))}[50.0]
    assert row["p50_us"] == 1_600, "the rung p50 is the PER-REQUEST value, not 108,282"
    assert row["p50_us"] < row["group_elapsed_us"], "the per-request p50 is strictly smaller"
    assert row["p95_us"] == 2_100 and row["p99_us"] == 2_500
    assert row["p99_us"] <= row["group_elapsed_us"], "per-request p99 is bounded by the group value"


def test_t2_the_driver_never_consumes_a_request_before_its_due_time() -> None:
    """The scheduler forms a group only from requests ALREADY READY, so no request is issued with
    ``issue_us < due_us`` and ``early_dispatches`` is 0. The field must reach the window record and
    the curve; before the fix the driver consumed future-scheduled requests and neither field
    existed."""
    line = _latency_window(50.0, 1, requests=1, group_elapsed_us=1_600,
                           service_p50=1_600, service_p95=1_600, service_p99=1_600, early=0)
    rec = m.window_records([line], window_ms=1000)[0]
    assert rec["early_dispatches"] == 0
    assert m.summarize_curve([rec])[0]["early_dispatches"] == 0
    # A regression is visible rather than silently dropped.
    bad = m.window_records([_latency_window(50.0, 1, requests=1, group_elapsed_us=1_600,
                                            service_p50=1_600, service_p95=1_600, service_p99=1_600,
                                            early=3)], window_ms=1000)
    assert m.summarize_curve(bad)[0]["early_dispatches"] == 3
    # ... and the run summary carries the total.
    summary = m.parse_load_summary(
        "load_summary requests=8000 completed=8000 errors=0 early_dispatches=0 "
        "service_ceiling_rejected_windows=0 lag_ceiling_rejected_windows=0 "
        "elapsed_ms=1000 achieved_rps=8000")
    assert summary["early_dispatches"] == 0


def test_t3_service_and_lag_ceilings_report_different_reasons() -> None:
    """One window over the LAG ceiling with a fine service p99, one over the SERVICE ceiling with a
    fine lag: they must be reported with different reasons and counted in different fields, so the
    report can say WHICH bound rejected a window."""
    lag_line = _latency_window(50.0, 50, requests=50, group_elapsed_us=1_200,
                               service_p50=200, service_p95=300, service_p99=400,
                               lag_p50=5_000, lag_p95=9_000, lag_p99=40_000,
                               invalid=True, reject_reason="lag_ceiling", lag_breached=True)
    service_line = _latency_window(50.0, 50, requests=50, group_elapsed_us=200_000,
                                   service_p50=150_000, service_p95=190_000, service_p99=200_000,
                                   lag_p50=100, lag_p95=200, lag_p99=300,
                                   invalid=True, reject_reason="service_ceiling",
                                   service_breached=True)
    recs = m.window_records([lag_line, service_line], window_ms=1000)
    assert {r["reject_reason"] for r in recs} == {"lag_ceiling", "service_ceiling"}, \
        "the two ceilings must be named differently"
    row = m.summarize_curve(recs, ceiling_us=10_000)[0]
    assert row["lag_ceiling_rejected_windows"] == 1
    assert row["service_ceiling_rejected_windows"] == 1
    assert row["ceiling_rejected_windows"] == 2
    assert row["reject_reasons"] == {"lag_ceiling": 1, "service_ceiling": 1}
    assert row["sustained"] is False


# ---------------------------------------------------------------------------
# Slice 48: per-window group statistics, absolute minimum lag and group-over-ceiling count
# ---------------------------------------------------------------------------
def test_group_statistics_are_consistent_and_bounded() -> None:
    """The window's ``requests`` is the sum of its group sizes, so ``requests / groups`` is the mean
    group size. The driver now emits ``groups``, ``mean_group_size`` and ``max_group_size``; before
    the fix none existed, so group formation could not be checked at all. Consistency: the per-window
    requests sum to the run total, and each window's mean size lies between 1 and the group cap."""
    cap = 64
    lines = [
        _latency_window(50.0, 64, requests=64, group_elapsed_us=108_282,
                        service_p50=1_600, service_p95=2_100, service_p99=2_500,
                        groups=1, max_group_size=64),
        _latency_window(50.0, 64, requests=64, group_elapsed_us=20_000,
                        service_p50=1_500, service_p95=2_000, service_p99=2_400,
                        groups=2, max_group_size=40),
        "load_summary requests=128 completed=128 errors=0 elapsed_ms=1000 achieved_rps=128",
    ]
    recs = m.window_records(lines, window_ms=1000)
    summary = m.parse_load_summary(lines[-1])
    assert sum(r["requests"] for r in recs) == summary["requests"] == 128, \
        "per-window requests must sum to the run total"
    for r in recs:
        assert r["groups"] >= 1
        assert abs(r["mean_group_size"] - r["requests"] / r["groups"]) < 1e-9, \
            "mean group size must equal requests / groups"
        assert 1.0 <= r["mean_group_size"] <= cap
        assert 1 <= r["max_group_size"] <= cap
    row = {r["offered_rps"]: r for r in m.summarize_curve(recs)}[50.0]
    assert row["groups"] == 3, "the rung groups is the sum over its windows"
    assert row["max_group_size"] == 64
    assert abs(row["mean_group_size"] - 128 / 3) < 1e-9, "rung mean = total requests / total groups"


def test_min_lag_is_present_and_non_negative() -> None:
    """The percentile lags are clamped at 0, so only ``min_lag_us`` can show the window's actual
    floor. It must be present and non-negative (a negative value would mean an early dispatch, which
    the scheduler never does); before the fix the field did not exist."""
    line = _latency_window(50.0, 1, requests=1, group_elapsed_us=1_600,
                           service_p50=1_600, service_p95=1_600, service_p99=1_600,
                           lag_p50=10, lag_p95=20, lag_p99=30, min_lag=7)
    rec = m.window_records([line], window_ms=1000)[0]
    assert rec["min_lag_us"] == 7
    assert rec["min_lag_us"] >= 0
    row = m.summarize_curve([rec])[0]
    assert row["min_lag_us"] == 7, "the rung min lag is the min over its windows"


def test_a_group_over_the_service_ceiling_is_counted() -> None:
    """When the largest group elapsed exceeds the service ceiling, the window must report at least
    one group over the ceiling, so a reader can tell one group's serial work from many independent
    stalls. Before the fix the count did not exist and read as 0."""
    line = _latency_window(50.0, 64, requests=64, group_elapsed_us=500_000,
                           service_p50=10_000, service_p95=400_000, service_p99=480_000,
                           service_breached=True, groups=8, groups_over_service_ceiling=2)
    rec = m.window_records([line], window_ms=1000)[0]
    assert rec["group_elapsed_us"] > rec["service_ceiling_us"], "the largest group exceeded the ceiling"
    assert rec["groups_over_service_ceiling"] >= 1, "the exceeding group must be counted"
    row = m.summarize_curve([rec], ceiling_us=10_000)[0]
    assert row["groups_over_service_ceiling"] == 2, "the rung sums the per-window group counts"
