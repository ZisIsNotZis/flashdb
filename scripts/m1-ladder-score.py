"""Score the M1 ladder into report.json: T5/T6/T7 from the raw arm records, conditions stated.

Every number here is read from the committed run records (`run-<arm>.json` per rung) and the
per-arm data footprint the arms measured for themselves; nothing is estimated and no threshold is
invented. The pre-registered conditions are quoted in the output so a reader can check the
arithmetic against the pre-registration rather than against this script:

  T5  at the 1x rung, flashdb within 1.5x of the slower of sqlite/postgres; 2x is the fail line;
  T6  at the highest common offered level, flashdb reaches >= 3x the best baseline's SUSTAINED
      throughput under the same latency rule -- reported with both readings, the pre-registered one
      at the top rung (evaluable only when a baseline sustained that rung) and the
      sustained-vs-sustained one (highest sustained level per arm);
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

# The milestone's unverified list and its honest summary, kept beside the arithmetic so the JSON
# carries them too. The eight entries are the same eight facts as `report.md` §7, in the same order;
# anything listed here is NOT a pass.
UNVERIFIED = (
    "T6 at the pre-registered regime: it needs a ladder at >=10x, and the committed arm-step"
    " boundaries put a four-rung 10x ladder at ~8 h of measured cost (8 x 3,075 s flashdb + 4 x"
    " 330 s sqlite + 4 x 839 s postgres = 29,276 s); run B measured the 1x rung only.",
    "The sustained-rule outcome at the regime rung is not robustly determined: run B carried 6.14"
    " on 8 cores at the flashdb arm step's start (before its 416 s seed) and 3.39 at its end, with"
    " the measured phase's own load unrecorded, and no arm sustained 50 rps (flashdb 12 of 20"
    " windows rejected, sqlite 1 of 20 with a 1.97 s stall, postgres 3 of 20), while the earlier"
    " pre-fix run on the same seed and data (loadavg not recorded, its provenance defect) had"
    " sqlite and postgres clean and flashdb 1 of 20. A quiet-box repeat decides it.",
    "The pre-registered 10-seed median/IQR aggregation: both runs are one seed, one rung, one repeat.",
    "Run A is a second measurement of the same traces, not the first one repaired: its SQL windows"
    " were re-measured on 2026-10-17 at loadavg 2.2-4.9 rather than the 1.5-2.5 of the first ladder,"
    " so its numbers differ from the earlier report's in several cells; the traces are byte-identical.",
    "The growth clause's effect on the previously committed SQL windows cannot be re-derived: those"
    " windows carry no per-window late-request count, so they were re-measured rather than re-scored,"
    " and in the re-measured windows the clause changed no SQL verdict.",
    "The 12.546x footprint is re-derivable only by re-running the build (416 s): the instance files"
    " are git-ignored, and the record carries the total and the four largest files, not the full"
    " tiling.",
    "The regime run exercised only the take branch (694 takes, 0 backorders), so at 12.546x the"
    " backorder checks are vacuous; `runs/m1-build-else` exercises that branch at 0.02x, not at the"
    " regime.",
    "The shared box: run A saw 2.2-4.9 on 8 cores and run B 2.7-6.1 at the run boundaries; a quiet"
    " box would move the rejection counts, not the mechanism.",
)
SUMMARY = (
    "The instrument works, and its two acceptance-shaped holes are closed or named. One harness"
    " drives four arms over one frozen trace (same sha256 per rung on every arm) at matched"
    " durability, each engine in its own 128 MiB scope, under one latency rule - the lag ceiling,"
    " the 3-consecutive-window growth clause and errors, bound by a test to flashdb-load's own"
    " verdicts - and every inventory oracle passes everywhere it was run, now including flashdb's"
    " 10x read-back (12 checks over 10.6M cells). At the pre-registered regime (flashdb 1.68 GB ="
    " 12.546x of the cap, one seed, the 1x rung) all three arms hold 50 rps with zero errors and"
    " T5's throughput deficit is 1.001 against the 1.5x line, and T7 with it; but that run was"
    " contended (6.14 at the flashdb arm step's start, its measured phase unrecorded): flashdb was"
    " rejected on 12 of 20 windows, sqlite on 1 (a 1.97 s"
    " stall) and postgres on 3, so NO arm sustained the rung there, while the earlier pre-fix run"
    " on the same seed and data had both baselines clean. T5's throughput reading therefore"
    " reproduces across both runs and its sustained reading does not: the T5/T7 verdict is not"
    " robustly determined and needs a quiet-box repeat. T6 at the regime - the actual"
    " external-superiority claim - remains unevaluated (no ladder at 10x). In the warm ladder T6's"
    " pre-registered comparison has no denominator at 400 rps (no baseline sustained that level),"
    " and the sustained-vs-sustained reading is 0.50x for the declared arm and 1.00x for the"
    " learned one against the 3x line, i.e. FAIL. No result here supports a claim that flashdb"
    " beats postgres or sqlite at scale, a claim about the learned arm (in M1 it is the declared"
    " wiring), or a claim about drift and reorg cost."
)


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def load_average(path: Path) -> list[dict]:
    """The runner's own `loadavg.jsonl` boundaries for a rung directory (empty when absent).

    The schedule-lag rule is a scheduler-latency rule on a shared box, so the machine's load at every
    prepare/arm/report boundary is part of the evidence, not a footnote: it is what separates a
    quiet-box measurement from a contended one.
    """
    record = Path(path) / "loadavg.jsonl"
    if not record.exists():
        return []
    return [json.loads(line) for line in record.read_text().splitlines() if line.strip()]


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
                "data_bytes": data.get("data_bytes") or data.get("instance_bytes"),
                "footprint_files": data.get("instance_files") or data.get("sqlite_files"),
                "oracle": data["oracle"]["status"],
                "oracle_checks": len(data["oracle"].get("checks", [])),
                "mean_group_size": data.get("mean_group_size"),
                "seed_s": data.get("seed_s"),
                "oracle_s": data.get("oracle_s"),
                "durability": data.get("durability"),
                "durability_observed": data.get("durability_observed"),
                "sqlite_cache_kib": data.get("sqlite_cache_kib"),
                "postgres_settings": data.get("postgres_settings"),
                # Memory, kept in its two scopes: `engine_memory` is the SERVING peak measured over
                # the measured phase, `construction_memory`/`arm_scope_peak_bytes` are the seeding
                # and lifetime numbers. A seeding peak is never read as serving pressure.
                "engine_cap_mib": data.get("engine_cap_mib"),
                "engine_memory": data.get("engine_memory"),
                "construction_memory": data.get("construction_memory"),
                "arm_scope_peak_bytes": data.get("arm_scope_peak_bytes"),
                "arm_scope_peak_scope": data.get("arm_scope_peak_scope"),
            }
        rungs.append({
            "offered_rps": offered,
            "requests": manifest["total_requests"],
            "trace_sha256": manifest["trace_sha256"],
            "window_ms": spec["window_ms"],
            "batch": spec["batch"],
            "on_hand": spec["on_hand"],
            "runner": spec.get("runner"),
            "load_average": load_average(path),
            "arms": arms,
        })
    return rungs


def sustained_level(rungs: list[dict], arm: str) -> dict | None:
    """An arm's HIGHEST SUSTAINED offered level, with the achieved rate it held there.

    The replacement for the retired `sustained_verdict()` helper: a sustained reading is the highest
    level the arm actually sustained under the rule, never the achieved rate at a higher rung it did
    not sustain.
    """
    best = None
    for rung in rungs:
        value = rung["arms"].get(arm)
        if not value or not value.get("sustained") or not value.get("achieved_rps"):
            continue
        if best is None or value["achieved_rps"] > best["achieved_rps"]:
            best = {"offered_rps": rung["offered_rps"], "achieved_rps": value["achieved_rps"]}
    return best


def deficit(flashdb_rps: float, baseline_rps: float) -> float:
    """How many times slower flashdb is than the baseline (`<=1` means flashdb is faster)."""
    return baseline_rps / flashdb_rps if flashdb_rps else float("inf")


def t_state(passed, like_for_like: bool) -> str:
    """The tri-state a JSON-only reader needs: `pass`, `fail` or `not-robustly-determined`.

    The pre-registered metric is throughput, so `pass` records the throughput arithmetic. But a
    throughput pass on a rung where the arm (or a baseline) was rejected is not a like-for-like
    reading, and a consumer that reads only `pass: true` would report a PASS the report does not
    claim. The tri-state makes the sustained half unmissable.
    """
    if passed is None:
        return "not-evaluable"
    if not passed:
        return "fail"
    return "pass" if like_for_like else "not-robustly-determined"


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
    baselines_sustained = all(v["sustained"] for v in baselines.values())
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
            # What the pass rests on, so a reader can see whether it is the throughput reading or
            # also the sustained one: the pre-registered metric is throughput, but a rung where the
            # flashdb arm was rejected and the baselines were not is not a like-for-like comparison.
            "sustained_reading": ("like-for-like (every arm sustained the rung)" if
                                  value["sustained"] and baselines_sustained else
                                  "not like-for-like (flashdb and the baselines did not all"
                                  " sustain the rung)"),
            "verdict": t_state(d <= T5_PASS_RATIO,
                               bool(value["sustained"] and baselines_sustained)),
        }
    return out


def t6(rungs: list[dict]) -> dict:
    """High-load superiority at the highest common offered level, sustained against sustained.

    The pre-registered condition is ">= 3x the best baseline's SUSTAINED throughput under the same
    latency rule" AT the highest common offered level. An achieved rate at a level the arm did not
    sustain is not a sustained rate, so it can be neither the numerator nor the denominator of that
    ratio. Two readings are therefore produced from the same records:

    * `at_top_rung` -- the pre-registered comparison. It is evaluable only when a baseline SUSTAINED
      the highest common offered level; otherwise the ratio has no denominator there, and the honest
      answer is that it is not evaluable rather than a substitute number.
    * `sustained_reading` -- each flashdb arm's highest SUSTAINED level (whichever offered level that
      was) against the best baseline's highest sustained level. That reading crosses offered levels,
      so it is labelled as such and never presented as the pre-registered comparison.
    """
    common = [r for r in rungs if all(arm in r["arms"] for arm in ARMS)]
    if not common:
        return {"evaluable": False, "reason": "no rung ran every arm"}
    if len(common) < 2:
        # T6 compares the highest common offered level with a sustained baseline reading; with one
        # level it would compare a rung with itself and always read ~1.0x.
        return {"evaluable": False,
                "reason": "a single offered level is not a ladder; T6 needs at least two levels"}
    top = common[-1]
    baselines_sustained = {arm: bool(top["arms"][arm]["sustained"]) for arm in BASELINES}
    denominator = max((top["arms"][arm]["achieved_rps"] for arm in BASELINES
                       if top["arms"][arm]["sustained"]), default=0.0)
    out = {
        "condition": f"at the highest common offered level ({top['offered_rps']:.0f} rps) flashdb"
                     f" must reach >= {T6_MULTIPLE}x the best baseline's SUSTAINED throughput under"
                     " the same latency rule",
        "highest_common_offered_rps": top["offered_rps"],
        "at_top_rung": {
            "evaluable": denominator > 0,
            "baselines_sustained": baselines_sustained,
            "baseline_denominator_rps": denominator,
            "arms": {arm: {"achieved_rps": top["arms"][arm]["achieved_rps"],
                           "sustained": top["arms"][arm]["sustained"],
                           "invalid_windows": top["arms"][arm]["invalid_windows"],
                           "reject_reasons": top["arms"][arm]["reject_reasons"]}
                     for arm in FLASHDB},
        },
        "sustained_reading": {"arms": {}, "crosses_offered_levels": True},
    }
    if denominator <= 0:
        out["at_top_rung"]["reason"] = (
            f"no baseline sustained {top['offered_rps']:.0f} rps under the rule"
            f" ({baselines_sustained}), so the pre-registered ratio has no denominator at the"
            " highest common offered level")
        out["at_top_rung"]["multiple_of_best_baseline"] = None
        out["at_top_rung"]["pass"] = None
    else:
        for value in out["at_top_rung"]["arms"].values():
            multiple = value["achieved_rps"] / denominator
            value["multiple_of_best_baseline"] = round(multiple, 3)
            value["pass"] = bool(value["sustained"]) and multiple >= T6_MULTIPLE
        out["at_top_rung"]["pass"] = all(v["pass"]
                                         for v in out["at_top_rung"]["arms"].values())
    best_baseline = None
    for arm in BASELINES:
        level = sustained_level(rungs, arm)
        if level and (best_baseline is None
                      or level["achieved_rps"] > best_baseline["achieved_rps"]):
            best_baseline = {**level, "arm": arm}
    out["sustained_reading"]["best_baseline"] = best_baseline
    for arm in FLASHDB:
        level = sustained_level(rungs, arm)
        if level is None or best_baseline is None:
            out["sustained_reading"]["arms"][arm] = {
                "highest_sustained": level,
                "multiple_of_best_baseline": None,
                "pass": False,
                "reason": ("the arm sustained no offered level" if level is None
                           else "no baseline sustained any offered level"),
            }
            continue
        multiple = level["achieved_rps"] / best_baseline["achieved_rps"]
        out["sustained_reading"]["arms"][arm] = {
            "highest_sustained": level,
            "multiple_of_best_baseline": round(multiple, 3),
            "pass": multiple >= T6_MULTIPLE,
        }
    out["evaluable"] = out["at_top_rung"]["evaluable"]
    return out


def t6_verdict_row(t6: dict) -> dict:
    """The merged report's one-line T6 verdict: both readings, never one without the other.

    `at_top_rung` is the pre-registered comparison and says why it is or is not evaluable;
    `sustained_vs_sustained` is the reading that uses sustained levels only.
    """
    top = t6.get("at_top_rung", {})
    reading = t6.get("sustained_reading", {})
    arms = top.get("arms", {})
    return {
        "condition": t6.get("condition"),
        "highest_common_offered_rps": t6.get("highest_common_offered_rps"),
        "evaluable": bool(t6.get("evaluable")),
        "reason": top.get("reason") or t6.get("reason"),
        "baselines_sustained": top.get("baselines_sustained"),
        "baseline_denominator_rps": top.get("baseline_denominator_rps"),
        "at_top_rung": {arm: {"sustained": v.get("sustained"),
                              "achieved_rps": v.get("achieved_rps"),
                              "invalid_windows": v.get("invalid_windows"),
                              "reject_reasons": v.get("reject_reasons"),
                              "multiple_of_best_baseline":
                                  v.get("multiple_of_best_baseline"),
                              "pass": v.get("pass")}
                        for arm, v in arms.items()} if arms else
                       {arm: {"multiple_of_best_baseline": v.get("multiple_of_best_baseline"),
                              "pass": v.get("pass")} for arm, v in t6.get("arms", {}).items()},
        "sustained_vs_sustained": {
            "crosses_offered_levels": True,
            "best_baseline": reading.get("best_baseline"),
            "arms": {arm: {"highest_sustained": v.get("highest_sustained"),
                           "multiple_of_best_baseline": v.get("multiple_of_best_baseline"),
                           "pass": v.get("pass"),
                           "reason": v.get("reason")}
                     for arm, v in reading.get("arms", {}).items()},
        },
    }


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
                                   "pass": v["pass"],
                                   "verdict": v.get("verdict"),
                                   "sustained": v["sustained"],
                                   "sustained_reading": v.get("sustained_reading"),
                                   "invalid_windows": v["invalid_windows"]}
                             for arm, v in run["t5"].get("arms", {}).items()},
                      "t6": t6_verdict_row(run["t6"]),
                      "t7": {"pass": run["t7"]["pass"],
                             "verdict": run["t7"].get("verdict"),
                             "deficit_vs_slower_baseline":
                                 run["t7"]["deficit_vs_slower_baseline"],
                             "sustained": run["t5"].get("arms", {}).get(
                                 "flashdb-declared", {}).get("sustained"),
                             "note": run["t7"].get("note")},
                      "regime": {"floor_met_by": run["regime"]["floor_met_by"],
                                 "provisional": run["regime"]["provisional"],
                                 "data_to_cap": {arm: v["data_to_cap"]
                                                 for arm, v in run["footprint"].items()}},
                  } for run in runs],
                  "unverified": list(UNVERIFIED),
                  "summary": SUMMARY}
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
    report["t5"]["baselines_sustained"] = {
        arm: value["sustained"] for arm, value in report["t5"].get("baselines", {}).items()}
    declared = report["t5"]["arms"].get("flashdb-declared", {})
    declared_like_for_like = bool(
        declared.get("sustained")
        and all(report["t5"].get("baselines_sustained", {}).values()))
    report["t7"] = {
        "condition": "the declared arm alone must already pass T5",
        "pass": declared.get("pass"),
        "verdict": t_state(declared.get("pass"), declared_like_for_like),
        "sustained": declared.get("sustained"),
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
