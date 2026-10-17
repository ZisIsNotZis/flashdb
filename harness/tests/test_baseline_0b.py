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
import time
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
# The pacer's two shapes: closed loop (the device floor) and open loop (the SQL arms, flashdb-load's)
# ---------------------------------------------------------------------------
OPEN_LOOP_RATE = 100          # 10 ms between requests
OPEN_LOOP_COMMIT_S = 0.03     # three times the inter-arrival time: the commit cannot keep up


def _slow_executor(commit_s, calls):
    """An `execute_batch` that records the group it was handed and then pays a slow commit."""
    def execute_batch(payloads):
        calls.append([payload["id"] for payload in payloads])
        time.sleep(commit_s)
        return {"take": len(payloads), "backorder": 0, "miss": 0, "errors": 0, "first_error": None}
    return execute_batch


def _paced(count: int):
    return ({"id": index} for index in range(count))


def test_open_loop_pacer_does_not_charge_dispatch_for_the_commit():
    """The fairness fix: an open-loop client is scored on its SCHEDULE, not on its own commit.

    A 30 ms commit carrying a 10 ms schedule cannot keep up either way -- `batch=2` caps the batch
    at two requests per commit while three arrive -- and the difference is where the cost lands.
    Closed loop (the floor's shape) it lands in schedule lag; open loop (the SQL arms', and
    flashdb-load's) the dispatch keeps to the schedule and the cost lands in the per-request
    service latency the report reads beside the rule.
    """
    manifest = {"segments": [{"offered_rate": OPEN_LOOP_RATE, "count": 40}], "total_requests": 40}
    due, rates = b0.build_schedule(manifest)
    closed_calls, open_calls = [], []
    closed = b0.run_paced_loop(due, rates, _paced(40), 2, 200,
                               _slow_executor(OPEN_LOOP_COMMIT_S, closed_calls))
    opened = b0.run_paced_loop(due, rates, _paced(40), 2, 200,
                               _slow_executor(OPEN_LOOP_COMMIT_S, open_calls), open_loop=True)

    assert closed["curve"][0]["lag_p99_us"] > 100_000   # dispatch queued behind the commit
    assert opened["curve"][0]["lag_p99_us"] < 50_000    # dispatch stayed on the schedule
    assert opened["curve"][0]["p99_us"] >= 25_000       # the commit's cost did not vanish, it moved
    assert opened["max_group_size"] <= 2
    for paced in (closed, opened):
        assert paced["requests_run"] == 40
        assert paced["counters"] == {b0.TAKE: 40, b0.BACKORDER: 0, b0.MISS: 0, "errors": 0,
                                     "first_error": None}
    assert [i for call in open_calls for i in call] == list(range(40))


def test_open_loop_pacer_commits_one_group_of_the_requests_already_ready():
    """One durable commit per group, the same `batch` cap, and no group held open to fill."""
    manifest = {"segments": [{"offered_rate": 50, "count": 60}], "total_requests": 60}
    due, rates = b0.build_schedule(manifest)
    calls = []
    paced = b0.run_paced_loop(due, rates, _paced(60), 8, 200, _slow_executor(0.05, calls),
                              open_loop=True)
    # Every request executed exactly once, in schedule order: a group is a prefix of the ready queue.
    assert [i for call in calls for i in call] == list(range(60))
    assert paced["groups"] == len(calls)          # one durable commit per group
    assert paced["requests_run"] == sum(len(call) for call in calls)
    assert 1 < paced["max_group_size"] <= 8        # ready requests batched, never past the cap
    assert paced["mean_group_size"] == paced["requests_run"] / paced["groups"]


def test_run_sql_arm_records_the_pacing_it_ran(tmp_path):
    """The run record must state the client shape, because the lag rule reads differently in each."""
    _emitted, manifest, requests, _order_flows = tiny_trace()
    open_arm = b0.sqlite_arm(tmp_path / "open.db")
    open_arm.seed(*b0.seed_docs(manifest, TINY_ON_HAND))
    opened = b0.run_sql_arm(open_arm, manifest, iter(requests), open_loop=True)
    assert opened["pacing"].startswith("open loop")
    assert opened["counters"]["errors"] == 0
    assert opened["requests_run"] == len(requests)

    closed_arm = b0.sqlite_arm(tmp_path / "closed.db")
    closed_arm.seed(*b0.seed_docs(manifest, TINY_ON_HAND))
    closed = b0.run_sql_arm(closed_arm, manifest, iter(requests), open_loop=False)
    assert closed["pacing"].startswith("closed loop")
    assert closed["requests_run"] == len(requests)


# ---------------------------------------------------------------------------
# sqlite's durability is OBSERVED before the measured phase, not assumed from the setter
# ---------------------------------------------------------------------------
def test_sqlite_measured_durability_is_read_back(tmp_path):
    arm = b0.sqlite_arm(tmp_path / "observed.db")
    assert arm.require_measured_durability() == {
        "journal_mode": "wal", "synchronous": b0.SQLITE_SYNCHRONOUS_FULL,
        "synchronous_name": "FULL", "wal_autocheckpoint_pages": 0, "journal_size_limit": -1}


def test_sqlite_wal_autocheckpoint_is_stated_read_back_and_checked(tmp_path):
    """The WAL policy is a statement the run makes, so the read-back must hold it, not sqlite's."""
    stated = b0.sqlite_arm(tmp_path / "stated.db", wal_autocheckpoint=4000)
    assert stated.require_measured_durability()["wal_autocheckpoint_pages"] == 4000
    # A connection whose policy drifted from the stated one must stop the run, not pass silently.
    stated.wal_autocheckpoint = 1000
    with pytest.raises(b0.SqlArmError, match="wal_autocheckpoint"):
        stated.require_measured_durability()


def test_the_deferred_checkpoint_is_recorded_and_truncates_the_wal(tmp_path):
    """`wal_autocheckpoint=0` defers sqlite's checkpoint out of the measured phase, and it is run
    and recorded after it, so the WAL bytes the run deferred and the write-back are both visible."""
    db = tmp_path / "deferred.db"
    arm = b0.sqlite_arm(db, wal_autocheckpoint=0)
    arm.cur.execute("CREATE TABLE probe (value INTEGER)")
    for value in range(200):
        arm.cur.execute("BEGIN IMMEDIATE")
        arm.cur.execute("INSERT INTO probe (value) VALUES (?)", (value,))
        arm.cur.execute("COMMIT")
    record = b0.sqlite_wal_record(arm, db)
    assert record["autocheckpoint_pages"] == 0
    assert record["wal_bytes_after_run"] > 0
    assert record["checkpoint"]["pragma"] == "wal_checkpoint(TRUNCATE)"
    assert record["files_after_checkpoint"].get("wal", 0) == 0
    assert record["data_bytes_after_checkpoint"] >= 200 * 8
    # A successful truncating checkpoint reports (0, 0, 0) on this build, so the frames it moved are
    # named by the PASSIVE row recorded beside it -- the TRUNCATE row alone evidences nothing.
    busy, frames_in_wal, frames_moved = record["passive_checkpoint"]["result"]
    assert busy == 0
    assert frames_in_wal > 0
    assert frames_moved == frames_in_wal


def test_the_deferred_write_back_is_refused_when_the_frames_were_not_moved():
    """Negative control: a deferred write-back the record cannot evidence must stop the run.

    The committed ticket-82 records show `wal_bytes_after_run` in the tens of MB beside a
    `wal_checkpoint(TRUNCATE)` row of `(0, 0, 0)`, which on this sqlite build is exactly what a
    SUCCESSFUL truncating checkpoint returns -- and also what an already-empty WAL returns. The
    verdict on the write-back therefore rests on the PASSIVE row, and these cases are the ones it
    must refuse.
    """
    unmoved = {"pragma": "wal_checkpoint(PASSIVE)", "result": [0, 0, 0]}
    with pytest.raises(b0.SqlArmError, match="did not move every WAL frame"):
        b0._require_deferred_write_back(17_703_672, unmoved, {"db": 1, "wal": 0})
    partially = {"pragma": "wal_checkpoint(PASSIVE)", "result": [0, 4_321, 4_000]}
    with pytest.raises(b0.SqlArmError, match="did not move every WAL frame"):
        b0._require_deferred_write_back(17_703_672, partially, {"db": 1, "wal": 0})
    with pytest.raises(b0.SqlArmError, match="left a WAL"):
        b0._require_deferred_write_back(0, unmoved, {"db": 1, "wal": 512})
    # An empty WAL is a state the record CAN describe: nothing was deferred, nothing had to move.
    b0._require_deferred_write_back(0, unmoved, {"db": 1})


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


# ---------------------------------------------------------------------------
# Ticket 82's controlled protocol: the pre-load snapshot, the device floor, the machine state and
# the scorer's arithmetic. Each test binds one thing the controlled run's evidence depends on.
# ---------------------------------------------------------------------------
def test_instance_snapshot_round_trip_is_byte_identical_and_detects_a_changed_snapshot(tmp_path):
    """A restore succeeds only on the bytes the snapshot recorded (ticket 82's stop condition)."""
    live_root = tmp_path / "live"
    instance = live_root / "instance-flashdb-declared"
    instance.mkdir(parents=True)
    (instance / "manifest").write_bytes(b"manifest-bytes")
    (instance / "tile-1.tile").write_bytes(b"tile-bytes")
    snapshot_dir = tmp_path / "snapshot"
    snap = b0.snapshot_instance("flashdb-declared", tmp_path, snapshot_dir, live_root)
    digest, total, files = b0.tree_sha256(instance)
    assert (snap["sha256"], snap["bytes"], snap["files"]) == (digest, total, files)

    # Serving the trace mutates the live instance; a restore has to put the seeded bytes back.
    (instance / "tile-1.tile").write_bytes(b"changed by the measured load")
    (instance / "wal").write_bytes(b"new file")
    state = b0.restore_instance("flashdb-declared", tmp_path, snapshot_dir, live_root)
    assert state["verified"] is True
    assert b0.tree_sha256(instance)[0] == digest
    assert not (instance / "wal").exists()

    # The snapshot is the reference, so a snapshot whose bytes changed must stop the run instead of
    # quietly measuring a different state.
    (snapshot_dir / "flashdb-declared" / "tile-1.tile").write_bytes(b"tampered")
    with pytest.raises(SystemExit, match="does NOT reproduce the seeded state"):
        b0.restore_instance("flashdb-declared", tmp_path, snapshot_dir, live_root)


def test_sqlite_snapshot_is_a_single_file_and_restores(tmp_path):
    """sqlite's instance is one file; its `-wal`/`-shm` sidecars are not part of the snapshot."""
    db = b0.instance_path("sqlite", tmp_path, tmp_path / "live")
    db.parent.mkdir(parents=True)
    db.write_bytes(b"SQLite format 3\x00" + b"x" * 100)
    (Path(f"{db}-wal")).write_bytes(b"wal")
    snap = b0.snapshot_instance("sqlite", tmp_path, tmp_path / "snapshot", tmp_path / "live")
    assert snap["files"] == 1
    b0.restore_instance("sqlite", tmp_path, tmp_path / "snapshot", tmp_path / "live")
    assert db.read_bytes().startswith(b"SQLite format 3")
    assert not Path(f"{db}-wal").exists()


def test_instance_path_names_the_instance_of_each_arm(tmp_path):
    live = tmp_path / "live"
    assert b0.instance_path("flashdb-declared", tmp_path, live) == live / "instance-flashdb-declared"
    assert b0.instance_path("sqlite", tmp_path, live) == live / "sqlite.db"
    assert b0.instance_path("sqlite", tmp_path) == tmp_path / "sqlite.db"
    assert b0.instance_path("postgres", tmp_path, live).name == "data"


def test_prepare_reuses_the_frozen_seed_trace_by_link_and_records_its_digest(tmp_path):
    """Every rung and block of a controlled run must be seeded from one frozen dataset."""
    frozen = tmp_path / "frozen-seed.jsonl"
    frozen.write_text('{"block":"seed-00000000","docs":[]}\n', encoding="utf-8")
    manifest = {"n_customers": 1, "n_sku": 1, "n_loc": 1, "seed": 1}
    record = b0.prepare_seed_trace(tmp_path / "seed.jsonl", manifest, str(frozen))
    assert record["method"] == "hard link"
    assert record["sha256"] == b0.file_sha256(frozen)
    assert b0.file_sha256(tmp_path / "seed.jsonl") == record["sha256"]
    assert (tmp_path / "seed.jsonl").read_text() == frozen.read_text()

    regenerated = b0.prepare_seed_trace(tmp_path / "seed.jsonl", manifest, None)
    assert regenerated["method"] == "generate"
    assert regenerated["bytes"] > 0


def test_device_floor_runs_the_arms_pacer_with_no_engine(tmp_path):
    """The floor is measured by the same pacer and window rule as the arms, with no engine."""
    manifest = {"segments": [{"offered_rate": 100, "count": 40}], "total_requests": 40}
    due, rates = b0.build_schedule(manifest)
    for bench in ("idle", "fsync"):
        paced = b0.run_floor_bench(bench, due, rates, b0.DEFAULT_BATCH, 200, tmp_path / f"{bench}.dat")
        assert paced["requests_run"] == 40
        assert paced["counters"]["errors"] == 0
        row = paced["curve"][0]
        assert row["offered_rps"] == 100
        assert row["windows"] >= 1
        # The fsync bench pays a durable commit per request, so its service latency is the device's.
        if bench == "fsync":
            assert row["p99_us"] >= row["lag_p99_us"] >= 0
            assert (tmp_path / "fsync.dat").stat().st_size == 40 * len(
                json.dumps({"bench": "fsync", "record": "x" * 256}).encode())
        else:
            assert row["p99_us"] >= 0


def test_machine_state_records_the_fields_the_lag_rule_depends_on():
    state = b0.machine_state()
    for key in ("ac_online", "power_profile", "governor", "epp", "scaling_cur_freq_khz",
                "loadavg", "nproc", "top_cpu", "concurrent_work"):
        assert key in state
    # The governor NAME is not the frequency state on this platform, so the state carries the EPP
    # hint and the effective frequency beside it rather than relying on that one word.
    assert isinstance(state["epp"], list) and state["epp"]
    assert isinstance(state["scaling_cur_freq_khz"], list) and state["scaling_cur_freq_khz"]
    assert state["governor"] is not None
    assert not state["concurrent_work"], state["concurrent_work"]


def test_the_arm_step_passes_every_keyword_its_arm_builders_accept():
    """`cmd_arm`'s calls and the arm builders' signatures must agree.

    This is a structural check because the failure it catches is structural: the postgres arm step
    was written to pass `verify`/`restored`/`database` while `run_postgres_arm` accepted neither
    some of them, and the TypeError surfaced only after a 7-minute seed and a 2.2 GB restore, halfway
    through the first measured block. The keyword sets are asserted here instead.
    """
    import inspect

    for function, required in (
        (b0.run_postgres_arm, {"restored", "database", "verify", "engine_cap_mib"}),
        (b0.run_flashdb_load, {"engine_cap_mib", "instance_root", "batch", "window_ms"}),
        (b0.run_flashdb_arm, {"engine_cap_mib", "seed_cap_mib"}),
        (b0.seed_flashdb_instance, {"seed_cap_mib", "instance_root"}),
        (b0.seed_sqlite_instance, {"instance_root", "cache_kib"}),
    ):
        params = set(inspect.signature(function).parameters)
        missing = required - params
        assert not missing, f"{function.__name__} does not accept {sorted(missing)}"
