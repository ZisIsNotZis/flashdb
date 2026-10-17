"""Tests for the M1 request-level external-baseline harness.

The scored properties are: one seed yields one trace (so every arm measures the same work), that
trace is the adapter's dialect unchanged, the pacer's schedule is the offered rate accumulated as
intervals, a window's lag/CEILING/GROWTH/error rule rejects a rung exactly where `flashdb-load`
rejects it, sqlite's measured durability is read back from sqlite rather than assumed from the
setter, and both inventory oracles pass a consistent state and fail a corrupted one (the flashdb
oracle through a synthetic replayed iterable, so it has a negative control without the binaries).

What these tests do NOT cover: the postgres arm (needs the user-space cluster and psycopg) and the
flashdb arm's own run (needs the Rust binaries). Both are exercised by
`scripts/m1-baseline-run.sh` and evidenced in the ticket's measurement record.
"""

import hashlib
import json
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import baseline_0b as b0  # noqa: E402
from flashdb_harness.cold_0b import summarize_curve, window_records  # noqa: E402
from flashdb_harness.generator import MIX, Generator, render  # noqa: E402

# Small enough to run in a test, and `on_hand=3` with qty in 1..5 forces BOTH order_flow branches
# (a fulfilled take and a guard-failing backorder) inside a few dozen requests.
TINY = dict(seed=5, n_requests=40, n_customers=3, n_sku=4, n_loc=2, offered_rate=100)
TINY_ON_HAND = 3


def tiny_trace() -> tuple[bytes, dict, list[dict], int]:
    """`(trace_bytes, manifest, parsed_requests, order_flow_count)` for the tiny trace."""
    emitted, manifest = b0.build_trace_baseline(**TINY)
    requests = [json.loads(line) for line in emitted.decode().splitlines()]
    order_flows = sum(1 for req in requests if "take" in req["blocks"])
    return emitted, manifest, requests, order_flows


def test_trace_is_byte_identical_for_one_seed():
    first, manifest_a = b0.build_trace_baseline(**TINY)
    second, manifest_b = b0.build_trace_baseline(**TINY)
    assert first == second
    assert manifest_a["trace_sha256"] == manifest_b["trace_sha256"]
    assert manifest_a["trace_sha256"] == hashlib.sha256(first).hexdigest()
    assert manifest_a["total_requests"] == TINY["n_requests"]
    assert manifest_a["segments"] == [{
        "index": 0, "start_request": 0, "count": TINY["n_requests"],
        "offered_rate": TINY["offered_rate"], "mix": MIX, "drift": None,
    }]


def test_trace_is_the_generator_dialect_unchanged():
    """The baseline trace is the same stream `generator.Generator` emits, byte for byte."""
    emitted, _manifest, _requests, _order_flows = tiny_trace()
    gen = Generator(TINY["seed"], TINY["n_requests"], TINY["n_customers"],
                    TINY["n_sku"], TINY["n_loc"])
    expected = "".join(next(render([req])) + "\n" for req in gen.requests())
    assert emitted.decode() == expected


def test_schedule_accumulates_each_segment_interval():
    manifest = {
        "total_requests": 6,
        "segments": [{"index": 0, "start_request": 0, "count": 4, "offered_rate": 100},
                     {"index": 1, "start_request": 4, "count": 2, "offered_rate": 50}],
    }
    due, rates = b0.build_schedule(manifest)
    assert rates == [100.0, 100.0, 100.0, 100.0, 50.0, 50.0]
    assert due == [0.0, 10_000.0, 20_000.0, 30_000.0, 40_000.0, 60_000.0]


def _rung_curve(lag_us: float, errors: int = 0):
    """One full window at 50 rps, scored by the same aggregation every arm uses."""
    windows = b0.WindowSamples(window_ms=1000)
    windows.batch([lag_us] * 50, [lag_us + 500.0] * 50, 500.0, 50.0, errors)
    windows.finish(1_000_000.0, 50.0)
    lines = [json.dumps(record, sort_keys=True) for record in windows.records]
    records = window_records(lines, 1000)
    return records, summarize_curve(records, None)


def test_a_clean_window_sustains_and_a_late_or_errored_one_does_not():
    records, curve = _rung_curve(lag_us=100.0)
    assert records[0]["invalid"] is False
    assert curve[0]["sustained"] is True, curve[0]

    records, curve = _rung_curve(lag_us=20_000.0)  # above the 10,000 us lag ceiling
    assert records[0]["lag_ceiling_breached"] is True
    assert records[0]["reject_reason"] == "lag_ceiling"
    assert curve[0]["sustained"] is False
    assert curve[0]["reject_reasons"] == {"lag_ceiling": 1}

    records, curve = _rung_curve(lag_us=100.0, errors=1)
    assert records[0]["reject_reason"] == "errors"
    assert curve[0]["sustained"] is False
    assert curve[0]["reject_reasons"] == {"errors": 1}


def _sqlite_arm(manifest: dict, on_hand: int) -> b0.SqliteArm:
    arm = b0.sqlite_arm()
    customers, stock = b0.seed_docs(manifest, on_hand)
    arm.seed(customers, stock)
    return arm


def _replay(arm: b0.SqlArm, requests: list[dict], batch: int = 8) -> dict:
    counters = {b0.TAKE: 0, b0.BACKORDER: 0, b0.MISS: 0, "errors": 0, "first_error": None}
    for start in range(0, len(requests), batch):
        state = arm.execute_batch(requests[start:start + batch])
        for key in (b0.TAKE, b0.BACKORDER, b0.MISS, "errors"):
            counters[key] += state[key]
        counters["first_error"] = counters["first_error"] or state["first_error"]
    return counters


def test_oracle_passes_a_committed_tiny_trace():
    _emitted, manifest, requests, order_flows = tiny_trace()
    arm = _sqlite_arm(manifest, TINY_ON_HAND)
    counters = _replay(arm, requests)

    # Both branches ran, so the oracle's "counts explain every order_flow" check has teeth.
    assert counters[b0.TAKE] > 0 and counters[b0.BACKORDER] > 0
    assert counters[b0.TAKE] + counters[b0.BACKORDER] == order_flows
    result = b0.oracle_sql(arm, manifest, counters, order_flows, TINY_ON_HAND)
    assert result["status"] == "PASS"
    assert all(check["ok"] for check in result["checks"])


def test_oracle_fails_loudly_on_a_corrupted_stock_count():
    _emitted, manifest, requests, order_flows = tiny_trace()
    arm = _sqlite_arm(manifest, TINY_ON_HAND)
    counters = _replay(arm, requests)
    arm.cur.execute("UPDATE stock SET on_hand = on_hand + 1 WHERE sku = 'S000001' AND loc = 'L000'")
    with pytest.raises(b0.OracleViolation, match="on_hand_matches_movements"):
        b0.oracle_sql(arm, manifest, counters, order_flows, TINY_ON_HAND)


def test_oracle_fails_loudly_on_a_duplicate_order_number():
    _emitted, manifest, requests, order_flows = tiny_trace()
    arm = _sqlite_arm(manifest, TINY_ON_HAND)
    counters = _replay(arm, requests)
    taken = arm.cur.execute('SELECT order_no, sku, qty FROM "order" LIMIT 1').fetchone()
    assert taken is not None, "the tiny trace committed at least one fulfilled order"
    arm.cur.execute("INSERT INTO backorder (order_no, sku, qty) VALUES (?, ?, ?)", taken)
    with pytest.raises(b0.OracleViolation, match="no_duplicate_order_no"):
        b0.oracle_sql(arm, manifest, counters, order_flows, TINY_ON_HAND)


# ---------------------------------------------------------------------------
# The latency rule is ONE rule: the SQL reducer must reject what `flashdb-load` rejects
# ---------------------------------------------------------------------------
# `engine/src/bin/flashdb-load.rs` grows `late_growth_windows` by one on `behind || window_late > 0`
# and resets it otherwise, calls a window `saturated` at 3 consecutive such windows, and names
# `lag_saturation` in `reject_reason` when the ceiling did not itself fire. `window_late` counts
# requests whose (clamped) schedule lag passed the ceiling -- which is why a window can grow with its
# p99 still under the ceiling: one late request in 201 (the real shape at rung 200, where
# `windows-flashdb-declared.jsonl` window 17 has lag p99 7,400 us and `reject_reason:
# lag_saturation`). This is the agreement the M1 acceptance asks for, against the same inputs.
FLASHDB_LOAD_GROWTH_WINDOWS = 3


def _flashdb_load_rejections(windows: list[tuple[int, int, int]]) -> list[str | None]:
    """`flashdb-load`'s per-window verdict, transcribed from the driver's own source.

    Each window is `(lag_p99_us, window_late, errors)`. The harness passes no service ceiling, so
    the driver's service-ceiling branch cannot fire here and the transcription has none.
    """
    streak = 0
    reasons: list[str | None] = []
    for lag_p99, window_late, errors in windows:
        behind = lag_p99 > b0.LAG_CEILING_US
        streak = streak + 1 if (behind or window_late > 0) else 0
        saturated = streak >= FLASHDB_LOAD_GROWTH_WINDOWS
        parts = []
        if behind:
            parts.append("lag_ceiling")
        if saturated and not behind:
            parts.append("lag_saturation")
        if errors:
            parts.append("errors")
        reasons.append("+".join(parts) or None)
    return reasons


def _reduce_lag_windows(late_per_window: list[int], requests_per_window: int = 201):
    """Emit one window per entry with that many requests past the ceiling.

    Returns the emitted window records (what `windows-<arm>.jsonl` holds, including the growth
    clause's own inputs) and the scored records `summarize_curve` reads.
    """
    windows = b0.WindowSamples(window_ms=1000)
    for index, late in enumerate(late_per_window):
        lags = [200.0] * (requests_per_window - late) + [20_000.0] * late
        windows.batch(lags, [lag + 500.0 for lag in lags], 500.0, 200.0, 0)
        windows.flush((index + 1) * 1_000_000.0, 200.0)
    emitted = list(windows.records)
    lines = [json.dumps(record, sort_keys=True) for record in emitted]
    return emitted, window_records(lines, 1000)


def test_lag_growth_is_rejected_exactly_where_flashdb_load_rejects_it():
    """3 consecutive windows that each grow (one late request, p99 under the ceiling) saturate."""
    # The third growing window is the first the growth clause rejects; a clean one resets the streak.
    emitted, records = _reduce_lag_windows([1, 1, 1, 0])
    assert [record["late_requests"] for record in emitted] == [1, 1, 1, 0]
    assert all(record["lag_ceiling_breached"] is False for record in emitted)
    assert all(record["lag_p99_us"] <= b0.LAG_CEILING_US for record in emitted)
    assert [record["invalid"] for record in emitted] == [False, False, True, False]
    assert [record["reject_reason"] for record in emitted] == [None, None, "lag_saturation", None]

    # Agreement with the driver's own rule on the same per-window inputs (the p99 it computed, the
    # late count it counts, the errors it counts).
    inputs = [(record["lag_p99_us"], record["late_requests"], record["errors"])
              for record in emitted]
    assert [record["reject_reason"] for record in emitted] == _flashdb_load_rejections(inputs)

    curve = summarize_curve(records, None)
    assert curve[0]["reject_reasons"] == {"lag_saturation": 1}
    assert curve[0]["invalid_windows"] == 1
    assert curve[0]["sustained"] is False


def test_the_growth_streak_resets_on_a_clean_window():
    """Two growing windows, one clean one, two growing ones: never three in a row, so no rejection."""
    emitted, records = _reduce_lag_windows([1, 1, 0, 1, 1])
    inputs = [(record["lag_p99_us"], record["late_requests"], record["errors"])
              for record in emitted]
    assert [record["reject_reason"] for record in emitted] == [None] * 5
    assert [record["reject_reason"] for record in emitted] == _flashdb_load_rejections(inputs)
    assert summarize_curve(records, None)[0]["sustained"] is True


def test_a_clean_window_series_is_not_saturated():
    """A control: the growth clause must not invent a rejection out of an undisturbed rung."""
    emitted, records = _reduce_lag_windows([0, 0, 0, 0])
    assert [record["reject_reason"] for record in emitted] == [None] * 4
    assert [record["saturated"] for record in emitted] == [False] * 4
    assert summarize_curve(records, None)[0]["sustained"] is True


# ---------------------------------------------------------------------------
# sqlite's durability is OBSERVED before the measured phase, not assumed from the setter
# ---------------------------------------------------------------------------
def test_sqlite_measured_durability_is_read_back(tmp_path):
    arm = b0.sqlite_arm(tmp_path / "observed.db")
    assert arm.require_measured_durability() == {
        "journal_mode": "wal", "synchronous": b0.SQLITE_SYNCHRONOUS_FULL,
        "synchronous_name": "FULL"}


def test_sqlite_seed_restores_measured_durability(tmp_path):
    """The bulk load runs with journal_mode=OFF -- the bulk load must hand back a WAL database."""
    _emitted, manifest, _requests, _order_flows = tiny_trace()
    arm = b0.sqlite_arm(tmp_path / "seeded.db")
    customers, stock = b0.seed_docs(manifest, TINY_ON_HAND)
    arm.seed(customers, stock)
    observed = arm.require_measured_durability()
    assert observed["journal_mode"] == "wal"
    assert observed["synchronous"] == b0.SQLITE_SYNCHRONOUS_FULL


def test_sqlite_durability_read_back_catches_a_failed_restore(tmp_path):
    """A `PRAGMA journal_mode=WAL` that did not take effect must stop the run, not pass silently."""
    arm = b0.sqlite_arm(tmp_path / "regressed.db")
    arm.cur.execute("PRAGMA journal_mode=OFF")
    arm.cur.execute("PRAGMA synchronous=OFF")
    with pytest.raises(b0.SqlArmError, match="not WAL"):
        arm.require_measured_durability()


# ---------------------------------------------------------------------------
# The flashdb oracle's negative controls: a synthetic replayed iterable that must FAIL
# ---------------------------------------------------------------------------
FLASHDB_ORACLE_MANIFEST = {"n_sku": 1, "n_loc": 1, "n_customers": 1}
FLASHDB_ORACLE_INITIAL = 1000


def _replayed(*, cell_on_hand=995, movement_delta=-5, movement_order_no="O000001",
              scan_order_no="O000001", movement_cell=("S000001", "L000"),
              probe_cell=("S000001", "L000"), drop_cell=False, customer_row=True):
    """One replayed probe stream in `flashdb-replay`'s dialect: movements, customer, cell.

    `blocks.q` is `request.BlockResult` in Rust (`status`, `csn`, `row`), which is the shape
    `oracle_flashdb` reads.
    """
    movements = {"q": {"status": "ok", "csn": 3, "row": [
        {"movement_no": "M000001", "sku": movement_cell[0], "loc": movement_cell[1],
         "delta": movement_delta, "order_no": movement_order_no}]}}
    customer = {"q": {"status": "ok", "csn": 3,
                      "row": {"email": "a@b"} if customer_row else None}}
    cell = {"q": {"status": "ok", "csn": 3, "row": None if drop_cell else {
        "sku": probe_cell[0], "loc": probe_cell[1], "on_hand": cell_on_hand}}}
    for target, blocks in (
        (f"probe-movements:{scan_order_no}", movements),
        ("probe-customer:a@b", customer),
        (f"probe-stock:{probe_cell[0]}:{probe_cell[1]}", cell),
    ):
        yield {"id": target, "blocks": blocks}


def _flashdb_oracle_counters(**overrides) -> dict:
    counters = {"take": 1, "errors": 0, "first_error": None}
    counters.update(overrides)
    return counters


def test_flashdb_oracle_passes_a_consistent_synthetic_replay():
    result = b0.oracle_flashdb(_replayed(), FLASHDB_ORACLE_MANIFEST,
                               _flashdb_oracle_counters(), FLASHDB_ORACLE_INITIAL)
    assert result["status"] == "PASS"
    assert all(check["ok"] for check in result["checks"])
    assert {check["check"] for check in result["checks"]} >= {
        "on_hand_matches_movements", "every_movement_cell_probed", "cell_count"}


@pytest.mark.parametrize("corruption, expected", [
    ({"cell_on_hand": 994}, "on_hand_matches_movements"),
    ({"cell_on_hand": -1, "movement_delta": -1001}, "on_hand_non_negative"),
    ({"drop_cell": True}, "stock_cell_present"),
    ({"movement_order_no": "O000002"}, "movement_order_no_matches"),
    # The movement books its delta against a cell that is never read back: the arithmetic per cell
    # still holds for the cell that IS read, so only the fold can catch it.
    ({"movement_cell": ("S000002", "L000"), "cell_on_hand": 1000}, "every_movement_cell_probed"),
    ({"customer_row": False}, "customer_present"),
])
def test_flashdb_oracle_negative_controls_fail_loudly(corruption, expected):
    """Every corruption of the synthetic replay must raise, naming the check that caught it."""
    with pytest.raises(b0.OracleViolation, match=expected):
        b0.oracle_flashdb(_replayed(**corruption), FLASHDB_ORACLE_MANIFEST,
                          _flashdb_oracle_counters(), FLASHDB_ORACLE_INITIAL)


@pytest.mark.parametrize("overrides, expected", [
    ({"errors": 1, "first_error": "req-7: boom"}, "no_request_errors"),
    ({"take": 2}, "movement_count_is_committed_takes"),
])
def test_flashdb_oracle_negative_controls_on_the_committed_counters(overrides, expected):
    with pytest.raises(b0.OracleViolation, match=expected):
        b0.oracle_flashdb(_replayed(), FLASHDB_ORACLE_MANIFEST,
                          _flashdb_oracle_counters(**overrides), FLASHDB_ORACLE_INITIAL)
