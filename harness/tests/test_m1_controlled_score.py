"""Tests for ticket 82's controlled-run scorer (`scripts/m1-controlled-score.py`).

The scorer is the one place where a committed arm-run becomes a T5/T6/T7 verdict, so its arithmetic
is bound here on synthetic records whose answers are known: the median/IQR of a spread, T5's deficit
against the SLOWER baseline, prong B's sustained requirement, and T6's highest-common-level search.
"""

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
SCORER = REPO / "scripts" / "m1-controlled-score.py"


def load_scorer():
    spec = importlib.util.spec_from_file_location("m1_controlled_score", SCORER)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


scorer = load_scorer()


def test_percentile_and_spread_are_the_median_and_interquartile_range():
    assert scorer.percentile([1, 2, 3, 4, 5], 0.5) == 3.0
    assert scorer.percentile([1, 2, 3, 4, 5], 0.25) == 2.0
    assert scorer.percentile([1, 2, 3, 4, 5], 0.75) == 4.0
    # Linear interpolation between ranks, the convention a spreadsheet's QUARTILE.INC uses.
    assert scorer.percentile([1, 2, 3, 4], 0.25) == 1.75
    assert scorer.percentile([1, 2, 3, 4], 0.5) == 2.5
    spread = scorer.spread([5, 1, 3])
    assert (spread["median"], spread["q1"], spread["q3"], spread["iqr"]) == (3.0, 2.0, 4.0, 2.0)
    assert scorer.spread([])["median"] is None


def _run_record(arm, *, achieved, lag_p99, rejected=0, reasons=None, sustained=None,
                data_bytes=1 << 30, errors=0):
    curve = {
        "offered_rps": 50, "achieved_rps": achieved, "achieved_fraction": achieved / 50,
        "windows": 20, "invalid_windows": rejected, "reject_reasons": reasons or {},
        # `cold_0b.summarize_curve`'s own rule, so a synthetic record cannot claim a sustained rung
        # the real reducer would not have.
        "sustained": (achieved >= 0.95 * 50 and rejected == 0 and errors == 0)
                     if sustained is None else sustained,
        "lag_p50_us": 100, "lag_p99_us": lag_p99, "p50_us": 200, "p99_us": lag_p99 + 500,
    }
    return {
        "arm": arm, "curve": [curve], "counters": {"errors": errors}, "mean_group_size": 1.0,
        "elapsed_s": 20.0, "engine_memory": {"peak_bytes": 100 << 20}, "engine_cap_mib": 128,
        "data_bytes": data_bytes, "trace_sha256": "a" * 64,
        "restore": {"verified": True, "sha256": f"{arm}-sha"}, "oracle": {"status": "PASS"},
    }


def _write_root(root: Path, blocks: dict):
    """`blocks` maps a block number to `{(rung, arm): run_record}`."""
    (root / "run-spec.json").write_text(json.dumps({"blocks": len(blocks)}), encoding="utf-8")
    for block, runs in blocks.items():
        for (rung, arm), record in runs.items():
            workdir = root / f"b{block}" / f"rung-{rung}"
            workdir.mkdir(parents=True, exist_ok=True)
            (workdir / f"run-{arm}.json").write_text(json.dumps(record), encoding="utf-8")
            (workdir / "machine-state.jsonl").write_text(json.dumps({
                "phase": "before", "arm": arm, "loadavg": "1.00 1.00 1.00", "ac_online": ["1"],
                "power_profile": "performance", "governor": "powersave", "epp": ["performance"],
                "scaling_cur_freq_khz": ["3600000"], "nproc": 8, "top_cpu": [],
                "concurrent_work": []}) + "\n", encoding="utf-8")
    floor_dir = root / "floor" / "before"
    floor_dir.mkdir(parents=True, exist_ok=True)
    benches = {}
    for name in ("idle", "fsync"):
        benches[name] = {
            "counters": {"errors": 0},
            "curve": [{"offered_rps": 50, "achieved_rps": 50.0, "windows": 20,
                       "invalid_windows": 0, "reject_reasons": {}, "sustained": True,
                       "lag_p50_us": 50, "lag_p99_us": 100, "p50_us": 200, "p99_us": 1000}],
            "windows_file": f"windows-floor-{name}.jsonl",
        }
    (floor_dir / "floor.json").write_text(json.dumps({
        "rate_rps": 50, "requests": 1000, "window_ms": 1000, "at": "2026-10-17T00:00:00",
        "machine": {}, "benches": benches}), encoding="utf-8")
    (floor_dir / "machine-state.jsonl").write_text("", encoding="utf-8")


def _score(root: Path) -> dict:
    out = root / "report.json"
    done = subprocess.run([sys.executable, str(SCORER), "--root", str(root), "--out", str(out)],
                          capture_output=True, text=True)
    assert done.returncode == 0, done.stderr
    return json.loads(out.read_text(encoding="utf-8"))


def test_t5_reads_the_slower_baseline_and_prong_b_needs_a_sustained_rung(tmp_path):
    blocks = {
        1: {(50, "flashdb-declared"): _run_record("flashdb-declared", achieved=40.0, lag_p99=900),
            (50, "sqlite"): _run_record("sqlite", achieved=50.0, lag_p99=800),
            (50, "postgres"): _run_record("postgres", achieved=50.0, lag_p99=700)},
        2: {(50, "flashdb-declared"): _run_record("flashdb-declared", achieved=40.0, lag_p99=900),
            (50, "sqlite"): _run_record("sqlite", achieved=50.0, lag_p99=800),
            (50, "postgres"): _run_record("postgres", achieved=50.0, lag_p99=700)},
    }
    _write_root(tmp_path, blocks)
    report = _score(tmp_path)
    t5 = report["verdicts"]["t5"]
    # The slower baseline achieved 50 and flashdb 40, so the deficit is 50/40 = 1.25 <= 1.5.
    assert t5["arithmetic"]["deficit_median"] == 1.25
    assert t5["prong_a"] == "PASS"
    # flashdb's own achieved rate is 40 of 50 offered, i.e. outside the 5% sustained tolerance, so
    # its run records were not sustained and prong B has no sustained level to compare.
    assert t5["arithmetic"]["sustained_blocks"]["flashdb-declared"] == 0
    assert t5["prong_b"] == "FAIL"
    assert t5["verdict"] == "FAIL"
    assert report["verdicts"]["t7"]["verdict"] == "FAIL"


def test_t6_reads_the_highest_common_offered_level_and_its_own_ceiling(tmp_path):
    blocks = {
        1: {(50, "flashdb-declared"): _run_record("flashdb-declared", achieved=50.0, lag_p99=500),
            (50, "sqlite"): _run_record("sqlite", achieved=50.0, lag_p99=400),
            (50, "postgres"): _run_record("postgres", achieved=50.0, lag_p99=400)},
        2: {(50, "flashdb-declared"): _run_record("flashdb-declared", achieved=50.0, lag_p99=500),
            (50, "sqlite"): _run_record("sqlite", achieved=50.0, lag_p99=400,
                                        rejected=1, reasons={"lag_ceiling": 1}),
            (50, "postgres"): _run_record("postgres", achieved=50.0, lag_p99=400)},
    }
    _write_root(tmp_path, blocks)
    report = _score(tmp_path)
    t6 = report["verdicts"]["t6"]
    # "Common" is the pre-registration's: every arm was OFFERED the level, so 50 is common even
    # though sqlite sustained it in only 1 of 2 blocks. flashdb's sustained 50 against the best
    # baseline's sustained 50 is 1.0x, far below the pre-registered 3x -- and the arithmetic states
    # that no pair of arms at a common level can exceed 1/SUSTAINED_TOLERANCE.
    assert t6["arithmetic"]["highest_common_offered_rung"] == 50
    assert t6["arithmetic"]["sustained_throughput_rps"] == {
        "flashdb-declared": 50.0, "sqlite": 50.0, "postgres": 50.0}
    assert t6["arithmetic"]["ratio"] == 1.0
    assert t6["arithmetic"]["ratio_upper_bound_for_any_pair_at_a_common_level"] == 1.0526
    assert t6["verdict"] == "FAIL"
    assert "cannot exceed" in t6["defect"]

    # A baseline that sustained no level at all gives the pre-registered ratio no denominator: the
    # verdict is still FAIL, the denominator is null (never an invented number), and the secondary
    # level reading is the one that still orders the arms.
    for block in (1, 2):
        blocks[block][(50, "sqlite")] = _run_record("sqlite", achieved=30.0, lag_p99=400,
                                                    rejected=20, reasons={"lag_ceiling": 20})
    _write_root(tmp_path, blocks)
    report = _score(tmp_path)
    t6 = report["verdicts"]["t6"]
    assert t6["arithmetic"]["highest_common_offered_rung"] == 50
    assert t6["arithmetic"]["sustained_throughput_rps"]["sqlite"] is None
    # postgres still sustained it, so the denominator exists and is postgres's own rate.
    assert t6["arithmetic"]["best_baseline"] == {"arm": "postgres", "achieved_rps": 50.0}
    assert t6["arithmetic"]["highest_sustained_rung_per_arm"]["sqlite"] is None
    assert t6["verdict"] == "FAIL"

    # With NO baseline sustaining any level, the secondary level reading becomes a lower bound: the
    # baseline sits below the lowest rung offered.
    for block in (1, 2):
        blocks[block][(50, "postgres")] = _run_record("postgres", achieved=30.0, lag_p99=400,
                                                      rejected=20, reasons={"lag_ceiling": 20})
    _write_root(tmp_path, blocks)
    report = _score(tmp_path)
    t6 = report["verdicts"]["t6"]
    assert t6["arithmetic"]["best_baseline"] is None
    assert t6["arithmetic"]["ratio"] is None
    assert "no denominator" in t6["defect"]
    assert t6["secondary_reading"]["ratio"] == 1.0
    assert t6["secondary_reading"]["is_lower_bound"] is True
    assert t6["verdict"] == "FAIL"


def test_metric_rows_carry_a_verdict_and_the_rejected_window_reasons(tmp_path):
    blocks = {
        1: {(50, "flashdb-declared"): _run_record("flashdb-declared", achieved=49.0, lag_p99=12_000,
                                                  rejected=3, reasons={"lag_ceiling": 2,
                                                                       "lag_saturation": 1}),
            (50, "sqlite"): _run_record("sqlite", achieved=50.0, lag_p99=800),
            (50, "postgres"): _run_record("postgres", achieved=50.0, lag_p99=700)},
    }
    _write_root(tmp_path, blocks)
    report = _score(tmp_path)
    rows = {(row["arm"], row["metric"]): row for row in report["metrics"]}
    assert rows[("flashdb-declared", "lag_p99_us")]["verdict"] == "FAIL"
    assert rows[("flashdb-declared", "rejected_windows")]["verdict"] == "FAIL"
    assert "lag_saturation" in rows[("flashdb-declared", "rejected_windows")]["detail"]
    # The fix belongs to the metric that failed: the unified rule scores SCHEDULE LAG, so a
    # service-p99 failure asks for instrumentation and a rejected-window failure names the rule.
    assert "schedule lag" in rows[("flashdb-declared", "rejected_windows")]["fix"]
    assert rows[("flashdb-declared", "service_p99_us")]["verdict"] == "FAIL"
    assert "group_elapsed_us" in rows[("flashdb-declared", "service_p99_us")]["fix"]
    assert rows[("sqlite", "lag_p99_us")]["verdict"] == "PASS"
    assert rows[("sqlite", "engine_peak_bytes")]["verdict"] == "PASS"
    # The floor-adjusted reading is reported beside the absolute rule, never used by it.
    adjusted = report["aggregate"][0].get("floor_adjusted") or {}
    assert adjusted.get("lag_p99_over_floor") is not None
    assert adjusted.get("device_floor_lag_p99_us") == 100
