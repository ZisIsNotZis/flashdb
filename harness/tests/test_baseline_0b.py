"""Tests for the M1 request-level external-baseline harness.

The scored properties are: one seed yields one trace (so every arm measures the same work), that
trace is the adapter's dialect unchanged, the pacer's schedule is the offered rate accumulated as
intervals, a window's lag/error rule rejects a rung, and the inventory oracle passes a committed
trace and fails a corrupted one.

What these tests do NOT cover: the postgres arm (needs the user-space cluster and psycopg) and the
flashdb arm (needs the Rust binaries). Both are exercised by `scripts/m1-baseline-run.sh` and
evidenced in the ticket's build record.
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
