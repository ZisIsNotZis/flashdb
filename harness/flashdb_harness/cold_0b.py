"""RUN experiment 0b: the three arms (plus the relaxed oracle) cold, at equal offered load (Slice 35).

What this measures
------------------
The pre-registration (``.scratch/15-objective-v0/pre-registration.md``) names four arms and four
falsification thresholds. Objective v1 (``.scratch/15-objective-v0/objective-v1.md``) says the
score is **sustained throughput at an offered load**, with latency a CONSTRAINT, and efficiency
reported. This module runs that matrix on the **cold** instance built by slice 29/30 (data held at
a data:cgroup-cap ratio of 8.17x, below the pre-registration 10x floor - the run says so), with the
offered load stepped 1x/2x/4x/8x of a reference rate.

The arms (equal durability: the same ``--group-requests`` batch everywhere, same trace, same rates):

* ``declared``           - one fixed candidate everywhere: the shipped default 4 KiB / merge 2;
* ``static-derived``     - the per-region argmin from a PRE-DRIFT sweep, frozen (never adapts);
* ``learned``            - the UCB1 per-region learner driven LIVE through ``--policy-file`` /
                           ``--policy-reload`` (one controller step per metrics window);
* ``oracle``             - the relaxed hindsight best over the measured candidate set, reported as
                           relative regret (``oracle_rps`` is the best measured policy for the
                           workload; regret = (oracle - arm) / oracle).

``lambda`` is the endurance price in the learner's reward proxy (``count*mean_ns +
lambda*(bytes_written + compaction_bytes)``). Slice 39 anchored it to the measured device write
bandwidth (831 MiB/s ~= 1.148 ns/byte): at lambda=1 a written byte costs exactly its device write
time, and lambda is the explicit endurance weight on top of that. Before slice 39 the write term
priced only ``bytes_written`` (WAL ingest, close to constant across candidates), so lambda could
not move the argmin; the reorganisation output the merge knob actually moves is now priced too.
It is a POLICY choice, not a measurement, so the runner sweeps it (default 0.1/1.0/10.0) and
every verdict names the lambda it belongs to. The runner reports the term magnitudes (service vs
ingest-write vs reorganisation-write) so a reader can see which term can move the argmin.

The drift workload runs at a FIXED offered rate, resolved from ``--drift-rate``: an explicit
rate, or ``auto`` (slice 39). ``auto`` runs a short pre-flight ladder on the PRE-DRIFT (win) mix,
takes its highest sustained level, and sets the drift rate to ``--drift-rate-fraction`` of it
(70% by default). Slice 38's drift arm used a fixed 150 rps while its trace is 64% scans (vs the
win mix's 45%), above the engine's capacity for that mix: every arm saturated, so "recover to
within 10% of pre-drift throughput" had no baseline. A rate below the pre-drift mix's capacity
makes the drift threshold evaluable.

The learner also carries CHURN CONTROL between re-explorations (slice 39): a per-region cooldown
after a change (``--learner-cooldown-windows``) and a per-window reorganisation budget derived
from the declared arm's measured reorganisation rate (``--learner-reorg-budget-factor``). Without
it the slice-38 drift learner wrote 402,984,851 reorganisation bytes against declared's 1,673,926
(240x).

What the cold instance already tells us, and why this runner is built the way it is
--------------------------------------------------------------------------------------
Two measured facts bound what any arm can move on this box:

1. **A per-region page size acts only on tiles written AFTER the switch.** Every tile stores its
   own page size and readers honour the stored value (the ``LayoutPolicy`` is not persisted), so
   changing the policy over an existing instance changes nothing on a read-only load. The runner
   therefore gives every arm the same write fraction and the same bounded checkpoint cadence
   (``--checkpoint-bytes``/``--rotate-bytes``), so each arm republishes its regions under its own
   policy and the read segments measure it.
2. **The offered-load driver now compacts (Slice 37).** ``--compact-bytes N`` calls the engine's
   existing ``maybe_compact_shape`` once per metrics window with the active per-region policy, so
   the learner's *merge* ``merge_min_tiles`` threshold governs the eligible regions and
   ``compaction_bytes`` is non-zero for arms whose policy makes regions eligible. Before this
   slice the driver published (``maybe_checkpoint``) and rotated (``rotate_wal``) but never
   compacted, so that knob was inert and the reorganisation-write threshold was NOT-MEASURABLE
   (0 vs 0). The driver field ``compact_bytes`` reports each window's merged bytes and the
   engine's per-region ``compaction_bytes``/``compaction_merges`` counters are populated.

Cold execution
--------------
Every load runs inside ``systemd-run --user --scope -p MemoryMax=<cap>`` (page cache is counted by
the cgroup, so the working set is evicted), against a fresh ``cp -a`` of the base instance. The
runner records the process RSS peak and the cgroup's own ``memory.peak`` per arm.

Usage (from the harness directory)::

    nohup uv run python -m flashdb_harness.cold_0b --release \\
        --instance /tmp/cold33/per_region-mixed --workdir /tmp/cold0b \\
        --report .scratch/37-0b-actuated/report.md \\
        --json .scratch/37-0b-actuated/results.json > /tmp/cold0b.log 2>&1 &

The long run is meant to be started with ``nohup`` and polled; it prints one line per window.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from statistics import median

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness.generator import Generator, Rng, loc_name, render, sku_name  # noqa: E402
from flashdb_harness.layout_learner import (  # noqa: E402
    DECLARED_DEFAULT,
    DEFAULT_LAMBDA_NS_PER_BYTE,
    LayoutCandidate,
    LayoutLearner,
    parse_window_line,
    policy_document,
    region_cost_ns,
)

REPO_ROOT = Path(__file__).resolve().parents[2]

# The pre-registration floor for the data:memory ratio. Slices 29/30 measured 8.17x on the
# instance this runner reuses; the report must say so rather than round it up.
PREREG_RATIO_FLOOR = 10.0
# The offered-load levels as multiples of the reference rate (objective v1 sec. 1).
RATE_MULTIPLES = (1, 2, 4, 8)
# A window is "sustained" when the offered rate is completed (within this tolerance) and the run's
# lag-growth rule did not reject it (the pacer marks invalid windows; none here).
SUSTAINED_TOLERANCE = 0.95

# A rung's achieved rate is its attributed completions over the longer of its OFFERED schedule
# span (requests/rate) and its elapsed wall time. The pacer is open loop, so the offered span is a
# floor that keeps achieved <= offered; a value above offered by more than this tolerance is a
# measurement error and the rung is never scored sustained. See `summarize_curve` for why the
# per-window median is NOT the right estimator (the grouped pacer emits a whole group in a burst).
RUNG_RATE_TOLERANCE = 0.02

# Slice 39: the default `--drift-rate auto` sets the fixed drift rate to this fraction of the
# pre-drift mix's highest sustained level (measured by a short pre-flight ladder on the win mix).
DRIFT_RATE_FRACTION = 0.70

# Per-segment shape mixes (integer weights). They are the *offered* mix; every arm gets the same.
# Write share raised from 15% to 40% (Slice 37): a per-region page size only acts on tiles written
# AFTER the switch, so a read-dominated trace cannot measure it above the lower bound. The write
# segment republishes its regions under the arm's active policy and drives the checkpoint / merge
# cycle that the ``--compact-bytes`` actuation now exercises.
WIN_MIX = {"stock": 20, "customer": 10, "scan": 30, "write": 40}
UNIFORM_MIX = {"stock": 30, "customer": 30, "scan": 0, "write": 40}


# ---------------------------------------------------------------------------
# Pure parsing helpers (tested without an engine)
# ---------------------------------------------------------------------------
def _kv(line: str, key: str) -> str | None:
    m = re.search(rf"(?:^|\s){re.escape(key)}=(\S+)", line)
    return m.group(1) if m else None


def parse_engine_open(text: str) -> dict:
    """The ``engine_open`` header: target, discover flag, active tiles, memtable entries."""
    for line in text.splitlines():
        if line.startswith("engine_open "):
            return {
                "target": _kv(line, "target"),
                "discover": _kv(line, "discover"),
                "active_tiles": int(_kv(line, "active_tiles")),
                "serving_memtable_entries": int(_kv(line, "serving_memtable_entries")),
            }
    return {}


def parse_load_summary(text: str) -> dict:
    """The final ``load_summary`` line plus the device baselines, or an empty dict."""
    out: dict = {"device_fsync_p50_us": None, "device_fsync_p99_us": None,
                 "cgroup_peak_bytes": None}
    for line in text.splitlines():
        if line.startswith("device_fsync_p50_us="):
            out["device_fsync_p50_us"] = int(_kv(line, "device_fsync_p50_us"))
            out["device_fsync_p99_us"] = int(_kv(line, "device_fsync_p99_us"))
        elif line.startswith("cgroup_peak_bytes="):
            out["cgroup_peak_bytes"] = int(_kv(line, "cgroup_peak_bytes"))
        elif line.startswith("load_peak "):
            stage = _kv(line, "stage")
            if stage == "end":
                v = _kv(line, "hwm_kb")
                out["load_hwm_kb"] = None if v in (None, "none") else int(v)
        elif line.startswith("load_summary "):
            for key in ("requests", "completed", "errors", "else_used", "elapsed_ms",
                        "invalid_windows", "ceiling_rejected_windows", "late_total",
                        "compact_bytes_total", "compact_merges"):
                v = _kv(line, key)
                if v is not None:
                    out[key] = int(v)
            out["achieved_rps"] = float(_kv(line, "achieved_rps"))
            v = _kv(line, "saturated_at_rps")
            out["saturated_at_rps"] = None if v in (None, "none") else float(v)
            v = _kv(line, "last_valid_rate_rps")
            out["last_valid_rate_rps"] = None if v in (None, "none") else float(v)
            out["first_error"] = _kv(line, "first_error")
    if out.get("requests"):
        out["commit_rate"] = out.get("completed", 0) / out["requests"]
    return out


def parse_peak_hwm(time_stderr: str) -> int | None:
    """Max ``Maximum resident set size (kbytes)`` across ``/usr/bin/time -v`` blocks."""
    vals = [int(v) for v in re.findall(r"Maximum resident set size \(kbytes\): (\d+)", time_stderr)]
    return max(vals) * 1024 if vals else None


def policy_active_map(policy: dict) -> dict[str, LayoutCandidate]:
    """Region -> the active candidate from an echoed policy document (regions fall back to default)."""
    if not policy:
        return {}
    default = policy.get("default", {})
    out: dict[str, LayoutCandidate] = {}
    for region, entry in policy.get("regions", {}).items():
        merged = {**default, **entry}
        out[region] = LayoutCandidate(int(merged["page_size"]), int(merged["merge_min_tiles"]))
    return out


def window_records(lines: list[str], window_ms: int) -> list[dict]:
    """One record per metrics window: rates, achieved throughput (completed delta), regions.

    ``completed`` is cumulative in the dump, so the window's own throughput is the delta over the
    nominal window duration. Per-region ``compaction_bytes``/``compaction_merges`` are summed so a
    report can quote the reorganisation volume, and the echoed policy is kept so a live run can
    attribute each window's cost to the candidate that was active for it.
    """
    out: list[dict] = []
    prev_completed = 0
    for line in lines:
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        completed = int(obj.get("completed", 0))
        # The window's ACTUAL wall time (driver field `window_us`). Older dumps omit it, so fall
        # back to the nominal `--window-ms`; a rung's achieved rate must divide by the actual time.
        window_us = int(obj.get("window_us", 0))
        window_s = window_us / 1e6 if window_us > 0 else window_ms / 1000.0
        delta = completed - prev_completed
        regions = {}
        for r in obj.get("current", {}).get("regions", []):
            regions[r["region"]] = {
                "count": r["count"],
                "probes": r.get("probes", 0),
                "mean_ns": r.get("mean_ns", 0.0),
                "p50_us": r.get("p50_ns", 0.0) / 1000.0,
                "p99_us": r.get("p99_ns", 0.0) / 1000.0,
                "bytes_read": r.get("bytes_read", 0),
                "bytes_written": r.get("bytes_written", 0),
                "compaction_bytes": r.get("compaction_bytes", 0),
                "compaction_merges": r.get("compaction_merges", 0),
            }
        # The pacer marks a window invalid when its schedule lag ran away (saturation), its p99
        # exceeded the declared ceiling, or a request errored. `--metrics-json` must carry those
        # fields (it now does): before, only the human line had them, so a JSONL consumer read
        # `invalid` as always false and could score a saturated rung as sustained. Keep them.
        invalid = bool(obj.get("invalid", False))
        lag_p99 = int(obj.get("lag_p99_us", 0))
        out.append({
            "window": int(obj.get("window", len(out))),
            "rate_rps": float(obj.get("rate_rps", 0.0)),
            "requests": int(obj.get("requests", 0)),
            "completed": completed,
        # This window's own completions and the wall time it took; `summarize_curve` sums both per
        # offered rate and divides by the longer of the offered span and this wall time, instead of
        # taking a median of the group-quantised per-window rate.
            "completed_delta": delta,
            "window_s": window_s,
            "errors": int(obj.get("errors", 0)),
            "achieved_rps": max(0.0, delta / window_s),
            "invalid": invalid,
            "lag_p99_us": lag_p99,
            "ceiling_us": obj.get("ceiling_us"),
            "ceiling_breached": bool(obj.get("ceiling_breached", False)),
            "p50_us": int(obj.get("p50_us", 0)),
            "p99_us": int(obj.get("p99_us", 0)),
            "regions": regions,
            "policy": obj.get("policy"),
            # Slice 37 driver field: bytes merged by the `--compact-bytes` actuation call at this
            # window's boundary. The per-region `compaction_bytes` above is the same merge split
            # by region; this is the driver's own per-window total.
            "compact_bytes": int(obj.get("compact_bytes", 0)),
        })
        prev_completed = completed
    return out


def _ceiling_rejected(w: dict, ceiling_us: int | None) -> bool:
    """True when the window breached the latency ceiling: the driver's own flag, or (for windows
    built without it) a p99 above the ceiling."""
    if w.get("ceiling_breached", False):
        return True
    return ceiling_us is not None and w["p99_us"] > ceiling_us


def summarize_curve(windows: list[dict], ceiling_us: int | None = None) -> list[dict]:
    """Aggregate windows by offered rate into the capacity curve (one row per rate level).

    A level's ``achieved_rps`` is its attributed completions divided by the longer of its OFFERED
    schedule span (requests/rate) and the elapsed wall time they took, NOT the median of the
    per-window rates. The grouped pacer (``--group-requests``) issues a whole group when its FIRST
    request is due, so at a low offered rate one window carries a full 64 request burst (64 rps)
    and the next carries none; the median then reports the burst size over the window (128% of a
    50 rps offer in the published slice-39 run) instead of the offered rate. Summed over the rung,
    completions over the offered span is the offered rate the engine actually held.

    The offered span is a floor on the denominator, so a rung's achieved rate cannot exceed its
    offered rate; a value above it by more than ``RUNG_RATE_TOLERANCE`` is a measurement error and
    the level is never scored sustained (``rate_exceeded``). A level is ``sustained`` when the
    achieved rate is within ``SUSTAINED_TOLERANCE`` of the offered rate, no window had an error,
    no window was rejected by the pacer (schedule lag ran away, i.e. the engine was not keeping
    up), and no window breached the latency ceiling.
    """
    by_rate: dict[float, list[dict]] = {}
    for w in windows:
        by_rate.setdefault(w["rate_rps"], []).append(w)
    curve = []
    for rate in sorted(by_rate):
        ws = by_rate[rate]
        valid = [w for w in ws if not w.get("invalid", False)]
        # Achieved rate = completions attributed to the rung / the longer of its OFFERED schedule
        # span (requests/rate) and its actual elapsed wall time. The grouped pacer commits a whole
        # group when its first request is due, so the last members land ahead of their scheduled
        # time and the rung's wall time can be SHORTER than its offered span (which is why the old
        # per-window median reported 128%). Dividing by at least the offered span bounds achieved
        # by the offered rate; a saturated rung's wall time exceeds the offered span and shows up
        # as a lower achieved rate.
        offered_s = sum(w["requests"] for w in ws) / rate if rate else 0.0
        wall_s = sum(w.get("window_s", 0.0) for w in ws)
        span_s = max(offered_s, wall_s)
        completions = sum(w.get("completed_delta", 0) for w in ws)
        achieved = (completions / span_s) if span_s > 0 else 0.0
        p99 = max(w["p99_us"] for w in valid) if valid else max(w["p99_us"] for w in ws)
        rejected = any(w["errors"] > 0 for w in ws) or any(w.get("invalid", False) for w in ws)
        ceiling_rejected_windows = sum(1 for w in ws if _ceiling_rejected(w, ceiling_us))
        rate_exceeded = achieved > rate * (1.0 + RUNG_RATE_TOLERANCE)
        curve.append({
            "offered_rps": rate,
            "windows": len(ws),
            "achieved_rps": achieved,
            "p50_us": median(w["p50_us"] for w in ws),
            "p99_us": p99,
            "achieved_fraction": achieved / rate if rate else 0.0,
            "invalid_windows": sum(1 for w in ws if w.get("invalid", False)),
            "error_windows": sum(1 for w in ws if w["errors"] > 0),
            "ceiling_rejected_windows": ceiling_rejected_windows,
            "valid_windows": len(valid),
            "lag_p99_us": max(w.get("lag_p99_us", 0) for w in ws),
            "compact_bytes": sum(w.get("compact_bytes", 0) for w in ws),
            "rate_exceeded": rate_exceeded,
            "sustained": (achieved >= SUSTAINED_TOLERANCE * rate and not rejected
                          and ceiling_rejected_windows == 0 and not rate_exceeded),
        })
    return curve


def highest_sustained_rate(curve: list[dict]) -> float:
    """The highest offered rate the arm sustained (0.0 when none did)."""
    rates = [row["offered_rps"] for row in curve if row["sustained"]]
    return max(rates) if rates else 0.0


def ladder_top_rate(curve: list[dict]) -> float:
    """The highest offered level present in a curve (0.0 for an empty curve)."""
    return max((row["offered_rps"] for row in curve), default=0.0)


def ladder_underpowered(records: dict, workload: str,
                        top_rate: float | None = None) -> bool:
    """True when every measured arm for ``workload`` sustained the ladder's top step.

    A ladder whose top step is sustained by every arm cannot separate the arms on
    "highest sustained load level" - that metric ties by construction at the top, so a T1 line
    drawn from it says nothing about the layout. The guard flags that case so the verdict becomes
    NOT-MEASURABLE instead of a spurious tie, and the caller can raise the top step and retry.
    ``top_rate`` defaults to the highest level any arm saw; an explicit value is the ceiling the
    run claims to have reached.
    """
    arms = [r for r in records.values() if r.get("workload") == workload]
    if not arms:
        return False
    if top_rate is None:
        top_rate = max(ladder_top_rate(r.get("curve", [])) for r in arms)
    if top_rate <= 0.0:
        return False
    return all(highest_sustained_rate(r.get("curve", [])) >= top_rate - 1e-9 for r in arms)


def is_ladder(plan: WorkloadPlan) -> bool:
    """True for a stepped offered-load plan (win/adversarial); drift runs at one fixed rate."""
    return all(b > a for a, b in zip(plan.rates, plan.rates[1:]))


def extend_ladder(plan: WorkloadPlan, factor: int = 2) -> WorkloadPlan:
    """The same plan with one more offered-load step at ``factor`` x the old top.

    The mix list is extended in lockstep with the rate list: ``build_trace`` emits
    ``segment_requests`` requests per mix, so a rate without a mix would truncate the trace.
    """
    if factor < 2:
        raise ValueError("ladder extension factor must be >= 2")
    return WorkloadPlan(plan.name, plan.rates + [plan.rates[-1] * factor], plan.segment_requests,
                        plan.mixes + [dict(plan.mixes[-1])])


def region_compaction_totals(windows: list[dict]) -> dict[str, dict]:
    """Per-region summed bytes_read / bytes_written / compaction_bytes / merges over the run.

    Also accumulates the region's total **service time** (``sum(count * mean_ns)``), so the report
    can price the reward's two terms on the same run (slice 39).
    """
    totals: dict[str, dict] = {}
    for w in windows:
        for region, r in w["regions"].items():
            t = totals.setdefault(region, {"bytes_read": 0, "bytes_written": 0,
                                           "compaction_bytes": 0, "compaction_merges": 0,
                                           "count": 0, "service_ns": 0.0, "windows": 0})
            for key in ("bytes_read", "bytes_written", "compaction_bytes", "compaction_merges"):
                t[key] += r[key]
            t["count"] += r["count"]
            t["service_ns"] += r["count"] * r["mean_ns"]
            t["windows"] += 1
    return totals


def run_window_count(record: dict) -> int:
    """The number of driver windows in an arm's run.

    Sums the capacity-curve rows when present, else falls back to the largest per-region window
    count, and only then to 1, so a record with neither still yields a usable rate denominator.
    """
    total = sum(row.get("windows", 0) for row in (record.get("curve") or []))
    if total > 0:
        return total
    per_region = max((t.get("windows", 0) for t in (record.get("regions") or {}).values()),
                     default=0)
    return per_region if per_region > 0 else 1


def total_reorg_bytes(record: dict) -> int:
    """The arm's summed per-region reorganisation (compaction output) bytes over the run."""
    return sum(r["compaction_bytes"] for r in (record.get("regions") or {}).values())


def declared_reorg_rate_bytes_per_window(record: dict) -> float:
    """The declared arm's observed reorganisation rate -- the learner's budget (slice 39)."""
    return total_reorg_bytes(record) / max(1, run_window_count(record))


def reward_term_totals(record: dict, lambda_ns_per_byte: float) -> dict:
    """Price the reward's two terms over a whole arm so a reader can see which can move it.

    ``service_ns`` is ``sum(count * mean_ns)``; the write term is split into the WAL-ingest part
    (the pre-slice-39 quantity, candidate-invariant) and the reorganisation part the merge knob
    moves. The ratios are of each write sub-term to the service term at the arm's lambda: a term
    whose ratio is tiny cannot change an ``argmin`` no matter how lambda is swept.
    """
    regs = record.get("regions") or {}
    service_ns = sum(t.get("service_ns", 0.0) for t in regs.values())
    ingest_bytes = sum(t.get("bytes_written", 0) for t in regs.values())
    reorg_bytes = sum(t.get("compaction_bytes", 0) for t in regs.values())
    ingest_ns = lambda_ns_per_byte * ingest_bytes
    reorg_ns = lambda_ns_per_byte * reorg_bytes
    base = max(service_ns, 1.0)
    return {
        "service_ns": service_ns,
        "ingest_bytes": ingest_bytes,
        "reorg_bytes": reorg_bytes,
        "ingest_write_term_ns": ingest_ns,
        "reorg_write_term_ns": reorg_ns,
        "ingest_to_service_ratio": ingest_ns / base,
        "reorg_to_service_ratio": reorg_ns / base,
    }


def resolve_drift_rate(spec, *, pre_drift_highest_sustained_rps: float | None,
                       reference_rate: int, fraction: float = DRIFT_RATE_FRACTION) -> tuple[int, dict]:
    """Resolve ``--drift-rate``: an explicit int, or ``auto`` -> a fraction of the pre-drift capacity.

    ``auto`` needs the pre-drift (win) mix's highest sustained level, measured by the pre-flight
    ladder (or read from a win arm's curve when the win workload already ran). Below that level
    the fixed drift rate is sustainable, which is what the recovery threshold needs; above it
    (slice 38's fixed 150 rps) every arm saturates and the threshold is undefined. Returns
    ``(rate_rps, info)`` and the info is reported so the rule and its inputs are auditable.
    """
    info: dict = {"requested": spec, "fraction": fraction,
                  "pre_drift_highest_sustained_rps": pre_drift_highest_sustained_rps}
    if spec != "auto":
        info.update(mode="explicit", rate_rps=int(spec))
        return int(spec), info
    if pre_drift_highest_sustained_rps and pre_drift_highest_sustained_rps > 0:
        rate = max(1, int(round(pre_drift_highest_sustained_rps * fraction)))
        info.update(mode="auto_from_pre_drift_capacity", rate_rps=rate)
        return rate, info
    info.update(mode="auto_fallback_reference_rate", rate_rps=int(reference_rate))
    return int(reference_rate), info


def build_sweep(windows: list[dict], max_rate: float | None = None) -> dict:
    """Per-region per-candidate mean cost, attributed from the echoed policy of each window.

    ``max_rate`` restricts the sweep to the pre-drift (lowest offered) level so a frozen
    ``static-derived`` policy is derived from pre-drift evidence only. Cost uses
    ``lambda``=1.0 for the derivation (the same convention as ``layout_headroom``'s tie-break);
    the arm under test may run at any lambda.
    """
    sweep: dict[str, dict[str, dict]] = {}
    for w in windows:
        if max_rate is not None and w["rate_rps"] > max_rate + 1e-9:
            continue
        active = policy_active_map(w.get("policy"))
        for region, r in w["regions"].items():
            if region not in active or r["count"] <= 0:
                continue
            key = str(active[region])
            acc = sweep.setdefault(region, {}).setdefault(
                key, {"n": 0, "sum_cost": 0.0, "page_size": active[region].page_size,
                      "merge_min_tiles": active[region].merge_min_tiles})
            acc["n"] += 1
            acc["sum_cost"] += region_cost_ns_from_window(r, 1.0)
    return sweep


def region_cost_ns_from_window(r: dict, lambda_ns_per_byte: float) -> float:
    return r["count"] * r["mean_ns"] + lambda_ns_per_byte * r["bytes_written"]


def argmin_from_sweep(sweep: dict, seed: int = 0) -> dict[str, LayoutCandidate]:
    """Per-region argmin cost over the observed candidates (the frozen static-derived policy)."""
    out: dict[str, LayoutCandidate] = {}
    for region, by_candidate in sweep.items():
        best_key = min(
            by_candidate,
            key=lambda k: (by_candidate[k]["sum_cost"] / by_candidate[k]["n"], k),
        )
        acc = by_candidate[best_key]
        out[region] = LayoutCandidate(int(acc["page_size"]), int(acc["merge_min_tiles"]))
    return out


# ---------------------------------------------------------------------------
# Trace generation (deterministic; uniform draws keep the working set cold)
# ---------------------------------------------------------------------------
@dataclass
class WorkloadPlan:
    """One workload: segment count, per-segment shape weights, and the offered rate schedule."""

    name: str
    rates: list[int]
    segment_requests: int
    mixes: list[dict]

    def to_dict(self) -> dict:
        return {"name": self.name, "rates": self.rates,
                "segment_requests": self.segment_requests, "mixes": self.mixes}


def plan_workload(name: str, reference_rate: int, segment_requests: int,
                  drift_shape: str = "scan", drift_factor: int = 10,
                  drift_rate: int | None = None) -> WorkloadPlan:
    """Build the offered-load plan for one workload (``win`` / ``adversarial`` / ``drift``)."""
    if name == "win":
        return WorkloadPlan("win", [reference_rate * m for m in RATE_MULTIPLES],
                            segment_requests, [dict(WIN_MIX)] * len(RATE_MULTIPLES))
    if name == "adversarial":
        return WorkloadPlan("adversarial", [reference_rate * m for m in RATE_MULTIPLES],
                            segment_requests, [dict(UNIFORM_MIX)] * len(RATE_MULTIPLES))
    if name == "drift":
        # Fixed offered rate so "recover to within 10% of pre-drift throughput" is well defined;
        # the mix changes deterministically at the boundary after the pre-drift segments. The rate
        # is chosen to sit in the engine's sustainable band on this box (not the lowest step).
        rate = drift_rate or reference_rate
        n = 6
        pre = 2
        mixes = [dict(WIN_MIX)] * pre
        drifted = dict(WIN_MIX)
        drifted[drift_shape] = drifted[drift_shape] * drift_factor
        mixes += [drifted] * (n - pre)
        return WorkloadPlan("drift", [rate] * n, segment_requests, mixes)
    raise ValueError(f"unknown workload {name!r}")


def preflight_plan(reference_rate: int, segment_requests: int) -> WorkloadPlan:
    """The `--drift-rate auto` pre-flight: a short ladder on the PRE-DRIFT (win) mix.

    Its highest sustained level is the pre-drift capacity the drift rate is a fraction of. Only
    the declared policy is run on it (the pre-flight is a capacity probe, not a comparison).
    """
    return WorkloadPlan("preflight", [reference_rate * m for m in RATE_MULTIPLES],
                        segment_requests, [dict(WIN_MIX)] * len(RATE_MULTIPLES))


def run_preflight(base: Path, workroot: Path, bins: dict, args: argparse.Namespace,
                  cap_bytes: int, env: dict) -> dict:
    """Run the pre-flight declared arm on the win mix and return its record (Slice 39)."""
    plan = preflight_plan(args.reference_rate, args.drift_preflight_segment_requests)
    trace = workroot / "preflight.jsonl"
    build_trace(trace, plan, args)
    return run_arm("preflight", "preflight", base, workroot, bins, trace, plan,
                   policy_document({}), None, args, cap_bytes, env)


def _stock_probe(rng: Rng, gen: Generator, skus: int, locs: int, i: int) -> dict:
    where = {"sku": sku_name(1 + rng.below(skus)), "loc": loc_name(rng.below(locs))}
    return {"id": f"b0-s-{i:07d}",
            "class": {"durability": "batched", "max_staleness": 2},
            "blocks": {"q": {"get": {"Stock": {"where": where}}}}}


def _customer_probe(rng: Rng, gen: Generator, customers: int, i: int) -> dict:
    return {"id": f"b0-c-{i:07d}", "class": {"durability": "batched"},
            "blocks": {"q": {"find": {"Customer": {"email": gen.emails[rng.below(customers)]}}}}}


def _movements_scan(rng: Rng, gen: Generator, write_requests: int, i: int) -> dict:
    order_no = f"O{1 + rng.below(max(1, write_requests)):08d}"
    # The adapter's `find` carries the entity plus the sibling `where`/`order`/`limit` fields
    # (the shape `cold_axis_compare.emit_read_trace` emits); `where` lives inside the entity.
    find = {"StockMovement": {"where": {"order_no": order_no}},
            "limit": 100, "order": ["movement_no"]}
    return {"id": f"b0-m-{i:07d}",
            "class": {"durability": "batched", "max_staleness": 5},
            "blocks": {"q": {"find": find}}}


def _order_flow(gen: Generator, on_hand: int, force_else: bool) -> dict:
    req = gen.order_flow()
    if force_else:
        # Force the stock guard's `on_hand >= qty` predicate false without touching stock: the
        # else branch (backorder) then fires deterministically, so `else_used` is a real signal.
        req["params"]["qty"] = on_hand + 1
    # A non-forced write takes the `ops` path (a real StockMovement + Order put), which is what
    # republishes those regions under the arm's policy. Both branches are exercised every segment.
    return req


def build_trace(path: Path, plan: WorkloadPlan, args: argparse.Namespace) -> dict:
    """Emit the plan's trace as JSONL; return per-shape counts and the manifest description."""
    rng = Rng(args.seed)
    total = plan.segment_requests * len(plan.rates)
    gen = Generator(args.seed, total, args.customers, args.skus, args.locs)
    # The instance already committed orders O1..O<write_requests>; start above that range so the
    # run's order_flow writes are new work, not "order number already used" conflicts.
    gen.order_no = args.write_requests
    counts = {"stock": 0, "customer": 0, "scan": 0, "write": 0}
    index = 0
    with open(path, "w", encoding="utf-8", newline="\n") as out:
        for mix in plan.mixes:
            total_weight = sum(mix.values())
            for _ in range(plan.segment_requests):
                u = rng.next_u64() % total_weight
                acc = 0
                for shape in ("stock", "customer", "scan", "write"):
                    acc += mix[shape]
                    if u < acc:
                        break
                if shape == "stock":
                    req = _stock_probe(rng, gen, args.skus, args.locs, index)
                elif shape == "customer":
                    req = _customer_probe(rng, gen, args.customers, index)
                elif shape == "scan":
                    req = _movements_scan(rng, gen, args.write_requests, index)
                else:
                    req = _order_flow(gen, args.on_hand, force_else=counts["write"] % 2 == 0)
                    counts["write"] += 1
                counts[shape] += 1 if shape != "write" else 0
                out.write(next(render([req])) + "\n")
                index += 1
    return {"total": index, "counts": counts, "plan": plan.to_dict()}


# ---------------------------------------------------------------------------
# Instance handling
# ---------------------------------------------------------------------------
def dir_bytes(path: Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


def coldness_evidence(summary: dict, cap_bytes: int, data_bytes: int) -> dict:
    """Per-arm proof the run was cache-evicting: the cgroup peak and the data:cap ratio.

    The cgroup counts page cache, so a run whose peak reaches the cap has been under memory
    pressure and cannot have kept the working set resident. This matters because warm runs on
    this box measured the opposite page-size conclusion (slice 25 flat vs slice 29 +54% cold), so
    "cold" must be shown per arm, not asserted. ``cache_evicting`` is true when the observed peak
    reached at least 90% of the cap - the signal the box actually pressured memory.
    """
    peak = summary.get("cgroup_peak_bytes")
    load_hwm_kb = summary.get("load_hwm_kb")
    return {
        "cap_bytes": cap_bytes,
        "data_bytes": data_bytes,
        "data_cap_ratio": data_bytes / cap_bytes if cap_bytes else None,
        "cgroup_peak_bytes": peak,
        "load_hwm_bytes": load_hwm_kb * 1024 if load_hwm_kb else None,
        "peak_over_cap": peak / cap_bytes if peak and cap_bytes else None,
        "cache_evicting": peak is not None and cap_bytes > 0 and peak >= 0.9 * cap_bytes,
    }


def copy_instance(src: Path, dst: Path) -> None:
    if dst.exists():
        shutil.rmtree(dst)
    subprocess.run(["cp", "-a", str(src), str(dst)], check=True)


# ---------------------------------------------------------------------------
# Subprocess plumbing: the load runs inside a MemoryMax scope, streaming windows
# ---------------------------------------------------------------------------
def scope_command(cap_bytes: int, payload: str) -> list[str]:
    """``systemd-run --user --scope`` with a hard memory cap, echoing the scope's memory.peak."""
    inner = (
        "set -u\n"
        f"{payload}\n"
        "rc=$?\n"
        'cg=$(sed -n "s/^0:://p" /proc/self/cgroup 2>/dev/null)\n'
        'if [ -n "$cg" ] && [ -r "/sys/fs/cgroup$cg/memory.peak" ]; then\n'
        '  echo "cgroup_peak_bytes=$(cat "/sys/fs/cgroup$cg/memory.peak")"\n'
        "fi\n"
        "exit $rc\n"
    )
    return ["systemd-run", "--user", "--scope", "-q",
            "-p", f"MemoryMax={cap_bytes}", "-p", "MemorySwapMax=0", "bash", "-c", inner]


class LiveLoad:
    """A running ``flashdb-load`` under the cap, with an optional per-window controller."""

    def __init__(self, cmd: list[str], env: dict, controller=None, policy_path: Path | None = None,
                 stderr_path: Path | None = None, timeout_s: float = 1800.0) -> None:
        self.controller = controller
        self.policy_path = policy_path
        self.lines: list[str] = []
        self.decisions: list[dict] = []
        self.stderr_path = stderr_path or Path("/dev/null")
        self.peak_bytes: int | None = None
        self._stderr_file = open(self.stderr_path, "w", encoding="utf-8")
        self.proc = subprocess.Popen(cmd, cwd=REPO_ROOT, env=env,
                                     stdout=subprocess.PIPE, stderr=self._stderr_file,
                                     text=True, bufsize=1)
        self._timer = threading.Timer(timeout_s, self._kill)
        self._timer.daemon = True
        self._timer.start()

    def _kill(self) -> None:
        if self.proc.poll() is None:
            self.proc.kill()

    def write_policy(self, document: dict) -> None:
        """Atomically install a policy document where the driver re-reads it."""
        tmp = self.policy_path.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(document, sort_keys=True, indent=2), encoding="utf-8")
        os.replace(tmp, self.policy_path)

    def run(self) -> int:
        for line in self.proc.stdout:
            self.lines.append(line.rstrip("\n"))
            if self.controller is not None and line.startswith("{"):
                window = parse_window_line(line)
                if window is not None:
                    self.controller.observe(window)
                    self.write_policy(self.controller.policy_document())
                    self.decisions.append({"window": window.window,
                                           "selections": {k: str(v) for k, v
                                                          in self.controller.selections().items()}})
        self.proc.wait()
        self._timer.cancel()
        self._stderr_file.close()
        return self.proc.returncode


# ---------------------------------------------------------------------------
# One arm run
# ---------------------------------------------------------------------------
def flashdb_load_command(bins: dict, target: Path, trace: Path, plan: WorkloadPlan,
                         policy_path: Path, *, reload: bool, checkpoint_bytes: int,
                         rotate_bytes: int, group_requests: int, window_ms: int,
                         ceiling_us: int | None, compact_bytes: int | None) -> str:
    rates = ",".join(str(r) for r in plan.rates)
    cmd = [bins["load"], str(target), str(trace),
           "--rate-rps", rates, "--segment-requests", str(plan.segment_requests),
           "--window-ms", str(window_ms), "--metrics-json", "--peak-report",
           "--group-requests", str(group_requests),
           "--checkpoint-bytes", str(checkpoint_bytes), "--rotate-bytes", str(rotate_bytes),
           "--region-mode", "per_region",
           "--policy-file", str(policy_path)]
    if compact_bytes:
        # Slice 37: actuate the per-region merge knob once per window with the active policy.
        # 0 or None leaves the flag off (the pre-slice inert-merge path).
        cmd += ["--compact-bytes", str(compact_bytes)]
    if reload:
        cmd.append("--policy-reload")
    if ceiling_us is not None:
        cmd += ["--ceiling-us", str(ceiling_us)]
    return " ".join(cmd)


def run_arm(name: str, workload: str, base: Path, workroot: Path, bins: dict,
            trace: Path, plan: WorkloadPlan, policy: dict | None, controller,
            args: argparse.Namespace, cap_bytes: int, env: dict) -> dict:
    """Run one arm cold on a fresh copy of the base instance; return its machine-readable record."""
    engine_dir = workroot / f"{workload}-{name}"
    copy_instance(base, engine_dir)
    policy_path = workroot / f"{workload}-{name}.policy.json"
    # Install the initial policy BEFORE the driver opens: a live controller's first document (all
    # declared-default until it has seen a window) must be on disk when the driver reads it at open.
    initial = controller.policy_document() if controller is not None else (policy or {})
    policy_path.write_text(json.dumps(initial, sort_keys=True, indent=2), encoding="utf-8")
    payload = flashdb_load_command(
        bins, engine_dir, trace, plan, policy_path, reload=controller is not None,
        checkpoint_bytes=args.checkpoint_bytes, rotate_bytes=args.rotate_bytes,
        group_requests=args.group_requests, window_ms=args.window_ms, ceiling_us=args.ceiling_us,
        compact_bytes=args.compact_bytes)
    print(f"+ [{workload}/{name}] {payload}", flush=True)
    live = LiveLoad(scope_command(cap_bytes, payload), env, controller=controller,
                    policy_path=policy_path, stderr_path=workroot / f"{workload}-{name}.stderr",
                    timeout_s=args.arm_timeout_s)
    rc = live.run()
    engine_open = parse_engine_open("\n".join(live.lines))
    summary = parse_load_summary("\n".join(live.lines))
    windows = window_records(live.lines, args.window_ms)
    record = {
        "arm": name,
        "workload": workload,
        "lambda_ns_per_byte": getattr(controller, "lambda_ns_per_byte", None),
        "policy": policy if policy is not None else (controller.policy_document() if controller else None),
        "final_policy": controller.policy_document() if controller else policy,
        "rc": rc,
        "engine_open": engine_open,
        "summary": summary,
        "windows": windows,
        "curve": summarize_curve(windows, args.ceiling_us),
        "regions": region_compaction_totals(windows),
        "coldness": coldness_evidence(summary, cap_bytes, dir_bytes(base)),
        "noise": {},
        "learner_decisions": live.decisions,
        "restarts": getattr(controller, "restarts", lambda: None)() if controller else None,
        "gated_switches": getattr(controller, "gated_switches", lambda: None)() if controller else None,
        "recovery_windows": getattr(controller, "recovery_windows", lambda: None)() if controller else None,
        "budget_blocks": getattr(controller, "budget_blocks", lambda: None)() if controller else None,
        "cooldown_holds": getattr(controller, "cooldown_holds", lambda: None)() if controller else None,
        "reorg_budget_bytes_per_window": getattr(controller, "reorg_budget_bytes_per_window", None),
        "cooldown_windows": getattr(controller, "cooldown_windows", None),
    }
    arm_lambda = getattr(controller, "lambda_ns_per_byte", None)
    record["reward_terms"] = reward_term_totals(
        record, arm_lambda if arm_lambda is not None else DEFAULT_LAMBDA_NS_PER_BYTE)
    print(f"  rc={rc} achieved_rps={summary.get('achieved_rps')} "
          f"commit_rate={summary.get('commit_rate')} sustained={highest_sustained_rate(record['curve'])} "
          f"invalid={summary.get('invalid_windows')} reorg_bytes="
          f"{sum(r['compaction_bytes'] for r in record['regions'].values())} "
          f"cgroup_peak={record['coldness']['cgroup_peak_bytes']} "
          f"data:cap={record['coldness']['data_cap_ratio']:.2f}x "
          f"(cold={record['coldness']['cache_evicting']})", flush=True)
    # Keep the copy only on request: the record has everything the report needs.
    if not args.keep_engines:
        shutil.rmtree(engine_dir, ignore_errors=True)
    return record


# ---------------------------------------------------------------------------
# Threshold evaluation (pure over the arm records)
# ---------------------------------------------------------------------------
def _curve_row(record: dict, rate: float) -> dict | None:
    for row in record["curve"]:
        if abs(row["offered_rps"] - rate) < 1e-9:
            return row
    return None


def throughput_at_common_sustained(a: dict, b: dict) -> tuple[float, float] | None:
    """Achieved throughput of two arms at the highest common offered level both sustained."""
    sa = {r["offered_rps"] for r in a["curve"] if r["sustained"]}
    sb = {r["offered_rps"] for r in b["curve"] if r["sustained"]}
    common = sa & sb
    if not common:
        return None
    highest = max(common)
    ra = _curve_row(a, highest)
    rb = _curve_row(b, highest)
    if ra is None or rb is None:
        return None
    return ra["achieved_rps"], rb["achieved_rps"]


def threshold1_verdict(declared_win: dict, learned_win: dict, lam: float,
                       noise_band: float | None) -> dict:
    """Threshold 1 (win): learned beats declared by > 15% in sustained load or achieved throughput.

    Extracted from :func:`evaluate_thresholds` so a run without the win workload can still emit
    T2/T3/T4 (slice 39: the required adversarial+drift run does not include win).
    """
    decl_rate = highest_sustained_rate(declared_win["curve"])
    learn_rate = highest_sustained_rate(learned_win["curve"])
    top = max(ladder_top_rate(declared_win["curve"]), ladder_top_rate(learned_win["curve"]))
    common = throughput_at_common_sustained(learned_win, declared_win)
    both_top = top > 0.0 and decl_rate >= top - 1e-9 and learn_rate >= top - 1e-9
    if both_top:
        # Both arms cleared the ladder's top step, so the highest-sustained-level metric ties by
        # construction. A > 15% throughput win at that common level still decides (the
        # pre-registration's alternative); anything else is an under-powered ladder, not a tie.
        decl_top = _curve_row(declared_win, top)
        learn_top = _curve_row(learned_win, top)
        gain = ((learn_top["achieved_rps"] - decl_top["achieved_rps"]) / decl_top["achieved_rps"]
                if decl_top and learn_top and decl_top["achieved_rps"] else None)
        if gain is not None and gain > 0.15:
            status, detail = "PASS", (
                f"both arms sustained the top step ({top:.0f} rps); learned throughput "
                f"{learn_top['achieved_rps']:.1f} vs declared {decl_top['achieved_rps']:.1f} "
                f"({gain:+.1%} > 15%) at that common level")
        else:
            status = "NOT-MEASURABLE"
            detail = (
                f"ladder under-powered: both arms sustained the top step ({top:.0f} rps) - "
                f"declared achieved {_fmt(decl_top and decl_top['achieved_rps'], '.1f')} rps, "
                f"learned {_fmt(learn_top and learn_top['achieved_rps'], '.1f')} rps "
                f"(gain {_fmt(gain, '+.1%') if gain is not None else 'n/a'}). 'Highest sustained "
                "load level' ties by construction when every arm clears the top, so a tie is "
                "not a verdict; raise the top step until at least one arm fails.")
    elif decl_rate == 0.0 or learn_rate == 0.0:
        status, detail = "NOT-MEASURABLE", (
            f"declared sustained rate {decl_rate}, learned {learn_rate}: no sustained level "
            "to compare (the box could not complete even the reference level)")
        gain = None
    elif learn_rate > decl_rate * 1.001:
        status, detail = "PASS", (f"learned sustained {learn_rate} > declared {decl_rate} "
                                  "(at least one load level higher)")
        gain = (learn_rate - decl_rate) / decl_rate
    elif common is not None and common[0] > common[1] * 1.15:
        gain = (common[0] - common[1]) / common[1]
        status, detail = "PASS", (f"learned throughput {common[0]:.1f} vs declared {common[1]:.1f} "
                                  f"at the highest common sustained level ({gain:+.1%} > 15%)")
    else:
        gain = (common[0] - common[1]) / common[1] if common else None
        status, detail = "FAIL", (f"both sustain {decl_rate}/{learn_rate} rps; throughput gain "
                                  f"at the highest common level = "
                                  f"{gain:+.1%}" if gain is not None else "no common sustained level")
        if noise_band is not None and gain is not None and abs(gain) <= noise_band:
            status = "NOT-MEASURABLE"
            detail += f" (inside the measured noise band +/-{noise_band:.1%})"
    return {"threshold": "T1 learned > declared by 15% on win", "lambda": lam,
            "status": status, "detail": detail, "gain": gain}


def evaluate_thresholds(records: dict[str, dict], win: str = "win", adv: str = "adversarial",
                       drift: str = "drift", lambdas: tuple[float, ...] = (0.1, 1.0, 10.0),
                       noise_band: float | None = None,
                       compaction_actuated: bool = True) -> list[dict]:
    """Compute the four pre-registered comparisons for each lambda; PASS/FAIL/NOT-MEASURABLE.

    ``compaction_actuated`` names whether the run passed ``--compact-bytes``, so a 0-vs-0
    reorganisation result is reported as an actuation DEFECT when it was on and as an inert-knob
    non-result when it was off. Both are NOT-MEASURABLE; neither is a pass.
    """
    verdicts: list[dict] = []
    declared_win = records.get(f"{win}/declared")
    for lam in lambdas:
        learned_win = records.get(f"{win}/learned@{lam}")
        # T1 needs the win workload; T2/T3/T4 are computed independently so an adversarial+drift
        # run (no win) still emits them.
        if learned_win is not None and declared_win is not None:
            verdicts.append(threshold1_verdict(declared_win, learned_win, lam, noise_band))

        # Threshold 2: learned must not lose > 5% on adversarial-uniform.
        adv_decl = records.get(f"{adv}/declared")
        adv_learn = records.get(f"{adv}/learned@{lam}")
        if adv_decl is not None and adv_learn is not None:
            dc = highest_sustained_rate(adv_decl["curve"])
            lc = highest_sustained_rate(adv_learn["curve"])
            common_a = throughput_at_common_sustained(adv_learn, adv_decl)
            if common_a is not None:
                loss = (common_a[1] - common_a[0]) / common_a[1]
            else:
                loss = (dc - lc) / dc if dc else 0.0
            status = "PASS" if loss <= 0.05 else "FAIL"
            detail = (f"learned loses {loss:+.1%} vs declared on adversarial-uniform "
                      f"(declared sustained {dc}, learned {lc})")
            if noise_band is not None and abs(loss) <= noise_band and status == "FAIL":
                status = "NOT-MEASURABLE"
                detail += f" (inside the measured noise band +/-{noise_band:.1%})"
            verdicts.append({"threshold": "T2 learned loses <= 5% on adversarial-uniform",
                             "lambda": lam, "status": status, "detail": detail, "loss": loss})

        # Threshold 3: reorganisation writes <= 3x declared (win workload).
        if declared_win is not None and learned_win is not None:
            decl_reorg = sum(r["compaction_bytes"] for r in declared_win["regions"].values())
            learn_reorg = sum(r["compaction_bytes"] for r in learned_win["regions"].values())
            if decl_reorg == 0 and learn_reorg == 0:
                status = "NOT-MEASURABLE"
                if compaction_actuated:
                    detail = ("0 vs 0 compaction bytes WITH the per-window `--compact-bytes` "
                              "actuation ON: the calls produced no merge at all. That is a DEFECT in "
                              "this run (actuation did not fire, or no region was eligible), not a "
                              "pass - investigate before quoting T3")
                else:
                    detail = ("0 vs 0 compaction bytes and compaction is not actuated "
                              "(`--compact-bytes 0`): the merge threshold is inert on this path, so "
                              "the comparison says nothing (0 vs 0 is not a pass)")
            else:
                ratio = learn_reorg / decl_reorg if decl_reorg else float("inf")
                status = "PASS" if ratio <= 3.0 else "FAIL"
                detail = f"learned reorg {learn_reorg} B vs declared {decl_reorg} B (ratio {ratio:.2f})"
            verdicts.append({"threshold": "T3 reorganisation <= 3x declared", "lambda": lam,
                             "status": status, "detail": detail,
                             "declared_bytes": decl_reorg, "learned_bytes": learn_reorg})

        # Threshold 4: drift recovery within 2 windows while static-derived does not.
        drift_learn = records.get(f"{drift}/learned@{lam}")
        if drift_learn is not None:
            rec = drift_learn.get("recovery_windows") or {}
            shortest = min(rec.values()) if rec else None
            # Drift reorganisation (reported with T4; not itself a pre-registered threshold).
            drift_decl = records.get(f"{drift}/declared")
            drift_static = records.get(f"{drift}/static-derived")
            reorg_note = ""
            if drift_decl is not None:
                d_bytes = total_reorg_bytes(drift_decl)
                l_bytes = total_reorg_bytes(drift_learn)
                reorg_note = (f" drift reorg: declared {d_bytes} B, learned {l_bytes} B "
                              f"(ratio {l_bytes / d_bytes:.2f})" if d_bytes else
                              f" drift reorg: declared {d_bytes} B, learned {l_bytes} B")
                if drift_static is not None:
                    reorg_note += f", static-derived {total_reorg_bytes(drift_static)} B"
                reorg_note += "."
            if shortest is None:
                status = "NOT-MEASURABLE"
                detail = ("the learner recorded no drift-triggered re-exploration window count "
                          "(the drift segment may have been too short to move an active region's "
                          "cost past drift_margin for drift_windows).") + reorg_note
            elif shortest <= 2:
                status = "PASS"
                detail = f"learner recovered in {shortest} windows." + reorg_note
            else:
                status = "NOT MEETABLE WITH CURRENT PARAMETERS"
                detail = (f"learner recovered in {shortest} windows; slice 34's arithmetic "
                          "(drift_reexplore_candidates * min_samples = 6 with defaults) bounds "
                          "this, so the 2-window target is unreachable at the published defaults."
                          + reorg_note)
            verdicts.append({"threshold": "T4 drift recovery within 2 windows", "lambda": lam,
                             "status": status, "detail": detail,
                             "recovery_windows": rec})
    return verdicts


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--release", action="store_true")
    ap.add_argument("--instance", type=Path, default=Path("/tmp/cold33/per_region-mixed"),
                    help="cold base instance to reuse (a fresh copy is made per arm)")
    ap.add_argument("--workdir", type=Path, default=Path("/tmp/cold0b"))
    ap.add_argument("--report", type=Path, default=None)
    ap.add_argument("--json", type=Path, default=None)
    ap.add_argument("--cap-mib", type=int, default=192)
    ap.add_argument("--window-ms", type=int, default=1000)
    ap.add_argument("--group-requests", type=int, default=64)
    ap.add_argument("--checkpoint-bytes", type=int, default=16384)
    ap.add_argument("--rotate-bytes", type=int, default=16384)
    ap.add_argument("--compact-bytes", type=int, default=64 << 20,
                    help="merge budget for the per-window compaction actuation (Slice 37); "
                         "0 disables it and reproduces the pre-slice inert-merge path")
    ap.add_argument("--reference-rate", type=int, default=50)
    ap.add_argument("--drift-rate", type=str, default="auto",
                    help="fixed offered rate for the drift workload: an integer, or 'auto' to set "
                         "it to --drift-rate-fraction of the pre-drift mix's highest sustained "
                         "level (a short pre-flight ladder on the win mix; Slice 39)")
    ap.add_argument("--drift-rate-fraction", type=float, default=DRIFT_RATE_FRACTION,
                    help="fraction of the pre-drift highest sustained level used by --drift-rate auto")
    ap.add_argument("--drift-preflight-segment-requests", type=int, default=1000,
                    help="requests per level in the pre-flight ladder that --drift-rate auto runs")
    ap.add_argument("--segment-requests", type=int, default=2000)
    ap.add_argument("--ceiling-us", type=int, required=True,
                    help="latency ceiling in microseconds; REQUIRED. The pre-registration validity "
                         "rule scores a level sustained only if its p99 stays under the declared "
                         "ceiling, so a run that supplies none is not scored (fail closed) rather "
                         "than defaulting to a permissive value.")
    ap.add_argument("--arm-timeout-s", type=float, default=1200.0)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--customers", type=int, default=2_000_000)
    ap.add_argument("--skus", type=int, default=750_000)
    ap.add_argument("--locs", type=int, default=4)
    ap.add_argument("--on-hand", type=int, default=1_000_000)
    ap.add_argument("--write-requests", type=int, default=30_000)
    ap.add_argument("--lambdas", type=float, nargs="+", default=[0.1, 1.0, 10.0])
    ap.add_argument("--workloads", nargs="+", default=["win", "adversarial", "drift"])
    ap.add_argument("--arms", nargs="+", default=["declared", "learned", "static-derived", "oracle"])
    ap.add_argument("--learner-min-samples", type=int, default=3)
    ap.add_argument("--learner-seed", type=int, default=0)
    ap.add_argument("--learner-cooldown-windows", type=int, default=3,
                    help="per-region cooldown after a change: no further change for N windows "
                         "(Slice 39 churn control; 0 disables)")
    ap.add_argument("--learner-reorg-budget-factor", type=float, default=1.0,
                    help="learner's per-window reorganisation budget = declared arm's observed "
                         "reorg rate x this factor (Slice 39 churn control)")
    ap.add_argument("--noise-repeat", action="store_true",
                    help="run declared twice on win to measure the box's noise band")
    ap.add_argument("--ladder-extension-steps", type=int, default=1,
                    help="if every arm sustains the ladder's top step (under-powered ladder), raise "
                         "the top by 2x and rerun the workload, up to this many times")
    ap.add_argument("--ladder-ceiling-rps", type=float, default=None,
                    help="stop extending the ladder once its top step reaches this rate")
    ap.add_argument("--keep-engines", action="store_true")
    ap.add_argument("--quick", action="store_true", help="one workload, one arm set, tiny trace")
    a = ap.parse_args(argv)

    if a.quick:
        a.workloads = ["win"]
        a.lambdas = [1.0]
        a.reference_rate = 50
        a.segment_requests = 500

    cap_bytes = a.cap_mib << 20
    workroot = a.workdir.resolve()
    workroot.mkdir(parents=True, exist_ok=True)
    base = a.instance.resolve()
    if not base.is_dir():
        print(f"ERROR: instance {base} does not exist; build it with "
              f"flashdb_harness.cold_axis_compare first", file=sys.stderr)
        return 2
    ratio = dir_bytes(base) / cap_bytes
    print(f"instance={base} data_bytes={dir_bytes(base)} cap_bytes={cap_bytes} ratio={ratio:.2f}x",
          flush=True)
    if ratio < PREREG_RATIO_FLOOR:
        print(f"WARNING: data:cap {ratio:.2f}x is below the pre-registration {PREREG_RATIO_FLOOR}x "
              "floor; every throughput verdict below must be read with that caveat", flush=True)

    bins = build_binaries(a.release)
    env = os.environ.copy()
    records: dict[str, dict] = {}

    # Slice 39: resolve the drift rate BEFORE the drift trace is built. `auto` runs a short
    # pre-flight ladder on the PRE-DRIFT (win) mix and takes a fraction of its highest sustained
    # level; an explicit integer is used as given. The pre-flight numbers are reported.
    resolved_drift_rate: int | None = None
    drift_rate_info: dict | None = None
    if "drift" in a.workloads:
        if a.drift_rate == "auto":
            pre = run_preflight(base, workroot, bins, a, cap_bytes, env)
            pre_rate = highest_sustained_rate(pre["curve"])
            resolved_drift_rate, drift_rate_info = resolve_drift_rate(
                "auto", pre_drift_highest_sustained_rps=pre_rate,
                reference_rate=a.reference_rate, fraction=a.drift_rate_fraction)
            drift_rate_info["preflight"] = {
                "segment_requests": a.drift_preflight_segment_requests,
                "rates": [row["offered_rps"] for row in pre["curve"]],
                "achieved_rps": [row["achieved_rps"] for row in pre["curve"]],
                "sustained": [row["sustained"] for row in pre["curve"]],
                "highest_sustained_rps": pre_rate,
            }
        else:
            resolved_drift_rate, drift_rate_info = resolve_drift_rate(
                a.drift_rate, pre_drift_highest_sustained_rps=None, reference_rate=a.reference_rate)
        print(f"drift-rate resolved: {drift_rate_info}", flush=True)

    # Traces: one per workload, emitted once and shared by every arm (equal offered load).
    plans: dict[str, WorkloadPlan] = {}
    traces: dict[str, Path] = {}
    for wl in a.workloads:
        plan = plan_workload(wl, a.reference_rate, a.segment_requests, drift_rate=resolved_drift_rate)
        plans[wl] = plan
        trace = workroot / f"{wl}.jsonl"
        meta = build_trace(trace, plan, a)
        traces[wl] = trace
        print(f"trace {wl}: total={meta['total']} counts={meta['counts']} rates={plan.rates}", flush=True)

    def key(workload: str, arm: str, lam: float | None = None) -> str:
        return f"{workload}/{arm}" + (f"@{lam}" if lam is not None else "")

    def run_workload_arms(wl: str, plan: WorkloadPlan, trace: Path) -> None:
        """Run every configured arm for one workload against one plan/trace."""
        # declared
        if "declared" in a.arms:
            records[key(wl, "declared")] = run_arm(
                "declared", wl, base, workroot, bins, trace, plan,
                policy_document({}), None, a, cap_bytes, env)
        # Slice 39 churn control: the learner's per-window reorganisation budget is the declared
        # arm's OBSERVED reorg rate (bytes/window) on the same workload, x the configured factor.
        declared_rec = records.get(key(wl, "declared"))
        budget = None
        if declared_rec is not None:
            budget = declared_reorg_rate_bytes_per_window(declared_rec) * a.learner_reorg_budget_factor
            print(f"  [{wl}] learner reorg budget = {budget:.0f} B/window "
                  f"(declared rate {declared_reorg_rate_bytes_per_window(declared_rec):.0f} B/window)",
                  flush=True)
        # learned at each lambda (live UCB1)
        if "learned" in a.arms:
            for lam in a.lambdas:
                learner = LayoutLearner(lambda_ns_per_byte=lam, min_samples=a.learner_min_samples,
                                        seed=a.learner_seed,
                                        cooldown_windows=a.learner_cooldown_windows,
                                        reorg_budget_bytes_per_window=budget)
                records[key(wl, "learned", lam)] = run_arm(
                    f"learned@{lam}", wl, base, workroot, bins, trace, plan,
                    None, learner, a, cap_bytes, env)
        # static-derived: the pre-drift argmin from the learner's own burn-in sweep, frozen
        if "static-derived" in a.arms and "learned" in a.arms:
            seed_lam = a.lambdas[0]
            src = records.get(key(wl, "learned", seed_lam))
            if src is not None:
                sweep = build_sweep(src["windows"], max_rate=plan.rates[0])
                frozen = argmin_from_sweep(sweep, seed=a.learner_seed)
                static_controller = _FrozenController(frozen)
                records[key(wl, "static-derived")] = run_arm(
                    "static-derived", wl, base, workroot, bins, trace, plan,
                    None, static_controller, a, cap_bytes, env)
                print(f"  static-derived policy={ {k: str(v) for k, v in frozen.items()} }", flush=True)
        # oracle: the winsome measured policy for this workload (reported as relative regret)
        if "oracle" in a.arms:
            records[key(wl, "oracle")] = run_arm(
                "oracle", wl, base, workroot, bins, trace, plan,
                _oracle_policy(records, wl, a), None, a, cap_bytes, env)

    for wl in a.workloads:
        plan = plans[wl]
        trace = traces[wl]
        run_workload_arms(wl, plan, trace)
        # Ladder under-power guard: when every arm clears the top step the level metric ties by
        # construction, so raise the top (and rerun the whole workload) until an arm fails or the
        # stated ceiling is reached. Bounded by `--ladder-extension-steps`.
        extensions = 0
        while (extensions < a.ladder_extension_steps and is_ladder(plan)
               and ladder_underpowered(records, wl)
               and (a.ladder_ceiling_rps is None or plan.rates[-1] < a.ladder_ceiling_rps)):
            plan = extend_ladder(plan)
            plans[wl] = plan
            trace = workroot / f"{wl}.jsonl"
            meta = build_trace(trace, plan, a)
            traces[wl] = trace
            extensions += 1
            print(f"LADDER under-powered for {wl}: raising the top step to {plan.rates[-1]} rps "
                  f"(extension {extensions}/{a.ladder_extension_steps}); trace total={meta['total']}",
                  flush=True)
            run_workload_arms(wl, plan, trace)
        if extensions >= a.ladder_extension_steps and is_ladder(plan) and ladder_underpowered(records, wl):
            print(f"LADDER still under-powered for {wl} at {plan.rates[-1]} rps after "
                  f"{extensions} extension(s): T1 will report NOT-MEASURABLE", flush=True)

    # Noise band: same arm, same trace, run twice.
    noise_band = None
    if a.noise_repeat and "win" in a.workloads:
        first = records.get(key("win", "declared"))
        second = run_arm("declared-repeat", "win", base, workroot, bins, traces["win"], plans["win"],
                         policy_document({}), None, a, cap_bytes, env)
        records[key("win", "declared-repeat")] = second
        if first is not None:
            a_rates = {r["offered_rps"]: r["achieved_rps"] for r in first["curve"]}
            b_rates = {r["offered_rps"]: r["achieved_rps"] for r in second["curve"]}
            diffs = [abs(b_rates[r] - a_rates[r]) / a_rates[r] for r in a_rates if r in b_rates and a_rates[r]]
            noise_band = max(diffs) if diffs else None
            first["noise"] = {"band_relative_max": noise_band}
            print(f"noise band (declared repeat, max relative diff) = "
                  f"{noise_band:.3%}" if noise_band is not None else "noise band not measured", flush=True)

    for wl in a.workloads:
        for rec in records.values():
            if rec["workload"] == wl:
                rec.setdefault("noise", {})
    verdicts = evaluate_thresholds(records, lambdas=tuple(a.lambdas), noise_band=noise_band,
                                   compaction_actuated=a.compact_bytes > 0)

    result = {
        "schema": "flashdb.cold-0b.v1",
        "instance": str(base),
        "data_bytes": dir_bytes(base),
        "cap_bytes": cap_bytes,
        "ratio": ratio,
        "prereg_ratio_floor": PREREG_RATIO_FLOOR,
        "ratio_below_floor": ratio < PREREG_RATIO_FLOOR,
        "reference_rate_rps": a.reference_rate,
        "rate_multiples": list(RATE_MULTIPLES),
        "drift_rate": resolved_drift_rate,
        "drift_rate_rule": drift_rate_info,
        "learner_cooldown_windows": a.learner_cooldown_windows,
        "learner_reorg_budget_factor": a.learner_reorg_budget_factor,
        "mix": {"win": dict(WIN_MIX), "adversarial": dict(UNIFORM_MIX)},
        "compact_bytes": a.compact_bytes,
        "ceiling_us": a.ceiling_us,
        "lambdas": a.lambdas,
        "noise_band_relative": noise_band,
        "workloads": {wl: plans[wl].to_dict() for wl in a.workloads},
        "arms": {k: _slim_record(v) for k, v in records.items()},
        "thresholds": verdicts,
        "ladder_underpowered": {wl: ladder_underpowered(records, wl) for wl in a.workloads},
        "not_concluded": [
            "data:cap is %.2fx, below the 10x pre-registration floor" % ratio,
            "compaction is actuated once per metrics window with a %d-byte merge budget, so the "
            "learner's merge_min_tiles knob is live; a region whose threshold exceeds its tile "
            "count still never merges (a true 0), so the reorganisation comparison is bounded by "
            "the observed tile counts, not by the knob's full range" % a.compact_bytes,
            "per-region page size acts only on tiles written after the switch; the write share was "
            "raised to %d%% so compaction and the write segment republish regions under the arm's "
            "active policy, but a policy switched late still acts only on what is written after it"
            % WIN_MIX["write"],
            "the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; "
            "no simulated verdict is quoted",
            "the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)",
            "the churn budget is a per-region projection (challenger's observed reorganisation "
            "rate minus the incumbent's); the engine chooses which region compacts globally, so "
            "deferral in one region can funnel merges into an eager neighbour (slice 37/38), "
            "which a per-region reward cannot see. The drift reorganisation result is therefore a "
            "bound under this control, not a proof that the learner is reorganisation-optimal",
        ],
    }
    if "drift" in a.workloads and resolved_drift_rate is not None:
        result["not_concluded"].append(
            "the drift rate is a stated fraction (%.0f%%) of the pre-drift mix's highest "
            "sustained level measured by the pre-flight; it is not the win mix's true capacity "
            "and the fraction is an author-owned choice, not a measurement" % (a.drift_rate_fraction * 100))
    if a.ladder_extension_steps <= 0:
        result["not_concluded"].append(
            "the ladder under-power guard detected and reported but was not allowed to extend "
            "(--ladder-extension-steps 0), so a fully-sustained ladder is NOT-MEASURABLE")
    if a.json:
        a.json.parent.mkdir(parents=True, exist_ok=True)
        a.json.write_text(json.dumps(result, indent=2, default=str), encoding="utf-8")
    if a.report:
        a.report.parent.mkdir(parents=True, exist_ok=True)
        a.report.write_text(render_report(result), encoding="utf-8")
    print("\n" + render_report(result), flush=True)
    return 0


class _FrozenController:
    """``flashdb-static-derived`` as a Controller: one frozen document, never adapts."""

    def __init__(self, selections: dict[str, LayoutCandidate], candidate: LayoutCandidate = DECLARED_DEFAULT):
        self._selections = dict(selections)
        self._candidate = candidate
        self.lambda_ns_per_byte = None

    def observe(self, window) -> None:
        return

    def selections(self) -> dict[str, LayoutCandidate]:
        return dict(self._selections)

    def policy_document(self) -> dict:
        return policy_document(self._selections, default=self._candidate)


def _oracle_policy(records: dict, workload: str, args: argparse.Namespace) -> dict:
    """The relaxed oracle: the per-region argmin over the candidates measured so far.

    Prefers a record that actually visited more than one candidate (a live ``learned`` run), so the
    sweep has something to take the argmin over; with no such evidence it falls back to the
    declared default everywhere. This is the hindsight best over the *measured* candidate set (not
    a per-tile prescient oracle), reported as relative regret.
    """
    best: dict[str, LayoutCandidate] = {}
    best_candidates = 0
    for rec in records.values():
        if rec["workload"] != workload:
            continue
        sweep = build_sweep(rec["windows"])
        picks = argmin_from_sweep(sweep, seed=args.learner_seed)
        if len(picks) > best_candidates:
            best, best_candidates = picks, len(picks)
    return policy_document(best) if best else policy_document({})


def _slim_record(rec: dict) -> dict:
    """The record without the per-window firehose (the curves and totals already summarise them)."""
    out = dict(rec)
    out["window_count"] = len(rec.pop("windows", []))
    return out


def build_binaries(release: bool) -> dict:
    profile = ["--release"] if release else []
    proc = subprocess.run(["cargo", "build", "--offline", *profile,
                           "--bin", "flashdb-scale", "--bin", "flashdb-load",
                           "--bin", "flashdb-replay"], cwd=REPO_ROOT,
                          capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(proc.stderr[-4000:])
    d = REPO_ROOT / "target" / ("release" if release else "debug")
    return {"scale": str(d / "flashdb-scale"), "load": str(d / "flashdb-load"),
            "replay": str(d / "flashdb-replay")}


# ---------------------------------------------------------------------------
# Markdown report
# ---------------------------------------------------------------------------
def _fmt(v, spec=""):
    return "n/a" if v is None else (f"{v:{spec}}" if spec else str(v))


def render_report(result: dict) -> str:
    """Markdown report: per-arm capacity curves, telemetry, and the threshold table."""
    L: list[str] = []
    w = L.append
    w("# 0b - the cold arms at equal offered load (Slices 35-39: compaction actuated, unit-anchored "
      "reward, churn control)")
    w("")
    w(f"Instance `{result['instance']}`: data {result['data_bytes']:,} B, cgroup cap "
      f"{result['cap_bytes']:,} B, **ratio {result['ratio']:.2f}x**")
    if result["ratio_below_floor"]:
        w("")
        w(f"> **WARNING: data:cap {result['ratio']:.2f}x is below the pre-registration "
          f"{result['prereg_ratio_floor']:.0f}x floor.** The instance was reused from slices 29/30 "
          "rather than rebuilt larger; every throughput verdict is qualified by this.")
    w("")
    w("Objective (objective-v1): score = sustained throughput at the offered load, latency a "
      "constraint, efficiency reported. Offered levels are the reference rate x "
      f"{result['rate_multiples']}. `lambda` is the endurance weight in the learner's reward proxy "
      "(a policy choice, not a measurement); it is anchored to the device write cost in slice 39 "
      "and every verdict names it.")
    mix = result.get("mix") or {}
    if mix:
        w("")
        rendered = "; ".join(f"`{wl}` `{dict(sorted(m.items()))}`"
                            for wl, m in sorted(mix.items()))
        w("**Offered mix** (integer weights; every arm gets the same trace): " + rendered)
    comp = result.get("compact_bytes")
    if comp is not None:
        w("")
        note = (" (so the learner's `merge_min_tiles` governs)." if comp
                else " - disabled (`--compact-bytes 0`), the pre-slice inert path.")
        w("**Compaction actuation (Slice 37):** the driver calls the engine's existing "
          f"`maybe_compact_shape` once per metrics window with a `{comp:,}`-byte merge budget and "
          "the active per-region policy" + note)
    if result.get("noise_band_relative") is not None:
        w("")
        w(f"**Measured noise band** (declared repeated at the same offered load, max relative "
          f"difference in achieved throughput): +/-{result['noise_band_relative']:.1%}.")
    elif result.get("noise_band_relative") is None:
        w("")
        w("**Noise band: not measured this run** (the `--noise-repeat` control did not run); "
          "differences smaller than ~10-20% (the band slices 12/29 measured on this shared box) "
          "must be treated as noise-ambiguous.")
    w("")
    under = result.get("ladder_underpowered") or {}
    if under:
        w("**Ladder under-power guard** (per workload): " + "; ".join(
            f"`{wl}` " + ("UNDER-POWERED - T1 NOT-MEASURABLE" if flag
                          else "bracketed (at least one arm failed the top step)")
            for wl, flag in sorted(under.items())) + ".")
        if any(under.values()):
            w("")
            w("When every arm sustains the ladder's top step the \"highest sustained load level\" "
              "metric ties by construction, so T1 is reported NOT-MEASURABLE rather than as a tie.")
    # Slice 39: the reward unit derivation and the measured term magnitudes per arm.
    w("")
    w("**Reward units (Slice 39).** The learner's cost per region-window is "
      "`count*mean_ns + lambda*(bytes_written + compaction_bytes)`, both terms in ns of service "
      "time. `lambda` is anchored to the measured device write bandwidth 831 MiB/s: "
      "1 byte = 1e9/(831*2**20) ~= 1.148 ns, so lambda=1 prices a byte at exactly its device write "
      "time and lambda is the explicit endurance weight on top. The write term prices the "
      "reorganisation output the merge knob moves, not only the (candidate-invariant) WAL ingest.")
    term_rows = [(key, rec["reward_terms"]) for key, rec in sorted(result["arms"].items())
                 if rec.get("reward_terms")]
    if term_rows:
        w("")
        w("| arm | service ns | ingest write term ns | reorg write term ns | ingest/service | "
          "reorg/service |")
        w("|---|---:|---:|---:|---:|---:|")
        for key, rt in term_rows:
            w(f"| {key} | {rt['service_ns']:.4g} | {rt['ingest_write_term_ns']:.4g} | "
              f"{rt['reorg_write_term_ns']:.4g} | {rt['ingest_to_service_ratio']:.3%} | "
              f"{rt['reorg_to_service_ratio']:.3%} |")
        w("")
        w("A write term whose ratio to the service term is a tiny fraction of a percent cannot "
          "move any candidate's `argmin` however lambda is swept; that is the pre-slice-39 "
          "ingest-only term (slice 37 measured lambda 0.1 -> 10 moving the win reorg output only "
          "4.13x -> 4.27x). The reorganisation term is the one that can move the merge choice.")
    rule = result.get("drift_rate_rule")
    if rule is not None:
        w("")
        w(f"**Drift rate (Slice 39):** resolved to **{result.get('drift_rate')} rps** by rule "
          f"`{rule.get('mode')}`. Rule: the fixed drift rate is `--drift-rate-fraction` = "
          f"{rule.get('fraction'):.0%} of the PRE-DRIFT (win) mix's highest sustained level, "
          f"measured by a short pre-flight ladder; pre-flight highest sustained = "
          f"{_fmt(rule.get('pre_drift_highest_sustained_rps'))} rps.")
        pf = rule.get("preflight")
        if pf:
            w("")
            w(f"Pre-flight ladder (declared policy, win mix, {pf['segment_requests']} requests/level): "
              + "; ".join(f"{int(r)} rps -> achieved {a:.1f} ({'sustained' if s else 'failed/saturated'})"
                          for r, a, s in zip(pf["rates"], pf["achieved_rps"], pf["sustained"])) + ".")
    if result.get("learner_cooldown_windows") is not None:
        w("")
        w(f"**Learner churn control (Slice 39):** per-region cooldown "
          f"{result['learner_cooldown_windows']} windows after a change; per-window reorganisation "
          f"budget = the declared arm's observed rate x {result['learner_reorg_budget_factor']} "
          "(a region is refused a switch whose projected added reorganisation rate exceeds the "
          "remaining budget). An unexplored challenger projects 0 added bytes, so the bounded "
          "burn-in is never blocked.")
        churn_rows = [(key, rec) for key, rec in sorted(result["arms"].items())
                      if rec.get("restarts") is not None]
        if churn_rows:
            w("")
            w("| arm | restarts | gated switches | budget blocks | cooldown holds | reorg bytes |")
            w("|---|---:|---:|---:|---:|---:|")
            for key, rec in churn_rows:
                reorg = sum(r["compaction_bytes"] for r in (rec.get("regions") or {}).values())
                w(f"| {key} | {sum(rec['restarts'].values())} | "
                  f"{sum((rec.get('gated_switches') or {}).values())} | "
                  f"{sum((rec.get('budget_blocks') or {}).values())} | "
                  f"{sum((rec.get('cooldown_holds') or {}).values())} | {reorg:,} |")
    w("")
    w("## Per-arm capacity curve")
    w("")
    w("| workload | arm | lambda | offered | achieved rps | achieved % | p50 us | p99 us | "
      "sustained | commit | else | active tiles | reorg bytes |")
    w("|---|---|---|---:|---:|---:|---:|---:|:--:|---:|---:|---:|---:|")
    for key, rec in sorted(result["arms"].items()):
        summary = rec.get("summary", {})
        ti = rec.get("engine_open", {}) or {}
        reorg = sum(r["compaction_bytes"] for r in (rec.get("regions") or {}).values())
        rows = rec.get("curve") or []
        if not rows:
            w(f"| {rec['workload']} | {rec['arm']} | {_fmt(rec.get('lambda_ns_per_byte'))} | - | - | "
              f"- | - | - | - | {_fmt(summary.get('commit_rate'), '.3f')} | "
              f"{_fmt(summary.get('else_used'))} | {_fmt(ti.get('active_tiles'))} | {reorg} |")
        for row in rows:
            w(f"| {rec['workload']} | {rec['arm']} | {_fmt(rec.get('lambda_ns_per_byte'))} | "
              f"{row['offered_rps']:.0f} | {row['achieved_rps']:.1f} | {row['achieved_fraction']:.0%} | "
              f"{row['p50_us']} | {row['p99_us']} | {'yes' if row['sustained'] else 'no'} | "
              f"{_fmt(summary.get('commit_rate'), '.3f')} | {_fmt(summary.get('else_used'))} | "
              f"{_fmt(ti.get('active_tiles'))} | {reorg} |")
    w("")
    w("`commit` is completed/requests (1.0 = every offered request committed); `else` is the stock "
      "guard's else/backorder branch count; `reorg bytes` is the summed per-region "
      "`compaction_bytes` over the run. `achieved rps` is the rung's attributed completions over "
      "the longer of its offered schedule span and its elapsed wall time (not a median of the "
      "group-quantised per-window rate), so it cannot exceed the offered rate; a row is "
      f"`sustained` when it is within "
      f"{1 - SUSTAINED_TOLERANCE:.0%} of the offered rate with no error, no window the pacer "
      "rejected (schedule lag ran away), and no window over the latency ceiling.")
    ceiling_us = result.get("ceiling_us")
    if ceiling_us is not None:
        per_arm = {key: sum(r.get("ceiling_rejected_windows", 0) for r in (rec.get("curve") or []))
                   for key, rec in sorted(result["arms"].items())}
        total = sum(per_arm.values())
        detail = ", ".join(f"`{k}` {v}" for k, v in per_arm.items() if v) or "none"
        w("")
        w(f"**Latency ceiling:** `--ceiling-us {ceiling_us}` per window. A window whose p99 exceeds "
          f"it is rejected and its rung cannot be scored sustained. Windows the ceiling rejected "
          f"this run: **{total}** ({detail}). A run without a ceiling is not scored.")
    w("")
    w("## Compaction actuation (`compact_bytes`, driver field summed per offered level)")
    w("")
    w("| workload | arm | offered | compact_bytes |")
    w("|---|---|---:|---:|")
    for key, rec in sorted(result["arms"].items()):
        for row in rec.get("curve") or []:
            w(f"| {rec['workload']} | {rec['arm']} | {row['offered_rps']:.0f} | "
              f"{row.get('compact_bytes', 0):,} |")
    w("")
    w("`compact_bytes` is the bytes merged by the actuation call at each window boundary, summed "
      "over the windows of that offered level; the same merges are split by region in the telemetry "
      "section below.")
    w("")
    w("## Coldness evidence (per arm)")
    w("")
    w("The cgroup counts page cache, so a run whose peak reached its cap has been under memory "
      "pressure and evicted the working set. It is reported per arm because warm runs on this box "
      "reversed the page-size conclusion (slice 25 flat vs slice 29 +54% cold). `data:cap` is the "
      "instance's whole-data ratio to the cap; `cache-evicting` is the 90%-of-cap memory-pressure "
      "signal.")
    w("")
    w("| workload | arm | cgroup peak B | cap B | peak/cap | data B | data:cap | load hwm B | "
      "cache-evicting |")
    w("|---|---|---:|---:|---:|---:|---:|---:|:--:|")
    for key, rec in sorted(result["arms"].items()):
        c = rec.get("coldness") or {}
        w(f"| {rec['workload']} | {rec['arm']} | {_fmt(c.get('cgroup_peak_bytes'))} | "
          f"{_fmt(c.get('cap_bytes'))} | {_fmt(c.get('peak_over_cap'), '.3f')} | "
          f"{_fmt(c.get('data_bytes'))} | {_fmt(c.get('data_cap_ratio'), '.2f')}x | "
          f"{_fmt(c.get('load_hwm_bytes'))} | {'yes' if c.get('cache_evicting') else 'no'} |")
    w("")
    w("## Per-region telemetry (summed over the run)")
    w("")
    for key, rec in sorted(result["arms"].items()):
        regs = rec.get("regions") or {}
        if not regs:
            continue
        w(f"### {key}")
        w("")
        w("| region | windows | bytes_read | bytes_written | compaction_bytes | merges | "
          "rewrite factor |")
        w("|---|---:|---:|---:|---:|---:|---:|")
        for region, t in sorted(regs.items()):
            rw = t["compaction_bytes"] / t["bytes_written"] if t["bytes_written"] else None
            w(f"| {region} | {t['windows']} | {t['bytes_read']:,} | {t['bytes_written']:,} | "
              f"{t['compaction_bytes']:,} | {t['compaction_merges']} | {_fmt(rw, '.4f')} |")
        w("")
    w("## Learner decisions and drift")
    w("")
    for key, rec in sorted(result["arms"].items()):
        if rec.get("recovery_windows") is not None or rec.get("learner_decisions"):
            w(f"### {key}")
            w("")
            w(f"- gated switches: {rec.get('gated_switches')}")
            w(f"- drift restarts: {rec.get('restarts')}")
            w(f"- recovery windows: {rec.get('recovery_windows')}")
            dec = rec.get("learner_decisions") or []
            if dec:
                first = dec[0].get("selections")
                last = dec[-1].get("selections")
                w(f"- first policy: {first}")
                # The learner's decisions OVER TIME: only the windows where the per-region
                # selection actually changed, so the adaptation is visible without 80 rows.
                changes = []
                prev = None
                for d in dec:
                    sel = d.get("selections")
                    if sel != prev:
                        changes.append(f"w{d.get('window')}: {sel}")
                        prev = sel
                w(f"- policy changes ({len(changes)}): {changes}")
                w(f"- final policy: {last}")
            w("")
    w("## Pre-registered thresholds")
    w("")
    w("| threshold | lambda | status | detail |")
    w("|---|---:|:--:|---|")
    for v in result["thresholds"]:
        w(f"| {v['threshold']} | {v['lambda']} | **{v['status']}** | {v['detail']} |")
    w("")
    w("## What is NOT concluded")
    w("")
    for line in result["not_concluded"]:
        w(f"- {line}")
    w("")
    w("## Verbatim project verification")
    w("")
    w("See the slice commit message / task report for: `cargo test --workspace --offline` "
      "(`test result:` lines), `cargo build --workspace --offline` (zero warnings), "
      "`uv run --with pytest pytest -q` in `harness/`, and `git diff --check`.")
    return "\n".join(L) + "\n"


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
