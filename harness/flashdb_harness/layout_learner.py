"""Per-region layout learner (Slice 33) -- the harness-side controller.

What this is
------------
The engine already exposes a per-region **actuator**: a JSON policy that sets, per
prefix-region, the tile ``page_size`` and the region's ``merge_min_tiles``
(compaction trigger). It is injected with ``FLASHDB_POLICY_FILE`` (or
``--policy-file``), re-read before every metrics window with ``--policy-reload``,
and echoed back inside the ``--metrics-json`` dump (slice 26/27). This module is
the **controller** that chooses those per-region values from the telemetry the
engine already emits. It runs *in the harness*, out of process: read the window,
update, write the policy file, let the driver re-read it.

It deliberately contains no engine code and no subprocesses. The core is
:func:`run_controller`, which takes an injectable ``read_telemetry`` /
``write_policy`` pair, so the whole loop can be driven against simulated rewards
in the tests with no engine and no long run.

The published candidate set
---------------------------
A region's candidate is a :class:`LayoutCandidate`, the cross product of

* ``page_size`` in ``{4096, 65536}`` (the engine's ``MIN_PAGE_SIZE`` /
  ``MAX_PAGE_SIZE``; the cold axis measured a +54% throughput swing between the
  global 4 KiB and per-region mixed-page layouts), and
* ``merge_min_tiles`` in ``{2, 8, 200}`` (the automatic compaction trigger;
  the cold axis measured a 300x compaction-output difference between eager and
  deferred at equal throughput, at the price of a doubled p99).

That is **6 candidates per region**, ``DEFAULT_CANDIDATES``. It is configurable:
pass ``candidates=...`` to :class:`LayoutLearner`. ``bloom_bits_per_key`` is
deliberately *not* a learned axis here: slice 29 measured it dead on this flow
(absent probes are range-rejected before the bloom is consulted), and the task's
published set names only page size and merge threshold.

The reward proxy
----------------
For one region in one window, from the slice-26 telemetry row, define the
region's **cost** (LOWER is better), in nanoseconds of service time::

    cost_ns = count * mean_ns  +  lambda_ns_per_byte * bytes_written

``count * mean_ns`` is the region's total service time in the window -- exactly
the throughput term the objective scores (objective v1: sustained throughput
under offered load). ``bytes_written`` is the endurance term, priced by the
explicit weight ``lambda_ns_per_byte``. The weight exists because the two knobs
have **measured, conflicting gradients** (slice 29): deferring merges cuts
compaction output ~300x at statistically equal throughput but roughly doubles
p99, so a controller that minimises read cost alone would defer everything and
abandon tail latency. Lambda is the endurance price that makes a byte written
comparable to a nanosecond of service time. It is a **policy choice, not a
measurement** (mirrors ``write_weight_us_per_byte`` in ``layout_headroom.py``):
the default ``DEFAULT_LAMBDA_NS_PER_BYTE = 1.0`` prices a byte at roughly the
raw device write cost (831 MiB/s ~ 1.15 ns/byte). The reported numbers are
comparable only within one lambda; it is stated with every result.

The **write side the merge threshold actually moves** is now observable per
region: slice 34 added ``compaction_bytes`` (the merged output tile size written
for the region) and ``compaction_merges`` to the per-region telemetry, and
:class:`RegionStats` parses both. The **per-region rewrite factor** is defined,
and must be reported, as::

    rewrite_factor(region) = compaction_bytes(region) / bytes_written(region)

over the same window(s) -- compaction output divided by the region's ingested
(WAL op) bytes. It is the per-region form of ``flashdb-scale compact``'s
run-level ``compaction_bytes / tile_bytes_before``. This slice keeps the priced
endurance term at ``bytes_written`` (WAL ingest, close to constant on a real
engine); the compaction counters are exposed so a report can quote the rewrite
factor and a later slice can price it directly.

The bandit
----------
:class:`LayoutLearner` is **UCB1**, in its minimisation form (a lower confidence
bound on the region's cost: ``mean_cost - c * sqrt(ln(N) / n)``, pulled arm =
``argmin``). UCB1 is chosen over epsilon-greedy because it needs **no RNG in the
decision path**: for a fixed sequence of observed rewards the arm sequence is
fixed, which the project's determinism/replay discipline requires, and its
regret is logarithmic rather than linear. The one place a seed is used is exact
**tie-breaking** among candidates with identical scores (a seeded permutation of
the candidate set); this never changes a gated decision, only which of two
equally-scored candidates is picked.

Three guards sit on top of the bandit:

* **minimum-sample gate** -- a candidate cannot be adopted as the incumbent
  until it has been observed ``min_samples`` windows (and every other available
  candidate has too). Until then the learner is in a bounded, named *burn-in*:
  each candidate is sampled ``min_samples`` windows, then the gate opens.
* **hysteresis** -- once the gate is open, the incumbent is replaced only when a
  challenger's mean cost beats it by more than ``margin`` (relative) for
  ``stability_window`` consecutive steps. A one-window noise spike therefore
  cannot move the policy.
* **p99 hard constraint** -- a window whose region ``p99_ns`` exceeds the
  region's declared ceiling is an *invalid* window (objective v1's validity
  rule): it is not counted as reward evidence, and the candidate that produced
  it is **disqualified for that region** and never selectable again. Once
  proven to violate a ceiling a candidate stays out, even across drift restarts.

Non-stationarity (drift) is what makes the learner adapt and the
``flashdb-static-derived`` control not. **Drift trigger (per region):** the
active candidate's window cost deviates from the region's reference cost (set
when the gate first opened) by more than ``drift_margin`` (default ``0.5`` =
50% relative) for ``drift_windows`` (default 2) consecutive windows (the
pre-registration's drift workload changes the access shape, so the active
region's cost moves).

The response is **targeted**. First, it is scoped to the region that triggered:
only that region re-explores, and every other region keeps its committed
candidate and all of its evidence untouched. Second, the re-exploration within
the region is **capped** at ``drift_reexplore_candidates`` (default 2): the
incumbent plus the best ``K - 1`` challengers by recorded cost are re-sampled to
``min_samples`` each, so the response costs ``K * min_samples`` windows rather
than ``candidates * min_samples``, while the remaining candidates keep their
committed evidence. ``drift_reexplore_candidates=None`` restores the full
re-exploration, which is required when a candidate that was *worse* than the
incumbent before the drift becomes the best after it (the targeted set is chosen
from pre-drift evidence and cannot see that). ``recovery_windows()`` reports the
windows each region's last re-exploration needed, and ``reexploring_regions()``
the regions currently re-exploring. With no gradient and no drift no restart
fires and the learner **picks once and stays** -- the anti-overfitting property
whose test is the point of the whole exercise.

**Pre-registration gap (stated, not hidden).** Even the targeted response cannot
meet the pre-registration's *2-window* recovery target with the published grid:
the response needs ``drift_reexplore_candidates * min_samples`` windows (6 with
the defaults: 2 candidates x 3 samples) before the gate can even compare arms,
plus ``stability_window`` for the hysteresis to commit. The binding term is
``min_samples x candidates-re-explored``: to recover in 2 windows the author must
relax ``min_samples`` (or ``drift_reexplore_candidates``), not the learner. The
full re-exploration is ``candidates * min_samples`` = 18 windows at the defaults,
so the targeted response is a 3x reduction, not a solution.

Controls
--------
:class:`DeclaredController` is ``flashdb-declared`` (one fixed candidate for
every region) and :class:`StaticDerivedController` is ``flashdb-static-derived``
(the argmin over the candidate set computed once from a pre-drift sweep and then
frozen). Both share the :class:`Controller` interface with the learner.

What the learner CANNOT do here
-------------------------------
* It **cannot rewrite existing tiles.** A ``page_size`` change is honoured only
  by tiles written *after* the switch (the engine's ``LayoutPolicy`` is not
  persisted; every tile stores its own page size in its superblock and readers
  honour the stored value). The reward proxy does **not** model this lag: the
  cost of a window is attributed to the candidate that was active for it, so a
  freshly switched-to page size is credited with a window it only partly
  affected. This is a stated bias, not compensated.
* It prices the endurance term with **``bytes_written``** (the region's ingested
  WAL op bytes, close to constant on a real engine), not with the region's
  **compaction output** that ``merge_min_tiles`` actually moves. Slice 34 now
  exposes ``compaction_bytes``/``compaction_merges`` and the rewrite factor is
  derivable (:func:`region_rewrite_factor`), but the reward proxy does not price
  that quantity yet, so the merge choice is still driven mostly by the read-cost
  term plus the p99 constraint. (The synthetic tests make ``bytes_written`` vary
  by candidate so the lambda mechanism itself is exercised.)
* It does **not price memory**, CPU per op, or reorganisation payback windows.
* It does not take effect immediately: the driver re-reads the policy once per
  window, and a merge threshold only gates *future* automatic merges.
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable, Mapping, Protocol

from flashdb_harness.generator import Rng

# ---------------------------------------------------------------------------
# The published candidate set
# ---------------------------------------------------------------------------
DEFAULT_PAGE_SIZES = (4096, 65536)
DEFAULT_MERGE_MIN_TILES = (2, 8, 200)
DEFAULT_BLOOM_BITS = 10
DEFAULT_LAMBDA_NS_PER_BYTE = 1.0


@dataclass(frozen=True, order=True)
class LayoutCandidate:
    """One published per-region policy: a tile page size and a merge threshold."""

    page_size: int
    merge_min_tiles: int

    def __post_init__(self) -> None:
        if self.page_size not in (4096, 65536):
            raise ValueError(f"page_size {self.page_size} is not in {DEFAULT_PAGE_SIZES}")
        if self.merge_min_tiles < 2:
            raise ValueError(f"merge_min_tiles {self.merge_min_tiles} must be >= 2")

    def entry(self) -> dict:
        """The engine's per-region policy entry (bloom bits fall back to the default)."""
        return {"page_size": self.page_size, "merge_min_tiles": self.merge_min_tiles}


# The 2 x 3 published set: 6 candidates. Both axes have a measured cold gradient.
DEFAULT_CANDIDATES: tuple[LayoutCandidate, ...] = tuple(
    LayoutCandidate(p, m) for p in DEFAULT_PAGE_SIZES for m in DEFAULT_MERGE_MIN_TILES
)
# The engine's shipped default (LayoutPolicy::declared_default): 4 KiB pages, 10
# bloom bits, merge_min_tiles = 2. It is the region's initial active candidate.
DECLARED_DEFAULT = LayoutCandidate(4096, 2)


def is_policy_region(name: str) -> bool:
    """True for a real region spelling (``P|``, ``U|`` or ``R|`` + non-empty entity).

    The engine folds every region past its cap into ``"other"``, which is not a
    valid policy key, so the controller skips it.
    """
    return any(name.startswith(p) and len(name) > len(p) for p in ("P|", "U|", "R|"))


# ---------------------------------------------------------------------------
# Telemetry (the slice-26 window schema)
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class RegionStats:
    """One region's row of a metrics window (``current.regions[]``)."""

    region: str
    probes: int
    probe_misses: int
    bytes_read: int
    bytes_written: int
    commit_blocks: int
    commit_ops: int
    count: int
    mean_ns: float
    p50_ns: float
    p99_ns: float
    # Slice 34: the write side of the merge threshold (merged output tile bytes
    # and the merge count). Absent from a pre-slice-34 dump, so defaulted to 0.
    compaction_bytes: int = 0
    compaction_merges: int = 0

    @classmethod
    def from_json(cls, obj: Mapping) -> "RegionStats":
        return cls(
            region=str(obj["region"]),
            probes=int(obj.get("probes", 0)),
            probe_misses=int(obj.get("probe_misses", 0)),
            bytes_read=int(obj.get("bytes_read", 0)),
            bytes_written=int(obj.get("bytes_written", 0)),
            commit_blocks=int(obj.get("commit_blocks", 0)),
            commit_ops=int(obj.get("commit_ops", 0)),
            count=int(obj.get("count", 0)),
            mean_ns=float(obj.get("mean_ns", 0.0)),
            p50_ns=float(obj.get("p50_ns", 0.0)),
            p99_ns=float(obj.get("p99_ns", 0.0)),
            compaction_bytes=int(obj.get("compaction_bytes", 0)),
            compaction_merges=int(obj.get("compaction_merges", 0)),
        )


@dataclass(frozen=True)
class WindowTelemetry:
    """One ``--metrics-json`` window line, already parsed."""

    window: int
    rate_rps: float
    regions: dict[str, RegionStats]
    p50_us: float = 0.0
    p99_us: float = 0.0
    policy: Mapping | None = None

    @classmethod
    def from_json(cls, obj: Mapping) -> "WindowTelemetry":
        regions = {}
        for row in obj.get("current", {}).get("regions", []):
            stats = RegionStats.from_json(row)
            regions[stats.region] = stats
        return cls(
            window=int(obj.get("window", 0)),
            rate_rps=float(obj.get("rate_rps", 0.0)),
            regions=regions,
            p50_us=float(obj.get("p50_us", 0.0)),
            p99_us=float(obj.get("p99_us", 0.0)),
            policy=obj.get("policy"),
        )


def parse_window_line(line: str) -> WindowTelemetry | None:
    """Parse one ``--metrics-json`` line; ``None`` for any non-JSON or malformed line.

    The pacer's stdout interleaves non-JSON header/summary lines (``device_fsync...``,
    ``window=...``, ``load_summary ...``) with the ``{``-prefixed JSON windows, so a
    JSONL reader must skip them rather than fail.
    """
    line = line.strip()
    if not line.startswith("{"):
        return None
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    if not isinstance(obj, dict):
        return None
    return WindowTelemetry.from_json(obj)


def region_cost_ns(stats: RegionStats, lambda_ns_per_byte: float = DEFAULT_LAMBDA_NS_PER_BYTE) -> float:
    """The reward proxy: service time (``count * mean_ns``) plus the priced write term.

    Lower is better. See the module docstring for why the endurance term exists.
    """
    return stats.count * stats.mean_ns + lambda_ns_per_byte * stats.bytes_written


def region_rewrite_factor(stats: RegionStats) -> float | None:
    """The per-region rewrite factor of one window (``None`` when undefined).

    ``compaction_bytes / bytes_written`` -- the compaction output attributed to
    the region divided by its ingested (WAL op) bytes over the same window. Over
    a run, sum both counters across windows first. A window with no ingest for
    the region is undefined (division by zero) and returns ``None`` rather than
    0, so a real rewrite factor is never reported as zero. This is the same
    quantity `metrics.rs` defines and `flashdb-scale compact` reports at the run
    level.
    """
    if stats.bytes_written <= 0:
        return None
    return stats.compaction_bytes / stats.bytes_written


def policy_document(selections: Mapping[str, LayoutCandidate], *,
                    default: LayoutCandidate = DECLARED_DEFAULT) -> dict:
    """The engine's frozen JSON policy-file schema for a set of per-region selections.

    Regions without a selection inherit ``default`` (the engine's shipped 4 KiB /
    merge-2 layout unless the controller overrides it). ``bloom_bits_per_key`` is
    omitted so the engine's own default applies.
    """
    regions = {name: selections[name].entry() for name in sorted(selections)}
    return {
        "default": {
            "page_size": default.page_size,
            "bloom_bits_per_key": DEFAULT_BLOOM_BITS,
            "merge_min_tiles": default.merge_min_tiles,
        },
        "regions": regions,
    }


def _seeded_order(candidates: tuple[LayoutCandidate, ...], seed: int) -> dict[LayoutCandidate, int]:
    """A seeded permutation of the candidate set, used only to break exact ties."""
    order = list(range(len(candidates)))
    rng = Rng(seed)
    for i in range(len(order) - 1, 0, -1):
        j = rng.below(i + 1)
        order[i], order[j] = order[j], order[i]
    return {c: order[i] for i, c in enumerate(candidates)}


# ---------------------------------------------------------------------------
# The UCB1 learner
# ---------------------------------------------------------------------------
@dataclass
class _Arm:
    n: int = 0
    sum_cost: float = 0.0
    violating: bool = False       # proved to break this region's p99 ceiling

    @property
    def mean_cost(self) -> float:
        return self.sum_cost / self.n if self.n else math.inf


@dataclass
class _RegionState:
    arms: dict[LayoutCandidate, _Arm] = field(default_factory=dict)
    active: LayoutCandidate | None = None
    pending: LayoutCandidate | None = None
    pending_streak: int = 0
    reference_cost: float | None = None
    deviation_streak: int = 0
    gated_switches: int = 0
    constraint_switches: int = 0
    restarts: int = 0
    reexploring: bool = False
    reexplore_start: int | None = None
    recovery_windows: int | None = None


@dataclass
class LayoutLearner:
    """UCB1 per-region chooser with a min-sample gate, hysteresis and a p99 constraint.

    Every parameter is named in the module docstring; the defaults are advisory
    and are echoed in the slice report. ``seed`` is used **only** to order exact
    ties (see :func:`_seeded_order`).
    """

    candidates: tuple[LayoutCandidate, ...] = DEFAULT_CANDIDATES
    lambda_ns_per_byte: float = DEFAULT_LAMBDA_NS_PER_BYTE
    min_samples: int = 3
    margin: float = 0.05
    stability_window: int = 2
    exploration_c: float = math.sqrt(2)
    ceilings_ns: Mapping[str, float] = field(default_factory=dict)
    drift_margin: float = 0.5
    drift_windows: int = 2
    # Targeted drift response: re-explore the incumbent plus the best K-1
    # challengers (None = the full candidate set, the correctness fallback).
    drift_reexplore_candidates: int | None = 2
    seed: int = 0

    def __post_init__(self) -> None:
        self.candidates = tuple(self.candidates)
        if not self.candidates:
            raise ValueError("candidate set must not be empty")
        if self.min_samples < 1:
            raise ValueError("min_samples must be >= 1")
        if self.stability_window < 1:
            raise ValueError("stability_window must be >= 1")
        if self.drift_windows < 1:
            raise ValueError("drift_windows must be >= 1")
        if self.drift_reexplore_candidates is not None and self.drift_reexplore_candidates < 1:
            raise ValueError("drift_reexplore_candidates must be >= 1 or None")
        self.ceilings_ns = dict(self.ceilings_ns)
        self._tie = _seeded_order(self.candidates, self.seed)
        self._regions: dict[str, _RegionState] = {}

    # -- state helpers ------------------------------------------------------
    def _region(self, name: str) -> _RegionState:
        st = self._regions.get(name)
        if st is None:
            st = _RegionState(arms={c: _Arm() for c in self.candidates})
            start = DECLARED_DEFAULT if DECLARED_DEFAULT in st.arms else self.candidates[0]
            st.active = start
            self._regions[name] = st
        return st

    def _available(self, st: _RegionState) -> list[LayoutCandidate]:
        return [c for c in self.candidates if not st.arms[c].violating]

    def _in_exploitation(self, st: _RegionState) -> bool:
        available = self._available(st)
        if not available:
            return False
        return min(st.arms[c].n for c in available) >= self.min_samples

    @staticmethod
    def _reset_gate(st: _RegionState) -> None:
        st.pending = None
        st.pending_streak = 0

    # -- the driver-facing interface ---------------------------------------
    def observe(self, window: WindowTelemetry) -> None:
        """Fold one metrics window in, then take one control step per region."""
        regions = [name for name in window.regions if is_policy_region(name)]
        for name in regions:
            st = self._region(name)
            if st.active is not None:
                self.record(name, st.active, window.regions[name])
            # A drift detected by `record` above starts the epoch in THIS window
            # (the reset already happened and one arm may be re-sampled), so the
            # response is counted from here, not from the next window.
            if st.reexploring and st.reexplore_start is None:
                st.reexplore_start = window.window
        self.step(regions)
        # Close a re-exploration epoch once the min-sample gate has reopened (all
        # targeted candidates re-sampled), and record how many windows it took.
        # The normal hysteresis then decides the committed candidate; it can add
        # up to `stability_window` windows, reported separately. Only regions that
        # actually drifted ever enter here; unchanged regions are never touched.
        for name in regions:
            st = self._region(name)
            if st.reexploring and st.reexplore_start is not None and self._in_exploitation(st):
                st.recovery_windows = window.window - st.reexplore_start + 1
                st.reexploring = False
                st.reexplore_start = None

    def record(self, region: str, candidate: LayoutCandidate, stats: RegionStats) -> None:
        """Count one window's observation of ``candidate`` for ``region``.

        A window that breaks the region's declared p99 ceiling is invalid: it is
        not counted as reward evidence, and ``candidate`` is disqualified for the
        region. Public so a test can drive the gate with a controlled evidence set.
        """
        st = self._region(region)
        ceiling = self.ceilings_ns.get(region)
        if ceiling is not None and stats.p99_ns > ceiling:
            st.arms[candidate].violating = True
            return
        cost = region_cost_ns(stats, self.lambda_ns_per_byte)
        if candidate == st.active and self._in_exploitation(st) and st.reference_cost is not None:
            self._note_deviation(st, cost)
        arm = st.arms[candidate]
        arm.n += 1
        arm.sum_cost += cost

    def step(self, regions: Iterable[str], *, explore: bool = True) -> None:
        """One policy step for each region: burn-in exploration, else the gate.

        ``explore=False`` runs the hysteresis gate only (the min-sample gate stays
        closed until every available candidate is sampled); tests use it to pin
        the gate's behaviour against a fixed evidence set.
        """
        for name in regions:
            if not is_policy_region(name):
                continue
            st = self._region(name)
            if explore and self._burn_in_needed(st):
                self._explore(st)
            else:
                self._gate(st)

    # -- gate / explore -----------------------------------------------------
    def _burn_in_needed(self, st: _RegionState) -> bool:
        available = self._available(st)
        return bool(available) and min(st.arms[c].n for c in available) < self.min_samples

    def _explore(self, st: _RegionState) -> None:
        available = self._available(st)
        if not available:
            return
        if st.active is None or st.active not in available:
            if st.active is not None:
                st.constraint_switches += 1
            st.active = self._initial_pick(st, available)
        else:
            st.active = self._ucb_pick(st, available)
        self._reset_gate(st)
        st.reference_cost = None

    def _gate(self, st: _RegionState) -> None:
        available = self._available(st)
        if not available:
            return
        if st.active is None or st.active not in available:
            if st.active is not None:
                st.constraint_switches += 1
            st.active = self._initial_pick(st, available)
            self._reset_gate(st)
            st.reference_cost = None
            return
        if not self._in_exploitation(st):
            return  # min-sample gate: a challenger (or the incumbent) is short of evidence
        if st.reference_cost is None:
            st.reference_cost = st.arms[st.active].mean_cost
        incumbent = st.active
        incumbent_cost = st.arms[incumbent].mean_cost
        others = [c for c in available if c != incumbent]
        if not others:
            self._reset_gate(st)
            return
        # UCB1 names the challenger; the margin below compares MEANS, so the
        # exploration bonus can never on its own move the policy.
        challenger = self._ucb_pick(st, others)
        improvement = (incumbent_cost - st.arms[challenger].mean_cost) / max(abs(incumbent_cost), 1.0)
        if improvement > self.margin:
            if st.pending == challenger:
                st.pending_streak += 1
            else:
                st.pending = challenger
                st.pending_streak = 1
            if st.pending_streak >= self.stability_window:
                st.active = challenger
                st.gated_switches += 1
                self._reset_gate(st)
                st.reference_cost = None
        else:
            self._reset_gate(st)

    def _initial_pick(self, st: _RegionState, available: list[LayoutCandidate]) -> LayoutCandidate:
        gated = [c for c in available if st.arms[c].n >= self.min_samples]
        if gated:
            return min(gated, key=lambda c: (st.arms[c].mean_cost, self._tie[c]))
        return min(available, key=lambda c: (st.arms[c].n, self._tie[c]))

    def _ucb_pick(self, st: _RegionState, eligible: list[LayoutCandidate]) -> LayoutCandidate:
        """UCB1 in minimisation form: ``argmin`` of a lower confidence bound on cost.

        An arm below the minimum-sample gate is treated as *unexplored* (``-inf``),
        exactly as standard UCB1 treats an unpulled arm, so the burn-in samples
        every candidate to the gate before any switch. The confidence bonus is
        scaled to the region's observed cost magnitude (otherwise it would be
        negligible against nanosecond costs and exploration would never happen).
        """
        total = sum(st.arms[c].n for c in eligible)
        log_total = math.log(max(total, 2))
        scale = max((st.arms[c].mean_cost for c in eligible if st.arms[c].n > 0), default=1.0)
        scale = max(scale, 1.0)

        def lcb(c: LayoutCandidate) -> float:
            arm = st.arms[c]
            if arm.n < self.min_samples:
                return -math.inf
            return arm.mean_cost - self.exploration_c * scale * math.sqrt(log_total / arm.n)

        return min(eligible, key=lambda c: (lcb(c), self._tie[c]))

    # -- drift --------------------------------------------------------------
    def _note_deviation(self, st: _RegionState, cost: float) -> None:
        reference = st.reference_cost
        if reference is None:
            return
        deviation = abs(cost - reference) / max(abs(reference), 1.0)
        if deviation > self.drift_margin:
            st.deviation_streak += 1
            if st.deviation_streak >= self.drift_windows:
                self._restart(st)
        else:
            st.deviation_streak = 0

    def _restart(self, st: _RegionState) -> None:
        """Open a TARGETED re-exploration epoch after drift (slice 34).

        Only ``drift_reexplore_candidates`` candidates are reset -- the incumbent
        plus the best challengers by recorded cost -- so the response is capped
        at ``K * min_samples`` windows and the remaining candidates keep their
        committed evidence. The p99 disqualifications survive the epoch.
        """
        for candidate in self._reexplore_targets(st):
            st.arms[candidate].n = 0
            st.arms[candidate].sum_cost = 0.0
        self._reset_gate(st)
        st.reference_cost = None
        st.deviation_streak = 0
        st.restarts += 1
        st.reexploring = True
        st.reexplore_start = None

    def _reexplore_targets(self, st: _RegionState) -> set[LayoutCandidate]:
        """The candidates a drift epoch re-samples: the incumbent + the best K-1.

        ``drift_reexplore_candidates=None`` (or ``K`` >= the available count)
        re-samples every available candidate, the correctness-complete fallback.
        """
        available = self._available(st)
        k = self.drift_reexplore_candidates
        if k is None or k >= len(available):
            return set(available)
        ordered: list[LayoutCandidate] = []
        if st.active is not None and st.active in available:
            ordered.append(st.active)
        ordered.extend(sorted(
            (c for c in available if c != st.active),
            key=lambda c: (st.arms[c].mean_cost, self._tie[c]),
        ))
        return set(ordered[:k])

    # -- readout ------------------------------------------------------------
    def selections(self) -> dict[str, LayoutCandidate]:
        return {name: st.active for name, st in self._regions.items() if st.active is not None}

    def policy_document(self) -> dict:
        return policy_document(self.selections())

    def gated_switches(self) -> dict[str, int]:
        return {name: st.gated_switches for name, st in self._regions.items()}

    def constraint_switches(self) -> dict[str, int]:
        return {name: st.constraint_switches for name, st in self._regions.items()}

    def restarts(self) -> dict[str, int]:
        return {name: st.restarts for name, st in self._regions.items()}

    def reexploring_regions(self) -> set[str]:
        """Regions currently inside a drift re-exploration epoch."""
        return {name for name, st in self._regions.items() if st.reexploring}

    def recovery_windows(self) -> dict[str, int]:
        """Re-exploration windows of the last drift response, per drifted region.

        Counted from the trigger window until the min-sample gate reopens (all
        targeted candidates re-sampled). The hysteresis that then commits the
        candidate can add up to ``stability_window`` windows.
        """
        return {name: st.recovery_windows for name, st in self._regions.items() if st.recovery_windows is not None}

    def violating_candidates(self, region: str) -> set[LayoutCandidate]:
        st = self._regions.get(region)
        if st is None:
            return set()
        return {c for c, arm in st.arms.items() if arm.violating}

    def pulls(self, region: str) -> dict[LayoutCandidate, int]:
        st = self._regions.get(region)
        return {c: arm.n for c, arm in st.arms.items()} if st else {}

    @property
    def candidate_order(self) -> tuple[LayoutCandidate, ...]:
        """The seeded tie-break permutation (where the seed is used)."""
        return tuple(sorted(self.candidates, key=lambda c: self._tie[c]))


# ---------------------------------------------------------------------------
# The two declared controls (same interface)
# ---------------------------------------------------------------------------
class Controller(Protocol):
    """The interface the driver loop requires of any controller."""

    def observe(self, window: WindowTelemetry) -> None: ...
    def selections(self) -> dict[str, LayoutCandidate]: ...
    def policy_document(self) -> dict: ...


@dataclass
class DeclaredController:
    """``flashdb-declared``: one fixed candidate for every region, never adapting."""

    candidate: LayoutCandidate = DECLARED_DEFAULT

    def __post_init__(self) -> None:
        self._regions: dict[str, LayoutCandidate] = {}

    def observe(self, window: WindowTelemetry) -> None:
        for name in window.regions:
            if is_policy_region(name):
                self._regions[name] = self.candidate

    def selections(self) -> dict[str, LayoutCandidate]:
        return dict(self._regions)

    def policy_document(self) -> dict:
        return policy_document(self._regions, default=self.candidate)


@dataclass
class StaticDerivedController:
    """``flashdb-static-derived``: the argmin over the candidate set from a pre-drift
    sweep, computed once and frozen; ``observe`` never changes a selection."""

    lambda_ns_per_byte: float = DEFAULT_LAMBDA_NS_PER_BYTE
    ceilings_ns: Mapping[str, float] = field(default_factory=dict)
    candidates: tuple[LayoutCandidate, ...] = DEFAULT_CANDIDATES
    seed: int = 0

    def __post_init__(self) -> None:
        self.candidates = tuple(self.candidates)
        self.ceilings_ns = dict(self.ceilings_ns)
        self._tie = _seeded_order(self.candidates, self.seed)
        self._frozen: dict[str, LayoutCandidate] = {}

    def derive(self, sweep: Mapping[str, Mapping[LayoutCandidate, RegionStats]]) -> None:
        """Freeze each region's argmin cost from a pre-drift sweep (all candidates)."""
        for region, by_candidate in sweep.items():
            if not is_policy_region(region):
                continue
            ceiling = self.ceilings_ns.get(region)
            eligible = {
                c: by_candidate[c]
                for c in self.candidates
                if c in by_candidate
                and not (ceiling is not None and by_candidate[c].p99_ns > ceiling)
            }
            if not eligible:
                continue
            self._frozen[region] = min(
                eligible, key=lambda c: (region_cost_ns(eligible[c], self.lambda_ns_per_byte), self._tie[c])
            )

    def observe(self, window: WindowTelemetry) -> None:
        return  # frozen: a static-derived control never adapts

    def selections(self) -> dict[str, LayoutCandidate]:
        return dict(self._frozen)

    def policy_document(self) -> dict:
        return policy_document(self._frozen)


# ---------------------------------------------------------------------------
# The engine-agnostic driver loop
# ---------------------------------------------------------------------------
def run_controller(read_telemetry: Callable[[], WindowTelemetry | None],
                   write_policy: Callable[[dict], None],
                   controller: Controller,
                   *, max_windows: int | None = None) -> list[tuple[int, dict]]:
    """Read a window, update the controller, write a new policy file: repeat.

    ``read_telemetry`` returns the next :class:`WindowTelemetry` or ``None`` at
    end of stream. ``write_policy`` receives the frozen-schema document; against a
    live engine it writes the file the driver re-reads with ``--policy-reload``.
    The loop is engine-agnostic on purpose, so a test can supply a simulated window
    producer and a recording writer. Returns ``(window_index, selections)`` per window.
    """
    history: list[tuple[int, dict]] = []
    count = 0
    while max_windows is None or count < max_windows:
        window = read_telemetry()
        if window is None:
            break
        controller.observe(window)
        write_policy(controller.policy_document())
        history.append((window.window, controller.selections()))
        count += 1
    return history


def write_policy_file(path: str | Path, document: dict) -> None:
    """Write the controller's policy document where ``FLASHDB_POLICY_FILE`` points.

    Sorted keys make the file byte-deterministic for a given policy.
    """
    Path(path).write_text(json.dumps(document, sort_keys=True, indent=2), encoding="utf-8")


def read_telemetry_jsonl(path: str | Path):
    """Yield parsed metric windows from a ``--metrics-json`` output file, or stop.

    A convenience adapter for a live run (the pacer streams windows to stdout /
    a file); the core loop only needs a callable, so it is not required here.
    """
    with open(path, encoding="utf-8") as stream:
        for line in stream:
            window = parse_window_line(line)
            if window is not None:
                yield window
