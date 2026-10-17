#!/usr/bin/env python3
"""Score ticket 82's controlled M1 run: per (arm, rung) median/IQR, the device floor, T5/T6/T7.

Inputs (all committed evidence, nothing re-derived from a summary):

    runs/b<block>/rung-<rung>/run-<arm>.json      one arm-run
    runs/b<block>/rung-<rung>/machine-state.jsonl AC/EPP/frequency/load at both boundaries
    runs/b<block>/rung-<rung>/spec.json           the frozen trace + the runner's own argv
    runs/floor/<label>/floor.json                 the device floor (idle pacer + fsync pacer)
    runs/snapshot/<arm>.json                      the pre-load snapshot each restore verified
    runs/run-spec.json                            the orchestrator's argv and dataset

Output: `report.json` (the machine-readable document) and a markdown table.

The rules this script applies, and where each comes from
--------------------------------------------------------
* The unified validity rule is `cold_0b.summarize_curve`'s, evaluated per arm-run by the harness:
  a window is rejected when its per-request schedule-lag p99 exceeds 10,000 us, when lag grew for
  three consecutive windows, or when any request errored; a rung is SUSTAINED iff the achieved rate
  is within 5% of the offered rate and no window was rejected. This script never re-scores windows;
  it aggregates the arms' own verdicts and quotes their arithmetic block by block.
* T5 (1x rung, 50 rps): flashdb within 1.5x of the SLOWER baseline at the same rung, 2x is the fail
  line. Read on the sustained rule: prong A is the throughput deficit, prong B is whether each arm
  actually sustained the rung (an arm that did not sustain it has no sustained level to compare).
* T6: at the highest offered level every arm sustained, flashdb must reach 3x the best baseline.
* T7: the declared arm alone must already pass T5.

A verdict is always PASS or FAIL. Where the reading is PASS but the instrument leaves a residual risk
(variance across five blocks only, one seed, a shared desktop), the row carries that as a named
methodology defect with its smallest fix, so no column hides behind "cannot verify".
"""

from __future__ import annotations

import argparse
import json
import math
import statistics
from pathlib import Path

# Must match `cold_0b.SUSTAINED_TOLERANCE` / `baseline_0b.LAG_CEILING_US`; they are the
# pre-registered numbers, restated here only so a reader of the report can check the arithmetic.
SUSTAINED_TOLERANCE = 0.95
LAG_CEILING_US = 10_000
T5_REQUIRED_RATIO = 1.5
T6_REQUIRED_RATIO = 3.0
REGIME_FOOTPRINT_X = 10.0
DECLARED_ARM = "flashdb-declared"
BASELINES = ("sqlite", "postgres")


def read_json(path: Path):
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


def read_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    with open(path, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def percentile(values: list[float], q: float) -> float | None:
    """Linear-interpolated percentile (the same convention as a spreadsheet's QUARTILE.INC)."""
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    pos = q * (len(ordered) - 1)
    low = math.floor(pos)
    high = math.ceil(pos)
    if low == high:
        return float(ordered[low])
    return ordered[low] + (ordered[high] - ordered[low]) * (pos - low)


def spread(values: list[float]) -> dict:
    """median / Q1 / Q3 / IQR over the blocks, with the raw values kept beside them."""
    vals = [float(v) for v in values if v is not None]
    if not vals:
        return {"median": None, "q1": None, "q3": None, "iqr": None, "values": []}
    q1 = percentile(vals, 0.25)
    q3 = percentile(vals, 0.75)
    return {"median": round(statistics.median(vals), 3), "q1": round(q1, 3), "q3": round(q3, 3),
            "iqr": round(q3 - q1, 3), "values": [round(v, 3) for v in vals]}


# ---------------------------------------------------------------------------
# The arm-runs
# ---------------------------------------------------------------------------
def block_dirs(root: Path) -> list[tuple[int, int, Path]]:
    """`(block, rung, workdir)` for every prepared crossing, in block then rung order."""
    out = []
    for path in sorted(root.glob("b*/rung-*")):
        if not path.is_dir():
            continue
        block = int(path.parent.name[1:])
        rung = int(path.name.split("-")[1])
        out.append((block, rung, path))
    return out


def arm_run(row: dict) -> dict:
    """One arm-run reduced to the numbers the protocol compares, from the record itself."""
    curve = row["curve"][0]
    memory = row.get("engine_memory") or {}
    data_bytes = row.get("data_bytes") or row.get("instance_bytes")
    cap = row.get("engine_cap_mib")
    return {
        "arm": row["arm"],
        "offered_rps": curve["offered_rps"],
        "achieved_rps": round(curve["achieved_rps"], 3),
        "achieved_fraction": round(curve["achieved_fraction"], 5),
        "windows": curve["windows"],
        "rejected_windows": curve["invalid_windows"],
        "reject_reasons": curve["reject_reasons"],
        "sustained": bool(curve["sustained"]),
        "lag_p99_us": curve["lag_p99_us"],
        "lag_p50_us": curve["lag_p50_us"],
        "service_p99_us": curve["p99_us"],
        "service_p50_us": curve["p50_us"],
        "errors": int((row.get("counters") or {}).get("errors", 0)),
        "mean_group_size": round(float(row.get("mean_group_size") or 0.0), 3),
        "max_group_size": int(row.get("max_group_size") or 0),
        "elapsed_s": round(float(row.get("elapsed_s") or 0.0), 3),
        "engine_peak_bytes": memory.get("peak_bytes"),
        "engine_cap_mib": cap,
        "data_bytes": data_bytes,
        "restore_bytes": (row.get("restore") or {}).get("bytes"),
        "footprint_x_cap": (round(data_bytes / (cap * (1 << 20)), 3)
                            if data_bytes and cap else None),
        "restore_sha256": (row.get("restore") or {}).get("sha256"),
        "restore_verified": bool((row.get("restore") or {}).get("verified", False)),
        "oracle": (row.get("oracle") or {}).get("status"),
    }


def collect(root: Path) -> tuple[list[dict], dict[str, list[dict]]]:
    runs: list[dict] = []
    states: dict[str, list[dict]] = {}
    for block, rung, workdir in block_dirs(root):
        states[str(workdir)] = read_jsonl(workdir / "machine-state.jsonl")
        for path in sorted(workdir.glob("run-*.json")):
            row = read_json(path)
            entry = arm_run(row)
            entry["block"] = block
            entry["rung"] = rung
            entry["workdir"] = str(workdir.relative_to(root))
            entry["trace_sha256"] = row.get("trace_sha256")
            runs.append(entry)
    return runs, states


# ---------------------------------------------------------------------------
# Aggregation, the device floor and the readings built on it
# ---------------------------------------------------------------------------
AGGREGATED = ("achieved_rps", "lag_p99_us", "service_p99_us", "rejected_windows", "errors",
              "engine_peak_bytes", "mean_group_size", "max_group_size")


def aggregate(runs: list[dict]) -> list[dict]:
    groups: dict[tuple[str, int], list[dict]] = {}
    for run in runs:
        groups.setdefault((run["arm"], run["rung"]), []).append(run)
    out = []
    for (arm, rung) in sorted(groups, key=lambda key: (key[1], key[0])):
        rows = sorted(groups[(arm, rung)], key=lambda row: row["block"])
        entry = {"arm": arm, "rung": rung, "blocks": len(rows),
                 "sustained_blocks": sum(1 for row in rows if row["sustained"]),
                 "restore_verified_blocks": sum(1 for row in rows if row["restore_verified"]),
                 "trace_sha256": sorted({row["trace_sha256"] for row in rows}),
                 "restore_sha256": sorted({row["restore_sha256"] for row in rows}),
                 "footprint_x_cap": rows[0]["footprint_x_cap"],
                 "engine_cap_mib": rows[0]["engine_cap_mib"],
                 "data_bytes": rows[0]["data_bytes"],
                 "restore_bytes": rows[0]["restore_bytes"],
                 "oracle": sorted({str(row["oracle"]) for row in rows}),
                 "reject_reasons": {},
                 "per_block": [{key: row[key] for key in
                                ("block", "achieved_rps", "lag_p99_us", "service_p99_us",
                                 "rejected_windows", "errors", "sustained")}
                               for row in rows]}
        for key in AGGREGATED:
            entry[key] = spread([row[key] for row in rows])
        reasons: dict[str, int] = {}
        for row in rows:
            for reason, count in row["reject_reasons"].items():
                reasons[reason] = reasons.get(reason, 0) + count
        entry["reject_reasons"] = reasons
        out.append(entry)
    return out


def device_floor(root: Path) -> dict:
    """The floor benches, keyed by label, with the numbers the readings are adjusted by."""
    floors = {}
    for path in sorted(root.glob("floor/*/floor.json")):
        label = path.parent.name
        payload = read_json(path)
        benches = {}
        for name, bench in payload["benches"].items():
            curve = bench["curve"][0]
            benches[name] = {
                "offered_rps": curve["offered_rps"],
                "achieved_rps": round(curve["achieved_rps"], 3),
                "windows": curve["windows"],
                "rejected_windows": curve["invalid_windows"],
                "reject_reasons": curve["reject_reasons"],
                "lag_p50_us": curve["lag_p50_us"],
                "lag_p99_us": curve["lag_p99_us"],
                "service_p50_us": curve["p50_us"],
                "service_p99_us": curve["p99_us"],
                "errors": bench["counters"]["errors"],
                "sustained": bool(curve["sustained"]),
                "windows_file": bench.get("windows_file"),
            }
        floors[label] = {
            "label": label,
            "at": payload.get("at"),
            "rate_rps": payload["rate_rps"],
            "requests": payload["requests"],
            "window_ms": payload["window_ms"],
            "machine": payload.get("machine"),
            "benches": benches,
            "file": str(path.relative_to(root)),
            "machine_state_file": str(path.parent / "machine-state.jsonl"),
        }
    return floors


def floor_reference(floors: dict) -> dict | None:
    """The floor the arms are adjusted against: the element-wise MINIMUM across the floor phases.

    Two floors are taken (before and after the measured phase) so drift is visible, and the reading
    is adjusted against the SMALLER of them: a smaller device floor makes an arm's multiple of it
    larger and its floor-adjusted headroom tighter, so taking the minimum cannot flatter an arm.
    The `before` floor in this run was taken while the instance snapshots were still being written
    back, which is exactly why the two are reported rather than one being picked silently.
    """
    if not floors:
        return None
    idle = [f["benches"]["idle"]["lag_p99_us"] for f in floors.values()]
    fsync_p99 = [f["benches"]["fsync"]["service_p99_us"] for f in floors.values()]
    fsync_p50 = [f["benches"]["fsync"]["service_p50_us"] for f in floors.values()]
    return {
        "labels": sorted(floors),
        "lag_p99_us": min(idle),
        "fsync_p50_us": min(fsync_p50),
        "fsync_p99_us": min(fsync_p99),
        "fsync_lag_p99_us": min(f["benches"]["fsync"]["lag_p99_us"] for f in floors.values()),
        "idle_rejected_windows": {label: f["benches"]["idle"]["rejected_windows"]
                                  for label, f in floors.items()},
        "fsync_rejected_windows": {label: f["benches"]["fsync"]["rejected_windows"]
                                   for label, f in floors.items()},
        "rule": "the element-wise minimum across the floor phases, so no arm is flattered",
    }


def adjust(aggregated: list[dict], floor: dict | None) -> None:
    """Add the floor-adjusted reading to every aggregate: the multiple of the device floor."""
    if not floor:
        return
    lag_floor = floor["lag_p99_us"] or 0
    fsync_floor = floor["fsync_p99_us"] or 0
    for entry in aggregated:
        lag = entry["lag_p99_us"]["median"]
        service = entry["service_p99_us"]["median"]
        entry["floor_adjusted"] = {
            "device_floor_lag_p99_us": lag_floor,
            "device_floor_fsync_p99_us": fsync_floor,
            "lag_p99_over_floor": round(lag / lag_floor, 2) if lag_floor and lag else None,
            "lag_p99_minus_floor_us": (lag - lag_floor) if lag is not None else None,
            "service_p99_over_fsync_floor": (round(service / fsync_floor, 2)
                                             if fsync_floor and service else None),
            # The secondary reading: would the rung still be rejected if the machine's own floor
            # were subtracted from every window's lag? Reported, never used as the primary rule.
            "blocks_clean_below_ceiling_plus_floor": sum(
                1 for row in entry["per_block"]
                if row["lag_p99_us"] <= LAG_CEILING_US + lag_floor),
        }


# ---------------------------------------------------------------------------
# T5 / T6 / T7
# ---------------------------------------------------------------------------
def by_key(aggregated: list[dict]) -> dict[tuple[str, int], dict]:
    return {(entry["arm"], entry["rung"]): entry for entry in aggregated}


def rung_of(t5_rung: int, aggregated: list[dict]) -> int:
    rungs = sorted({entry["rung"] for entry in aggregated})
    return t5_rung if t5_rung in rungs else rungs[0]


def t5(aggregated: list[dict], t5_rung: int, blocks: int) -> dict:
    """Low-load non-inferiority at the 1x rung (pre-registration: within 1.5x of the slower one).

    The pre-registered metric is achieved throughput at the 1x rung, and that is prong A. Prong B is
    the ticket-80 cross-check that keeps a contended rung from being read as a pass: flashdb must
    itself have SUSTAINED the rung under the unified rule, because an arm that did not hold it has no
    sustained level to be non-inferior at. The baselines' own sustained status is reported beside it
    and does NOT flip the verdict: T5 is a claim about flashdb, and a baseline that misses the rule
    at a smaller footprint is evidence FOR flashdb, not against it (it is T6's denominator that this
    deprives, which is why T6 carries it).
    """
    index = by_key(aggregated)
    rung = rung_of(t5_rung, aggregated)
    declared = index[(DECLARED_ARM, rung)]
    deficits = []
    for row in declared["per_block"]:
        block = row["block"]
        flash = row["achieved_rps"]
        baselines = [index[(arm, rung)]["per_block"][block - 1]["achieved_rps"]
                     for arm in BASELINES if (arm, rung) in index]
        if not baselines or not flash:
            continue
        slower = min(baselines)
        deficits.append({"block": block, "flashdb_achieved_rps": flash,
                         "slower_baseline_achieved_rps": slower,
                         "deficit": round(slower / flash, 4)})
    deficit_median = statistics.median([row["deficit"] for row in deficits]) if deficits else None
    sustained_blocks = {arm: index[(arm, rung)]["sustained_blocks"]
                        for arm in (DECLARED_ARM, *BASELINES) if (arm, rung) in index}
    required = max(1, blocks - 1)
    prong_a = deficit_median is not None and deficit_median <= T5_REQUIRED_RATIO
    prong_b = sustained_blocks.get(DECLARED_ARM, 0) >= required
    verdict = "PASS" if (prong_a and prong_b) else "FAIL"
    baseline_sustained = {arm: sustained_blocks.get(arm) for arm in BASELINES}
    defects = []
    if not prong_a:
        defects.append("the throughput deficit exceeded the 1.5x requirement")
    if not prong_b:
        defects.append(f"the declared arm sustained the rung in only"
                       f" {sustained_blocks.get(DECLARED_ARM, 0)} of {blocks} blocks")
    if any((count or 0) < required for count in baseline_sustained.values()):
        defects.append(
            "the baselines did not sustain the 1x rung in enough blocks"
            f" ({baseline_sustained}), so the sustained-level comparison has no baseline"
            " denominator; this weakens T6, not T5, and it is disclosed rather than smoothed")
    return {
        "threshold": "T5 low-load non-inferiority: at the 1x rung flashdb must be within 1.5x of the"
                     " slower baseline (2x is the fail line, 10x a design failure)",
        "rung_rps": rung,
        "condition": f"prong A (pre-registered): median over {len(deficits)} blocks of"
                     f" (slower baseline achieved / flashdb achieved) <= {T5_REQUIRED_RATIO};"
                     f" prong B (ticket-80 cross-check): flashdb sustained the rung under the"
                     f" unified rule in at least {required} of {blocks} blocks",
        "arithmetic": {
            "per_block": deficits,
            "deficit_median": round(deficit_median, 4) if deficit_median is not None else None,
            "deficit_required": T5_REQUIRED_RATIO,
            "sustained_blocks": sustained_blocks,
            "sustained_blocks_required": required,
        },
        "verdict": verdict,
        "prong_a": "PASS" if prong_a else "FAIL",
        "prong_b": "PASS" if prong_b else "FAIL",
        # What the arithmetic can and cannot say. Both arms' achieved rate is bounded by the offered
        # rate of the fixed rung, so a flashdb that HOLDS the rung can only show a deficit up to
        # 1/SUSTAINED_TOLERANCE = 1.0526: prong A is a non-inferiority statement at a paced rung, and
        # prong B (flashdb actually sustained it) carries the substance of the PASS.
        "reading_limit": "with both arms paced at one offered rate, an achieved rate cannot exceed"
                         " that rate, so a declared arm that holds the rung scores a deficit of at"
                         f" most {1 / SUSTAINED_TOLERANCE:.4f}; read this PASS as non-inferiority at"
                         " a paced rung, not as a throughput advantage",
        "defect": "; ".join(defects) if defects else None,
        "fix": (None if verdict == "PASS" else
                "re-run the rung with more blocks so a single host stall cannot decide prong B, and"
                " investigate the declared arm's own rejections before re-reading T5"),
    }


def t6(aggregated: list[dict], blocks: int) -> dict:
    """High-load superiority at the highest common OFFERED level (the pre-registration's wording).

    The pre-registration reads "At the highest common offered level flashdb must reach at least 3x
    the best baseline's sustained throughput under the same latency rule". A level is COMMON when
    every arm was OFFERED it -- not "sustained by every arm in a majority of blocks", which is a rule
    the pre-registration does not state and which the f70d91c scorer applied, leaving the arithmetic
    with an empty denominator at a ladder whose floor none of the baselines sustained. The
    numerator here is flashdb's achieved rate over the blocks in which IT sustained that level, and
    the denominator is the same quantity for the best baseline; an arm that never sustained the
    level contributes no number, which the arithmetic reports as null instead of inventing one.

    The reading carries its own limit, because the limit is what decides the verdict:
    `cold_0b.summarize_curve` bounds every arm's achieved rate by the OFFERED rate (its
    `rate_exceeded` guard), so two arms that both sustain a common offered level are each within
    `SUSTAINED_TOLERANCE` of that same rate and their ratio can only lie in
    [0.95, 1/0.95 = 1.0526]. The pre-registered 3x is therefore unreachable at a common offered
    level for ANY pair of arms, and a FAIL here means "the gate is not met / not demonstrated", not
    "flashdb was measured to be slow". The smallest fix is a ladder with rungs BELOW the 50 rps
    floor, so each baseline's sustained level exists and the ratio can be read as a level ratio;
    that changes the pre-registered metric and is not done here.
    """
    index = by_key(aggregated)
    rungs = sorted({entry["rung"] for entry in aggregated})
    arms = (DECLARED_ARM, *BASELINES)
    required = max(1, blocks - 1)
    sustained_by_rung = {
        rung: {arm: index[(arm, rung)]["sustained_blocks"] for arm in arms if (arm, rung) in index}
        for rung in rungs}

    def sustained_throughput(arm: str, rung: int | None) -> float | None:
        """The arm's median achieved rate over the blocks it SUSTAINED that level (None: never)."""
        if rung is None or (arm, rung) not in index:
            return None
        rates = [row["achieved_rps"] for row in index[(arm, rung)]["per_block"] if row["sustained"]]
        return round(statistics.median(rates), 3) if rates else None

    def highest_sustained(arm: str) -> int | None:
        return max((rung for rung in rungs
                    if sustained_by_rung[rung].get(arm, 0) >= required), default=None)

    common = [rung for rung in rungs if all((arm, rung) in index for arm in arms)]
    highest = max(common) if common else None
    flash = sustained_throughput(DECLARED_ARM, highest)
    baseline_rates = {arm: sustained_throughput(arm, highest) for arm in BASELINES}
    measured = {arm: rate for arm, rate in baseline_rates.items() if rate is not None}
    best = max(measured.items(), key=lambda pair: pair[1]) if measured else None
    ratio = round(flash / best[1], 4) if flash and best and best[1] else None
    achieved_baseline = (index[(best[0], highest)]["achieved_rps"]["median"]
                         if best else None)
    achieved_ratio = (round(index[(DECLARED_ARM, highest)]["achieved_rps"]["median"]
                            / achieved_baseline, 4)
                      if highest is not None and achieved_baseline else None)
    per_arm = {arm: highest_sustained(arm) for arm in arms}
    # The ratio of the highest SUSTAINED LEVELS: not the pre-registered metric, and reported so a
    # FAIL is not read as a measured shortfall. A baseline that sustained nothing sits below the
    # lowest rung offered, so its share is a lower bound rather than a value.
    level_ratio = None
    is_lower_bound = False
    if per_arm[DECLARED_ARM] is not None:
        levels = [per_arm[arm] for arm in BASELINES]
        known = [level for level in levels if level is not None]
        if known and len(known) == len(levels):
            level_ratio = round(per_arm[DECLARED_ARM] / max(known), 3)
        elif known:
            level_ratio, is_lower_bound = round(per_arm[DECLARED_ARM] / max(known), 3), True
        elif levels:
            level_ratio, is_lower_bound = round(per_arm[DECLARED_ARM] / rungs[0], 3), True
    verdict = "PASS" if (ratio is not None and ratio >= T6_REQUIRED_RATIO) else "FAIL"
    if verdict == "PASS":
        defect = None
    elif flash is None:
        defect = (f"flashdb did not sustain the highest common offered level ({highest} rps) in"
                  f" {required} of {blocks} blocks, so it has no sustained throughput there")
    elif best is None:
        defect = (f"no baseline sustained the highest common offered level ({highest} rps) in"
                  f" {required} of {blocks} blocks, so the pre-registered '3x the best baseline's"
                  f" sustained throughput' has no denominator (null)")
    else:
        defect = (f"at {highest} rps flashdb sustained {flash} rps and the best baseline"
                  f" ({best[0]}) sustained {best[1]} rps: {ratio}x against the required"
                  f" {T6_REQUIRED_RATIO}x -- and by construction: an achieved rate is bounded by the"
                  f" offered rate, so two arms that both sustain a common offered level differ by at"
                  f" most {1 / SUSTAINED_TOLERANCE - 1:.4f} and the ratio cannot exceed"
                  f" {1 / SUSTAINED_TOLERANCE:.4f}")
    return {
        "threshold": "T6 high-load superiority: at the highest common offered level flashdb must"
                     " reach at least 3x the best baseline's sustained throughput under the same"
                     " latency rule",
        "condition": f"a level is common when every arm was OFFERED it (the pre-registration's"
                     f" wording); at the highest such level, flashdb's median achieved rate over the"
                     f" blocks it sustained it is compared with the best baseline's same quantity"
                     f" (an arm sustaining the level in fewer than {required} of {blocks} blocks has"
                     f" none, reported as null)",
        "arithmetic": {
            "sustained_blocks_by_rung": {str(rung): counts
                                         for rung, counts in sustained_by_rung.items()},
            "highest_common_offered_rung": highest,
            "highest_sustained_rung_per_arm": per_arm,
            "sustained_throughput_rps": {"flashdb-declared": flash, **baseline_rates},
            "flashdb_achieved_rps": flash,
            "best_baseline": ({"arm": best[0], "achieved_rps": best[1]} if best else None),
            "ratio": ratio,
            "ratio_required": T6_REQUIRED_RATIO,
            "achieved_vs_achieved_ratio": achieved_ratio,
            "ratio_upper_bound_for_any_pair_at_a_common_level": round(1 / SUSTAINED_TOLERANCE, 4),
        },
        "secondary_reading": {
            "metric": "the ratio of each arm's highest sustained offered LEVEL (NOT the"
                      " pre-registered throughput ratio: reported so a FAIL is not read as a measured"
                      " shortfall)",
            "highest_sustained_rung_per_arm": per_arm,
            "ratio": level_ratio,
            "is_lower_bound": is_lower_bound,
        },
        "verdict": verdict,
        "defect": defect,
        "fix": (None if verdict == "PASS" else
                "add rungs BELOW the 50 rps floor so each baseline's sustained level exists, then"
                " read the ratio of the highest sustained LEVELS; the pre-registered throughput"
                " ratio at a common offered level is bounded by 1/SUSTAINED_TOLERANCE for any pair"
                " of arms and cannot reach 3x. Both are pre-registration changes and neither is"
                " made here"),
    }


def t7(t5_verdict: dict) -> dict:
    """The declared arm must already pass T5 on its own readings."""
    return {
        "threshold": "T7 initial-policy quality: the declared arm alone must pass T5",
        "condition": "the declared arm's own T5 prongs at the 1x rung",
        "arithmetic": {"t5_verdict": t5_verdict["verdict"],
                       "t5_deficit_median": t5_verdict["arithmetic"]["deficit_median"],
                       "declared_sustained_blocks":
                           t5_verdict["arithmetic"]["sustained_blocks"].get(DECLARED_ARM)},
        "verdict": t5_verdict["verdict"],
        "disclosure": "flashdb-learned was not run: M1 wires no learner controller, so a second"
                      " flashdb arm would measure the same declared wiring twice",
        "defect": t5_verdict["defect"],
        "fix": t5_verdict["fix"],
    }


# ---------------------------------------------------------------------------
# Per-metric verdicts
# ---------------------------------------------------------------------------
# What a failing metric actually needs, per metric: the earlier scorer emitted the rejected-window
# sentence for every failure, which told a reader that the service-p99 row had been rejected by the
# unified rule. That rule scores SCHEDULE LAG (`baseline_0b._reject_reason`), and every run record
# carries `service_ceiling_rejected_windows` 0, so a service failure asks for instrumentation, not a
# re-run of the rung.
METRIC_FIXES = {
    "achieved_rps": "re-run the rung and check the pacer kept its schedule: an achieved rate below"
                    " the 5% tolerance is the rung's wall time exceeding its offered span",
    "lag_p99_us": "attribute the lag to the engine before tuning: the device floor and the arm's own"
                  " service tail say whether the dispatch or the engine's work is late",
    "service_p99_us": "instrument the commit path for the > 100 ms outliers (a per-window"
                      " `group_elapsed_us` histogram) before tuning anything; no service ceiling is"
                      " pre-registered, so this row is reported beside the rule, not enforced by it",
    "rejected_windows": "re-run the rung with more blocks once the named rejection is understood; the"
                        " unified rule rejects the WINDOW (schedule lag p99 > 10 ms), so the rung has"
                        " no sustained level",
    "errors": "the failing request and its first error are in the run record; fix it and re-run the"
              " rung, because an errored window can never be sustained",
    "engine_peak_bytes": "the engine exceeded its memory cap: raise the cap only if the regime says"
                         " so, otherwise find the allocation that grew",
}


def metric_rows(aggregated: list[dict], floor: dict | None) -> list[dict]:
    rows = []
    fsync_floor_us = (floor or {}).get("fsync_p99_us") or 0
    for entry in aggregated:
        arm, rung = entry["arm"], entry["rung"]
        cap_bytes = (entry["engine_cap_mib"] or 0) * (1 << 20) or None
        checks = [
            ("achieved_rps", "the achieved rate is within 5% of the offered rate",
             entry["achieved_rps"]["median"] is not None
             and entry["achieved_rps"]["median"] >= SUSTAINED_TOLERANCE * rung,
             f"{entry['achieved_rps']['median']} rps of {rung} offered"),
            ("lag_p99_us", f"the per-request schedule-lag p99 is at or below {LAG_CEILING_US} us",
             (entry["lag_p99_us"]["median"] is not None
              and entry["lag_p99_us"]["median"] <= LAG_CEILING_US),
             f"{entry['lag_p99_us']['median']} us median, {entry['lag_p99_us']['iqr']} us IQR"),
            # No service ceiling is pre-registered: the rule is on SCHEDULE LAG. The per-request
            # service p99 (dispatch -> completion, i.e. lag + the engine's own work) is reported
            # against the same 10 ms bound adjusted by the device floor, so it says whether the
            # engine's own commit work is inside the bound the device can actually meet.
            ("service_p99_us", "the per-request service p99 is within 10,000 us of the device's own"
                               " fsync p99 (no service ceiling is pre-registered; secondary)",
             (entry["service_p99_us"]["median"] is not None
              and entry["service_p99_us"]["median"] - fsync_floor_us <= LAG_CEILING_US),
             f"{entry['service_p99_us']['median']} us median against a"
             f" {fsync_floor_us} us device fsync p99"),
            ("rejected_windows", "zero windows rejected by the unified rule in any block",
             entry["rejected_windows"]["values"] and
             all(value == 0 for value in entry["rejected_windows"]["values"]),
             f"{int(sum(entry['rejected_windows']['values']))} rejected over"
             f" {entry['blocks']} blocks; reasons {entry['reject_reasons'] or 'none'}"),
            ("errors", "zero request errors in any block",
             all(value == 0 for value in entry["errors"]["values"]),
             f"{int(sum(entry['errors']['values']))} errors over {entry['blocks']} blocks"),
            ("engine_peak_bytes", f"the engine's peak stays inside its {entry['engine_cap_mib']}"
                                  " MiB cap",
             (entry["engine_peak_bytes"]["median"] is not None and cap_bytes is not None
              and entry["engine_peak_bytes"]["median"] <= cap_bytes),
             f"{entry['engine_peak_bytes']['median']} B median of {cap_bytes} B cap"),
        ]
        for metric, rule, ok, detail in checks:
            rows.append({
                "arm": arm, "rung": rung, "metric": metric, "rule": rule,
                "median": entry[metric]["median"], "q1": entry[metric]["q1"],
                "q3": entry[metric]["q3"], "iqr": entry[metric]["iqr"],
                "blocks": entry["blocks"],
                "verdict": "PASS" if ok else "FAIL",
                "detail": detail,
                "defect": None if ok else
                          f"{metric} missed its rule at {rung} rps: {detail}",
                "fix": None if ok else METRIC_FIXES[metric],
            })
    return rows


# ---------------------------------------------------------------------------
# Machine state + disclosure
# ---------------------------------------------------------------------------
def _freq_summary(records: list[dict]) -> dict:
    cores = [int(value) for record in records
             for value in (record.get("scaling_cur_freq_khz") or []) if str(value).isdigit()]
    per_record_median = []
    for record in records:
        values = [int(v) for v in (record.get("scaling_cur_freq_khz") or []) if str(v).isdigit()]
        if values:
            per_record_median.append(statistics.median(values))
    return {
        "per_core_min_khz": min(cores) if cores else None,
        "per_core_median_khz": round(statistics.median(cores)) if cores else None,
        "per_core_max_khz": max(cores) if cores else None,
        "record_median_of_cores_khz": {
            "min": round(min(per_record_median)) if per_record_median else None,
            "median": round(statistics.median(per_record_median)) if per_record_median else None,
            "max": round(max(per_record_median)) if per_record_median else None},
    }


def machine_summary(states: dict[str, list[dict]], floors: dict, since: str | None = None) -> dict:
    """The machine state across the arm boundaries, with the excluded records counted.

    `since` keeps the summary to ONE run: a re-launched measurement appends its boundaries to the
    same files, so a boundary without its attempt would mix two machine states into one summary.
    """
    records = [state for rows in states.values() for state in rows]
    # The floor's own boundary records are evidence too: the floor and the arms must have been
    # measured at the same machine state.
    for floor in floors.values():
        records.extend(read_jsonl(Path(floor["machine_state_file"])))
    total = len(records)
    excluded = 0
    if since:
        kept = [record for record in records if (record.get("at") or "") >= since]
        excluded = total - len(kept)
        records = kept

    def uniq(key: str) -> list:
        values = []
        for record in records:
            value = record.get(key)
            if value not in values:
                values.append(value)
        return values

    def flat(key: str) -> list:
        """One sorted list of the observed values, per-CPU values included."""
        out = []
        for value in uniq(key):
            for item in (value if isinstance(value, list) else [value]):
                if item not in out:
                    out.append(item)
        return sorted(out, key=str)

    loads = []
    for record in records:
        parts = (record.get("loadavg") or "").split()
        if parts:
            loads.append(float(parts[0]))
    concurrent = [record for record in records if record.get("concurrent_work")]
    return {
        "records": len(records),
        "records_excluded_before_since": excluded,
        "since": since,
        "ac_online": flat("ac_online"),
        "power_profile": flat("power_profile"),
        "governor": flat("governor"),
        "epp": flat("epp"),
        # The EFFECTIVE frequency, not the governor's name: each boundary reads all eight cores, so
        # the per-core readings are summarised as a min/max plus the median of each record's own
        # median (a core that has gone idle reports ~0.4 GHz even at an EPP hint of `performance`).
        "scaling_cur_freq_khz": _freq_summary(records),
        "scaling_max_freq_khz": flat("scaling_max_freq_khz"),
        "nproc": flat("nproc"),
        "loadavg_1min": {"min": min(loads) if loads else None,
                         "max": max(loads) if loads else None,
                         "median": round(statistics.median(loads), 2) if loads else None},
        "top_cpu_observed": sorted({proc["comm"] for record in records
                                    for proc in (record.get("top_cpu") or [])}),
        "concurrent_work_records": len(concurrent),
        "concurrent_work": [{"phase": record.get("phase"), "arm": record.get("arm"),
                             "found": record["concurrent_work"]} for record in concurrent],
    }


# The instrument behind each arm's numerator. They are NOT the same measurement, so a ratio between
# two arms is not one dataset measured twice: flashdb's is its instance directory (live WAL included),
# sqlite's is its database plus sidecars, and postgres's is `pg_database_size`, which the WAL is
# outside of -- so postgres also carries the cluster data directory the restore verified.
FOOTPRINT_INSTRUMENTS = {
    DECLARED_ARM: "directory bytes of the instance the arm served, live WAL included",
    "sqlite": "database file plus its WAL and shared-memory sidecars, read after the post-run"
              " checkpoint",
    "postgres": "pg_database_size(current_database()): LOGICAL size, the WAL excluded",
}


def footprint(aggregated: list[dict]) -> list[dict]:
    out = []
    lowest = min(item["rung"] for item in aggregated)
    for entry in aggregated:
        if entry["rung"] != lowest:
            continue
        cap = entry["engine_cap_mib"] * (1 << 20) if entry["engine_cap_mib"] else None
        row = {"arm": entry["arm"], "data_bytes": entry["data_bytes"],
               "instrument": FOOTPRINT_INSTRUMENTS.get(entry["arm"]),
               "engine_cap_mib": entry["engine_cap_mib"],
               "footprint_x_cap": entry["footprint_x_cap"],
               "meets_regime_floor": (entry["footprint_x_cap"] is not None
                                      and entry["footprint_x_cap"] >= REGIME_FOOTPRINT_X)}
        if entry["arm"] == "postgres" and entry["restore_bytes"] and cap:
            row["data_dir_bytes"] = entry["restore_bytes"]
            row["data_dir_instrument"] = ("the cluster data directory the restore verified, WAL"
                                          " included (run record `restore.bytes`)")
            row["data_dir_x_cap"] = round(entry["restore_bytes"] / cap, 3)
        out.append(row)
    return out


def markdown_table(aggregated: list[dict]) -> str:
    lines = ["| arm | rung | achieved rps (median, IQR) | sustained blocks | lag p99 us (median, IQR)"
             " | service p99 us (median, IQR) | rejected windows (total) | errors | engine peak MiB"
             " | x floor |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    for entry in aggregated:
        peak = entry["engine_peak_bytes"]["median"]
        adjusted = entry.get("floor_adjusted") or {}
        lines.append(
            f"| {entry['arm']} | {entry['rung']} | {entry['achieved_rps']['median']}"
            f" ({entry['achieved_rps']['iqr']})"
            f" | {entry['sustained_blocks']}/{entry['blocks']}"
            f" | {entry['lag_p99_us']['median']} ({entry['lag_p99_us']['iqr']})"
            f" | {entry['service_p99_us']['median']} ({entry['service_p99_us']['iqr']})"
            f" | {int(sum(entry['rejected_windows']['values']))}"
            f" | {int(sum(entry['errors']['values']))}"
            f" | {round(peak / (1 << 20), 1) if peak is not None else 'not measured'}"
            f" | {adjusted.get('lag_p99_over_floor', '-')} |")
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="score ticket 82's controlled M1 run")
    ap.add_argument("--root", required=True)
    ap.add_argument("--out", default=None)
    ap.add_argument("--markdown", default=None)
    ap.add_argument("--since", default=None,
                    help="drop machine-state boundaries taken before this ISO instant, so a"
                         " re-launched measurement does not mix two machine states in one summary")
    ap.add_argument("--t5-rung", type=int, default=50,
                    help="the pre-registered 1x rung (50 rps at the >=10x regime)")
    args = ap.parse_args(argv)

    root = Path(args.root).resolve()
    runs, states = collect(root)
    if not runs:
        raise SystemExit(f"no run records under {root}; nothing to score")
    aggregated = aggregate(runs)
    floors = device_floor(root)
    reference = floor_reference(floors)
    adjust(aggregated, reference)
    blocks = max(run["block"] for run in runs)
    t5_verdict = t5(aggregated, args.t5_rung, blocks)
    report = {
        "schema": "flashdb.m1.controlled-report.v1",
        "run_spec": read_json(root / "run-spec.json") if (root / "run-spec.json").exists() else None,
        "blocks": blocks,
        "rungs": sorted({run["rung"] for run in runs}),
        "arms": sorted({run["arm"] for run in runs}),
        "device_floor": floors,
        "device_floor_reference": reference,
        "machine_state": machine_summary(states, floors, args.since),
        "footprint": footprint(aggregated),
        "table": aggregate(runs),
        "aggregate": aggregated,
        "metrics": metric_rows(aggregated, reference),
        "verdicts": {"t5": t5_verdict, "t6": t6(aggregated, blocks), "t7": t7(t5_verdict)},
    }
    out = Path(args.out) if args.out else root / "report.json"
    out.write_text(json.dumps(report, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    table = markdown_table(aggregated)
    if args.markdown:
        Path(args.markdown).write_text(table, encoding="utf-8")
    print(table)
    for name, verdict in report["verdicts"].items():
        print(f"{name.upper()}: {verdict['verdict']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(__import__("sys").argv[1:]))
