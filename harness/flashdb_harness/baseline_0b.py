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
A window is REJECTED when
  * its per-request schedule-lag p99 exceeds `LAG_CEILING_US` (10,000 us, the pre-registration's
    10 ms bound), named `lag_ceiling`; or
  * lag has GROWN for `LAG_GROWTH_WINDOWS` (3) consecutive windows -- a window counts toward the
    growth streak when its p99 breached or when any of its requests lagged past the ceiling even
    though the p99 did not -- named `lag_saturation`; or
  * any request in it errored, named `errors`.
The three flags are exactly `flashdb-load`'s (`late_growth_windows`/`saturated` and
`reject_reason`), so the SQL arms and the flashdb arms are rejected by the same instrument. A rung
is SUSTAINED iff its achieved rate is within 5% of the offered rate
(`cold_0b.SUSTAINED_TOLERANCE`) and zero windows were rejected. `summarize_curve` adds its
`rate_exceeded` guard: an achieved rate above the offer is a measurement error, never a sustain.

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
driver acks after the barrier). Each arm's settings are read BACK from the engine and recorded, not
taken from the code that set them: sqlite's `journal_mode`/`synchronous` via `PRAGMA` after the bulk
load (`SqliteArm.require_measured_durability`, because the seeding path runs journal-less and a
`PRAGMA` that cannot be applied raises nothing), postgres's via the server's own `SHOW`. At the
offered rates this work package runs, a group is one request (the batch ceiling never binds) and the
measured `mean_group_size` in the run record says so. `batch` is recorded on every arm so a later
rung at a higher rate stays comparable.

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
import shlex
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
from pathlib import Path

from flashdb_harness import load_profile
from flashdb_harness.cold_0b import summarize_curve, window_records
from flashdb_harness.generator import loc_name, render, sku_name
from flashdb_harness.seed import SeedGenerator

REPO_ROOT = Path(__file__).resolve().parents[2]

# The pre-registration's schedule-lag bound; the same value `flashdb-load --lag-ceiling-us`
# defaults to, and the one this harness passes to the driver explicitly.
LAG_CEILING_US = 10_000
# Consecutive windows whose lag grew before a rung is called saturated: `flashdb-load`'s
# `late_growth_windows >= 3` (`engine/src/bin/flashdb-load.rs`). Named here because the SQL arms
# must apply the same clause, not a weaker one.
LAG_GROWTH_WINDOWS = 3
DEFAULT_WINDOW_MS = 1_000
# `cold_0b`'s shipped `--group-requests` cadence. Named once here so every arm states the batch it
# actually ran with.
DEFAULT_BATCH = 64
# Rows per bulk-load statement during seeding (setup only; not the measured path).
SEED_CHUNK = 5_000
# The memory budget the MEASURED phase runs under, and the construction budget `seed-inventory`
# gets. They differ for the reason slice 51 states: construction is heavier than serving (the seed
# scope holds the whole dimension set in its memtable, ~500 B/doc), so the regime cap judges
# serving while the dataset is BUILT under a larger transient cap. Both are recorded per run.
ENGINE_CAP_MIB = 128
SEED_CAP_MIB = 192
# The construction cadence the published 10x build uses (slice 51): the shipped 16 KiB default
# exhausts the 1024-tile publish cap (slice 46), and with no cadence at all the seed memtable holds
# the whole dimension set (~500 B/doc measured here), which is what makes a 10^7-doc seed exceed any
# reasonable construction cap. These are construction settings only; the measured load is untouched.
SEED_ENV = {"FLASHDB_SEED_CHECKPOINT_BYTES": str(8 << 20),
            "FLASHDB_SEED_ROTATE_BYTES": str(8 << 20)}
# sqlite's page cache in KiB (`PRAGMA cache_size`, negative = KiB). sqlite's engine runs inside the
# arm's own process, so the engine's cache is stated here and the process is capped in the runner.
SQLITE_CACHE_KIB = 8_192
# The `PRAGMA synchronous` value the measured path requires. That pragma's scale is its own
# (0 OFF, 1 NORMAL, 2 FULL, 3 EXTRA) and is NOT the C API's `SQLITE_SYNC_*` flag constants.
SQLITE_SYNCHRONOUS_FULL = 2
# The line `scoped_command` prepends and appends: the engine cgroup's own `memory.current` at the
# start of the invocation and its `memory.peak`, both read from inside it.
SCOPE_START_PREFIX = "scope_memory_start_bytes="
ENGINE_PEAK_PREFIX = "engine_memory_peak_bytes="
SEED_PEAK_PREFIX = "seed_memory_peak_bytes="
# How often the measured-phase sampler reads `memory.current` (see `MemoryPeakSampler`).
MEMORY_SAMPLE_INTERVAL_S = 0.05
# The cgroup a runner-invoked arm's engine lives in when the engine is NOT in the arm's own cgroup
# (postgres: the server runs in its own scope and the client is outside it).
ENGINE_CGROUP_ENV = "M1_ENGINE_CGROUP"
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


def seed_docs(manifest: dict, on_hand: int = DEFAULT_ON_HAND):
    """The dimension rows every arm is seeded from: `(customers, stock)` in insert order.

    Both come from one `SeedGenerator` built from the manifest, so the external arms and
    `flashdb-scale seed-inventory` are seeded from the same rows. They are generators, not lists:
    a dimension set of 10^6-10^7 rows materialised here would be a memory decision about the
    instrument rather than about the engine, and it is the arm's own process (sqlite's, or the
    flashdb read-back) that pays for it.
    """
    gen = SeedGenerator(
        seed=manifest["seed"],
        n_customers=manifest["n_customers"],
        n_sku=manifest["n_sku"],
        n_loc=manifest["n_loc"],
        on_hand=on_hand,
        on_hand_jitter=0,
    )
    customers = ((d["doc"]["email"],) for d in gen.customer_docs())
    stock = ((d["doc"]["sku"], d["doc"]["loc"], d["doc"]["on_hand"]) for d in gen.stock_docs())
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


def iter_requests(trace: Path):
    """Stream one frozen trace request at a time.

    The parsed form of a request is ~7 KB of Python objects (nested blocks and strings), so
    materialising a 10^5-request trace is a decision about the harness's memory, not about the
    engine's. The pacer only ever needs the batch in front of it, so it streams.
    """
    with open(trace, encoding="utf-8") as fh:
        for line in fh:
            if line.strip():
                yield json.loads(line)


def iter_order_numbers(trace: Path):
    """The trace's `order_flow` order numbers, in trace order (the movement-scan probe targets)."""
    for req in iter_requests(trace):
        if "take" in req["blocks"]:
            yield req["params"]["order_no"]


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
    def begin_bulk_load(self) -> None:
        """Open the seeding transaction under the arm's fastest SAFE-for-setup settings."""
        raise NotImplementedError

    def end_bulk_load(self) -> None:
        """Restore the measured-path durability settings (the ticket's per-arm statement)."""
        raise NotImplementedError

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

    def seed(self, customers, stock) -> None:
        """Bulk-load the dimension rows: this is SETUP, not the measured request path.

        The streams are consumed in chunks and loaded in ONE transaction through the arm's bulk
        path (`begin_bulk_load`/`end_bulk_load`), so the row count never has to fit in the capped
        scope and the load does not pay one durable commit per row. The durability settings the
        measured replay runs under are restored by `end_bulk_load` before any request is issued.
        """
        self.begin_bulk_load()
        try:
            for chunk in _chunks(customers, SEED_CHUNK):
                self.cur.executemany(
                    f"INSERT INTO customer (email) VALUES ({self.placeholder})", chunk)
            for chunk in _chunks(stock, SEED_CHUNK):
                ph = self.placeholder
                self.cur.executemany(
                    f"INSERT INTO stock (sku, loc, on_hand) VALUES ({ph}, {ph}, {ph})", chunk)
            self.commit()
        except BaseException:
            self.rollback()
            raise
        self.end_bulk_load()

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
    """A request the arm must refuse (the adapter refuses it too), or a setting it must have held."""


def _chunks(rows, size: int):
    """Consume a row stream `size` at a time, so a bulk load runs in bounded memory."""
    chunk = []
    for row in rows:
        chunk.append(row)
        if len(chunk) == size:
            yield chunk
            chunk = []
    if chunk:
        yield chunk


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
        self._set_measured_durability()

    def set_engine_memory(self, cache_kib: int) -> None:
        """State sqlite's page-cache budget explicitly (`cache_size` in KiB, negative = KiB).

        sqlite runs INSIDE the arm's process, so unlike flashdb (a separate `flashdb-load`) and
        postgres (a separate server) its engine memory is not separated by a cgroup: this pragma is
        the engine's own cache bound, and the process cap in `scripts/m1-baseline-run.sh` bounds
        driver + engine together. Both are reported.
        """
        self.cur.execute(f"PRAGMA cache_size = -{cache_kib}")

    def _set_measured_durability(self) -> None:
        for pragma in ("journal_mode=WAL", "synchronous=FULL"):
            self.cur.execute(f"PRAGMA {pragma}")

    def observed_durability(self) -> dict:
        """The connection's durability, read BACK from sqlite rather than assumed from the setter.

        `PRAGMA journal_mode=WAL` that cannot be applied returns the old mode and raises nothing,
        and the seeding path deliberately runs with `journal_mode=OFF, synchronous=OFF`, so the
        measured path depends on this restore actually having taken effect.
        """
        self.cur.execute("PRAGMA journal_mode")
        mode = str(self.cur.fetchone()[0]).lower()
        self.cur.execute("PRAGMA synchronous")
        sync = int(self.cur.fetchone()[0])
        return {"journal_mode": mode, "synchronous": sync,
                "synchronous_name": "FULL" if sync == SQLITE_SYNCHRONOUS_FULL else str(sync)}

    def require_measured_durability(self) -> dict:
        """Fail loudly unless the measured path ran under WAL + `synchronous=FULL`."""
        observed = self.observed_durability()
        if (observed["journal_mode"], observed["synchronous"]) != ("wal",
                                                                  SQLITE_SYNCHRONOUS_FULL):
            raise SqlArmError(
                f"sqlite measured path is not WAL + synchronous=FULL: observed"
                f" journal_mode={observed['journal_mode']}"
                f" synchronous={observed['synchronous']}")
        return observed

    def begin_bulk_load(self) -> None:
        # Setup only (no request is issued while these settings are in force): no rollback journal
        # and no fsync turn a 10^7-row load from one fsync per row into one write. The file is
        # disposable (each arm run deletes and recreates it), so there is nothing to roll back.
        for pragma in ("journal_mode=OFF", "synchronous=OFF"):
            self.cur.execute(f"PRAGMA {pragma}")
        self.cur.execute("BEGIN IMMEDIATE")

    def end_bulk_load(self) -> None:
        self._set_measured_durability()

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

    def begin_bulk_load(self) -> None:
        # psycopg opens a transaction on the first statement and psycopg 3 pipelines executemany,
        # so one transaction is already the bulk path; the server's durability settings are the
        # ones the measured replay runs under.
        pass

    def end_bulk_load(self) -> None:
        pass

    def begin_write(self) -> None:
        # psycopg opens a transaction on the first statement of the batch.
        pass

    def commit(self) -> None:
        self.conn.commit()

    def rollback(self) -> None:
        self.conn.rollback()


# Created AFTER the measured replay and only for verification: the oracle's per-cell arithmetic
# needs `stock_movement` by (sku, loc), and without this index its cost is the product of the cells
# and the movements (hours at 10^7 cells). It cannot lengthen the measured path, because nothing
# writes after the replay ends.
VERIFY_INDEX = "CREATE INDEX stock_movement_sku_loc ON stock_movement (sku, loc)"


def add_verify_index(arm: SqlArm) -> None:
    arm.cur.execute(VERIFY_INDEX)
    arm.commit()


def sqlite_arm(path: str | Path = ":memory:", cache_kib: int = SQLITE_CACHE_KIB) -> SqliteArm:
    """A sqlite arm on `path` (`:memory:` by default), schema created, page cache stated."""
    arm = SqliteArm(sqlite3.connect(str(path)))
    arm.set_engine_memory(cache_kib)
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


def _reject_reason(lag_breached: bool, errored: bool, saturated: bool = False) -> str | None:
    """The window's rejection, spelled as `flashdb-load`'s `reject_reason` spells it.

    The driver names the service ceiling first, then the lag ceiling, then the growth rule (kept
    distinct from a single ceiling breach) and the errors last
    (`engine/src/bin/flashdb-load.rs` `reject_reason`). This harness passes no service ceiling, so
    only the other three can fire, and they are spelled identically.
    """
    parts = []
    if lag_breached:
        parts.append("lag_ceiling")
    if saturated and not lag_breached:
        parts.append("lag_saturation")
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
        # The lag-growth streak: consecutive windows in which lag grew (a breached p99, or any
        # request past the ceiling). It is the driver's `late_growth_windows`, carried ACROSS
        # windows, so `reset` must not clear it.
        self.late_growth_windows = 0
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

    def late_requests(self) -> int:
        """Requests in this window whose schedule lag passed the ceiling (the driver's `window_late`)."""
        return sum(1 for lag in self.lags if lag > self.lag_ceiling_us)

    def advance_growth(self) -> None:
        """Extend or reset the lag-growth streak with the window just closed.

        `flashdb-load` does this once per emitted window, on `behind || window_late > 0`, and a
        window with no requests at all resets the streak. Called by `flush`, which is the one place
        a window is closed, so the streak can never be advanced twice for one window.
        """
        if percentile_us(self.lags, 0.99) > self.lag_ceiling_us or self.late_requests() > 0:
            self.late_growth_windows += 1
        else:
            self.late_growth_windows = 0

    def snapshot(self, end_us: float, rate_fallback: float) -> dict:
        lag_p99 = percentile_us(self.lags, 0.99)
        lag_breached = lag_p99 > self.lag_ceiling_us
        errored = self.errors > 0
        saturated = self.late_growth_windows >= LAG_GROWTH_WINDOWS
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
            # The growth clause's own inputs, recorded so a reader (or a re-scoring pass) can see
            # WHY a window was saturated instead of taking the verdict on trust. The driver emits
            # neither, so `cold_0b.window_records` ignores them for its own windows.
            "late_requests": self.late_requests(),
            "late_growth_windows": self.late_growth_windows,
            "saturated": saturated,
            "invalid": lag_breached or errored or saturated,
            "reject_reason": _reject_reason(lag_breached, errored, saturated),
        }

    def flush(self, end_us: float, rate_fallback: float) -> None:
        self.advance_growth()
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


def scoped_command(cmd: list[str], cap_mib: int, peak_prefix: str) -> list[str]:
    """Run ONE engine invocation in its own memory-capped cgroup and report that cgroup's peak.

    The measured memory budget is the STORAGE ENGINE's. `flashdb-load` is the engine's own driver,
    so it runs here, in its own scope, exactly as the published T1-T4 runs put `flashdb-load` under
    the cap (`cold_0b`) while the Python harness that scored the windows stayed outside it. The
    child reports the scope's `memory.current` before the command runs and its `memory.peak` after,
    both read from inside the cgroup, so the peak belongs to this invocation and to nothing else.
    """
    inner = " ".join(shlex.quote(part) for part in cmd)
    cgroup = '$(grep "^0::" /proc/self/cgroup | cut -d: -f3)'
    script = (
        f'cg="/sys/fs/cgroup{cgroup}"; '
        f'echo "{SCOPE_START_PREFIX}$(cat $cg/memory.current 2>/dev/null || echo 0)"; '
        f"{inner}; rc=$?; "
        f'peak=$(cat $cg/memory.peak 2>/dev/null || echo 0); '
        f'echo "{peak_prefix}$peak"; exit $rc'
    )
    return ["systemd-run", "--user", "--scope", "-q",
            "-p", f"MemoryMax={cap_mib}M", "-p", "MemorySwapMax=0", "bash", "-c", script]


def _peak_from_log(lines: list[str], prefix: str) -> int | None:
    for line in reversed(lines):
        if line.startswith(prefix):
            value = line[len(prefix):].strip()
            return int(value) if value.isdigit() else None
    return None


def cgroup_self_path() -> Path | None:
    """This process's own cgroup v2 directory, or `None` outside cgroup v2."""
    try:
        for line in Path("/proc/self/cgroup").read_text().splitlines():
            if line.startswith("0::"):
                return Path("/sys/fs/cgroup" + line[3:])
    except OSError:
        return None
    return None


def cgroup_peak_bytes(cgroup: Path | None = None) -> int | None:
    """A cgroup's `memory.peak` (default: this process's own; None when unreadable)."""
    path = cgroup if cgroup is not None else cgroup_self_path()
    if path is None:
        return None
    try:
        return int((path / "memory.peak").read_text())
    except (OSError, ValueError):
        return None


class MemoryPeakSampler:
    """Sample a cgroup's `memory.current` across the MEASURED phase and report its maximum.

    Why sampling rather than `memory.peak`: the arm's capped scope also holds the SEED (the runner
    caps sqlite's whole arm step, and the postgres server scope holds the server across its bulk
    load), and this kernel ignores a reset of `memory.peak` (the write is accepted and has no
    effect), so a lifetime peak would report CONSTRUCTION memory as serving pressure. The maximum of
    sampled `memory.current` from the start of the measured phase bounds the SERVING peak. It is a
    sampled maximum, hence a lower bound on the kernel's true high-water mark, so the record carries
    the method and the sample count instead of implying an exact peak.
    """

    def __init__(self, cgroup: Path | None, interval_s: float = MEMORY_SAMPLE_INTERVAL_S) -> None:
        self.cgroup = cgroup
        self.interval_s = interval_s
        self.samples: list[int] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def _read(self) -> int | None:
        if self.cgroup is None:
            return None
        try:
            return int((self.cgroup / "memory.current").read_text())
        except (OSError, ValueError):
            return None

    def _poll(self) -> None:
        while not self._stop.wait(self.interval_s):
            value = self._read()
            if value is not None:
                self.samples.append(value)

    def start(self) -> "MemoryPeakSampler":
        value = self._read()
        if value is not None:
            self.samples.append(value)
            self._thread = threading.Thread(target=self._poll, daemon=True)
            self._thread.start()
        return self

    def stop(self) -> dict:
        self._stop.set()
        if self._thread is not None:
            self._thread.join()
        if not self.samples:
            return {"peak_bytes": None, "start_bytes": None, "samples": 0,
                    "method": f"unavailable: {self.cgroup} has no readable memory.current",
                    "scope": "measured phase"}
        return {"peak_bytes": max(self.samples), "start_bytes": self.samples[0],
                "samples": len(self.samples),
                "method": f"sampled memory.current every {int(self.interval_s * 1000)} ms",
                "scope": "measured phase"}


def run_sql_arm(
    arm: SqlArm,
    manifest: dict,
    requests,
    batch: int = DEFAULT_BATCH,
    window_ms: int = DEFAULT_WINDOW_MS,
    engine_cgroup: Path | None = None,
) -> dict:
    """Replay the trace against one SQL arm and return its run record.

    `requests` is an ITERABLE of parsed requests, not a list: the pacer pulls only the batch in
    front of it, so a long trace costs no more memory than its batch. The loop is closed: a batch is
    executed and committed before the next one is dispatched, so a slow commit shows up as schedule
    lag on the next request rather than as hidden queueing.

    `engine_cgroup` is the cgroup the ENGINE ran in: this process's own for sqlite (embedded), the
    server's scope for postgres. Its `memory.current` is sampled across this measured phase only, so
    the reported peak is serving pressure and never the bulk load that preceded it.
    """
    sampler = MemoryPeakSampler(engine_cgroup).start()
    due, rates = build_schedule(manifest)
    total = len(due)
    stream = iter(requests)
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
        pending = [next(stream)]
        while len(pending) < batch and i + len(pending) < total and due[i + len(pending)] <= now:
            pending.append(next(stream))
        j = i + len(pending)
        start = now_us()
        state = arm.execute_batch(pending)
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
    engine_memory = sampler.stop()

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
        "engine_memory": engine_memory,
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
    engine_cap_mib: int = ENGINE_CAP_MIB,
    seed_cap_mib: int = SEED_CAP_MIB,
) -> dict:
    """Seed a fresh instance, run the trace in the engine's own capped scope, parse its windows.

    Two scopes, two budgets, both recorded: the dataset is BUILT under `seed_cap_mib` (construction
    is heavier than serving, slice 51's measured convention) and the measured load runs under
    `engine_cap_mib`. The harness itself is the instrument and stays outside both.
    """
    instance = workdir / f"instance-{name}"
    if instance.exists():
        shutil.rmtree(instance)
    seed_path = workdir / "seed.jsonl"
    trace = workdir / "trace.jsonl"
    seed_t0 = time.perf_counter()
    seeded = subprocess.run(
        scoped_command([str(bins / "flashdb-scale"), "seed-inventory", str(instance),
                        str(seed_path)], seed_cap_mib, SEED_PEAK_PREFIX),
        capture_output=True, text=True, env={**os.environ, **SEED_ENV})
    seed_s = time.perf_counter() - seed_t0
    if seeded.returncode != 0:
        raise RuntimeError(f"seed-inventory failed (rc={seeded.returncode}): {seeded.stderr[-2000:]}")
    seed_peak = _peak_from_log(seeded.stdout.splitlines(), SEED_PEAK_PREFIX)
    cmd = flashdb_load_command(bins, instance, trace, manifest, batch, window_ms)
    driver_log = workdir / f"driver-{name}.log"
    t0 = time.perf_counter()
    # The driver's dump is written straight to its log: the measured process must not hold a whole
    # run's window samples in the instrument's memory.
    with open(driver_log, "w", encoding="utf-8") as log:
        proc = subprocess.run(scoped_command(cmd, engine_cap_mib, ENGINE_PEAK_PREFIX),
                              stdout=log, stderr=subprocess.STDOUT, text=True)
    elapsed = time.perf_counter() - t0
    lines = driver_log.read_text(encoding="utf-8").splitlines()
    engine_peak = _peak_from_log(lines, ENGINE_PEAK_PREFIX)
    if proc.returncode != 0:
        raise RuntimeError(f"flashdb-load failed (rc={proc.returncode}): {driver_log}"
                           f" tail={lines[-5:]}")
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
        "seed_s": seed_s,
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
        "instance_files": _largest_files(instance),
        "engine_cap_mib": engine_cap_mib,
        "engine_memory": {
            "peak_bytes": engine_peak,
            "start_bytes": _peak_from_log(lines, SCOPE_START_PREFIX),
            "samples": None,
            "method": "exact memory.peak of the flashdb-load invocation's own scope, read inside it",
            "scope": "measured phase only: the instance is BUILT in a separate construction scope",
        },
        "construction_memory": {"cap_mib": seed_cap_mib, "peak_bytes": seed_peak},
        "arm_scope_peak_bytes": None,
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


def _largest_files(path: Path, limit: int = 4) -> dict[str, int]:
    """The instance's biggest files by name, so a footprint can be re-derived from the record.

    The total alone cannot be audited: a 12x ratio is only meaningful when a reader can see how much
    of it is dataset and how much is the live WAL, and the tiles themselves are git-ignored.
    """
    files = sorted((p for p in path.rglob("*") if p.is_file()),
                   key=lambda p: p.stat().st_size, reverse=True)
    return {p.name: p.stat().st_size for p in files[:limit]}


def _db_bytes(path: Path) -> int:
    """sqlite's on-disk footprint: the database file plus its WAL and shared-memory sidecars."""
    return sum(_sqlite_files(path).values())


def _sqlite_files(path: Path) -> dict[str, int]:
    """`{"db": n, "wal": n, "shm": n}` for the sidecars that exist (a footprint breakdown)."""
    out = {}
    for name, candidate in (("db", path),
                            ("wal", path.with_suffix(path.suffix + "-wal")),
                            ("shm", path.with_suffix(path.suffix + "-shm"))):
        if candidate.exists():
            out[name] = candidate.stat().st_size
    return out


# ---------------------------------------------------------------------------
# The inventory oracle
# ---------------------------------------------------------------------------
def _check(checks: dict[str, dict], name: str, ok: bool, detail: str) -> None:
    """Count one subject of a named check and fail at the FIRST violation, naming it.

    The record is one entry per check name with the number of subjects it covered, not one entry
    per subject: the oracle's evidence is the arithmetic and the counts, and a per-cell list of
    thousands of identical "ok" rows is noise a reviewer would have to filter.
    """
    entry = checks.setdefault(name, {"check": name, "ok": True, "subjects": 0, "detail": detail})
    entry["subjects"] += 1
    if not ok:
        entry["ok"] = False
        entry["detail"] = detail
        raise OracleViolation(f"{name}: {detail}")


def oracle_sql(arm: SqlArm, manifest: dict, counters: dict, order_flow_requests: int,
               on_hand_initial: int = DEFAULT_ON_HAND) -> dict:
    """Prove the SQL arm's committed state is the state the trace produced.

    Every check reads the arm's own database after the run and compares it with the arithmetic the
    trace implies. It fails at the FIRST violation with the offending cell/order named.
    """
    checks: dict[str, dict] = {}
    n_cells = int(manifest["n_sku"]) * int(manifest["n_loc"])

    cells_seen = 0
    on_hand_sum = 0
    # The scan is STREAMED (a cursor, not `fetchall`): at 10^7 cells a materialised result set is a
    # memory decision about the verification, not about the data.
    # One aggregate over the movements, then a join: the per-cell arithmetic is the same check, but
    # its cost is the dataset rather than the product of the cells and the movements (a correlated
    # subquery per stock row is what made a 10^7-cell verification take tens of minutes).
    arm.cur.execute(
        "SELECT s.sku, s.loc, s.on_hand, COALESCE(g.sum_delta, 0) FROM stock s"
        " LEFT JOIN (SELECT sku, loc, SUM(delta) AS sum_delta FROM stock_movement"
        "            GROUP BY sku, loc) g ON g.sku = s.sku AND g.loc = s.loc")
    for sku, loc, on_hand, delta in arm.cur:
        cells_seen += 1
        on_hand_sum += int(on_hand)
        # `delta` is the signed movement (-qty), so the cell's arithmetic is initial + sum(delta) --
        # the ticket's "on_hand_initial - sum(delta)" with a decrement counted as its magnitude.
        expected = on_hand_initial + int(delta)
        _check(checks, "on_hand_matches_movements", int(on_hand) == expected,
               f"stock({sku},{loc}).on_hand={on_hand}, initial {on_hand_initial} + sum(delta)"
               f" {delta} = {expected}")
        _check(checks, "on_hand_non_negative", int(on_hand) >= 0,
               f"stock({sku},{loc}).on_hand={on_hand}")
    _check(checks, "cell_count", cells_seen == n_cells,
           f"{cells_seen} stock cells, expected {n_cells} ({manifest['n_sku']} sku x"
           f" {manifest['n_loc']} loc)")

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
           f" against {order_flow_requests} order_flow requests")
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
           on_hand_sum == on_hand_initial * n_cells
           - _scalar(arm, 'SELECT COALESCE(SUM(qty), 0) FROM "order"'),
           f"sum(on_hand) == {n_cells} cells x {on_hand_initial} - taken qty")
    return {"status": "PASS", "checks": list(checks.values()),
            "order_no_uniqueness": "queried"}


def _scalar(arm: SqlArm, sql: str) -> int:
    arm.cur.execute(sql)
    return int(arm.cur.fetchone()[0])


def iter_probe_trace(manifest: dict, order_nos, customers):
    """The read-only probe trace the flashdb oracle reads the instance back with.

    The adapter has no Order/Backorder read shape, so the state is read through the shapes it does
    have: every (sku, loc) cell via `stock_lookup`, every seeded email via `customer_lookup` (which
    proves no order_flow took the skipped path), and every order number in the trace via
    `movements_scan` (which yields that order's movement rows). The probe target is encoded in the
    request id so a replay line can be attributed without a second mapping.

    The probes are STREAMED, and ordered movements -> customers -> cells so the oracle can fold each
    movement's delta into its cell and then check that cell as it arrives, which is what keeps a
    10^7-cell verification's memory proportional to the trace rather than to the dataset. The
    cells still cover every (sku, loc), so the arithmetic is unchanged.
    """
    for order_no in order_nos:
        yield {
            "id": f"probe-movements:{order_no}",
            "class": {"durability": "batched", "max_staleness": 5},
            "blocks": {"q": {"find": {"StockMovement": {"where": {"order_no": order_no}},
                                      "order": ["movement_no"], "limit": 100}}},
        }
    for email in customers:
        yield {
            "id": f"probe-customer:{email}",
            "class": {"durability": "batched"},
            "blocks": {"q": {"find": {"Customer": {"email": email}}}},
        }
    for rank in range(1, int(manifest["n_sku"]) + 1):
        sku = sku_name(rank)
        for j in range(int(manifest["n_loc"])):
            loc = loc_name(j)
            yield {
                "id": f"probe-stock:{sku}:{loc}",
                "class": {"durability": "batched", "max_staleness": 2},
                "blocks": {"q": {"get": {"Stock": {"where": {"sku": sku, "loc": loc}}}}},
            }


def write_probe_trace(path: Path, manifest: dict, order_nos, customers) -> int:
    """Write the streamed probe trace; returns the probe count."""
    count = 0
    with open(path, "w", encoding="utf-8") as fh:
        for probe in iter_probe_trace(manifest, order_nos, customers):
            fh.write(json.dumps(probe, sort_keys=True) + "\n")
            count += 1
    return count


def oracle_flashdb(replayed, manifest: dict, counters: dict,
                   on_hand_initial: int = DEFAULT_ON_HAND) -> dict:
    """The same inventory arithmetic as `oracle_sql`, read back through the request interface.

    `replayed` is one `{line, id, blocks}` object per probe (the `flashdb-replay` output dialect).
    The limit is disclosed in the returned record: `Order`/`Backorder` have no read shape, so the
    order-number uniqueness check cannot be re-queried here; flashdb refuses a reused order number
    at write time (unique on both entities plus the adapter's cross-entity check) and any refusal
    is an error in the window record, which `oracle_flashdb` requires to be zero.
    """
    checks: dict[str, dict] = {}
    deltas: dict[tuple[str, str], int] = {}
    cells_seen = 0
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
            cells_seen += 1
            value = int(row["on_hand"])
            expected = on_hand_initial + deltas.pop((sku, loc), 0)
            _check(checks, "on_hand_matches_movements", value == expected,
                   f"stock({sku},{loc}).on_hand={value}, initial {on_hand_initial} + sum(delta)"
                   f" {expected - on_hand_initial} = {expected}")
            _check(checks, "on_hand_non_negative", value >= 0, f"stock({sku},{loc}).on_hand={value}")
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
    _check(checks, "cell_count", cells_seen == n_cells,
           f"{cells_seen} cells read back, expected {n_cells}")
    _check(checks, "no_skipped_take", customers_seen == int(manifest["n_customers"]),
           f"{customers_seen} customers read back, expected {manifest['n_customers']}")
    # Every movement's cell is probed, so an empty fold means every delta was accounted for.
    _check(checks, "every_movement_cell_probed", not deltas,
           f"{len(deltas)} movement cells were never read back: {list(deltas)[:5]}")
    _check(checks, "movement_count_is_committed_takes", movements == counters["take"],
           f"{movements} movements read back, {counters['take']} committed take blocks")
    _check(checks, "order_count_is_committed_takes", orders_with_movements == counters["take"],
           f"{orders_with_movements} orders carry a movement, {counters['take']} takes committed")
    _check(checks, "no_request_errors", counters["errors"] == 0,
           f"{counters['errors']} request errors (first: {counters.get('first_error')})")
    return {"status": "PASS", "checks": list(checks.values()),
            "order_no_uniqueness": "enforced at write time (no Order read shape); zero request"
                                   " errors required"}


def run_flashdb_probes(name: str, manifest: dict, workdir: Path, bins: Path, trace_orders):
    """Replay the probe trace read-only, streaming `flashdb-replay`'s one-JSON-line-per-probe output.

    Both sides are streamed: the probe file is written as it is generated and the replay's stdout is
    consumed line by line, so a 10^7-cell verification costs the size of one record rather than the
    size of the whole dump in the instrument's memory.
    """
    instance = workdir / f"instance-{name}"
    customers, _stock = seed_docs(manifest)
    probe_path = workdir / f"probe-{name}.jsonl"
    write_probe_trace(probe_path, manifest, trace_orders, (c[0] for c in customers))
    log_path = workdir / f"probe-{name}.log"
    with open(log_path, "w", encoding="utf-8") as log:
        proc = subprocess.Popen([str(bins / "flashdb-replay"), str(instance), str(probe_path)],
                                stdout=subprocess.PIPE, stderr=log, text=True, bufsize=1 << 20)
        assert proc.stdout is not None
        for line in proc.stdout:
            if line.startswith("{"):
                yield json.loads(line)
        rc = proc.wait()
    if rc != 0:
        raise RuntimeError(f"flashdb-replay failed (rc={rc}): {log_path}")


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
        "Generated by `python -m flashdb_harness.baseline_0b report` from `run-<arm>.json`; the"
        " per-window samples are in `windows-<arm>.jsonl`.",
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
    lines += ["", "## Durability, oracle and memory per arm", "",
              "| arm | durability | oracle | cap MiB | serving peak MiB | peak scope |"
              " arm-scope peak MiB | mean group size |",
              "|---|---|---|---|---|---|---|---|"]
    for record in records:
        memory = record.get("engine_memory") or {}
        peak = memory.get("peak_bytes")
        peak_col = f"{peak / (1 << 20):.1f}" if peak is not None else "not measured"
        scope_peak = record.get("arm_scope_peak_bytes")
        scope_col = f"{scope_peak / (1 << 20):.1f}" if scope_peak is not None else "-"
        lines.append(
            f"| {record['arm']} | {record['durability']} | {record['oracle']['status']} |"
            f" {record.get('engine_cap_mib')} | {peak_col} | {memory.get('scope', '-')} |"
            f" {scope_col} | {record['mean_group_size']:.2f} |")
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
        # The exact invocation that produced this work directory's trace/spec; the runner adds its
        # own `runner` block (argv, caps, lock) after `prepare`, so a committed run names the
        # command that made it instead of a plausible alternative.
        "argv": [Path(sys.argv[0]).name, *sys.argv[1:]],
    })
    blocks = write_seed_trace(workdir / "seed.jsonl", manifest, args.on_hand)
    print(f"trace {workdir / 'trace.jsonl'}: requests={manifest['total_requests']}"
          f" order_flow={order_flows} seed_blocks={blocks}"
          f" sha256={manifest['trace_sha256']}")
    return 0


def _load_workdir(workdir: Path) -> tuple[dict, dict, Path]:
    """`(manifest, spec, trace_path)`; the trace is hashed against the manifest and streamed later."""
    manifest = _read_json(workdir / "manifest.json")
    spec = _read_json(workdir / "spec.json")
    trace = workdir / "trace.jsonl"
    trace_sha = file_sha256(trace)
    if trace_sha != manifest["trace_sha256"]:
        raise SystemExit(f"trace {trace} hashes to {trace_sha} but the manifest"
                         f" says {manifest['trace_sha256']}; rerun prepare")
    return manifest, spec, trace


def cmd_arm(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    manifest, spec, trace = _load_workdir(workdir)
    name = args.arm
    if name in FLASHDB_ARMS:
        record = run_flashdb_arm(name, manifest, workdir, Path(args.bins),
                                 spec["order_flow_requests"], spec["batch"], spec["window_ms"],
                                 args.engine_cap_mib, args.seed_cap_mib)
        if args.skip_verify:
            # The read-back is verification, not workload: it is the `verify` step's job and it runs
            # outside the engine's memory budget (see `run_flashdb_probes`).
            record["oracle"] = {"status": "DEFERRED", "checks": [],
                                "order_no_uniqueness": "deferred to the verify step"}
        else:
            record["oracle"] = _verify_flashdb(name, manifest, workdir, Path(args.bins), trace,
                                               spec, record)
    elif name == "sqlite":
        db = workdir / f"{name}.db"
        for stale in (db, db.with_suffix(".db-wal"), db.with_suffix(".db-shm")):
            stale.unlink(missing_ok=True)
        arm = sqlite_arm(db, args.sqlite_cache_kib)
        customers, stock = seed_docs(manifest, spec["on_hand"])
        seed_t0 = time.perf_counter()
        arm.seed(customers, stock)
        seed_s = time.perf_counter() - seed_t0
        # Read the durability back BEFORE the measured phase: the bulk load ran under
        # journal_mode=OFF / synchronous=OFF and a PRAGMA that cannot be applied fails silently.
        observed_durability = arm.require_measured_durability()
        record = run_sql_arm(arm, manifest, iter_requests(trace), spec["batch"],
                             spec["window_ms"], engine_cgroup=cgroup_self_path())
        record["seed_s"] = seed_s
        record["sqlite_cache_kib"] = args.sqlite_cache_kib
        record["durability_observed"] = observed_durability
        record["engine_cap_mib"] = args.engine_cap_mib
        record["arm_scope_peak_bytes"] = cgroup_peak_bytes()
        record["arm_scope_peak_scope"] = ("this process's capped scope over the whole arm step"
                                          " (bulk load + measured phase)")
        record["sqlite_files"] = _sqlite_files(db)
        record["data_bytes"] = _db_bytes(db)
        add_verify_index(arm)
        record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                      spec["order_flow_requests"], spec["on_hand"])
    elif name == "postgres":
        record = run_postgres_arm(name, manifest, spec, iter_requests(trace),
                                  args.engine_cap_mib)
    else:
        raise SystemExit(f"unknown arm {name!r}; expected one of {FLASHDB_ARMS + SQL_ARMS}")
    record["trace_sha256"] = manifest["trace_sha256"]
    record["verdict"] = arm_verdict(record["curve"])
    # The windows are written once, as their own JSONL: the run record cites that file rather than
    # carrying a second copy of the same samples.
    (workdir / f"windows-{name}.jsonl").write_text(
        "\n".join(record["windows_jsonl"]) + "\n", encoding="utf-8")
    record = {k: v for k, v in record.items() if k not in ("windows_jsonl", "windows")}
    record["windows_file"] = f"windows-{name}.jsonl"
    _write_json(workdir / f"run-{name}.json", record)
    print(f"{name}: oracle {record['oracle']['status']};"
          f" sustained={record['verdict']['sustained']}"
          f" highest={record['verdict']['highest_sustained_rps']:.0f} rps"
          f" rejected_windows={record['verdict']['rejected_windows']}"
          f" serving_peak={record['engine_memory']['peak_bytes']}"
          f" cap_mib={record['engine_cap_mib']}"
          f" sha256={record['trace_sha256']}")
    return 0


def _verify_flashdb(name: str, manifest: dict, workdir: Path, bins: Path, trace: Path,
                    spec: dict, record: dict) -> dict:
    """The post-hoc read-back of the measured instance: every cell, customer and order number."""
    trace_orders = sorted(set(iter_order_numbers(trace)))
    probe_t0 = time.perf_counter()
    replayed = run_flashdb_probes(name, manifest, workdir, bins, trace_orders)
    record["oracle_s"] = time.perf_counter() - probe_t0
    return oracle_flashdb(replayed, manifest, {
        "take": record["counters"]["take"],
        "errors": record["counters"]["errors"],
        "first_error": record["counters"]["first_error"],
    }, spec["on_hand"])


def cmd_verify(args: argparse.Namespace) -> int:
    """Run the deferred inventory read-back for a flashdb arm and merge it into its run record.

    Separate from `arm` on purpose: the read-back is verification (`iter_probe_trace` +
    `flashdb-replay` + `oracle_flashdb`), it touches the instance only through read requests, and
    its own memory therefore must not be charged to the engine's measured budget.
    """
    workdir = Path(args.workdir).resolve()
    manifest, spec, trace = _load_workdir(workdir)
    path = workdir / f"run-{args.arm}.json"
    record = _read_json(path)
    if record["trace_sha256"] != manifest["trace_sha256"]:
        raise SystemExit(f"{path} measured a different trace than {workdir / 'trace.jsonl'}")
    record["oracle"] = _verify_flashdb(args.arm, manifest, workdir, Path(args.bins), trace, spec,
                                       record)
    _write_json(path, record)
    print(f"{args.arm}: oracle {record['oracle']['status']} (verify_s={record['oracle_s']:.1f})")
    return 0


def run_postgres_arm(name: str, manifest: dict, spec: dict, requests,
                     engine_cap_mib: int = ENGINE_CAP_MIB) -> dict:
    """The postgres arm against the user-space cluster `scripts/baseline-env.sh` provides.

    The engine here is the SERVER, in its own capped scope; this client is outside it. The runner
    passes that scope's cgroup as `M1_ENGINE_CGROUP`, so the client can sample the SERVER's
    `memory.current` across the measured phase instead of charging the instrument's memory to the
    engine.
    """
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
        seed_t0 = time.perf_counter()
        arm.seed(customers, stock)
        seed_s = time.perf_counter() - seed_t0
        engine_cgroup = Path(os.environ[ENGINE_CGROUP_ENV]) if os.environ.get(
            ENGINE_CGROUP_ENV) else None
        record = run_sql_arm(arm, manifest, requests, spec["batch"], spec["window_ms"],
                             engine_cgroup=engine_cgroup)
        record["seed_s"] = seed_s
        record["durability"] = PostgresArm.durability
        record["engine_cap_mib"] = engine_cap_mib
        # The server scope's LIFETIME peak (bulk load + measured phase) is patched by the runner
        # after this client step; it is an arm-scope number, never the serving peak.
        record["arm_scope_peak_bytes"] = None
        record["arm_scope_peak_scope"] = ("the postgres server scope's lifetime (bulk load +"
                                          " measured phase); the client runs outside it")
        add_verify_index(arm)
        record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                      spec["order_flow_requests"], spec["on_hand"])
        record["postgres_settings"] = {
            "synchronous_commit": _setting(conn, "synchronous_commit"),
            "fsync": _setting(conn, "fsync"),
            "shared_buffers": _setting(conn, "shared_buffers"),
            "server_version": _setting(conn, "server_version"),
        }
        # The arm's data:memory ratio is reported from the server's own catalog, not from a guess.
        record["data_bytes"] = _scalar(arm, "SELECT pg_database_size(current_database())")
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
    arm.add_argument("--engine-cap-mib", type=int, default=ENGINE_CAP_MIB,
                     help="memory budget of the engine's own cgroup for the measured load")
    arm.add_argument("--seed-cap-mib", type=int, default=SEED_CAP_MIB,
                     help="transient budget for `seed-inventory` (construction, not serving)")
    arm.add_argument("--sqlite-cache-kib", type=int, default=SQLITE_CACHE_KIB,
                     help="sqlite `PRAGMA cache_size` in KiB (its engine is embedded)")
    arm.add_argument("--skip-verify", action="store_true",
                     help="defer the flashdb read-back to the `verify` step (outside the engine cap)")
    arm.set_defaults(func=cmd_arm)

    verify = sub.add_parser("verify", help="run a flashdb arm's deferred inventory read-back")
    verify.add_argument("--workdir", required=True)
    verify.add_argument("--arm", required=True)
    verify.add_argument("--bins", default=str(REPO_ROOT / "target" / "release"))
    verify.set_defaults(func=cmd_verify)

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
