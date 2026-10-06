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
          bytes_written: int = 0, p99_ns: float = 0.0) -> ll.RegionStats:
    return ll.RegionStats(
        region=region, probes=count, probe_misses=0, bytes_read=0,
        bytes_written=bytes_written, commit_blocks=count, commit_ops=count,
        count=count, mean_ns=mean_ns, p50_ns=mean_ns, p99_ns=p99_ns,
    )



def make_learner(**kw):
    """Construct a learner in the HISTORICAL absolute units these tests were tuned in.

    The production default prices writes relative to a calibrated typical window; with identity
    references the reward is ``service + lambda * bytes`` exactly as these synthetic numbers assume.
    """
    kw.setdefault("service_ref_ns", 1.0)
    kw.setdefault("bytes_ref", 1.0)
    return ll.LayoutLearner(**kw)


def make_static_derived(**kw):
    kw.setdefault("service_ref_ns", 1.0)
    kw.setdefault("bytes_ref", 1.0)
    return ll.StaticDerivedController(**kw)


def window(index: int, tables: dict, selection: dict) -> ll.WindowTelemetry:
    """One simulated metrics window: each region reports its active candidate's row."""
    rows = {}
    for region, table in tables.items():
        candidate = selection.get(region, ll.DECLARED_DEFAULT)
        mean_ns, bytes_written, p99_ns = table[candidate]
        rows[region] = stats(region, mean_ns=mean_ns, bytes_written=bytes_written, p99_ns=p99_ns)
    return ll.WindowTelemetry(window=index, rate_rps=1000.0, regions=rows)


def uniform_table(base_mean: float, best: ll.LayoutCandidate | None = None,
                  best_mean: float = 0.0, *, bytes_written: int = 0,
                  p99_ns: float = 0.0) -> dict:
    return {
        c: (best_mean if c == best else base_mean, bytes_written, p99_ns)
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
        out[region] = ll.LayoutCandidate(entry["page_size"], entry["merge_min_tiles"])
    return out


# ---------------------------------------------------------------------------
# Candidate set and reward proxy
# ---------------------------------------------------------------------------
def test_candidate_set_is_the_published_2x3_grid():
    assert len(ll.DEFAULT_CANDIDATES) == 6
    assert {c.page_size for c in ll.DEFAULT_CANDIDATES} == {4096, 65536}
    assert {c.merge_min_tiles for c in ll.DEFAULT_CANDIDATES} == {2, 8, 200}
    assert ll.DECLARED_DEFAULT in ll.DEFAULT_CANDIDATES


def test_reward_proxy_is_service_time_plus_lambda_times_bytes():
    row = stats("P|Stock", count=10, mean_ns=200.0, bytes_written=500)
    assert ll.region_cost_ns(row, 1.0, 1.0, 1.0) == 10 * 200.0 + 500
    # lambda prices endurance: a bigger lambda makes a write-heavy candidate costlier
    assert ll.region_cost_ns(row, 4.0, 1.0, 1.0) == 10 * 200.0 + 4 * 500


def test_candidate_set_is_configurable():
    learner = make_learner(candidates=(A, B))
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    assert set(learner.pulls("P|Stock")) == {A, B}


def test_folded_other_region_is_ignored():
    learner = make_learner()
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
    learner = make_learner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=1)
    drive(learner, tables, windows=40)
    assert learner.selections()["P|Stock"] == best_stock
    assert learner.selections()["U|Customer"] == best_customer


def test_no_churn_when_all_candidates_are_equal():
    tables = {"P|Stock": uniform_table(3000.0)}
    learner = make_learner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=3)
    trace = drive(learner, tables, windows=60)
    burn_in = len(ll.DEFAULT_CANDIDATES) * 2
    # Exploration is a bounded, named burn-in; after it the committed policy never moves.
    assert all(t == trace[-1] for t in trace[burn_in:])
    assert sum(learner.gated_switches().values()) == 0
    assert sum(learner.restarts().values()) == 0


def test_min_sample_gate_delays_a_switch():
    region = "P|Stock"
    learner = make_learner(candidates=(A, B), min_samples=3, margin=0.05,
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
    spiky = make_learner(candidates=(A, B), min_samples=3, margin=0.05,
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
    sustained = make_learner(candidates=(A, B), min_samples=3, margin=0.05,
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
        table[c] = (100.0, 0, 9000.0) if c == tempting else (1000.0, 0, 500.0)
    learner = make_learner(min_samples=1, margin=0.05, stability_window=1,
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
    learner = make_learner(min_samples=2, stability_window=2, margin=0.05,
                               drift_margin=10.0, seed=1)
    drive(learner, tables, windows=30)
    assert learner.selections()[region] == best


def test_determinism_same_seed_same_decisions():
    tables = {"P|Stock": uniform_table(5000.0, C, 1000.0),
              "U|Stock": uniform_table(3000.0)}
    first = drive(make_learner(min_samples=2, seed=11), tables, windows=30)
    second = drive(make_learner(min_samples=2, seed=11), tables, windows=30)
    assert first == second


def test_seed_only_orders_exact_ties():
    # The seed is used nowhere but the tie-break permutation, and that permutation
    # is stable for a fixed seed and a permutation of the candidate set.
    learner = make_learner(seed=0)
    assert learner.candidate_order == make_learner(seed=0).candidate_order
    assert sorted(learner.candidate_order) == sorted(ll.DEFAULT_CANDIDATES)
    seeds = {tuple(make_learner(seed=s).candidate_order) for s in range(8)}
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
    static = make_static_derived(candidates=(A, B, C))
    static.derive(sweep)
    assert static.selections()[region] == B


def test_static_derived_does_not_adapt_after_drift_while_learner_does():
    region = "P|Stock"
    old_best = ll.LayoutCandidate(4096, 2)     # A
    new_best = ll.LayoutCandidate(65536, 200)  # best only after the drift
    pre = uniform_table(5000.0, old_best, 1000.0)
    post = uniform_table(5000.0, new_best, 1000.0)
    post[old_best] = (6000.0, 0, 0.0)          # the old best degrades: the drift signal

    sweep = {region: {c: stats(region, mean_ns=pre[c][0]) for c in pre}}
    static = make_static_derived()
    static.derive(sweep)
    assert static.selections()[region] == old_best

    # Full re-exploration (None): the new best is not the pre-drift runner-up
    # (all non-incumbents were tied), so the complete candidate set must be
    # re-sampled. This pins the correctness fallback of the targeted response.
    learner = make_learner(min_samples=2, stability_window=2, margin=0.05,
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
    stable = {A: (1000.0, 0, 0.0), B: (5000.0, 0, 0.0)}
    drifted = {A: (9000.0, 0, 0.0), B: (1000.0, 0, 0.0)}   # A worsens, B improves
    learner = make_learner(candidates=(A, B), min_samples=2, stability_window=2,
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
        for i in range(8):
            learner.observe(window(i, {region: pre}, selection))
            selection = learner.selections()
        for i in range(8, 60):
            learner.observe(window(i, {region: post}, selection))
            selection = learner.selections()
        assert region not in learner.reexploring_regions()
        return learner.recovery_windows()[region]

    min_samples = 2
    targeted = make_learner(candidates=(A, B), min_samples=min_samples,
                                stability_window=2, margin=0.05, drift_margin=0.25,
                                drift_windows=2, drift_reexplore_candidates=2, seed=0)
    targeted_windows = measure(targeted, {A: (1000.0, 0, 0.0), B: (5000.0, 0, 0.0)},
                               {A: (9000.0, 0, 0.0), B: (1000.0, 0, 0.0)})
    assert targeted_windows == 2 * min_samples, (
        f"targeted drift response took {targeted_windows} windows "
        f"(K=2 x min_samples={min_samples}); the pre-registration target is 2")

    new_best = ll.LayoutCandidate(65536, 200)
    full = make_learner(candidates=ll.DEFAULT_CANDIDATES, min_samples=min_samples,
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
        make_learner(drift_reexplore_candidates=0)
    assert make_learner(drift_reexplore_candidates=None).drift_reexplore_candidates is None


# ---------------------------------------------------------------------------
# The driver loop + telemetry parsing
# ---------------------------------------------------------------------------
def test_driver_loop_runs_against_simulated_rewards():
    region = "P|Stock"
    best = ll.LayoutCandidate(65536, 200)
    tables = {region: uniform_table(1000.0, best, 100.0)}
    learner = make_learner(min_samples=2, stability_window=2, margin=0.05,
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
    learner = make_learner()
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    doc = learner.policy_document()
    assert json.loads(json.dumps(doc)) == doc
    entry = doc["regions"]["P|Stock"]
    assert set(entry) == {"page_size", "merge_min_tiles"}
    assert doc["default"]["bloom_bits_per_key"] == 10


def test_write_policy_file_round_trips(tmp_path):
    path = tmp_path / "policy.json"
    learner = make_learner()
    learner.observe(window(0, {"P|Stock": uniform_table(1.0)}, {}))
    ll.write_policy_file(path, learner.policy_document())
    assert json.loads(path.read_text(encoding="utf-8")) == learner.policy_document()


def test_lambda_actually_bites_with_calibrated_references():
    """Regression for slice 35's finding: lambda 0.1 -> 10 barely moved the policy.

    With the old absolute reward the write term was ~300x smaller than the service term, so the
    endurance price did nothing. This pins the arithmetic directly (the bandit's convergence is
    covered by the other tests): with references calibrated to the arm's own typical window, a
    candidate that is faster but writes more must be preferred at a low endurance price and beaten
    at a high one - i.e. lambda must be able to change the ranking.
    """
    from flashdb_harness import layout_learner as ll2
    fast_but_writey = stats("P|Stock", count=1000, mean_ns=40_000.0, bytes_written=40_000)
    slow_but_cheap = stats("P|Stock", count=1000, mean_ns=80_000.0, bytes_written=2_000)
    refs = (ll2.DEFAULT_SERVICE_REF_NS, ll2.DEFAULT_BYTES_REF)

    # low endurance price: the faster, write-heavier candidate is cheaper overall
    assert (ll2.region_cost_ns(fast_but_writey, 0.1, *refs)
            < ll2.region_cost_ns(slow_but_cheap, 0.1, *refs))
    # high endurance price: the ordering flips
    assert (ll2.region_cost_ns(fast_but_writey, 10.0, *refs)
            > ll2.region_cost_ns(slow_but_cheap, 10.0, *refs))

    # the references are the values measured on the inventory workload, not arbitrary constants:
    # with a 1e7/1e6 pair (my first attempt) the write term stays ~30x too small to matter.
    assert ll2.DEFAULT_SERVICE_REF_NS == 6.5e6
    assert ll2.DEFAULT_BYTES_REF == 2.0e4
