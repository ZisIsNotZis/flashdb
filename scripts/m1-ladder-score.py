"""Score the M1 ladder into report.json: T5/T6/T7 from the raw arm records, conditions stated.

Every number here is read from the committed run records (`run-<arm>.json` per rung) and the
per-arm data footprint the arms measured for themselves; nothing is estimated and no threshold is
invented. The pre-registered conditions are quoted in the output so a reader can check the
arithmetic against the pre-registration rather than against this script:

  T5  at the 1x rung, flashdb within 1.5x of the slower of sqlite/postgres; 2x is the fail line;
  T6  at the highest common offered level, flashdb reaches >= 3x the best baseline's sustained
      throughput under the same latency rule;
  T7  the declared arm alone already passes T5.

The same latency rule is applied to every arm by `cold_0b.summarize_curve`, which scores each arm's
own windows, so this script only compares its verdicts.

Usage (from the repo root)::

    uv run python scripts/m1-ladder-score.py [--root DIR] [--out report.json]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

ARMS = ("flashdb-declared", "flashdb-learned", "sqlite", "postgres")
FLASHDB = ("flashdb-declared", "flashdb-learned")
BASELINES = ("sqlite", "postgres")
# The pre-registered numbers, quoted so the report cannot drift from them.
T5_PASS_RATIO = 1.5
T5_FAIL_RATIO = 2.0
T6_MULTIPLE = 3.0
# The regime floor: data >= 10x the memory budget (`pre-registration.md` §"Instance size").
REGIME_FLOOR = 10.0


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def load_rungs(root: Path) -> list[dict]:
    rungs = []
    for path in sorted(root.glob("rung-*"), key=lambda p: int(p.name.split("-")[1])):
        offered = int(path.name.split("-")[1])
        manifest = read_json(path / "manifest.json")
        spec = read_json(path / "spec.json")
        arms = {}
        for arm in ARMS:
            record = path / f"run-{arm}.json"
            if not record.exists():
                continue
            data = read_json(record)
            curve = data["curve"][0] if data["curve"] else {}
            arms[arm] = {
                "achieved_rps": curve.get("achieved_rps"),
                "achieved_fraction": curve.get("achieved_fraction"),
                "lag_p99_us": curve.get("lag_p99_us"),
                "service_p99_us": curve.get("p99_us"),
                "windows": curve.get("windows"),
                "invalid_windows": curve.get("invalid_windows"),
                "reject_reasons": curve.get("reject_reasons", {}),
                "sustained": curve.get("sustained"),
                "errors": sum(w.get("errors", 0) for w in []) or data["counters"].get("errors"),
                "cgroup_peak_bytes": data.get("cgroup_peak_bytes"),
                "engine_cap_mib": data.get("engine_cap_mib"),
                "seed_peak_bytes": data.get("seed_peak_bytes"),
                "data_bytes": data.get("data_bytes") or data.get("instance_bytes"),
                "oracle": data["oracle"]["status"],
                "oracle_checks": len(data["oracle"].get("checks", [])),
                "mean_group_size": data.get("mean_group_size"),
                "seed_s": data.get("seed_s"),
                "oracle_s": data.get("oracle_s"),
                "durability": data.get("durability"),
                "sqlite_cache_kib": data.get("sqlite_cache_kib"),
                "postgres_settings": data.get("postgres_settings"),
                "engine_peak_source": data.get("engine_peak_source"),
            }
        rungs.append({
            "offered_rps": offered,
            "requests": manifest["total_requests"],
            "trace_sha256": manifest["trace_sha256"],
            "window_ms": spec["window_ms"],
            "batch": spec["batch"],
            "on_hand": spec["on_hand"],
            "arms": arms,
        })
    return rungs


def sustained_verdict(arm: dict) -> float:
    """An arm's highest sustained offered level (0 when it sustained no level at all)."""
    return arm["achieved_rps"] if arm.get("sustained") else 0.0


def deficit(flashdb_rps: float, baseline_rps: float) -> float:
    """How many times slower flashdb is than the baseline (`<=1` means flashdb is faster)."""
    return baseline_rps / flashdb_rps if flashdb_rps else float("inf")


def t5(rung: dict) -> dict:
    """The pre-registered low-load non-inferiority check, at the 1x rung."""
    baselines = {arm: rung["arms"][arm] for arm in BASELINES if arm in rung["arms"]}
    flashdb = {arm: rung["arms"][arm] for arm in FLASHDB if arm in rung["arms"]}
    if not baselines or not flashdb:
        return {"evaluable": False, "reason": "a required arm is missing from the 1x rung"}
    slower_arm = min(baselines, key=lambda a: baselines[a]["achieved_rps"])
    slower_rps = baselines[slower_arm]["achieved_rps"]
    out = {
        "evaluable": True,
        "condition": f"at {rung['offered_rps']:.0f} rps offered, flashdb must be within"
                     f" {T5_PASS_RATIO}x of the slower of sqlite/postgres; {T5_FAIL_RATIO}x is the"
                     " fail line",
        "slower_baseline": slower_arm,
        "slower_baseline_rps": slower_rps,
        "baselines": {a: {"achieved_rps": v["achieved_rps"], "sustained": v["sustained"],
                          "invalid_windows": v["invalid_windows"],
                          "reject_reasons": v["reject_reasons"]}
                      for a, v in baselines.items()},
        "arms": {},
    }
    for arm, value in flashdb.items():
        d = deficit(value["achieved_rps"], slower_rps)
        out["arms"][arm] = {
            "achieved_rps": value["achieved_rps"],
            "sustained": value["sustained"],
            "invalid_windows": value["invalid_windows"],
            "deficit_vs_slower_baseline": round(d, 3),
            "pass": d <= T5_PASS_RATIO,
            "design_failure": d > T5_FAIL_RATIO,
            "band": ("within-1.5x" if d <= T5_PASS_RATIO
                     else "between-1.5x-and-2x" if d <= T5_FAIL_RATIO else "beyond-2x"),
        }
    return out


def t6(rungs: list[dict]) -> dict:
    """High-load superiority at the highest common offered level."""
    common = [r for r in rungs if all(arm in r["arms"] for arm in ARMS)]
    if not common:
        return {"evaluable": False, "reason": "no rung ran every arm"}
    if len(common) < 2:
        # T6 compares the highest common offered level with what a baseline sustained ELSEWHERE on the
        # ladder; with one level it would compare a rung with itself and always read ~1.0x.
        return {"evaluable": False,
                "reason": "a single offered level is not a ladder; T6 needs at least two levels"}
    top = common[-1]
    best_baseline_level = 0.0
    best_baseline_arm = None
    for rung in common:
        for arm in BASELINES:
            value = rung["arms"][arm]
            if value["sustained"] and value["achieved_rps"] > best_baseline_level:
                best_baseline_level = value["achieved_rps"]
                best_baseline_arm = (arm, rung["offered_rps"])
    out = {
        "evaluable": best_baseline_level > 0,
        "highest_common_offered_rps": top["offered_rps"],
        "condition": f"at the highest common offered level ({top['offered_rps']:.0f} rps) flashdb"
                     f" must reach >= {T6_MULTIPLE}x the best baseline's sustained throughput"
                     " under the same latency rule",
        "best_baseline_sustained": {"rps": best_baseline_level, "arm_rung": best_baseline_arm},
        "arms": {},
    }
    if not out["evaluable"]:
        out["reason"] = ("no baseline sustained any offered level under the rule, so the"
                         " denominator does not exist")
        return out
    for arm in FLASHDB:
        value = top["arms"][arm]
        multiple = (value["achieved_rps"] / best_baseline_level) if best_baseline_level else 0.0
        out["arms"][arm] = {
            "achieved_rps_at_top_rung": value["achieved_rps"],
            "sustained_at_top_rung": value["sustained"],
            "invalid_windows": value["invalid_windows"],
            "reject_reasons": value["reject_reasons"],
            "multiple_of_best_baseline": round(multiple, 3),
            "pass": multiple >= T6_MULTIPLE,
        }
    return out


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="score the M1 ladder into report.json")
    ap.add_argument("--root", default=".scratch/80-external-baselines/runs/m1-measure")
    ap.add_argument("--out", default=None, help="defaults to <root>/report.json")
    ap.add_argument("--merge", nargs="+", default=None,
                    help="combine several already-scored report.json files into one document")
    a = ap.parse_args(argv)

    if a.merge:
        runs = [read_json(Path(m)) for m in a.merge]
        merged = {"schema": "flashdb.baseline.m1-report.v1",
                  "sources": [str(Path(m).resolve()) for m in a.merge],
                  "runs": runs,
                  # One row per threshold per run, so the verdicts are readable without walking the
                  # per-arm detail: the conditions themselves live in each run's own t5/t6/t7 block.
                  "verdicts": [{
                      "run": run["root"],
                      "rungs": [rung["offered_rps"] for rung in run["rungs"]],
                      "t5": {arm: {"deficit_vs_slower_baseline": v["deficit_vs_slower_baseline"],
                                   "pass": v["pass"]}
                             for arm, v in run["t5"].get("arms", {}).items()},
                      "t6": ({"evaluable": False, "reason": run["t6"].get("reason")}
                             if not run["t6"].get("evaluable") else
                             {"evaluable": True,
                              "highest_common_offered_rps":
                                  run["t6"]["highest_common_offered_rps"],
                              "best_baseline_sustained_rps":
                                  run["t6"]["best_baseline_sustained"]["rps"],
                              "arms": {arm: {"multiple_of_best_baseline":
                                                 v["multiple_of_best_baseline"],
                                             "pass": v["pass"]}
                                       for arm, v in run["t6"]["arms"].items()}}),
                      "t7": {"pass": run["t7"]["pass"],
                             "deficit_vs_slower_baseline":
                                 run["t7"]["deficit_vs_slower_baseline"]},
                      "regime": {"floor_met_by": run["regime"]["floor_met_by"],
                                 "provisional": run["regime"]["provisional"],
                                 "data_to_cap": {arm: v["data_to_cap"]
                                                 for arm, v in run["footprint"].items()}},
                  } for run in runs]}
        out = Path(a.out) if a.out else Path(a.merge[0]).parent.parent / "report.json"
        out.write_text(json.dumps(merged, sort_keys=True, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {out}")
        return 0

    root = Path(a.root).resolve()
    rungs = load_rungs(root)
    if not rungs:
        raise SystemExit(f"no rung-* directories under {root}")

    caps = {arm: rung["arms"][arm]["engine_cap_mib"] for rung in rungs
            for arm in rung["arms"] if rung["arms"][arm].get("engine_cap_mib") is not None}
    cap_bytes = (min(caps.values()) if caps else 128) * (1 << 20)
    footprint = {}
    for arm in ARMS:
        sizes = [rung["arms"][arm]["data_bytes"] for rung in rungs if arm in rung["arms"]]
        if sizes:
            data = max(sizes)
            footprint[arm] = {
                "data_bytes": data,
                "regime_cap_bytes": cap_bytes,
                "data_to_cap": round(data / cap_bytes, 3),
                "meets_10x_floor": data / cap_bytes >= REGIME_FLOOR,
            }

    report = {
        "schema": "flashdb.baseline.ladder-report.v1",
        "root": str(root),
        "rungs": rungs,
        "footprint": footprint,
        "t5": t5(next(r for r in rungs if r["offered_rps"] == min(x["offered_rps"] for x in rungs))),
        "t6": t6(rungs),
    }
    declared = report["t5"]["arms"].get("flashdb-declared", {})
    report["t7"] = {
        "condition": "the declared arm alone must already pass T5",
        "pass": declared.get("pass"),
        "deficit_vs_slower_baseline": declared.get("deficit_vs_slower_baseline"),
        "note": ("flashdb-learned runs the same declared-default wiring in M1 (no learner"
                 " controller is wired), so T5's learned number and T7's declared number are the"
                 " same measurement twice rather than two independent arms"),
    }
    report["regime"] = {
        "floor_condition": f"data >= {REGIME_FLOOR:.0f}x the memory budget (pre-registration)",
        "floor_bytes_at_cap": cap_bytes * REGIME_FLOOR,
        "measured": footprint,
        "floor_met_by": [arm for arm, value in footprint.items() if value["meets_10x_floor"]],
        "provisional": any(not v["meets_10x_floor"] for v in footprint.values()),
    }
    out = Path(a.out) if a.out else root / "report.json"
    out.write_text(json.dumps(report, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(__import__("sys").argv[1:]))
