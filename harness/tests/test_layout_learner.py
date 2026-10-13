"""Tests for the per-region layout learner and its controls (Slice 33).

Everything here runs against **simulated rewards** -- a region -> candidate ->
``(mean_ns, bytes_written, p99_ns)`` table fed through the real
:class:`WindowTelemetry` / :func:`run_controller` paths -- so there is no engine
and no long run. The scored behaviours are: a genuine per-region gradient is
found; a no-gradient workload is not churned; the min-sample gate and the
hysteresis window gate switches; a p99-violating candidate is never selectable;
``flashdb-static-derived`` does not adapt after drift while the learner does; and
the whole thing is deterministic for a fixed seed.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import layout_learner as ll  # noqa: E402

A = ll.DECLARED_DEFAULT                       # LayoutCandidate(4096, 2)
B = ll.LayoutCandidate(4096, 8)
C = ll.LayoutCandidate(4096, 200)


def stats(region: str, *, count: int = 100, mean_ns: float = 1000.0,
          bytes_written: int = 0, compaction_bytes: int = 0,
          index_output_bytes: int = 0, p99_ns: float = 0.0) -> ll.RegionStats:
    return ll.RegionStats(
        region=region, probes=count, probe_misses=0, bytes_read=0,
        bytes_written=bytes_written, commit_blocks=count, commit_ops=count,
        count=count, mean_ns=mean_ns, p50_ns=mean_ns, p99_ns=p99_ns,
        compaction_bytes=compaction_bytes, index_output_bytes=index_output_bytes,
    )


def window(index: int, tables: dict, selection: dict) -> ll.WindowTelemetry:
    """One simulated metrics window: each region reports its active candidate's row."""
    rows = {}
    for region, table in tables.items():
        candidate = selection.get(region, ll.DECLARED_DEFAULT)
        mean_ns, bytes_written, compaction_bytes, p99_ns = table[candidate]
        rows[region] = stats(region, mean_ns=mean_ns, bytes_written=bytes_written,
                             compaction_bytes=compaction_bytes, p99_ns=p99_ns)
    return ll.WindowTelemetry(window=index, rate_rps=1000.0, regions=rows)


def uniform_table(base_mean: float, best: ll.LayoutCandidate | None = None,
                  best_mean: float = 0.0, *, bytes_written: int = 0,
                  compaction_bytes: int = 0, p99_ns: float = 0.0) -> dict:
    return {
        c: (best_mean if c == best else base_mean, bytes_written, compaction_bytes, p99_ns)
        for c in ll.DEFAULT_CANDIDATES
    }


def drive(controller, tables: dict, windows: int, *, start: int = 0) -> list[dict]:
    """Feed ``windows`` simulated windows, respecting the last written policy."""
    selection: dict = {}
    trace = []
    for i in range(start, start + windows):
        w = window(i, tables, selection)
        controller.observe(w)
        selection = controller.selections()
        trace.append(dict(selection))
    return trace


def selection_from_document(document: dict, regions) -> dict:
    out = {}
    for region in regions:
        entry = document["regions"].get(region, document["default"])
        out[region] = ll.LayoutCandidate(
            entry["page_size"], entry["merge_min_tiles"],
            bool(entry.get("index_order_no", False)))
    return out


# ---------------------------------------------------------------------------
# Candidate set and reward proxy
# ---------------------------------------------------------------------------
def test_candidate_set_is_the_published_2x3_grid():
    assert len(ll.DEFAULT_CANDIDATES) == 6
    assert {c.page_size for c in ll.DEFAULT_CANDIDATES} == {4096, 65536}
    assert {c.merge_min_tiles for c in ll.DEFAULT_CANDIDATES} == {2, 8, 200}
    assert ll.DECLARED_DEFAULT in ll.DEFAULT_CANDIDATES


def test_order_no_index_axis_is_offered_only_for_the_movement_region():
    learner = ll.LayoutLearner(seed=0)
    # The base candidate set stays 6 everywhere.
    assert learner._candidates_for("P|Stock") == ll.DEFAULT_CANDIDATES
    # The movement primary region doubles to 12: the same 6 layouts, with and
    # without the order_no secondary access path.
    movement = learner._candidates_for(ll.INDEXED_REGION)
    assert len(movement) == 12
    assert {c.index_order_no for c in movement} == {False, True}
    assert movement[:6] == ll.DEFAULT_CANDIDATES
    # Every indexed candidate is the same page/merge grid with the flag set.
    assert all(c.page_size in (4096, 65536) and c.merge_min_tiles in (2, 8, 200)
               for c in movement)


def test_policy_document_emits_the_order_no_index_flag():
    indexed = ll.LayoutCandidate(65536, 2, index_order_no=True)
    doc = ll.policy_document({"P|StockMovement": indexed})
    assert doc["regions"]["P|StockMovement"]["index_order_no"] is True
    assert "index_order_no" not in ll.policy_document({"P|Stock": ll.DECLARED_DEFAULT})["regions"]["P|Stock"]
    # An entry() never emits the flag unless it is selected.
    assert ll.DECLARED_DEFAULT.entry() == {"page_size": 4096, "merge_min_tiles": 2}


def test_reward_proxy_is_service_time_plus_lambda_times_bytes():
    row = stats("P|Stock", count=10, mean_ns=200.0, bytes_written=500)
    assert ll.region_cost_ns(row, 1.0) == 10 * 200.0 + 500
    # lambda prices endurance: a bigger lambda makes a write-heavy candidate costlier
    assert ll.region_cost_ns(row, 4.0) == 10 * 200.0 + 4 * 500


def test_reward_prices_the_order_no_index_build_output_but_not_as_merge_churn():
    row = stats("P|StockMovement", count=10, mean_ns=200.0, bytes_written=500,
                index_output_bytes=2_000_000)
    assert ll.region_cost_ns(row, 1.0) == 10 * 200.0 + 500 + 2_000_000
    # The index build output is priced in the reward but does NOT enter the
    # churn budget, which keys off the merge reorganisation mean only.
    arm = ll._Arm()
    arm.n = 1
    arm.compaction_n = 1
    arm.sum_compaction_bytes = 0.0
    assert arm.mean_compaction_bytes == 0.0


def test_reward_prices_reorganisation_bytes_at_the_device_cost_unit():
    """Slice 39: the endurance term prices ALL bytes written, including the reorganisation
    output the merge knob moves, and the default lambda is the measured device write cost."""
    # 1 byte / 831 MiB/s, i.e. the nanoseconds of device write bandwidth a byte consumes.
    assert ll.DEVICE_WRITE_NS_PER_BYTE == pytest.approx(1e9 / (831 * 2 ** 20))
    assert ll.DEFAULT_LAMBDA_NS_PER_BYTE == ll.DEVICE_WRITE_NS_PER_BYTE
    row = stats("P|Stock", count=0, mean_ns=0.0, bytes_written=100, compaction_bytes=900)
    # ingest + reorganisation are both charged, at the device-cost unit by default
    assert ll.region_cost_ns(row, 1.0) == 1000
    assert ll.region_cost_ns(row) == pytest.approx(1000 * ll.DEVICE_WRITE_NS_PER_BYTE)


def test_endurance_weight_sweep_over_two_orders_changes_the_policy():
    """Sweeping lambda over 0.1 -> 10 on the SAME reward sequence must change the policy.

    The trap candidate has a cheaper read but writes far more reorganisation; the low weight buys
    the cheap read, the high weight refuses it. This is the measurement the slice-37 run could not
    make when the priced write term was the candidate-invariant WAL ingest.
    """
    region = "P|Stock"
    declared = ll.DECLARED_DEFAULT                     # 4096 / 2
    trap = ll.LayoutCandidate(4096, 200)               # cheap read, huge reorganisation
    table = {c: (5000.0, 0, 0, 0.0) for c in ll.DEFAULT_CANDIDATES}
    table[declared] = (2000.0, 0, 0, 0.0)
    table[trap] = (1000.0, 0, 500_000, 0.0)
    tables = {region: table}

    def final(lam: float):
        learner = ll.LayoutLearner(lambda_ns_per_byte=lam, min_samples=2, stability_window=2,
                                   margin=0.05, drift_margin=10.0, seed=1)
        drive(learner, tables, windows=60)
        return learner.selections()[region]

    low = final(0.1)     # 0.1 x the device cost: the cheap read wins
    high = final(10.0)   # 10 x: the reorganisation writes dominate
    assert low == trap, f"low endurance weight should buy the cheap read, got {low}"
    assert high == declared, f"high endurance weight should refuse the trap, got {high}"
    assert low != high, "sweeping lambda over two orders did not change the learned policy"


def test_candidate_set_is_configurable():
    learner = ll.LayoutLearner(candidates=(A, B))
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    assert set(learner.pulls("P|Stock")) == {A, B}


def test_folded_other_region_is_ignored():
    learner = ll.LayoutLearner()
    w = ll.WindowTelemetry(window=0, rate_rps=1.0,
                           regions={"other": stats("other"), "P|Stock": stats("P|Stock")})
    learner.observe(w)
    assert "other" not in learner.selections()
    assert "P|Stock" in learner.selections()


# ---------------------------------------------------------------------------
# The learner
# ---------------------------------------------------------------------------
def test_finds_the_best_candidate_in_a_synthetic_environment():
    best_stock = ll.LayoutCandidate(65536, 8)
    best_customer = ll.LayoutCandidate(4096, 200)
    tables = {
        "P|Stock": uniform_table(5000.0, best_stock, 1000.0),
        "U|Customer": uniform_table(4000.0, best_customer, 900.0),
    }
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=1)
    drive(learner, tables, windows=40)
    assert learner.selections()["P|Stock"] == best_stock
    assert learner.selections()["U|Customer"] == best_customer


def test_no_churn_when_all_candidates_are_equal():
    tables = {"P|Stock": uniform_table(3000.0)}
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=3)
    trace = drive(learner, tables, windows=60)
    burn_in = len(ll.DEFAULT_CANDIDATES) * 2
    # Exploration is a bounded, named burn-in; after it the committed policy never moves.
    assert all(t == trace[-1] for t in trace[burn_in:])
    assert sum(learner.gated_switches().values()) == 0
    assert sum(learner.restarts().values()) == 0


def test_min_sample_gate_delays_a_switch():
    region = "P|Stock"
    learner = ll.LayoutLearner(candidates=(A, B), min_samples=3, margin=0.05,
                               stability_window=1, drift_margin=10.0, seed=0)
    for _ in range(2):
        learner.record(region, A, stats(region, mean_ns=5000.0))
    for _ in range(2):
        learner.record(region, B, stats(region, mean_ns=1000.0))
    learner.step([region], explore=False)
    assert learner.selections()[region] == A
    assert learner.gated_switches().get(region, 0) == 0

    learner.record(region, B, stats(region, mean_ns=1000.0))   # B reaches min_samples
    learner.step([region], explore=False)
    assert learner.selections()[region] == A                     # incumbent still short
    assert learner.gated_switches().get(region, 0) == 0

    learner.record(region, A, stats(region, mean_ns=5000.0))     # incumbent reaches too
    learner.step([region], explore=False)
    assert learner.selections()[region] == B
    assert learner.gated_switches()[region] == 1


def test_hysteresis_resists_a_one_window_noise_spike():
    region = "P|Stock"

    def cheap():
        return stats(region, mean_ns=100.0)

    def good():
        return stats(region, mean_ns=1000.0)

    def bad():
        return stats(region, mean_ns=5000.0)

    # One very cheap window among three expensive ones does not move the challenger's mean.
    spiky = ll.LayoutLearner(candidates=(A, B), min_samples=3, margin=0.05,
                             stability_window=2, drift_margin=10.0, seed=0)
    for _ in range(3):
        spiky.record(region, A, good())
    spiky.record(region, B, cheap())
    spiky.record(region, B, bad())
    spiky.record(region, B, bad())
    spiky.step([region], explore=False)
    assert spiky.selections()[region] == A
    assert spiky.gated_switches().get(region, 0) == 0

    # A sustained improvement switches, but only after the stability window.
    sustained = ll.LayoutLearner(candidates=(A, B), min_samples=3, margin=0.05,
                                 stability_window=2, drift_margin=10.0, seed=0)
    for _ in range(3):
        sustained.record(region, A, bad())
    for _ in range(3):
        sustained.record(region, B, good())
    sustained.step([region], explore=False)
    assert sustained.selections()[region] == A
    assert sustained.gated_switches().get(region, 0) == 0
    sustained.step([region], explore=False)
    assert sustained.selections()[region] == B
    assert sustained.gated_switches()[region] == 1


def test_p99_constraint_excludes_a_violating_candidate():
    region = "P|Stock"
    tempting = ll.LayoutCandidate(65536, 200)
    ceiling = 2000.0
    table = {}
    for c in ll.DEFAULT_CANDIDATES:
        table[c] = (100.0, 0, 0, 9000.0) if c == tempting else (1000.0, 0, 0, 500.0)
    learner = ll.LayoutLearner(min_samples=1, margin=0.05, stability_window=1,
                               drift_margin=10.0, ceilings_ns={region: ceiling}, seed=0)

    selection: dict = {}
    for i in range(40):
        w = window(i, {region: table}, selection)
        learner.observe(w)
        selection = learner.selections()
        # The engine must run a candidate once to learn it violates the ceiling; from
        # that window on it is never selectable again (the hard constraint).
        if tempting in learner.violating_candidates(region):
            assert selection[region] != tempting, "a p99-violating candidate was selectable"
    assert tempting in learner.violating_candidates(region)
    assert learner.selections()[region] != tempting
    assert learner.constraint_switches().get(region, 0) >= 1


def test_learner_ignores_a_region_without_a_declared_ceiling():
    # With no ceiling declared the constraint is inert: the cheapest candidate wins.
    region = "P|Stock"
    best = ll.LayoutCandidate(65536, 200)
    tables = {region: uniform_table(5000.0, best, 100.0, p99_ns=99_000.0)}
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=1)
    drive(learner, tables, windows=30)
    assert learner.selections()[region] == best


def test_determinism_same_seed_same_decisions():
    tables = {"P|Stock": uniform_table(5000.0, C, 1000.0),
              "U|Stock": uniform_table(3000.0)}
    first = drive(ll.LayoutLearner(min_samples=2, seed=11), tables, windows=30)
    second = drive(ll.LayoutLearner(min_samples=2, seed=11), tables, windows=30)
    assert first == second


def test_seed_only_orders_exact_ties():
    # The seed is used nowhere but the tie-break permutation, and that permutation
    # is stable for a fixed seed and a permutation of the candidate set.
    learner = ll.LayoutLearner(seed=0)
    assert learner.candidate_order == ll.LayoutLearner(seed=0).candidate_order
    assert sorted(learner.candidate_order) == sorted(ll.DEFAULT_CANDIDATES)
    seeds = {tuple(ll.LayoutLearner(seed=s).candidate_order) for s in range(8)}
    assert len(seeds) > 1, "the seed must actually move the tie order"


# ---------------------------------------------------------------------------
# The controls
# ---------------------------------------------------------------------------
def test_declared_control_is_one_fixed_candidate_everywhere():
    declared = ll.DeclaredController(candidate=C)
    w = window(0, {"P|Stock": uniform_table(1.0)}, {})
    declared.observe(w)
    assert declared.selections() == {"P|Stock": C}
    assert declared.policy_document()["default"] == {
        "page_size": C.page_size, "bloom_bits_per_key": 10, "merge_min_tiles": C.merge_min_tiles}
    # never adapts, even given the same window repeatedly
    declared.observe(w)
    assert declared.selections() == {"P|Stock": C}


def test_static_derived_picks_the_cheapest_from_the_pre_drift_sweep():
    region = "P|Stock"
    sweep = {region: {
        A: stats(region, mean_ns=5000.0),
        B: stats(region, mean_ns=1000.0),
        C: stats(region, mean_ns=9000.0),
    }}
    static = ll.StaticDerivedController(candidates=(A, B, C))
    static.derive(sweep)
    assert static.selections()[region] == B


def test_static_derived_does_not_adapt_after_drift_while_learner_does():
    region = "P|Stock"
    old_best = ll.LayoutCandidate(4096, 2)     # A
    new_best = ll.LayoutCandidate(65536, 200)  # best only after the drift
    pre = uniform_table(5000.0, old_best, 1000.0)
    post = uniform_table(5000.0, new_best, 1000.0)
    post[old_best] = (6000.0, 0, 0, 0.0)       # the old best degrades: the drift signal

    sweep = {region: {c: stats(region, mean_ns=pre[c][0]) for c in pre}}
    static = ll.StaticDerivedController()
    static.derive(sweep)
    assert static.selections()[region] == old_best

    # Full re-exploration (None): the new best is not the pre-drift runner-up
    # (all non-incumbents were tied), so the complete candidate set must be
    # re-sampled. This pins the correctness fallback of the targeted response.
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=0.25, drift_windows=2,
                               drift_reexplore_candidates=None, seed=1)
    selection: dict = {}
    for i in range(80):
        table = pre if i < 30 else post
        w = window(i, {region: table}, selection)
        learner.observe(w)
        static.observe(w)
        selection = learner.selections()

    assert static.selections()[region] == old_best   # frozen: no adaptation, ever
    assert learner.selections()[region] == new_best  # the learner re-explored and moved
    assert learner.restarts().get(region, 0) >= 1


# ---------------------------------------------------------------------------
# Slice 34: the targeted drift response
# ---------------------------------------------------------------------------
def test_drift_reexplores_only_the_changed_regions():
    """A drift in two of five regions re-explores only those two; the other
    three keep their committed candidate untouched."""
    regions = ["P|A", "P|B", "P|C", "P|D", "P|E"]
    stable = {A: (1000.0, 0, 0, 0.0), B: (5000.0, 0, 0, 0.0)}
    drifted = {A: (9000.0, 0, 0, 0.0), B: (1000.0, 0, 0, 0.0)}   # A worsens, B improves
    learner = ll.LayoutLearner(candidates=(A, B), min_samples=2, stability_window=2,
                               margin=0.05, drift_margin=0.25, drift_windows=2,
                               drift_reexplore_candidates=2, seed=0)

    drive(learner, {r: stable for r in regions}, windows=10)   # burn-in + commit A
    committed = dict(learner.selections())
    assert set(committed.values()) == {A}

    post = {r: stable for r in regions}
    post["P|A"] = drifted
    post["P|B"] = drifted
    drive(learner, post, windows=14, start=10)

    changed = {"P|A", "P|B"}
    restarts = learner.restarts()
    assert {r for r in regions if restarts.get(r, 0) > 0} == changed, restarts
    for r in ("P|C", "P|D", "P|E"):
        assert learner.selections()[r] == committed[r] == A, f"unchanged region {r} moved"
    # The two drifted regions were targeted and re-committed their new best.
    assert learner.selections()["P|A"] == B and learner.selections()["P|B"] == B
    assert learner.reexploring_regions() == set(), "every epoch closed"


def test_targeted_drift_response_window_count_and_the_two_window_gap():
    """The response window count is measured here, not asserted in prose.

    With the published default (``drift_reexplore_candidates=2``, a 2-candidate
    set) the targeted response costs ``K * min_samples`` windows; the full
    re-exploration costs ``candidates * min_samples``. Both are reported by the
    assertion so the pre-registration's 2-window target can be judged.
    """
    region = "P|Stock"

    def measure(learner: ll.LayoutLearner, pre: dict, post: dict) -> int:
        selection: dict = {}
        # Enough pre-drift windows for the burn-in to complete before the drift
        # boundary, so the reference is a clean pre-drift baseline.
        pre_windows = len(learner.candidates) * min_samples
        for i in range(pre_windows):
            learner.observe(window(i, {region: pre}, selection))
            selection = learner.selections()
        for i in range(pre_windows, 60):
            learner.observe(window(i, {region: post}, selection))
            selection = learner.selections()
        assert region not in learner.reexploring_regions()
        return learner.recovery_windows()[region]

    min_samples = 2
    targeted = ll.LayoutLearner(candidates=(A, B), min_samples=min_samples,
                                stability_window=2, margin=0.05, drift_margin=0.25,
                                drift_windows=2, drift_reexplore_candidates=2, seed=0)
    targeted_windows = measure(targeted, {A: (1000.0, 0, 0, 0.0), B: (5000.0, 0, 0, 0.0)},
                               {A: (9000.0, 0, 0, 0.0), B: (1000.0, 0, 0, 0.0)})
    assert targeted_windows == 2 * min_samples, (
        f"targeted drift response took {targeted_windows} windows "
        f"(K=2 x min_samples={min_samples}); the pre-registration target is 2")

    new_best = ll.LayoutCandidate(65536, 200)
    full = ll.LayoutLearner(candidates=ll.DEFAULT_CANDIDATES, min_samples=min_samples,
                            stability_window=2, margin=0.05, drift_margin=0.25,
                            drift_windows=2, drift_reexplore_candidates=None, seed=0)
    full_windows = measure(full, uniform_table(5000.0, A, 1000.0),
                           uniform_table(5000.0, new_best, 1000.0))
    assert full_windows == len(ll.DEFAULT_CANDIDATES) * min_samples, (
        f"full re-exploration took {full_windows} windows "
        f"({len(ll.DEFAULT_CANDIDATES)} candidates x min_samples={min_samples}); "
        f"the targeted response ({targeted_windows}) must be shorter")
    assert targeted_windows < full_windows


def test_drift_reexplore_candidates_is_configurable_and_validated():
    with pytest.raises(ValueError):
        ll.LayoutLearner(drift_reexplore_candidates=0)
    assert ll.LayoutLearner(drift_reexplore_candidates=None).drift_reexplore_candidates is None


# ---------------------------------------------------------------------------
# Slice 39: churn control between re-explorations
# ---------------------------------------------------------------------------
def test_churn_parameters_are_validated():
    with pytest.raises(ValueError):
        ll.LayoutLearner(cooldown_windows=-1)
    with pytest.raises(ValueError):
        ll.LayoutLearner(reorg_budget_bytes_per_window=-1.0)


def test_cooldown_blocks_rapid_reswitching():
    """After a change the region holds its candidate for `cooldown_windows` steps."""
    region = "P|Stock"
    learner = ll.LayoutLearner(candidates=(A, B), min_samples=1, margin=0.05,
                               stability_window=1, drift_margin=10.0, cooldown_windows=4, seed=0)
    # Establish B as the cheap candidate and commit the A -> B switch at step 1.
    learner.record(region, A, stats(region, mean_ns=5000.0))
    learner.record(region, B, stats(region, mean_ns=100.0))
    learner.step([region], explore=False)
    assert learner.selections()[region] == B
    assert learner.gated_switches()[region] == 1
    # Now A (the incumbent's rival) becomes cheap, so the gate wants B -> A. The cooldown
    # (4 steps) must hold it: while it is held the selection stays B and the hold is counted.
    for _ in range(3):
        learner.record(region, A, stats(region, mean_ns=100.0))
        learner.record(region, B, stats(region, mean_ns=5000.0))
        learner.step([region], explore=False)
        assert learner.selections()[region] == B, "cooldown must hold the switch"
    assert sum(learner.cooldown_holds().values()) >= 1
    # Once the cooldown elapses the same evidence is allowed to move the policy, exactly once.
    for _ in range(3):
        learner.record(region, A, stats(region, mean_ns=100.0))
        learner.record(region, B, stats(region, mean_ns=5000.0))
        learner.step([region], explore=False)
    assert learner.selections()[region] == A
    assert learner.gated_switches()[region] == 2


def test_reorg_budget_blocks_a_high_rewrite_switch_and_leaves_burn_in_alone():
    """The budget refuses a switch whose projected added reorganisation exceeds it.

    Lambda=0 here so the COST term does not itself reject the trap: the reorganisation budget is
    the only thing standing between the learner and a candidate that writes 1000x more per window.
    """
    region = "P|Stock"
    cheap_read_high_write = C   # (4096, 200): cheap read, writes 1e6 B/window
    learner = ll.LayoutLearner(candidates=(A, C), lambda_ns_per_byte=0.0, min_samples=1,
                               stability_window=1, margin=0.05, drift_margin=10.0,
                               reorg_budget_bytes_per_window=10_000.0, seed=0)
    for _ in range(6):
        learner.record(region, A, stats(region, mean_ns=5000.0, compaction_bytes=1000))
        learner.step([region], explore=False)
    assert learner.selections()[region] == A
    # C is cheaper to read but its observed reorganisation (1e6 - 1000) dwarfs the 10k budget.
    for _ in range(6):
        learner.record(region, C, stats(region, mean_ns=100.0, compaction_bytes=1_000_000))
        learner.step([region], explore=False)
    assert learner.selections()[region] == A, "budget must refuse the high-rewrite switch"
    assert learner.budget_blocks()[region] >= 1
    # With the budget disabled the same evidence moves the policy: the control is what refused it.
    unbudgeted = ll.LayoutLearner(candidates=(A, C), lambda_ns_per_byte=0.0, min_samples=1,
                                  stability_window=1, margin=0.05, drift_margin=10.0, seed=0)
    for _ in range(6):
        unbudgeted.record(region, A, stats(region, mean_ns=5000.0, compaction_bytes=1000))
        unbudgeted.step([region], explore=False)
    for _ in range(6):
        unbudgeted.record(region, C, stats(region, mean_ns=100.0, compaction_bytes=1_000_000))
        unbudgeted.step([region], explore=False)
    assert unbudgeted.selections()[region] == C


def test_budget_does_not_block_the_bounded_burn_in():
    """Burn-in must still sample every candidate (unexplored projects 0 added bytes)."""
    region = "P|Stock"
    table = {c: (5000.0, 0, 0, 0.0) for c in ll.DEFAULT_CANDIDATES}
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05, drift_margin=10.0,
                               reorg_budget_bytes_per_window=1.0, seed=0)
    drive(learner, {region: table}, windows=40)
    assert all(n >= 2 for n in learner.pulls(region).values()), learner.pulls(region)


def test_churn_control_keeps_reorg_within_3x_declared_on_a_drift_sequence():
    """A drifted reward sequence: with churn control the learner's reorganisation stays <=3x the
    declared arm's; without it the same sequence churns far past the pre-registered bound.

    Lambda=0 so the cost term is read-only and the reorganisation budget is the deciding control
    (the slice-37/38 regime where the priced write term did not bite). Compaction bytes are
    attributed to the candidate active for the window -- the physical attribution, since a merge
    threshold only gates FUTURE merges.
    """
    region = "P|Stock"
    declared = ll.DECLARED_DEFAULT
    trap = ll.LayoutCandidate(4096, 200)
    genuine = ll.LayoutCandidate(4096, 8)
    DECLARED_COMP, GENUINE_COMP, TRAP_COMP = 200_000, 200_000, 2_000_000

    def rows(stage: str) -> dict:
        out = {c: (9000.0, 0, DECLARED_COMP, 0.0) for c in ll.DEFAULT_CANDIDATES}
        out[declared] = (2000.0, 0, DECLARED_COMP, 0.0)
        out[genuine] = (1000.0, 0, GENUINE_COMP, 0.0)
        out[trap] = (9000.0, 0, TRAP_COMP, 0.0)
        if stage == "post":
            # The drift makes the trap's read cheapest; the genuine best becomes expensive.
            out[declared] = (5000.0, 0, DECLARED_COMP, 0.0)
            out[genuine] = (5000.0, 0, GENUINE_COMP, 0.0)
            out[trap] = (100.0, 0, TRAP_COMP, 0.0)
        return out

    def reorg_for(learner: ll.LayoutLearner, windows: int, drift_at: int) -> tuple[int, int]:
        selection: dict = {}
        learned = declared_reorg = 0
        for i in range(windows):
            table = rows("post" if i >= drift_at else "pre")
            active = selection.get(region, declared)
            learned += table[active][2]
            declared_reorg += table[declared][2]
            learner.observe(window(i, {region: table}, selection))
            selection = learner.selections()
        return learned, declared_reorg

    windows, drift_at = 90, 30
    controlled = ll.LayoutLearner(lambda_ns_per_byte=0.0, min_samples=2, stability_window=2,
                                  margin=0.05, drift_margin=0.25, drift_windows=2,
                                  drift_reexplore_candidates=None,
                                  cooldown_windows=4, reorg_budget_bytes_per_window=1_000_000.0,
                                  seed=1)
    c_learned, c_declared = reorg_for(controlled, windows, drift_at)
    assert c_learned <= 3 * c_declared, (
        f"churn-controlled reorg {c_learned} B must stay within 3x declared {c_declared} B "
        f"(ratio {c_learned / c_declared:.2f}): restarts={controlled.restarts()} "
        f"selections={ {k: str(v) for k, v in controlled.selections().items()} }")

    uncontrolled = ll.LayoutLearner(lambda_ns_per_byte=0.0, min_samples=2, stability_window=2,
                                    margin=0.05, drift_margin=0.25, drift_windows=2,
                                    drift_reexplore_candidates=None, seed=1)
    u_learned, u_declared = reorg_for(uncontrolled, windows, drift_at)
    assert u_learned > 3 * u_declared, (
        "without churn control the drifted sequence must exceed 3x declared, proving the control "
        f"is what bounds it (got {u_learned} B vs declared {u_declared} B)")


def test_no_churn_when_all_candidates_are_equal_with_controls_enabled():
    """The existing no-churn property holds with the slice-39 controls enabled."""
    region = "P|Stock"
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, cooldown_windows=3,
                               reorg_budget_bytes_per_window=1000.0, seed=3)
    trace = drive(learner, {region: uniform_table(3000.0)}, windows=60)
    burn_in = len(ll.DEFAULT_CANDIDATES) * 2 + len(ll.DEFAULT_CANDIDATES) * 3
    assert all(t == trace[-1] for t in trace[burn_in:])
    assert sum(learner.gated_switches().values()) == 0
    assert sum(learner.restarts().values()) == 0


# ---------------------------------------------------------------------------
# WP-J: the adversarial-uniform churn fix (per-request, median-scored bandit)
# ---------------------------------------------------------------------------
def test_drift_is_load_invariant_a_rate_step_does_not_fire():
    """A rate-ladder step scales `count`, not per-request cost: it must not look like drift.

    The pre-change learner keyed drift on TOTAL window cost (``count * mean_ns``), so doubling the
    offered rate (and with it the region's request count) fired a spurious drift restart at every
    rung boundary -- the WP-H churn on adversarial-uniform. Per-request cost makes the signal
    load-invariant: a rate step now leaves it unchanged.
    """
    region = "P|Stock"
    learner = ll.LayoutLearner(min_samples=2, stability_window=1, margin=0.05,
                               drift_margin=0.5, drift_windows=2, seed=0)

    def w(i: int, count: int) -> ll.WindowTelemetry:
        return ll.WindowTelemetry(window=i, rate_rps=float(count), regions={
            region: stats(region, count=count, mean_ns=5000.0)})

    for i in range(24):
        learner.observe(w(i, 100))          # burn-in completes and the gate opens at the low rung
    assert learner.restarts().get(region, 0) == 0
    for i in range(24, 30):
        learner.observe(w(i, 200))          # offered rate doubles; per-request cost is unchanged
    assert learner.restarts().get(region, 0) == 0, learner.restarts()


def test_median_cost_holds_the_incumbent_through_a_one_window_spike():
    """A single shared-device latency spike must not make the gate switch to a noise-cheaper rival.

    The pre-change learner scored arms by MEAN cost, so one huge spike inflated the incumbent's
    mean and made an otherwise-worse rival look cheaper by >margin. The MEDIAN ignores the outlier,
    which is what lets a no-locality workload settle instead of churn.
    """
    region = "P|Stock"
    learner = ll.LayoutLearner(candidates=(A, B), min_samples=3, margin=0.05,
                               stability_window=1, drift_margin=10.0, seed=0)
    for _ in range(3):
        learner.record(region, A, stats(region, mean_ns=5000.0))
    for _ in range(3):
        learner.record(region, B, stats(region, mean_ns=5100.0))
    learner.step([region], explore=False)   # gate open; B is not 5% cheaper than A
    assert learner.selections()[region] == A
    learner.record(region, A, stats(region, mean_ns=500_000.0))   # one stall window
    learner.step([region], explore=False)
    assert learner.selections()[region] == A
    assert learner.gated_switches().get(region, 0) == 0


# ---------------------------------------------------------------------------
# The driver loop + telemetry parsing
# ---------------------------------------------------------------------------
def test_driver_loop_runs_against_simulated_rewards():
    region = "P|Stock"
    best = ll.LayoutCandidate(65536, 200)
    tables = {region: uniform_table(1000.0, best, 100.0)}
    learner = ll.LayoutLearner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=1)

    state = {"selection": {}, "written": [], "read": 0}

    def read_telemetry():
        if state["read"] >= 40:
            return None
        w = window(state["read"], tables, state["selection"])
        state["read"] += 1
        return w

    def write_policy(document):
        state["written"].append(document)
        state["selection"] = selection_from_document(document, tables)

    history = ll.run_controller(read_telemetry, write_policy, learner)
    assert len(history) == 40
    assert len(state["written"]) == 40
    assert all(doc["default"]["page_size"] == 4096 for doc in state["written"])
    assert learner.selections()[region] == best
    # the last written policy matches the learner's final selection
    assert state["written"][-1]["regions"][region] == best.entry()


def test_parse_window_line_reads_the_slice26_schema():
    line = json.dumps({
        "window": 3, "rate_rps": 120.0, "p50_us": 500, "p99_us": 1500,
        "current": {"patterns": [], "regions": [
            {"region": "P|Stock", "probes": 191, "probe_misses": 0, "bytes_read": 796070,
             "bytes_written": 11109, "commit_blocks": 161, "commit_ops": 161,
             "count": 191, "mean_ns": 190000.0, "p50_ns": 180000.0, "p99_ns": 251000.0},
        ]},
        "previous": None,
    })
    parsed = ll.parse_window_line(line)
    assert parsed is not None
    assert parsed.window == 3 and parsed.rate_rps == 120.0
    row = parsed.regions["P|Stock"]
    assert row.bytes_read == 796070 and row.bytes_written == 11109
    assert row.count == 191 and row.mean_ns == 190000.0 and row.p99_ns == 251000.0
    # Slice 34 compaction fields parse from the frozen dump, and the rewrite
    # factor is derivable only when the region ingested bytes in the window.
    computed = ll.parse_window_line(json.dumps({
        "window": 4, "rate_rps": 120.0,
        "current": {"patterns": [], "regions": [
            {"region": "P|Stock", "bytes_written": 40,
             "compaction_bytes": 4288, "compaction_merges": 1},
        ]},
        "previous": None,
    }))
    c = computed.regions["P|Stock"]
    assert c.compaction_bytes == 4288 and c.compaction_merges == 1
    assert ll.region_rewrite_factor(c) == 4288 / 40
    assert ll.region_rewrite_factor(row) == 0 / 11109
    assert ll.region_rewrite_factor(stats("P|Stock", bytes_written=0)) is None
    # non-JSON header/summary lines are skipped, not errors
    assert ll.parse_window_line("window=3 rate_rps=120 p50_us=500") is None
    assert ll.parse_window_line("load_summary requests=3") is None
    assert ll.parse_window_line("{not json") is None


def test_policy_document_is_valid_engine_json():
    learner = ll.LayoutLearner()
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    doc = learner.policy_document()
    assert json.loads(json.dumps(doc)) == doc
    entry = doc["regions"]["P|Stock"]
    assert set(entry) == {"page_size", "merge_min_tiles"}
    assert doc["default"]["bloom_bits_per_key"] == 10


def test_write_policy_file_round_trips(tmp_path):
    path = tmp_path / "policy.json"
    learner = ll.LayoutLearner()
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    ll.write_policy_file(path, learner.policy_document())
    assert json.loads(path.read_text(encoding="utf-8")) == learner.policy_document()
