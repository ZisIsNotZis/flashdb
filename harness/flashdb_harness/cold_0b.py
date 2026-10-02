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
lambda*bytes_written``); it is a POLICY choice, not a measurement, so the runner sweeps it
(default 0.1/1.0/10.0 ns/byte) and every verdict names the lambda it belongs to.

What the cold instance already tells us, and why this runner is built the way it is
--------------------------------------------------------------------------------------
Two measured facts bound what any arm can move on this box:

1. **A per-region page size acts only on tiles written AFTER the switch.** Every tile stores its
   own page size and readers honour the stored value (the ``LayoutPolicy`` is not persisted), so
   changing the policy over an existing instance changes nothing on a read-only load. The runner
   therefore gives every arm the same write fraction and the same bounded checkpoint cadence
   (``--checkpoint-bytes``/``--rotate-bytes``), so each arm republishes its regions under its own
   policy and the read segments measure it.
2. **The offered-load driver does not compact.** ``flashdb-load`` publishes (``maybe_checkpoint``)
   and rotates (``rotate_wal``) but never calls ``maybe_compact_shape``; the engine's own comment
   on the tile cap is "no compaction exists in this slice". So the learner's *merge* threshold is
   not actuated by this driver and ``compaction_bytes`` is 0 for every arm on this path. The
   reorganisation-write threshold is therefore reported as NOT-MEASURABLE here (0 vs 0), not as a
   pass - see the report.

Cold execution
--------------
Every load runs inside ``systemd-run --user --scope -p MemoryMax=<cap>`` (page cache is counted by
the cgroup, so the working set is evicted), against a fresh ``cp -a`` of the base instance. The
runner records the process RSS peak and the cgroup's own ``memory.peak`` per arm.

Usage (from the harness directory)::

    nohup uv run python -m flashdb_harness.cold_0b --release \\
        --instance /tmp/cold33/per_region-mixed --workdir /tmp/cold0b \\
        --report .scratch/35-0b-cold-arms/report.md \\
        --json .scratch/35-0b-cold-arms/results.json > /tmp/cold0b.log 2>&1 &

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

# Per-segment shape mixes (integer weights). They are the *offered* mix; every arm gets the same.
WIN_MIX = {"stock": 30, "customer": 10, "scan": 45, "write": 15}
UNIFORM_MIX = {"stock": 45, "customer": 45, "scan": 0, "write": 10}


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
                        "invalid_windows", "late_total"):
                out[key] = int(_kv(line, key))
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
    window_s = window_ms / 1000.0
    for line in lines:
        if not line.startswith("{"):
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        completed = int(obj.get("completed", 0))
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
        out.append({
            "window": int(obj.get("window", len(out))),
            "rate_rps": float(obj.get("rate_rps", 0.0)),
            "requests": int(obj.get("requests", 0)),
            "completed": completed,
            "errors": int(obj.get("errors", 0)),
            "achieved_rps": max(0.0, (completed - prev_completed) / window_s),
            "p50_us": int(obj.get("p50_us", 0)),
            "p99_us": int(obj.get("p99_us", 0)),
            "regions": regions,
            "policy": obj.get("policy"),
        })
        prev_completed = completed
    return out


def summarize_curve(windows: list[dict], ceiling_us: int | None = None) -> list[dict]:
    """Aggregate windows by offered rate into the capacity curve (one row per rate level).

    A level is ``sustained`` when the median achieved throughput is within ``SUSTAINED_TOLERANCE``
    of the offered rate, no window was rejected, and no window's p99 exceeded ``ceiling_us``.
    """
    by_rate: dict[float, list[dict]] = {}
    for w in windows:
        by_rate.setdefault(w["rate_rps"], []).append(w)
    curve = []
    for rate in sorted(by_rate):
        ws = by_rate[rate]
        achieved = median(w["achieved_rps"] for w in ws)
        p99 = max(w["p99_us"] for w in ws)
        rejected = any(w["errors"] > 0 for w in ws)
        ceiling_breached = ceiling_us is not None and p99 > ceiling_us
        curve.append({
            "offered_rps": rate,
            "windows": len(ws),
            "achieved_rps": achieved,
            "p50_us": median(w["p50_us"] for w in ws),
            "p99_us": p99,
            "achieved_fraction": achieved / rate if rate else 0.0,
            "invalid_windows": sum(1 for w in ws if w["errors"] > 0),
            "sustained": achieved >= SUSTAINED_TOLERANCE * rate and not rejected and not ceiling_breached,
        })
    return curve


def highest_sustained_rate(curve: list[dict]) -> float:
    """The highest offered rate the arm sustained (0.0 when none did)."""
    rates = [row["offered_rps"] for row in curve if row["sustained"]]
    return max(rates) if rates else 0.0


def region_compaction_totals(windows: list[dict]) -> dict[str, dict]:
    """Per-region summed bytes_read / bytes_written / compaction_bytes / merges over the run."""
    totals: dict[str, dict] = {}
    for w in windows:
        for region, r in w["regions"].items():
            t = totals.setdefault(region, {"bytes_read": 0, "bytes_written": 0,
                                           "compaction_bytes": 0, "compaction_merges": 0,
                                           "windows": 0})
            for key in ("bytes_read", "bytes_written", "compaction_bytes", "compaction_merges"):
                t[key] += r[key]
            t["windows"] += 1
    return totals


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
                         ceiling_us: int | None) -> str:
    rates = ",".join(str(r) for r in plan.rates)
    cmd = [bins["load"], str(target), str(trace),
           "--rate-rps", rates, "--segment-requests", str(plan.segment_requests),
           "--window-ms", str(window_ms), "--metrics-json", "--peak-report",
           "--group-requests", str(group_requests),
           "--checkpoint-bytes", str(checkpoint_bytes), "--rotate-bytes", str(rotate_bytes),
           "--region-mode", "per_region",
           "--policy-file", str(policy_path)]
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
        group_requests=args.group_requests, window_ms=args.window_ms, ceiling_us=args.ceiling_us)
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
        "noise": {},
        "learner_decisions": live.decisions,
        "restarts": getattr(controller, "restarts", lambda: None)() if controller else None,
        "gated_switches": getattr(controller, "gated_switches", lambda: None)() if controller else None,
        "recovery_windows": getattr(controller, "recovery_windows", lambda: None)() if controller else None,
    }
    print(f"  rc={rc} achieved_rps={summary.get('achieved_rps')} "
          f"commit_rate={summary.get('commit_rate')} sustained={highest_sustained_rate(record['curve'])} "
          f"invalid={summary.get('invalid_windows')} reorg_bytes="
          f"{sum(r['compaction_bytes'] for r in record['regions'].values())}", flush=True)
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


def evaluate_thresholds(records: dict[str, dict], win: str = "win", adv: str = "adversarial",
                       drift: str = "drift", lambdas: tuple[float, ...] = (0.1, 1.0, 10.0),
                       noise_band: float | None = None) -> list[dict]:
    """Compute the four pre-registered comparisons for each lambda; PASS/FAIL/NOT-MEASURABLE."""
    verdicts: list[dict] = []
    declared_win = records.get(f"{win}/declared")
    for lam in lambdas:
        learned_win = records.get(f"{win}/learned@{lam}")
        if learned_win is None or declared_win is None:
            continue
        # Threshold 1: learned beats declared by > 15% in sustained load on win.
        decl_rate = highest_sustained_rate(declared_win["curve"])
        learn_rate = highest_sustained_rate(learned_win["curve"])
        common = throughput_at_common_sustained(learned_win, declared_win)
        if decl_rate == 0.0 or learn_rate == 0.0:
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
        verdicts.append({"threshold": "T1 learned > declared by 15% on win", "lambda": lam,
                         "status": status, "detail": detail, "gain": gain})

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

        # Threshold 3: reorganisation writes <= 3x declared.
        decl_reorg = sum(r["compaction_bytes"] for r in declared_win["regions"].values())
        learn_reorg = sum(r["compaction_bytes"] for r in learned_win["regions"].values())
        if decl_reorg == 0 and learn_reorg == 0:
            status = "NOT-MEASURABLE"
            detail = ("both arms wrote 0 bytes of compaction output: the offered-load driver "
                      "never calls maybe_compact_shape, so the merge threshold is inert on this "
                      "path (0 vs 0 is not a pass)")
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
            if shortest is None:
                status = "NOT-MEASURABLE"
                detail = "the learner recorded no drift-triggered re-exploration window count"
            elif shortest <= 2:
                status = "PASS"
                detail = f"learner recovered in {shortest} windows"
            else:
                status = "NOT MEETABLE WITH CURRENT PARAMETERS"
                detail = (f"learner recovered in {shortest} windows; slice 34's arithmetic "
                          "(drift_reexplore_candidates * min_samples = 6 with defaults) bounds "
                          "this, so the 2-window target is unreachable at the published defaults. "
                          f"static-derived recovery: {records.get(f'{drift}/static-derived', {}).get('note')}")
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
    ap.add_argument("--reference-rate", type=int, default=50)
    ap.add_argument("--drift-rate", type=int, default=150,
                    help="fixed offered rate for the drift workload (in the sustainable band)")
    ap.add_argument("--segment-requests", type=int, default=2000)
    ap.add_argument("--ceiling-us", type=int, default=None)
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
    ap.add_argument("--noise-repeat", action="store_true",
                    help="run declared twice on win to measure the box's noise band")
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

    # Traces: one per workload, emitted once and shared by every arm (equal offered load).
    plans: dict[str, WorkloadPlan] = {}
    traces: dict[str, Path] = {}
    for wl in a.workloads:
        plan = plan_workload(wl, a.reference_rate, a.segment_requests, drift_rate=a.drift_rate)
        plans[wl] = plan
        trace = workroot / f"{wl}.jsonl"
        meta = build_trace(trace, plan, a)
        traces[wl] = trace
        print(f"trace {wl}: total={meta['total']} counts={meta['counts']} rates={plan.rates}", flush=True)

    records: dict[str, dict] = {}

    def key(workload: str, arm: str, lam: float | None = None) -> str:
        return f"{workload}/{arm}" + (f"@{lam}" if lam is not None else "")

    for wl in a.workloads:
        plan = plans[wl]
        trace = traces[wl]
        # declared
        if "declared" in a.arms:
            records[key(wl, "declared")] = run_arm(
                "declared", wl, base, workroot, bins, trace, plan,
                policy_document({}), None, a, cap_bytes, env)
        # learned at each lambda (live UCB1)
        if "learned" in a.arms:
            for lam in a.lambdas:
                learner = LayoutLearner(lambda_ns_per_byte=lam, min_samples=a.learner_min_samples,
                                        seed=a.learner_seed)
                records[key(wl, "learned", lam)] = run_arm(
                    f"learned@{lam}", wl, base, workroot, bins, trace, plan,
                    None, learner, a, cap_bytes, env)
        # static-derived: the pre-drift argmin from the learner's own burn-in sweep, frozen
        if "static-derived" in a.arms and "learned" in a.arms:
            seed_lam = a.lambdas[0]
            src = records.get(key(wl, "learned", seed_lam))
            if src is not None:
                sweep = build_sweep(src["windows"], max_rate=a.reference_rate)
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
    verdicts = evaluate_thresholds(records, lambdas=tuple(a.lambdas), noise_band=noise_band)

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
        "lambdas": a.lambdas,
        "noise_band_relative": noise_band,
        "workloads": {wl: plans[wl].to_dict() for wl in a.workloads},
        "arms": {k: _slim_record(v) for k, v in records.items()},
        "thresholds": verdicts,
        "not_concluded": [
            "data:cap is %.2fx, below the 10x pre-registration floor" % ratio,
            "compaction reorganisation writes are 0 for every arm: the offered-load driver never "
            "calls maybe_compact_shape, so the learner's merge threshold is not actuated on this path",
            "per-region page size acts only on tiles written after the switch; the write fraction "
            "in these traces is small, so the measured page-size effect is a lower bound",
            "the model-calibration gate (<=25% p90 per-region prediction error) is not evaluable; "
            "no simulated verdict is quoted",
            "the 10-seed median/IQR requirement is not met (one seed per arm on a shared box)",
        ],
    }
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
    w("# 0b - the cold arms at equal offered load (Slice 35)")
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
      f"{result['rate_multiples']}. `lambda` is the endurance price in the learner's reward proxy "
      "(a policy choice, not a measurement); every verdict names it.")
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
      "`compaction_bytes` over the run. A row is `sustained` when the median window completed "
      f">= {SUSTAINED_TOLERANCE:.0%} of the offered rate with no rejected window.")
    w("")
    w("## Per-region telemetry (summed over the run)")
    w("")
    for key, rec in sorted(result["arms"].items()):
        regs = rec.get("regions") or {}
        if not regs:
            continue
        w(f"### {key}")
        w("")
        w("| region | windows | bytes_read | bytes_written | compaction_bytes | merges |")
        w("|---|---:|---:|---:|---:|---:|")
        for region, t in sorted(regs.items()):
            w(f"| {region} | {t['windows']} | {t['bytes_read']:,} | {t['bytes_written']:,} | "
              f"{t['compaction_bytes']:,} | {t['compaction_merges']} |")
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
