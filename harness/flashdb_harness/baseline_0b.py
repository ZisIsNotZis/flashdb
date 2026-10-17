"""Request-level external-baseline harness (M1): one frozen trace, three arms, one rule.

What this is for
----------------
Pre-registration §"External baselines and thresholds" (B1/T5/T6/T7) compares flashdb against
sqlite and postgres on the SAME request stream, at matched durability, in the same memory-capped
regime, under the SAME latency rule. flashdb has no SQL and its payload is opaque, so the
comparison has to live at the request level: one trace of the four inventory templates
(`generator.py`), replayed by every arm, with comparable per-window samples.

This module is that harness. It is the driver work package of ticket 80 (WP1.3); the measurement
(WP1.4) is a later run at the pre-registered ladder.

What is reused, and why nothing here re-spells it
-------------------------------------------------
* The trace: `load_profile.LoadProfile` over a `Generator` stream, emitted through the existing
  JSONL dialect (`generator.render`). `build_trace_baseline` only fixes one rung of a profile, so
  a seed can never produce two different traces and the manifest is the same v1 document the Rust
  pacer reads.
* The dimensions: `seed.SeedGenerator`, so every arm is seeded from the same rows.
* The window/validity rule: the windows this harness emits use the flashdb driver's own JSONL
  dialect (`service_p*_us`, `lag_p*_us`, `min_lag_us`, cumulative `completed`, `window_us`,
  `invalid`/`reject_reason`, `lag_ceiling_breached`), so `cold_0b.window_records` parses an
  external arm's dump exactly as it parses flashdb's, and `cold_0b.summarize_curve` scores every
  arm's rung with one implementation.
* The pacer contract: requests become READY at their cumulative scheduled due time; a batch is
  formed only from requests already ready, capped at `batch`, and is never held open to fill. That
  is `flashdb-load`'s arrivals model (`ready_group`), and it is why a rung's achieved rate is
  bounded by its offered rate.

The rule a rung is scored by (the ticket's, applied identically to every arm)
-----------------------------------------------------------------------------
A window is REJECTED when its per-request schedule-lag p99 exceeds `LAG_CEILING_US` (10,000 us,
the pre-registration's 10 ms bound) or any request in it errored. A rung is SUSTAINED iff its
achieved rate is within 5% of the offered rate (`cold_0b.SUSTAINED_TOLERANCE`) and zero windows
were rejected. `summarize_curve` adds its `rate_exceeded` guard: an achieved rate above the offer
is a measurement error, never a sustain.

Latency definitions, and why two are recorded
---------------------------------------------
Per request the pacer records
  * schedule lag     = dispatch - due      (what the 10 ms rule scores; clamped at 0 for the
                                            percentile, the signed floor is `min_lag_us`),
  * work latency     = done - dispatch     (`work_p*_us`; the ticket's "start -> done"),
  * service latency  = done - due          (`service_p*_us`, `p*_us`).
`service` is `lag + work` and is the quantity `flashdb-load` reports as the per-request service
latency, so it is the field that may be compared across arms; `work` is reported alongside it and
is not used by the rule.

Durability, one disclosed batch size
------------------------------------
Every arm commits one durable batch per group: sqlite `journal_mode=WAL, synchronous=FULL` and one
COMMIT per group; postgres `fsync=on, synchronous_commit=on` (the disclosed `scripts/baseline-env.sh`
config) and one COMMIT per group; flashdb `--group-requests B` (one WAL barrier per group, the
driver acks after the barrier). At the offered rates this work package runs, a group is one
request (the batch ceiling never binds) and the measured `mean_group_size` in the run record says
so. `batch` is recorded on every arm so a later rung at a higher rate stays comparable.

Usage (driven by `scripts/m1-baseline-run.sh`; the three steps are separate so each arm can run
inside its own memory-capped scope)::

    uv run python -m flashdb_harness.baseline_0b prepare --workdir DIR --requests 2000 --rate 50
    uv run python -m flashdb_harness.baseline_0b arm --workdir DIR --arm sqlite
    uv run python -m flashdb_harness.baseline_0b report --workdir DIR
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

from flashdb_harness import load_profile
from flashdb_harness.cold_0b import summarize_curve, window_records
from flashdb_harness.generator import email_name, loc_name, render, sku_name
from flashdb_harness.seed import SeedGenerator

REPO_ROOT = Path(__file__).resolve().parents[2]

# The pre-registration's schedule-lag bound; the same value `flashdb-load --lag-ceiling-us`
# defaults to, and the one this harness passes to the driver explicitly.
LAG_CEILING_US = 10_000
DEFAULT_WINDOW_MS = 1_000
# `cold_0b`'s shipped `--group-requests` cadence. Named once here so every arm states the batch it
# actually ran with.
DEFAULT_BATCH = 64
# The seeded `Stock.on_hand` every cell starts at (uniform: `on_hand_jitter=0`), which is what makes
# the oracle's `on_hand == initial - sum(delta)` check checkable without a second table.
DEFAULT_ON_HAND = 1_000_000

SPEC_SCHEMA = "flashdb.baseline.run-spec.v1"

# flashdb arms and the policy each runs. M1 wires both names to the same declared-default wiring;
# `flashdb-learned` exists as a separate arm name so the scoreboard's shape is stable when a
# learner controller is wired in, and the run record says which policy actually ran.
FLASHDB_ARMS = ("flashdb-declared", "flashdb-learned")
SQL_ARMS = ("sqlite", "postgres")
ARM_NAMES = ("flashdb-declared", "sqlite", "postgres")

# Order-flow outcomes and read outcomes, one spelling each.
TAKE = "take"
BACKORDER = "backorder"
MISS = "miss"
READ = "read"


class OracleViolation(RuntimeError):
    """The inventory oracle found a committed state the trace cannot explain."""


# ---------------------------------------------------------------------------
# Trace + manifest (one rung of an existing load profile)
# ---------------------------------------------------------------------------
def build_trace_baseline(
    seed: int,
    n_requests: int,
    n_customers: int,
    n_sku: int,
    n_loc: int,
    offered_rate: int,
    theta: float = 1.0,
) -> tuple[bytes, dict]:
    """Emit one rung's trace bytes and its v1 manifest; same seed -> byte-identical bytes.

    The manifest is `load_profile`'s own document (schema `flashdb.load-profile.manifest.v1`): it
    carries the trace sha256, the dimension parameters and one segment with the offered rate, so
    the trace and the schedule it is scored under cannot drift apart.
    """
    profile = load_profile.LoadProfile(
        segments=[load_profile.Segment(offered_rate=offered_rate, count=n_requests)],
        seed=seed,
        n_customers=n_customers,
        n_sku=n_sku,
        n_loc=n_loc,
        theta=theta,
    )
    return load_profile.generate(profile)


def seed_docs(manifest: dict, on_hand: int = DEFAULT_ON_HAND) -> tuple[list[tuple], list[tuple]]:
    """The dimension rows every arm is seeded from: `(customers, stock)` in insert order.

    Both come from one `SeedGenerator` built from the manifest, so the external arms and
    `flashdb-scale seed-inventory` are seeded from the same rows.
    """
    gen = SeedGenerator(
        seed=manifest["seed"],
        n_customers=manifest["n_customers"],
        n_sku=manifest["n_sku"],
        n_loc=manifest["n_loc"],
        on_hand=on_hand,
        on_hand_jitter=0,
    )
    customers = [(d["doc"]["email"],) for d in gen.customer_docs()]
    stock = [(d["doc"]["sku"], d["doc"]["loc"], d["doc"]["on_hand"]) for d in gen.stock_docs()]
    return customers, stock


def write_seed_trace(path: Path, manifest: dict, on_hand: int = DEFAULT_ON_HAND) -> int:
    """Write the seeding trace `flashdb-scale seed-inventory` applies (`seed.py` dialect)."""
    gen = SeedGenerator(
        seed=manifest["seed"],
        n_customers=manifest["n_customers"],
        n_sku=manifest["n_sku"],
        n_loc=manifest["n_loc"],
        on_hand=on_hand,
        on_hand_jitter=0,
    )
    lines = [next(render([block])) for block in gen.blocks()]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    return len(lines)


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


# ---------------------------------------------------------------------------
# The SQL arms: the four request shapes, one durable commit per batch
# ---------------------------------------------------------------------------
# One spelling of the schema; the only dialect difference is the integer type. `"order"` is quoted
# because it is reserved in both products.
SCHEMA = (
    "CREATE TABLE customer (email TEXT PRIMARY KEY)",
    "CREATE TABLE stock (sku TEXT NOT NULL, loc TEXT NOT NULL, on_hand {int} NOT NULL,"
    " PRIMARY KEY (sku, loc))",
    "CREATE TABLE stock_movement (movement_no TEXT PRIMARY KEY, sku TEXT NOT NULL,"
    " loc TEXT NOT NULL, delta {int} NOT NULL, order_no TEXT NOT NULL)",
    "CREATE INDEX stock_movement_order ON stock_movement (order_no, movement_no)",
    'CREATE TABLE "order" (order_no TEXT PRIMARY KEY, buyer TEXT NOT NULL,'
    " status TEXT NOT NULL, sku TEXT NOT NULL, qty {int} NOT NULL)",
    "CREATE TABLE backorder (order_no TEXT PRIMARY KEY, sku TEXT NOT NULL, qty {int} NOT NULL)",
)


class SqlArm:
    """One SQL storage arm executing the generator's four templates.

    The shapes are the adapter's (`engine/src/request.rs`), translated to rows: `order_flow` reads
    the customer, refuses an order number that is already used in either order table, takes stock
    with a guarded decrement, and commits movement+order together -- or commits the backorder
    fallback. A missing customer writes nothing (the adapter skips `take` when `cust` is empty).
    """

    name = ""
    placeholder = "?"
    int_type = "INTEGER"

    def __init__(self, conn) -> None:
        self.conn = conn
        self.cur = conn.cursor()

    # -- dialect hooks ------------------------------------------------------
    def begin_write(self) -> None:
        raise NotImplementedError

    def commit(self) -> None:
        raise NotImplementedError

    def rollback(self) -> None:
        raise NotImplementedError

    # -- schema + seed ------------------------------------------------------
    def create_schema(self) -> None:
        for statement in SCHEMA:
            self.cur.execute(statement.replace("{int}", self.int_type))
        self.commit()

    def seed(self, customers: list[tuple], stock: list[tuple]) -> None:
        ph = self.placeholder
        self.cur.executemany(f"INSERT INTO customer (email) VALUES ({ph})", customers)
        self.cur.executemany(
            f"INSERT INTO stock (sku, loc, on_hand) VALUES ({ph}, {ph}, {ph})", stock)
        self.commit()

    # -- the four shapes ----------------------------------------------------
    def execute_one(self, req: dict) -> str:
        blocks = req["blocks"]
        if "take" in blocks:
            return self._order_flow(req)
        q = blocks["q"]
        if "get" in q:
            return self._stock_lookup(q["get"]["Stock"]["where"])
        find = q["find"]
        if "Customer" in find:
            return self._customer_lookup(find["Customer"]["email"])
        return self._movements_scan(find["StockMovement"]["where"]["order_no"])

    def _lookup(self, sql: str, args: tuple):
        self.cur.execute(sql, args)
        return self.cur.fetchone()

    def _order_flow(self, req: dict) -> str:
        ph = self.placeholder
        p = req["params"]
        sku, loc, qty, order_no = p["sku"], p["loc"], p["qty"], p["order_no"]
        if self._lookup(f"SELECT 1 FROM customer WHERE email={ph}", (p["email"],)) is None:
            return MISS
        # The adapter refuses an order number used by EITHER entity ("one logical order must not
        # exist both as a fulfilled Order and a Backorder"); the two primary keys cannot say that.
        for table in ('"order"', "backorder"):
            if self._lookup(f"SELECT 1 FROM {table} WHERE order_no={ph}", (order_no,)) is not None:
                raise SqlArmError(f"order number already used: {order_no}")
        self.cur.execute(
            f"UPDATE stock SET on_hand = on_hand - {ph}"
            f" WHERE sku={ph} AND loc={ph} AND on_hand >= {ph}",
            (qty, sku, loc, qty),
        )
        if self.cur.rowcount == 1:
            self.cur.execute(
                f"INSERT INTO stock_movement (movement_no, sku, loc, delta, order_no)"
                f" VALUES ({ph}, {ph}, {ph}, {ph}, {ph})",
                (f"M{order_no[1:]}", sku, loc, -qty, order_no),
            )
            self.cur.execute(
                f'INSERT INTO "order" (order_no, buyer, status, sku, qty)'
                f" VALUES ({ph}, {ph}, 'open', {ph}, {ph})",
                (order_no, p["email"], sku, qty),
            )
            return TAKE
        self.cur.execute(
            f"INSERT INTO backorder (order_no, sku, qty) VALUES ({ph}, {ph}, {ph})",
            (order_no, sku, qty),
        )
        return BACKORDER

    def _stock_lookup(self, where: dict) -> str:
        ph = self.placeholder
        self._lookup(
            f"SELECT sku, loc, on_hand FROM stock WHERE sku={ph} AND loc={ph}",
            (where["sku"], where["loc"]),
        )
        return READ

    def _customer_lookup(self, email: str) -> str:
        self._lookup(f"SELECT email FROM customer WHERE email={self.placeholder}", (email,))
        return READ

    def _movements_scan(self, order_no: str) -> str:
        ph = self.placeholder
        self.cur.execute(
            f"SELECT movement_no, sku, loc, delta, order_no FROM stock_movement"
            f" WHERE order_no={ph} ORDER BY movement_no LIMIT 100",
            (order_no,),
        )
        self.cur.fetchall()
        return READ

    # -- one batch ----------------------------------------------------------
    def execute_batch(self, reqs: list[dict]) -> dict:
        """Execute one batch and commit it once; a raising batch is rolled back and counted.

        The counters are per batch so the window record and the oracle share one accounting: a
        batch that raised contributes no committed write, so every request in it is an error.
        """
        counters = {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": 0, "first_error": None}
        if any("take" in r["blocks"] for r in reqs):
            self.begin_write()
        try:
            for req in reqs:
                outcome = self.execute_one(req)
                if outcome in counters:
                    counters[outcome] += 1
        except Exception as exc:  # noqa: BLE001 - the arm's failure is data, not a crash
            self.rollback()
            return {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": len(reqs),
                    "first_error": f"{reqs[0]['id']}: {exc}"}
        self.commit()
        return counters


class SqlArmError(RuntimeError):
    """A request the arm must refuse (the adapter refuses it too)."""


class SqliteArm(SqlArm):
    name = "sqlite"
    placeholder = "?"
    int_type = "INTEGER"
    durability = "sqlite WAL + synchronous=FULL, one COMMIT per batch"

    def __init__(self, conn: sqlite3.Connection) -> None:
        # Autocommit at the Python level: the transaction is explicit, so a batch's boundaries are
        # the pacer's and not sqlite3's implicit ones.
        conn.isolation_level = None
        super().__init__(conn)
        for pragma in ("journal_mode=WAL", "synchronous=FULL"):
            self.cur.execute(f"PRAGMA {pragma}")

    def begin_write(self) -> None:
        self.cur.execute("BEGIN IMMEDIATE")

    def commit(self) -> None:
        if self.conn.in_transaction:
            self.cur.execute("COMMIT")

    def rollback(self) -> None:
        if self.conn.in_transaction:
            self.cur.execute("ROLLBACK")


class PostgresArm(SqlArm):
    name = "postgres"
    placeholder = "%s"
    int_type = "BIGINT"
    durability = "postgres fsync=on + synchronous_commit=on (baseline-env.sh), one COMMIT per batch"

    def begin_write(self) -> None:
        # psycopg opens a transaction on the first statement of the batch.
        pass

    def commit(self) -> None:
        self.conn.commit()

    def rollback(self) -> None:
        self.conn.rollback()


def sqlite_arm(path: str | Path = ":memory:") -> SqliteArm:
    """A sqlite arm on `path` (`:memory:` by default) with its schema created."""
    arm = SqliteArm(sqlite3.connect(str(path)))
    arm.create_schema()
    return arm


# ---------------------------------------------------------------------------
# The pacer: cumulative due times, batches of ready requests, per-window samples
# ---------------------------------------------------------------------------
def build_schedule(manifest: dict) -> tuple[list[float], list[float]]:
    """`(due_us, rate_rps)` per request: the manifest's segments accumulated as intervals.

    Request `i` becomes ready at the cumulative offered interval before it, exactly as
    `flashdb-load`'s `interval_us`/`ready_group` compute it, so the Rust driver and this pacer
    offer the same schedule for the same manifest.
    """
    due: list[float] = []
    rates: list[float] = []
    t = 0.0
    for seg in manifest["segments"]:
        rate = float(seg["offered_rate"])
        interval = 1_000_000.0 / rate
        for _ in range(int(seg["count"])):
            due.append(t)
            rates.append(rate)
            t += interval
    if len(due) != manifest["total_requests"]:
        raise ValueError(
            f"manifest schedules {len(due)} requests but declares {manifest['total_requests']}")
    return due, rates


def percentile_us(samples: list[float], q: float) -> int:
    """Nearest-rank percentile in microseconds, the convention `flashdb-load` uses."""
    if not samples:
        return 0
    ordered = sorted(samples)
    rank = math.ceil(len(ordered) * q)
    return int(ordered[max(0, min(rank - 1, len(ordered) - 1))])


def _reject_reason(lag_breached: bool, errored: bool) -> str | None:
    """The window's rejection, spelled as `flashdb-load`'s `reject_reason` spells it."""
    parts = []
    if lag_breached:
        parts.append("lag_ceiling")
    if errored:
        parts.append("errors")
    return "+".join(parts) if parts else None


class WindowSamples:
    """One arm's per-window samples, emitted in the driver's window dialect.

    A window's requests are those DISPATCHED in it (the driver records schedule lag at dispatch
    too); `completed` is cumulative like the driver's, so `cold_0b.window_records` derives the
    window's own throughput as the delta over the nominal window span.
    """

    def __init__(self, window_ms: int, lag_ceiling_us: int = LAG_CEILING_US) -> None:
        self.window_us = window_ms * 1000
        self.lag_ceiling_us = lag_ceiling_us
        self.index = 0
        self.next_end_us = float(self.window_us)
        self.cum_completed = 0
        self.records: list[dict] = []
        self.reset(0.0)

    def reset(self, start_us: float) -> None:
        self.start_us = start_us
        self.lags: list[float] = []
        self.services: list[float] = []
        self.works: list[float] = []
        self.requests = 0
        self.groups = 0
        self.max_group = 0
        self.errors = 0
        self.early = 0
        self.min_lag_us = 0
        self.rate = 0.0

    def batch(self, lag_us: list[float], service_us: list[float], work_us: float,
              rate: float, errors: int) -> None:
        self.groups += 1
        self.requests += len(lag_us)
        self.max_group = max(self.max_group, len(lag_us))
        self.errors += errors
        self.cum_completed += len(lag_us) - errors
        self.rate = rate
        for raw, service in zip(lag_us, service_us):
            if raw < 0:
                self.early += 1
            self.lags.append(max(0.0, raw))
            self.services.append(service)
            self.works.append(work_us)
        self.min_lag_us = min([self.min_lag_us, *(int(x) for x in lag_us)])

    def snapshot(self, end_us: float, rate_fallback: float) -> dict:
        lag_p99 = percentile_us(self.lags, 0.99)
        lag_breached = lag_p99 > self.lag_ceiling_us
        errored = self.errors > 0
        return {
            "window": self.index,
            "rate_rps": self.rate or rate_fallback,
            "requests": self.requests,
            "completed": self.cum_completed,
            "errors": self.errors,
            # The window's own wall time; every window but a trailing partial one spans the
            # nominal window, so a rung's summed wall time is the run's duration.
            "window_us": int(round(end_us - self.start_us)),
            "service_p50_us": percentile_us(self.services, 0.50),
            "service_p95_us": percentile_us(self.services, 0.95),
            "service_p99_us": percentile_us(self.services, 0.99),
            "p50_us": percentile_us(self.services, 0.50),
            "p95_us": percentile_us(self.services, 0.95),
            "p99_us": percentile_us(self.services, 0.99),
            "work_p50_us": percentile_us(self.works, 0.50),
            "work_p99_us": percentile_us(self.works, 0.99),
            "lag_p50_us": percentile_us(self.lags, 0.50),
            "lag_p95_us": percentile_us(self.lags, 0.95),
            "lag_p99_us": lag_p99,
            "min_lag_us": self.min_lag_us,
            "groups": self.groups,
            "mean_group_size": (self.requests / self.groups) if self.groups else 0.0,
            "max_group_size": self.max_group,
            "early_dispatches": self.early,
            "service_ceiling_us": None,
            "service_ceiling_breached": False,
            "lag_ceiling_us": self.lag_ceiling_us,
            "lag_ceiling_breached": lag_breached,
            "invalid": lag_breached or errored,
            "reject_reason": _reject_reason(lag_breached, errored),
        }

    def flush(self, end_us: float, rate_fallback: float) -> None:
        self.records.append(self.snapshot(end_us, rate_fallback))
        self.index += 1
        self.reset(end_us)

    def flush_through(self, now_us: float, rate_fallback: float) -> None:
        """Emit every window that has ended; a stalled arm therefore emits empty windows."""
        while now_us >= self.next_end_us:
            self.flush(self.next_end_us, rate_fallback)
            self.next_end_us += self.window_us

    def finish(self, now_us: float, rate_fallback: float) -> None:
        if self.requests > 0 or now_us > self.start_us:
            self.flush(now_us, rate_fallback)


def _wait_until(t0_ns: int, due_us: float) -> None:
    """Sleep to the due instant, spinning only the last fraction of a millisecond."""
    target = t0_ns + int(due_us * 1000)
    while True:
        remaining = target - time.perf_counter_ns()
        if remaining <= 0:
            return
        if remaining > 400_000:
            time.sleep((remaining - 300_000) / 1e9)


def cgroup_peak_bytes() -> int | None:
    """The scope's `memory.peak`, read from inside the cgroup (None outside a scope)."""
    try:
        for line in Path("/proc/self/cgroup").read_text().splitlines():
            if line.startswith("0::"):
                peak = Path("/sys/fs/cgroup" + line[3:] + "/memory.peak")
                if peak.exists():
                    return int(peak.read_text())
    except OSError:
        return None
    return None


def run_sql_arm(
    arm: SqlArm,
    manifest: dict,
    requests: list[dict],
    batch: int = DEFAULT_BATCH,
    window_ms: int = DEFAULT_WINDOW_MS,
) -> dict:
    """Replay the trace against one SQL arm and return its run record.

    The loop is closed: a batch is executed and committed before the next one is dispatched, so a
    slow commit shows up as schedule lag on the next request rather than as hidden queueing.
    """
    due, rates = build_schedule(manifest)
    total = len(due)
    windows = WindowSamples(window_ms)
    counters = {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": 0, "first_error": None}
    groups = 0
    max_group = 0
    t0 = time.perf_counter_ns()

    def now_us() -> float:
        return (time.perf_counter_ns() - t0) / 1000.0

    i = 0
    while i < total:
        _wait_until(t0, due[i])
        now = now_us()
        windows.flush_through(now, rates[i])
        j = i
        while j < total and j - i < batch and due[j] <= now:
            j += 1
        start = now_us()
        state = arm.execute_batch(requests[i:j])
        done = now_us()
        groups += 1
        max_group = max(max_group, j - i)
        for key in (TAKE, BACKORDER, MISS, "errors"):
            counters[key] += state[key]
        if counters["first_error"] is None:
            counters["first_error"] = state["first_error"]
        windows.batch([start - due[k] for k in range(i, j)],
                      [done - due[k] for k in range(i, j)],
                      done - start, rates[i], state["errors"])
        i = j
    end_us = now_us()
    windows.finish(end_us, rates[-1])

    lines = [json.dumps(r, sort_keys=True) for r in windows.records]
    parsed = window_records(lines, window_ms)
    return {
        "arm": arm.name,
        "durability": arm.durability,
        "batch": batch,
        "window_ms": window_ms,
        "lag_ceiling_us": LAG_CEILING_US,
        "requests_run": total,
        "groups": groups,
        "mean_group_size": total / groups if groups else 0.0,
        "max_group_size": max_group,
        "elapsed_s": (end_us - due[0]) / 1e6,
        "counters": counters,
        "cgroup_peak_bytes": cgroup_peak_bytes(),
        "windows": parsed,
        "curve": summarize_curve(parsed, None),
        "windows_jsonl": lines,
    }


# ---------------------------------------------------------------------------
# The flashdb arm: the same trace through `flashdb-load`, the same windows
# ---------------------------------------------------------------------------
def flashdb_load_command(bins: Path, instance: Path, trace: Path, manifest: dict,
                         batch: int, window_ms: int) -> list[str]:
    """The driver invocation: one rung, `--group-requests B`, the pre-registered lag ceiling."""
    segments = manifest["segments"]
    rates = ",".join(str(seg["offered_rate"]) for seg in segments)
    cmd = [str(bins / "flashdb-load"), str(instance), str(trace),
           "--rate-rps", rates, "--window-ms", str(window_ms), "--metrics-json",
           "--group-requests", str(batch), "--lag-ceiling-us", str(LAG_CEILING_US)]
    if len(segments) > 1:
        counts = {int(seg["count"]) for seg in segments}
        if len(counts) != 1:
            raise ValueError("flashdb-load takes equal-sized segments; the manifest's differ")
        cmd += ["--segment-requests", str(counts.pop())]
    return cmd


def run_flashdb_arm(
    name: str,
    manifest: dict,
    workdir: Path,
    bins: Path,
    order_flow_requests: int,
    batch: int = DEFAULT_BATCH,
    window_ms: int = DEFAULT_WINDOW_MS,
) -> dict:
    """Seed a fresh instance from the same dimensions, run the trace, parse the driver's windows."""
    instance = workdir / f"instance-{name}"
    if instance.exists():
        shutil.rmtree(instance)
    seed_path = workdir / "seed.jsonl"
    trace = workdir / "trace.jsonl"
    seeded = subprocess.run(
        [str(bins / "flashdb-scale"), "seed-inventory", str(instance), str(seed_path)],
        capture_output=True, text=True)
    if seeded.returncode != 0:
        raise RuntimeError(f"seed-inventory failed (rc={seeded.returncode}): {seeded.stderr[-2000:]}")
    cmd = flashdb_load_command(bins, instance, trace, manifest, batch, window_ms)
    t0 = time.perf_counter()
    proc = subprocess.run(cmd, capture_output=True, text=True)
    elapsed = time.perf_counter() - t0
    lines = proc.stdout.splitlines()
    (workdir / f"driver-{name}.log").write_text(
        "\n".join([proc.stdout, proc.stderr]) + "\n", encoding="utf-8")
    if proc.returncode != 0:
        raise RuntimeError(f"flashdb-load failed (rc={proc.returncode}): {proc.stderr[-2000:]}")
    windows = window_records(lines, window_ms)
    summary = _parse_flashdb_summary(lines)
    groups = sum(w["groups"] for w in windows)
    requests_run = sum(w["requests"] for w in windows)
    return {
        "arm": name,
        "durability": f"flashdb --group-requests {batch} (one WAL barrier per group)",
        "policy": "declared default (no learner controller wired in M1)",
        "batch": batch,
        "window_ms": window_ms,
        "lag_ceiling_us": LAG_CEILING_US,
        "requests_run": requests_run,
        "groups": groups,
        "mean_group_size": requests_run / groups if groups else 0.0,
        "max_group_size": max((w["max_group_size"] for w in windows), default=0),
        "elapsed_s": summary.get("elapsed_ms", elapsed * 1000) / 1000.0,
        "counters": {"take": order_flow_requests - summary.get("else_used", 0)
                              - summary.get("errors", 0),
                     "backorder": summary.get("else_used"), "miss": 0,
                     "errors": summary.get("errors"),
                     "first_error": summary.get("first_error")},
        "summary": summary,
        "instance_bytes": _dir_bytes(instance),
        "cgroup_peak_bytes": cgroup_peak_bytes(),
        "windows": windows,
        "curve": summarize_curve(windows, None),
        "windows_jsonl": [json.dumps(w, sort_keys=True) for w in windows],
    }


# Every key of the driver's `load_summary` line this harness reads. `first_error` is a quoted Rust
# `Option` and can contain spaces, so it is kept only as a diagnostic prefix.
SUMMARY_INT_KEYS = ("requests", "completed", "errors", "else_used", "windows",
                    "invalid_windows", "lag_ceiling_rejected_windows", "early_dispatches",
                    "elapsed_ms")


def _parse_flashdb_summary(lines: list[str]) -> dict:
    """The driver's `load_summary` line, as the numbers the oracle needs."""
    out: dict = {}
    for line in lines:
        if not line.startswith("load_summary "):
            continue
        found = dict(re.findall(r"\b([a-z_]+)=(\S+)", line))
        for key in SUMMARY_INT_KEYS:
            if found.get(key, "").lstrip("-").isdigit():
                out[key] = int(found[key])
        first_error = found.get("first_error", "").strip('"')
        out["first_error"] = None if first_error in ("", "none") else first_error
    return out


def _dir_bytes(path: Path) -> int:
    return sum(p.stat().st_size for p in path.rglob("*") if p.is_file())


# ---------------------------------------------------------------------------
# The inventory oracle
# ---------------------------------------------------------------------------
def _check(checks: list[dict], name: str, ok: bool, detail: str) -> None:
    checks.append({"check": name, "ok": bool(ok), "detail": detail})
    if not ok:
        raise OracleViolation(f"{name}: {detail}")


def oracle_sql(arm: SqlArm, manifest: dict, counters: dict, order_flow_requests: int,
               on_hand_initial: int = DEFAULT_ON_HAND) -> dict:
    """Prove the SQL arm's committed state is the state the trace produced.

    Every check reads the arm's own database after the run and compares it with the arithmetic the
    trace implies. It fails at the FIRST violation with the offending cell/order named.
    """
    checks: list[dict] = []
    n_cells = int(manifest["n_sku"]) * int(manifest["n_loc"])
    ph = arm.placeholder

    arm.cur.execute(
        "SELECT s.sku, s.loc, s.on_hand,"
        " (SELECT COALESCE(SUM(m.delta), 0) FROM stock_movement m"
        "   WHERE m.sku = s.sku AND m.loc = s.loc) FROM stock s")
    cells = arm.cur.fetchall()
    _check(checks, "cell_count", len(cells) == n_cells,
           f"{len(cells)} stock cells, expected {n_cells} ({manifest['n_sku']} sku x"
           f" {manifest['n_loc']} loc)")
    for sku, loc, on_hand, delta in cells:
        # `delta` is the signed movement (-qty), so the cell's arithmetic is initial + sum(delta) --
        # the ticket's "on_hand_initial - sum(delta)" with a decrement counted as its magnitude.
        expected = on_hand_initial + int(delta)
        _check(checks, "on_hand_matches_movements", int(on_hand) == expected,
               f"stock({sku},{loc}).on_hand={on_hand}, initial {on_hand_initial} + sum(delta)"
               f" {delta} = {expected}")
        _check(checks, "on_hand_non_negative", int(on_hand) >= 0,
               f"stock({sku},{loc}).on_hand={on_hand}")

    arm.cur.execute(
        'SELECT order_no, COUNT(*) FROM (SELECT order_no FROM "order"'
        " UNION ALL SELECT order_no FROM backorder) AS all_orders"
        " GROUP BY order_no HAVING COUNT(*) > 1")
    dups = arm.cur.fetchall()
    _check(checks, "no_duplicate_order_no", not dups,
           f"{len(dups)} order numbers appear in both/again: {dups[:5]}")

    movements = _scalar(arm, "SELECT COUNT(*) FROM stock_movement")
    orders = _scalar(arm, 'SELECT COUNT(*) FROM "order"')
    backorders = _scalar(arm, "SELECT COUNT(*) FROM backorder")
    _check(checks, "movement_count_is_committed_takes", movements == counters[TAKE],
           f"{movements} movements, {counters[TAKE]} committed take blocks")
    _check(checks, "order_count_is_committed_takes", orders == counters[TAKE],
           f'{orders} orders, {counters[TAKE]} committed take blocks')
    _check(checks, "backorder_count_is_committed_fallbacks", backorders == counters[BACKORDER],
           f"{backorders} backorders, {counters[BACKORDER]} committed fallbacks")
    _check(checks, "every_order_flow_accounted",
           counters[TAKE] + counters[BACKORDER] + counters[MISS] == order_flow_requests,
           f"{counters[TAKE]} take + {counters[BACKORDER]} backorder + {counters[MISS]} miss"
           f" != {order_flow_requests} order_flow requests")
    _check(checks, "no_skipped_take", counters[MISS] == 0,
           f"{counters[MISS]} order_flow requests found no customer (every seeded email exists)")

    _check(checks, "movement_deltas_match_order_lines",
           _scalar(arm, 'SELECT COUNT(*) FROM stock_movement m LEFT JOIN "order" o'
                        " ON o.order_no = m.order_no"
                        " WHERE o.order_no IS NULL OR o.qty <> -m.delta OR o.sku <> m.sku") == 0,
           "every movement has its order, with delta == -qty and the same sku")
    _check(checks, "movement_delta_sum",
           _scalar(arm, "SELECT COALESCE(SUM(delta), 0) FROM stock_movement")
           == -_scalar(arm, 'SELECT COALESCE(SUM(qty), 0) FROM "order"'),
           "sum(delta) over movements == -sum(qty) over fulfilled orders")
    _check(checks, "stock_decrements_match_orders",
           sum(int(c[2]) for c in cells) == on_hand_initial * n_cells
           - _scalar(arm, 'SELECT COALESCE(SUM(qty), 0) FROM "order"'),
           f"sum(on_hand) == {n_cells} cells x {on_hand_initial} - taken qty")
    return {"status": "PASS", "checks": checks, "order_no_uniqueness": "queried"}


def _scalar(arm: SqlArm, sql: str) -> int:
    arm.cur.execute(sql)
    return int(arm.cur.fetchone()[0])


def build_probe_trace(manifest: dict, order_nos: list[str], customers: list[str]) -> list[dict]:
    """The read-only probe trace the flashdb oracle reads the instance back with.

    The adapter has no Order/Backorder read shape, so the state is read through the shapes it does
    have: every (sku, loc) cell via `stock_lookup`, every seeded email via `customer_lookup` (which
    proves no order_flow took the skipped path), and every order number in the trace via
    `movements_scan` (which yields that order's movement rows). The probe target is encoded in the
    request id so a replay line can be attributed without a second mapping.
    """
    probes: list[dict] = []
    for rank in range(1, int(manifest["n_sku"]) + 1):
        sku = sku_name(rank)
        for j in range(int(manifest["n_loc"])):
            loc = loc_name(j)
            probes.append({
                "id": f"probe-stock:{sku}:{loc}",
                "class": {"durability": "batched", "max_staleness": 2},
                "blocks": {"q": {"get": {"Stock": {"where": {"sku": sku, "loc": loc}}}}},
            })
    for email in customers:
        probes.append({
            "id": f"probe-customer:{email}",
            "class": {"durability": "batched"},
            "blocks": {"q": {"find": {"Customer": {"email": email}}}},
        })
    for order_no in order_nos:
        probes.append({
            "id": f"probe-movements:{order_no}",
            "class": {"durability": "batched", "max_staleness": 5},
            "blocks": {"q": {"find": {"StockMovement": {"where": {"order_no": order_no}},
                                      "order": ["movement_no"], "limit": 100}}},
        })
    return probes


def oracle_flashdb(replayed: list[dict], manifest: dict, counters: dict,
                   on_hand_initial: int = DEFAULT_ON_HAND) -> dict:
    """The same inventory arithmetic as `oracle_sql`, read back through the request interface.

    `replayed` is one `{line, id, blocks}` object per probe (the `flashdb-replay` output dialect).
    The limit is disclosed in the returned record: `Order`/`Backorder` have no read shape, so the
    order-number uniqueness check cannot be re-queried here; flashdb refuses a reused order number
    at write time (unique on both entities plus the adapter's cross-entity check) and any refusal
    is an error in the window record, which `oracle_flashdb` requires to be zero.
    """
    checks: list[dict] = []
    on_hand: dict[tuple[str, str], int] = {}
    deltas: dict[tuple[str, str], int] = {}
    movements = 0
    orders_with_movements = 0
    customers_seen = 0
    for entry in replayed:
        target = entry["id"]
        block = entry["blocks"]["q"]
        row = block.get("row")
        if target.startswith("probe-stock:"):
            _, sku, loc = target.split(":")
            _check(checks, "stock_cell_present", row is not None, f"Stock({sku},{loc}) is empty")
            on_hand[(sku, loc)] = int(row["on_hand"])
        elif target.startswith("probe-customer:"):
            _check(checks, "customer_present", row is not None, f"{target} is empty")
            customers_seen += 1
        else:
            order_no = target.split(":", 1)[1]
            rows = row or []
            _check(checks, "one_movement_per_order", len(rows) <= 1,
                   f"{order_no} returned {len(rows)} movements, expected at most 1")
            for mov in rows:
                _check(checks, "movement_order_no_matches",
                       mov["order_no"] == order_no,
                       f"scan for {order_no} returned movement of {mov['order_no']}")
                key = (mov["sku"], mov["loc"])
                deltas[key] = deltas.get(key, 0) + int(mov["delta"])
                movements += 1
            orders_with_movements += 1 if rows else 0

    n_cells = int(manifest["n_sku"]) * int(manifest["n_loc"])
    _check(checks, "cell_count", len(on_hand) == n_cells,
           f"{len(on_hand)} cells read back, expected {n_cells}")
    _check(checks, "no_skipped_take", customers_seen == int(manifest["n_customers"]),
           f"{customers_seen} customers read back, expected {manifest['n_customers']}")
    for (sku, loc), value in on_hand.items():
        expected = on_hand_initial + deltas.get((sku, loc), 0)
        _check(checks, "on_hand_matches_movements", value == expected,
               f"stock({sku},{loc}).on_hand={value}, initial {on_hand_initial} + sum(delta)"
               f" {deltas.get((sku, loc), 0)} = {expected}")
        _check(checks, "on_hand_non_negative", value >= 0, f"stock({sku},{loc}).on_hand={value}")
    _check(checks, "movement_count_is_committed_takes", movements == counters["take"],
           f"{movements} movements read back, {counters['take']} committed take blocks")
    _check(checks, "order_count_is_committed_takes", orders_with_movements == counters["take"],
           f"{orders_with_movements} orders carry a movement, {counters['take']} takes committed")
    _check(checks, "no_request_errors", counters["errors"] == 0,
           f"{counters['errors']} request errors (first: {counters.get('first_error')})")
    return {"status": "PASS", "checks": checks,
            "order_no_uniqueness": "enforced at write time (no Order read shape); zero request"
                                   " errors required"}


def run_flashdb_probes(name: str, manifest: dict, workdir: Path, bins: Path,
                       trace_orders: list[str]) -> list[dict]:
    """Replay the probe trace read-only and parse `flashdb-replay`'s one-JSON-line-per-probe output."""
    instance = workdir / f"instance-{name}"
    customers, _stock = seed_docs(manifest)
    probes = build_probe_trace(manifest, trace_orders, [c[0] for c in customers])
    probe_path = workdir / f"probe-{name}.jsonl"
    probe_path.write_text("\n".join(json.dumps(p, sort_keys=True) for p in probes) + "\n",
                          encoding="utf-8")
    proc = subprocess.run([str(bins / "flashdb-replay"), str(instance), str(probe_path)],
                          capture_output=True, text=True)
    (workdir / f"probe-{name}.log").write_text(proc.stdout + proc.stderr, encoding="utf-8")
    if proc.returncode != 0:
        raise RuntimeError(f"flashdb-replay failed (rc={proc.returncode}): {proc.stderr[-2000:]}")
    return [json.loads(line) for line in proc.stdout.splitlines() if line.startswith("{")]


# ---------------------------------------------------------------------------
# Shared reporting
# ---------------------------------------------------------------------------
def arm_verdict(curve: list[dict]) -> dict:
    """The ticket's verdict for one arm, computed only from `summarize_curve`'s rows."""
    sustained = [row for row in curve if row["sustained"]]
    return {
        "sustained": bool(sustained),
        "highest_sustained_rps": max((row["offered_rps"] for row in sustained), default=0.0),
        "rejected_windows": sum(row["invalid_windows"] for row in curve),
        "reject_reasons": {reason: count
                           for row in curve for reason, count in row["reject_reasons"].items()},
    }


def scoreboard_row(record: dict) -> str:
    """One markdown row: the numbers a reader compares, with the arm's condition named."""
    rows = []
    for curve in record["curve"]:
        rows.append(
            f"| {record['arm']} | {curve['offered_rps']:.0f} | {curve['achieved_rps']:.2f} |"
            f" {curve['achieved_fraction']:.1%} | {curve['lag_p99_us']} |"
            f" {curve['p99_us']} | {curve['windows']} | {curve['invalid_windows']} |"
            f" {'yes' if curve['sustained'] else 'no'} |"
            f" {', '.join(f'{k}={v}' for k, v in sorted(curve['reject_reasons'].items())) or '-'} |")
    return "\n".join(rows)


SCOREBOARD_HEADER = (
    "| arm | offered rps | achieved rps | achieved/offered | lag p99 us | service p99 us |"
    " windows | rejected | sustained | reject reasons |\n"
    "|---|---|---|---|---|---|---|---|---|---|")


def render_scoreboard(records: list[dict], manifest: dict, spec: dict) -> str:
    lines = [
        "# M1 baseline run",
        "",
        f"- trace sha256: `{manifest['trace_sha256']}`",
        f"- requests: {manifest['total_requests']}, segments: "
        f"{[(s['offered_rate'], s['count']) for s in manifest['segments']]}",
        f"- dimensions: {manifest['n_customers']} customers, {manifest['n_sku']} sku,"
        f" {manifest['n_loc']} loc, seed {manifest['seed']}, theta {manifest['theta']}",
        f"- batch B: {spec['batch']} (one durable commit per batch on every arm)",
        f"- lag ceiling: {spec['lag_ceiling_us']} us p99; sustained = achieved within 5% of"
        " offered and zero rejected windows",
        "",
        SCOREBOARD_HEADER,
    ]
    lines += [scoreboard_row(record) for record in records]
    lines += ["", "## Durability, oracle and footprint per arm", "",
              "| arm | durability | oracle | cgroup peak MiB | mean group size |",
              "|---|---|---|---|---|"]
    for record in records:
        peak = record.get("cgroup_peak_bytes")
        peak_col = f"{peak / (1 << 20):.1f}" if peak is not None else "not measured"
        lines.append(
            f"| {record['arm']} | {record['durability']} | {record['oracle']['status']} |"
            f" {peak_col} | {record['mean_group_size']:.2f} |")
    return "\n".join(lines) + "\n"


# ---------------------------------------------------------------------------
# CLI steps: prepare (trace + manifest + seed), arm (one arm), report
# ---------------------------------------------------------------------------
def _write_json(path: Path, payload) -> None:
    path.write_text(json.dumps(payload, sort_keys=True, indent=2) + "\n", encoding="utf-8")


def _read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def cmd_prepare(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    workdir.mkdir(parents=True, exist_ok=True)
    trace_bytes, manifest = build_trace_baseline(
        seed=args.seed, n_requests=args.requests, n_customers=args.customers,
        n_sku=args.skus, n_loc=args.locs, offered_rate=args.rate, theta=args.theta)
    (workdir / "trace.jsonl").write_bytes(trace_bytes)
    # The oracle's order-flow count is derived here, once, from the emitted trace and carried in the
    # spec the arm steps read; the v1 manifest itself stays exactly what `load_profile` writes, so
    # the Rust pacer's document is unchanged.
    requests = [json.loads(line) for line in trace_bytes.decode().splitlines()]
    order_flows = sum(1 for r in requests if "take" in r["blocks"])
    _write_json(workdir / "manifest.json", manifest)
    _write_json(workdir / "spec.json", {
        "schema": SPEC_SCHEMA,
        "on_hand": args.on_hand,
        "batch": args.batch,
        "window_ms": args.window_ms,
        "lag_ceiling_us": LAG_CEILING_US,
        "order_flow_requests": order_flows,
        "trace_sha256": manifest["trace_sha256"],
    })
    blocks = write_seed_trace(workdir / "seed.jsonl", manifest, args.on_hand)
    print(f"trace {workdir / 'trace.jsonl'}: requests={manifest['total_requests']}"
          f" order_flow={order_flows} seed_blocks={blocks}"
          f" sha256={manifest['trace_sha256']}")
    return 0


def _load_workdir(workdir: Path) -> tuple[dict, dict, list[dict]]:
    manifest = _read_json(workdir / "manifest.json")
    spec = _read_json(workdir / "spec.json")
    trace_sha = file_sha256(workdir / "trace.jsonl")
    if trace_sha != manifest["trace_sha256"]:
        raise SystemExit(f"trace {workdir / 'trace.jsonl'} hashes to {trace_sha} but the manifest"
                         f" says {manifest['trace_sha256']}; rerun prepare")
    requests = [json.loads(line) for line in
                (workdir / "trace.jsonl").read_text(encoding="utf-8").splitlines()]
    return manifest, spec, requests


def cmd_arm(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    manifest, spec, requests = _load_workdir(workdir)
    name = args.arm
    if name in FLASHDB_ARMS:
        record = run_flashdb_arm(name, manifest, workdir, Path(args.bins),
                                 spec["order_flow_requests"], spec["batch"], spec["window_ms"])
        trace_orders = sorted({r["params"]["order_no"] for r in requests if "take" in r["blocks"]})
        replayed = run_flashdb_probes(name, manifest, workdir, Path(args.bins), trace_orders)
        record["oracle"] = oracle_flashdb(replayed, manifest, {
            "take": record["counters"]["take"],
            "errors": record["counters"]["errors"],
            "first_error": record["counters"]["first_error"],
        }, spec["on_hand"])
    elif name == "sqlite":
        db = workdir / f"{name}.db"
        for stale in (db, db.with_suffix(".db-wal"), db.with_suffix(".db-shm")):
            stale.unlink(missing_ok=True)
        arm = sqlite_arm(db)
        customers, stock = seed_docs(manifest, spec["on_hand"])
        arm.seed(customers, stock)
        record = run_sql_arm(arm, manifest, requests, spec["batch"], spec["window_ms"])
        record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                      spec["order_flow_requests"], spec["on_hand"])
    elif name == "postgres":
        record = run_postgres_arm(name, manifest, spec, requests)
    else:
        raise SystemExit(f"unknown arm {name!r}; expected one of {FLASHDB_ARMS + SQL_ARMS}")
    record["trace_sha256"] = manifest["trace_sha256"]
    record["verdict"] = arm_verdict(record["curve"])
    windows_path = workdir / f"windows-{name}.jsonl"
    windows_path.write_text("\n".join(record["windows_jsonl"]) + "\n", encoding="utf-8")
    record = {k: v for k, v in record.items() if k != "windows_jsonl"}
    _write_json(workdir / f"run-{name}.json", record)
    print(f"{name}: oracle {record['oracle']['status']};"
          f" sustained={record['verdict']['sustained']}"
          f" highest={record['verdict']['highest_sustained_rps']:.0f} rps"
          f" rejected_windows={record['verdict']['rejected_windows']}"
          f" cgroup_peak={record['cgroup_peak_bytes']}"
          f" sha256={record['trace_sha256']}")
    return 0


def run_postgres_arm(name: str, manifest: dict, spec: dict, requests: list[dict]) -> dict:
    """The postgres arm against the user-space cluster `scripts/baseline-env.sh` provides."""
    import psycopg  # imported here: only this arm needs it, and only inside its capped scope

    def connect(dbname: str):
        return psycopg.connect(host=os.environ["M1_PGHOST"], port=os.environ["M1_PGPORT"],
                               user=os.environ["M1_PGUSER"], dbname=dbname, autocommit=True)

    dbname = f"m1_{name}_{os.getpid()}"
    with connect("postgres") as admin:
        admin.execute(f'DROP DATABASE IF EXISTS "{dbname}"')
        admin.execute(f'CREATE DATABASE "{dbname}"')
    try:
        conn = connect(dbname)
        conn.autocommit = False
        arm = PostgresArm(conn)
        arm.create_schema()
        customers, stock = seed_docs(manifest, spec["on_hand"])
        arm.seed(customers, stock)
        record = run_sql_arm(arm, manifest, requests, spec["batch"], spec["window_ms"])
        record["durability"] = PostgresArm.durability
        record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                      spec["order_flow_requests"], spec["on_hand"])
        record["postgres_settings"] = {
            "synchronous_commit": _setting(conn, "synchronous_commit"),
            "fsync": _setting(conn, "fsync"),
            "shared_buffers": _setting(conn, "shared_buffers"),
            "server_version": _setting(conn, "server_version"),
        }
        conn.close()
    finally:
        with connect("postgres") as admin:
            admin.execute(f'DROP DATABASE IF EXISTS "{dbname}"')
    return record


def _setting(conn, name: str) -> str:
    with conn.cursor() as cur:
        cur.execute(f"SHOW {name}")
        return cur.fetchone()[0]


def cmd_report(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    manifest = _read_json(workdir / "manifest.json")
    spec = _read_json(workdir / "spec.json")
    arms = args.arms or [a for a in ARM_NAMES if (workdir / f"run-{a}.json").exists()]
    records = [_read_json(workdir / f"run-{a}.json") for a in arms]
    text = render_scoreboard(records, manifest, spec)
    (workdir / "scoreboard.md").write_text(text, encoding="utf-8")
    _write_json(workdir / "scoreboard.json", {
        "trace_sha256": manifest["trace_sha256"], "spec": spec,
        "arms": [{k: v for k, v in record.items() if k not in ("windows", "curve")}
                 for record in records],
        "curve": {record["arm"]: record["curve"] for record in records},
    })
    print(text)
    violations = [record["arm"] for record in records if record["oracle"]["status"] != "PASS"]
    if violations:
        print(f"ORACLE FAILED for {violations}", file=sys.stderr)
        return 2
    return 0


def _arm_names(value: str) -> list[str]:
    names = [part.strip() for part in value.split(",") if part.strip()]
    for name in names:
        if name not in FLASHDB_ARMS + SQL_ARMS:
            raise argparse.ArgumentTypeError(f"unknown arm {name!r}")
    return names


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="M1 request-level external-baseline harness")
    sub = ap.add_subparsers(dest="step", required=True)

    prep = sub.add_parser("prepare", help="emit the trace, manifest, spec and seeding trace")
    prep.add_argument("--workdir", required=True)
    prep.add_argument("--seed", type=int, default=42)
    prep.add_argument("--requests", type=int, default=2_000)
    prep.add_argument("--rate", type=int, default=50, help="offered requests per second")
    prep.add_argument("--customers", type=int, default=200)
    prep.add_argument("--skus", type=int, default=500)
    prep.add_argument("--locs", type=int, default=4)
    prep.add_argument("--theta", type=float, default=1.0)
    prep.add_argument("--on-hand", type=int, default=DEFAULT_ON_HAND)
    prep.add_argument("--batch", type=int, default=DEFAULT_BATCH)
    prep.add_argument("--window-ms", type=int, default=DEFAULT_WINDOW_MS)
    prep.set_defaults(func=cmd_prepare)

    arm = sub.add_parser("arm", help="run one arm over the prepared trace")
    arm.add_argument("--workdir", required=True)
    arm.add_argument("--arm", required=True)
    arm.add_argument("--bins", default=str(REPO_ROOT / "target" / "release"),
                     help="directory holding flashdb-load / flashdb-scale / flashdb-replay")
    arm.set_defaults(func=cmd_arm)

    report = sub.add_parser("report", help="write the scoreboard from the arm records")
    report.add_argument("--workdir", required=True)
    report.add_argument("--arms", type=_arm_names, default=None)
    report.set_defaults(func=cmd_report)

    args = ap.parse_args(argv)
    try:
        return args.func(args)
    except OracleViolation as violation:
        print(f"ORACLE VIOLATION: {violation}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
