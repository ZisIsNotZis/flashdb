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

The reward proxy (Slice 39: unit-anchored, reorganisation-priced)
-----------------------------------------------------------------
For one region in one window, from the slice-26 telemetry row, define the
region's **cost** (LOWER is better), in nanoseconds of service time::

    cost_ns = count * mean_ns
              + lambda_ns_per_byte * (bytes_written + compaction_bytes + index_output_bytes)

Both terms are **nanoseconds of service time**. ``count * mean_ns`` is the
region's total service time in the window -- the throughput term the objective
scores (objective v1: sustained throughput under offered load). The second term
is the **endurance** price of the bytes the region wrote to storage in the
window: its WAL ingest (``bytes_written``) *plus* the reorganisation output the
merge knob actually moves (``compaction_bytes``, slice 34) *plus* the derived
secondary-access-path build output (``index_output_bytes``, WP-M). It is charged at the
region's own device cost.

**Unit derivation.**  ``lambda_ns_per_byte`` is the price of a written byte, in
ns of service time. It is anchored -- not free-floating -- to the measured device
write bandwidth, 831 MiB/s (slice 08/29; the same constant ``DEVICE_MIB_S`` as
``layout_headroom.py``)::

    1 byte / (831 MiB/s) = 1e9 ns / (831 * 2**20 bytes) ~= 1.148 ns/byte

so ``DEFAULT_LAMBDA_NS_PER_BYTE = DEVICE_WRITE_NS_PER_BYTE`` (1.148) prices a
byte at exactly the device time it consumes. A write then pays in the same unit
as a slow read: a 1% increase in written bytes and a 1% increase in service time
are the same kind of cost. Lambda remains the **explicit endurance knob** on top
of that unit: it multiplies the device time, so lambda=1 is "price endurance at
device cost" and lambda=10 is "endurance is worth 10x its device time". It is a
**policy choice, not a measurement**. The reported numbers are comparable only
within one lambda; it is stated with every result.

**Why the write term prices ``compaction_bytes``.**  Slice 37 measured the
learner that priced only ``bytes_written``: sweeping lambda 0.1 -> 10 moved the
win arm's reorganisation output only from 4.13x to 4.27x of declared, i.e. lambda
barely moved the learned policy. The diagnosis is in the term magnitudes:
``bytes_written`` is the region's WAL **ingest**, which the layout policy does
not change and which is tiny next to the service term (the ordering ratio is
reported per run), so ``lambda * bytes_written`` was a near-constant offset that
could not move any arm's ``argmin``. The quantity the merge threshold moves is
the **reorganisation output**, and slice 34 exposes it per region. Pricing
``bytes_written + compaction_bytes`` makes lambda act on the knob it is supposed
to price.

The **per-region rewrite factor** is defined, and must be reported, as::

    rewrite_factor(region) = compaction_bytes(region) / bytes_written(region)

over the same window(s) -- compaction output divided by the region's ingested
(WAL op) bytes. It is the per-region form of ``flashdb-scale compact``'s
run-level ``compaction_bytes / tile_bytes_before``.

The bandit
----------
:class:`LayoutLearner` is **UCB1**, in its minimisation form (a lower confidence
bound on the region's cost: ``median_cost - c * sqrt(ln(N) / n)``, pulled arm =
``argmin``). UCB1 is chosen over epsilon-greedy because it needs **no RNG in the
decision path**: for a fixed sequence of observed rewards the arm sequence is
fixed, which the project's determinism/replay discipline requires, and its
regret is logarithmic rather than linear. The one place a seed is used is exact
**tie-breaking** among candidates with identical scores (a seeded permutation of
the candidate set); this never changes a gated decision, only which of two
equally-scored candidates is picked.

The cost evidence is **per-request** cost (total window cost divided by the
region's request count; the absolute write cost for a request-free region), so
a rate-ladder step -- which scales the request count and the service term
together -- does not look like a cost change. Each arm is scored by the
**median** of its per-request samples rather than the mean, so a one-window
latency or compaction spike on a shared device cannot move an argmin; this is
the robust (heavy-tail-safe) form of the bandit, and it is what lets a
no-locality workload settle instead of churn.

Three guards sit on top of the bandit:

* **minimum-sample gate** -- a candidate cannot be adopted as the incumbent
  until it has been observed ``min_samples`` windows (and every other available
  candidate has too). Until then the learner is in a bounded, named *burn-in*:
  each candidate is sampled ``min_samples`` windows, then the gate opens.
* **hysteresis** -- once the gate is open, the incumbent is replaced only when a
  challenger's median cost beats it by more than ``margin`` (default ``0.5`` =
  50% relative) for ``stability_window`` consecutive steps. A one-window noise
  spike therefore cannot move the policy, and neither can the ~30% per-window
  cost noise of a shared device: the minimum improvement a challenger must show
  sits above the noise floor, so a no-locality workload settles while a genuine
  gradient (the win workload's per-region spread, or the drift workload's shape
  change) still switches.
* **p99 hard constraint** -- a window whose region ``p99_ns`` exceeds the
  region's declared ceiling is an *invalid* window (objective v1's validity
  rule): it is not counted as reward evidence, and the candidate that produced
  it is **disqualified for that region** and never selectable again. Once
  proven to violate a ceiling a candidate stays out, even across drift restarts.

Non-stationarity (drift) is what makes the learner adapt and the
``flashdb-static-derived`` control not. **Drift trigger (per region):** the
active candidate's per-request window cost deviates from the region's reference
cost (the active arm's median per-request cost, set when the gate first opened)
by more than ``drift_margin`` (default ``2.0`` = 200% relative) for
``drift_windows`` (default 2) consecutive windows (the pre-registration's drift
workload changes the access shape, so the active region's cost moves). The
200% floor is set above the device-saturation latency rise the rate ladder
produces at high offered rungs (2-9x per-request cost on a shared box): that is
a *load* artifact, not an access-shape change, and must not re-open exploration.

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

**Churn control between re-explorations (Slice 39).**  Under drift the learner
without churn control oscillated: 104 policy changes over 153 windows in the
slice-37/38 run (81 drift restarts, 50 gated switches), and each merge-threshold
change makes the next compaction rewrite more -- 402,984,851 reorganisation bytes
against declared's 1,673,926 (240x), already past the pre-registration's 3x
bound. Two controls sit between re-explorations:

* **per-region cooldown** (``cooldown_windows``) -- after a region's active
  candidate changes, it cannot change again for ``cooldown_windows`` steps. The
  burn-in and any post-restart re-exploration are stretched, not blocked; a
  bounded exploration still completes.
* **reorganisation budget per window** (``reorg_budget_bytes_per_window``) -- a
  region may not switch unless the *projected added* reorganisation rate of the
  challenger over the incumbent (``max(0, mean_compaction_bytes(challenger) -
  mean_compaction_bytes(incumbent))``) fits the remaining global budget for this
  window. The budget is derived by the runner from the declared arm's measured
  reorganisation rate. An unexplored challenger projects 0 added bytes, so the
  bounded burn-in is never blocked; only a switch to a candidate *observed* to
  write more reorganisation is refused.

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
* It prices the endurance term with **``bytes_written + compaction_bytes +
  index_output_bytes``** (all bytes the region wrote to storage in the window) at
  the device-cost unit (Slice 39). The WP-M `index_output_bytes` term is priced
  for endurance but excluded from the churn budget. It still cannot see the engine's *global* compaction-selection
  interaction: which region's tiles are merged each window depends on every
  region's merge threshold (slice 37/38 measured deferral in one region funnelling
  merges into an eager neighbour), and the reward attributes each region only its
  own counters. A per-region reward therefore cannot fully predict a
  workload-level reorganisation change.
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
# Device write bandwidth, measured cold with O_DIRECT (831 MiB/s; the same constant as
# `DEVICE_MIB_S` in `layout_headroom.py`). The endurance unit is the nanoseconds of device
# write bandwidth a byte consumes: 1e9 ns / (831 * 2**20 bytes) ~= 1.148 ns/byte.
DEVICE_WRITE_BANDWIDTH_MIB_PER_S = 831.0
DEVICE_WRITE_NS_PER_BYTE = 1e9 / (DEVICE_WRITE_BANDWIDTH_MIB_PER_S * (1 << 20))
# Anchored default (Slice 39): a byte is priced at its device cost, not a free-floating 1.0.
DEFAULT_LAMBDA_NS_PER_BYTE = DEVICE_WRITE_NS_PER_BYTE


@dataclass(frozen=True, order=True)
class LayoutCandidate:
    """One published per-region policy: a tile page size, a merge threshold, and
    whether to build the derived `order_no -> StockMovement` secondary access
    path (WP-M). The index axis is a no-op for regions other than
    ``P|StockMovement``; the engine ignores it there. The learner offers it only
    to ``P|StockMovement`` and only once that region has actually served a scan
    request (WP-O: the axis is demand-gated, not explored as a cross product)."""

    page_size: int
    merge_min_tiles: int
    index_order_no: bool = False

    def __post_init__(self) -> None:
        if self.page_size not in (4096, 65536):
            raise ValueError(f"page_size {self.page_size} is not in {DEFAULT_PAGE_SIZES}")
        if self.merge_min_tiles < 2:
            raise ValueError(f"merge_min_tiles {self.merge_min_tiles} must be >= 2")

    def entry(self) -> dict:
        """The engine's per-region policy entry (bloom bits fall back to the default)."""
        entry = {"page_size": self.page_size, "merge_min_tiles": self.merge_min_tiles}
        if self.index_order_no:
            entry["index_order_no"] = True
        return entry


# The 2 x 3 published set: 6 candidates. Both axes have a measured cold gradient.
DEFAULT_CANDIDATES: tuple[LayoutCandidate, ...] = tuple(
    LayoutCandidate(p, m) for p in DEFAULT_PAGE_SIZES for m in DEFAULT_MERGE_MIN_TILES
)
# The engine's shipped default (LayoutPolicy::declared_default): 4 KiB pages, 10
# bloom bits, merge_min_tiles = 2. It is the region's initial active candidate.
DECLARED_DEFAULT = LayoutCandidate(4096, 2)
# WP-O: the `order_no` secondary access path is a DEMAND-GATED, one-shot axis,
# not a 12-candidate cross product. `P|StockMovement` starts on the base 6-grid
# (index off); the first window in which it actually serves a scan (`count > 0`)
# flips its candidate set to the same 6 layouts with `index_order_no=True`, and
# the index stays on for the rest of the run. A region that never serves a scan
# (the adversarial workload's 0-scan `P|StockMovement`) never offers the index,
# so the learner cannot build a ~2.14 MB sidecar for zero read benefit.
# The region the secondary access path is built for (must match the engine's
# `P|StockMovement` region spelling under family-first key encoding).
INDEXED_REGION = "P|StockMovement"


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
    # WP-M: derived secondary-access-path tile bytes built for this region.
    # Priced in the reward (endurance) but NOT in the merge-churn budget.
    index_output_bytes: int = 0

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
            index_output_bytes=int(obj.get("index_output_bytes", 0)),
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

    Lower is better. The write term prices ALL bytes the region wrote to storage in the window
    -- WAL ingest (``bytes_written``), reorganisation output (``compaction_bytes``) and the
    derived secondary-access-path build output (``index_output_bytes``) -- at
    ``lambda_ns_per_byte`` ns/byte, whose default is the measured device write cost. The index
    build output is priced here (endurance) but deliberately excluded from the churn budget.
    """
    return (stats.count * stats.mean_ns
            + lambda_ns_per_byte
            * (stats.bytes_written + stats.compaction_bytes + stats.index_output_bytes))


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
    # Cost evidence is per-request cost where the region served requests (load-invariant: a rate
    # step scales count and total cost together, so their ratio is unchanged) and the absolute
    # write cost where it did not (a request-free region's only cost is its reorganisation). The
    # bandit compares MEDIANS of these samples -- not means -- so a one-window latency/compaction
    # spike on a shared device cannot move an argmin and make the learner churn on noise.
    cost_samples: list[float] = field(default_factory=list)
    # Reorganisation evidence is a STABLE property of a candidate (how many bytes its merges
    # write per window), unlike the non-stationary cost evidence, so it is NOT reset by a drift
    # restart -- otherwise a restart would forget that a candidate is expensive and re-admit it.
    compaction_n: int = 0
    sum_compaction_bytes: float = 0.0
    violating: bool = False       # proved to break this region's p99 ceiling

    @property
    def median_cost(self) -> float:
        """Median per-request cost across the sampled windows (``inf`` if unsampled)."""
        if not self.cost_samples:
            return math.inf
        samples = sorted(self.cost_samples)
        mid = len(samples) // 2
        if len(samples) % 2:
            return samples[mid]
        return (samples[mid - 1] + samples[mid]) / 2.0

    @property
    def mean_compaction_bytes(self) -> float:
        """Mean reorganisation bytes/window observed under this candidate (0.0 if unexplored).

        Unexplored arms report 0.0 -- not ``inf`` -- so the reorganisation budget never blocks
        the bounded burn-in from sampling a candidate for the first time; only a switch to a
        candidate *observed* to add reorganisation is refused.
        """
        return self.sum_compaction_bytes / self.compaction_n if self.compaction_n else 0.0


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
    # Churn control (Slice 39): the step index of the last active change.
    last_change_step: int | None = None
    budget_blocks: int = 0        # switches refused because projected added reorg exceeded budget
    cooldown_holds: int = 0       # steps a change was held back by the per-region cooldown
    # WP-O: the movement region has swapped its candidate set to the index-on grid.
    index_flipped: bool = False


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
    margin: float = 0.5
    stability_window: int = 2
    exploration_c: float = math.sqrt(2)
    ceilings_ns: Mapping[str, float] = field(default_factory=dict)
    drift_margin: float = 2.0
    drift_windows: int = 2
    # Targeted drift response: re-explore the incumbent plus the best K-1
    # challengers (None = the full candidate set, the correctness fallback).
    drift_reexplore_candidates: int | None = 2
    seed: int = 0
    # Churn control between re-explorations (Slice 39). A per-region cooldown of
    # `cooldown_windows` steps after a change (0 disables it), and a per-window
    # reorganisation budget shared across regions (None disables it; the runner derives it
    # from the declared arm's measured reorganisation rate).
    cooldown_windows: int = 0
    reorg_budget_bytes_per_window: float | None = None

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
        if self.cooldown_windows < 0:
            raise ValueError("cooldown_windows must be >= 0")
        if self.reorg_budget_bytes_per_window is not None and self.reorg_budget_bytes_per_window < 0:
            raise ValueError("reorg_budget_bytes_per_window must be >= 0 or None")
        self.ceilings_ns = dict(self.ceilings_ns)
        self._tie = _seeded_order(self.candidates, self.seed)
        self._tie_maps: dict[tuple[LayoutCandidate, ...], dict[LayoutCandidate, int]] = {}
        self._regions: dict[str, _RegionState] = {}
        self._step_index = 0
        self._window_added_reorg = 0.0

    # -- state helpers ------------------------------------------------------
    def _candidates_for(self, name: str) -> tuple[LayoutCandidate, ...]:
        """The base candidate set a region explores (the published 2 x 3 grid).

        The `order_no` secondary-access-path axis (WP-M/WP-O) is NOT part of this
        grid: it is offered to ``P|StockMovement`` only as a demand-gated,
        one-shot flip (see :meth:`_flip_index_axis`), so the movement region's
        burn-in stays the same 6 candidates as every other region.
        """
        return tuple(self.candidates)

    def _index_on_candidates(self) -> tuple[LayoutCandidate, ...]:
        """The same published grid with the `order_no` index switched on."""
        return tuple(
            LayoutCandidate(c.page_size, c.merge_min_tiles, index_order_no=True)
            for c in self.candidates
        )

    def _flip_index_axis(self, name: str, stats: RegionStats) -> None:
        """One-shot, demand-gated adoption of the `order_no` index axis (WP-O).

        Called once per window AFTER the window's evidence is recorded against
        the candidate that was active for it. The movement region starts on the
        base grid (index off); the first window in which it actually serves a
        scan request (``count > 0``) justifies the index, so the region's
        candidate set is replaced with the index-on grid and the burn-in restarts
        on that grid. The index is then STICKY for the rest of the run (the
        index-off candidates are gone), so the learner toggles it at most once
        per region per run instead of once per candidate change. A region that
        never serves a scan never flips: the axis is gated on measured demand,
        not on exploration.
        """
        if name != INDEXED_REGION or stats.count <= 0:
            return
        st = self._region(name)
        if st.index_flipped:
            return
        st.index_flipped = True
        indexed = self._index_on_candidates()
        st.arms = {c: _Arm() for c in indexed}
        # A fresh candidate set is a fresh burn-in: leave no stale incumbent,
        # pending gate or reference evidence from the index-off grid (its cost
        # profile is not comparable to the index-on grid's). The next `step`
        # re-adopts the seeded-first index-on candidate and starts the cooldown.
        st.active = None
        st.pending = None
        st.pending_streak = 0
        st.reference_cost = None
        st.deviation_streak = 0
        st.reexploring = False
        st.reexplore_start = None
        st.recovery_windows = None
        st.last_change_step = None

    def _tie_for(self, st: _RegionState) -> dict[LayoutCandidate, int]:
        """The seeded tie-break permutation for a region's candidate set (cached)."""
        cands = tuple(st.arms.keys())
        m = self._tie_maps.get(cands)
        if m is None:
            m = _seeded_order(cands, self.seed)
            self._tie_maps[cands] = m
        return m

    def _region(self, name: str) -> _RegionState:
        st = self._regions.get(name)
        if st is None:
            cands = self._candidates_for(name)
            st = _RegionState(arms={c: _Arm() for c in cands})
            start = DECLARED_DEFAULT if DECLARED_DEFAULT in st.arms else cands[0]
            st.active = start
            self._regions[name] = st
        return st

    def _available(self, st: _RegionState) -> list[LayoutCandidate]:
        return [c for c in st.arms if not st.arms[c].violating]

    def _admissible(self, st: _RegionState) -> list[LayoutCandidate]:
        """Available candidates the reorganisation budget can admit (Slice 39).

        A candidate whose observed reorganisation rate exceeds the current incumbent's by more
        than the per-window budget is not selectable, so it also must not hold the min-sample gate
        open (otherwise a budget-refused candidate would deadlock the burn-in). The incumbent is
        always admitted (projected added 0). With no budget every available candidate is admitted.
        """
        available = self._available(st)
        budget = self.reorg_budget_bytes_per_window
        if budget is None:
            return available
        return [c for c in available
                if c == st.active or self._projected_added_reorg(st, c) <= budget]

    def _in_exploitation(self, st: _RegionState) -> bool:
        admissible = self._admissible(st)
        if not admissible:
            return False
        return min(st.arms[c].n for c in admissible) >= self.min_samples

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
            # WP-O: adopt the order_no index axis on the first window with real
            # scan traffic, AFTER recording that window against the index-off
            # candidate it actually ran under. The flip replaces the candidate
            # set, so the policy written below is index-on for the NEXT window.
            self._flip_index_axis(name, window.regions[name])
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
        # Drift is detected on PER-REQUEST cost (load-invariant): a rate-ladder step scales
        # `count` and `cost` together, so only a genuine access-shape change moves this signal.
        if candidate == st.active and self._in_exploitation(st) and st.reference_cost is not None:
            if stats.count > 0:
                self._note_deviation(st, cost / stats.count)
        arm = st.arms[candidate]
        arm.n += 1
        arm.cost_samples.append(cost / stats.count if stats.count > 0 else cost)
        arm.compaction_n += 1
        arm.sum_compaction_bytes += stats.compaction_bytes

    def step(self, regions: Iterable[str], *, explore: bool = True) -> None:
        """One policy step for each region: burn-in exploration, else the gate.

        ``explore=False`` runs the hysteresis gate only (the min-sample gate stays
        closed until every available candidate is sampled); tests use it to pin
        the gate's behaviour against a fixed evidence set.
        """
        self._step_index += 1
        self._window_added_reorg = 0.0
        for name in regions:
            if not is_policy_region(name):
                continue
            st = self._region(name)
            if explore and self._burn_in_needed(st):
                self._explore(st)
            else:
                self._gate(st)

    # -- churn control (Slice 39) ------------------------------------------
    def _cooled_down(self, st: _RegionState) -> bool:
        """False while the region's per-change cooldown has not elapsed (counts the hold)."""
        if self.cooldown_windows <= 0 or st.last_change_step is None:
            return True
        if self._step_index - st.last_change_step < self.cooldown_windows:
            st.cooldown_holds += 1
            return False
        return True

    def _projected_added_reorg(self, st: _RegionState, challenger: LayoutCandidate) -> float:
        """The challenger's mean reorganisation rate minus the incumbent's, floored at 0.

        An unexplored challenger has ``mean_compaction_bytes == 0`` so it always projects 0
        added bytes -- the bounded burn-in is never blocked by the budget; only a switch to a
        candidate *observed* to write more reorganisation is refused.
        """
        incumbent = st.active
        if incumbent is None or incumbent not in st.arms:
            return 0.0
        return max(0.0, st.arms[challenger].mean_compaction_bytes
                   - st.arms[incumbent].mean_compaction_bytes)

    def _change_allowed(self, st: _RegionState, challenger: LayoutCandidate) -> float | None:
        """The projected added reorganisation if the switch is allowed, else ``None``.

        ``None`` means the per-region cooldown has not elapsed or the switch would exceed this
        window's remaining reorganisation budget. The caller must then hold the incumbent.
        """
        if not self._cooled_down(st):
            return None
        added = self._projected_added_reorg(st, challenger)
        budget = self.reorg_budget_bytes_per_window
        if budget is not None and self._window_added_reorg + added > budget:
            st.budget_blocks += 1
            return None
        return added

    def _set_active(self, st: _RegionState, candidate: LayoutCandidate, added_reorg: float = 0.0) -> None:
        st.active = candidate
        st.last_change_step = self._step_index
        self._window_added_reorg += added_reorg

    # -- gate / explore -----------------------------------------------------
    def _burn_in_needed(self, st: _RegionState) -> bool:
        admissible = self._admissible(st)
        return bool(admissible) and min(st.arms[c].n for c in admissible) < self.min_samples

    def _explore(self, st: _RegionState) -> None:
        available = self._admissible(st)
        if not available:
            return
        if st.active is None or st.active not in available:
            # A disqualified incumbent is a forced constraint switch: never held.
            if st.active is not None:
                st.constraint_switches += 1
            self._set_active(st, self._initial_pick(st, available))
            self._reset_gate(st)
            st.reference_cost = None
            return
        pick = self._ucb_pick(st, available)
        if pick == st.active:
            self._reset_gate(st)
            st.reference_cost = None
            return
        added = self._change_allowed(st, pick)
        if added is not None:
            self._set_active(st, pick, added)
        self._reset_gate(st)
        st.reference_cost = None

    def _gate(self, st: _RegionState) -> None:
        available = self._available(st)
        if not available:
            return
        if st.active is None or st.active not in available:
            if st.active is not None:
                st.constraint_switches += 1
            self._set_active(st, self._initial_pick(st, available))
            self._reset_gate(st)
            st.reference_cost = None
            return
        if not self._in_exploitation(st):
            return  # min-sample gate: a challenger (or the incumbent) is short of evidence
        if st.reference_cost is None:
            st.reference_cost = st.arms[st.active].median_cost
        incumbent = st.active
        incumbent_cost = st.arms[incumbent].median_cost
        others = [c for c in available if c != incumbent]
        if not others:
            self._reset_gate(st)
            return
        # UCB1 names the challenger; the margin below compares MEDIANS (robust,
        # load-invariant), so the exploration bonus can never on its own move the
        # policy and a rate step or one-window spike cannot fabricate an improvement.
        challenger = self._ucb_pick(st, others)
        improvement = (incumbent_cost - st.arms[challenger].median_cost) / max(abs(incumbent_cost), 1.0)
        if improvement > self.margin:
            if st.pending == challenger:
                st.pending_streak += 1
            else:
                st.pending = challenger
                st.pending_streak = 1
            if st.pending_streak >= self.stability_window:
                added = self._change_allowed(st, challenger)
                if added is None:
                    # Cooldown/budget refused: hold the incumbent. Dropping the pending
                    # candidate makes the streak restart from the current evidence.
                    self._reset_gate(st)
                    return
                self._set_active(st, challenger, added)
                st.gated_switches += 1
                self._reset_gate(st)
                st.reference_cost = None
        else:
            self._reset_gate(st)

    def _initial_pick(self, st: _RegionState, available: list[LayoutCandidate]) -> LayoutCandidate:
        tie = self._tie_for(st)
        gated = [c for c in available if st.arms[c].n >= self.min_samples]
        if gated:
            return min(gated, key=lambda c: (st.arms[c].median_cost, tie[c]))
        return min(available, key=lambda c: (st.arms[c].n, tie[c]))

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
        scale = max((st.arms[c].median_cost for c in eligible if st.arms[c].n > 0), default=1.0)
        scale = max(scale, 1.0)
        tie = self._tie_for(st)

        def lcb(c: LayoutCandidate) -> float:
            arm = st.arms[c]
            if arm.n < self.min_samples:
                return -math.inf
            return arm.median_cost - self.exploration_c * scale * math.sqrt(log_total / arm.n)

        return min(eligible, key=lambda c: (lcb(c), tie[c]))

    # -- drift --------------------------------------------------------------
    def _note_deviation(self, st: _RegionState, cost: float) -> None:
        reference = st.reference_cost
        if reference is None:
            return
        deviation = abs(cost - reference) / max(abs(reference), 1.0)
        if deviation > self.drift_margin:
            st.deviation_streak += 1
            if st.deviation_streak >= self.drift_windows and self._cooled_down(st):
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
            st.arms[candidate].cost_samples.clear()
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
        admissible = set(self._admissible(st))
        k = self.drift_reexplore_candidates
        if k is None or k >= len(admissible):
            return admissible
        tie = self._tie_for(st)
        ordered: list[LayoutCandidate] = []
        if st.active is not None and st.active in admissible:
            ordered.append(st.active)
        ordered.extend(sorted(
            (c for c in admissible if c != st.active),
            key=lambda c: (st.arms[c].median_cost, tie[c]),
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

    def budget_blocks(self) -> dict[str, int]:
        """Steps a region's change was refused because it exceeded the reorganisation budget."""
        return {name: st.budget_blocks for name, st in self._regions.items()}

    def cooldown_holds(self) -> dict[str, int]:
        """Steps a region's change was held back by the per-change cooldown."""
        return {name: st.cooldown_holds for name, st in self._regions.items()}

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
