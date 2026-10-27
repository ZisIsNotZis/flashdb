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
  bounded by its offered rate. The SQL arms dispatch on that schedule and let a worker execute and
  commit the group (`run_paced_loop(open_loop=True)`), exactly as `flashdb-load`'s dispatcher does
  not wait for its barrier; only the device floor keeps the closed-loop shape, where the next
  group waits for the previous commit.

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
import datetime
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
from flashdb_harness.cold_0b import (highest_primary_sustained_rate, summarize_curve,
                                     window_records)
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
# sqlite's WAL auto-checkpoint, in pages, STATED and recorded per run instead of left to sqlite's
# compiled default (1000 pages ~ 4 MiB). 0 defers the checkpoint out of the measured phase: the
# automatic checkpoint runs on the committing thread, and against the 10x instance (542 MB) the
# write-back blocks that thread for ~254 ms, which is what the f70d91c arm-runs show as a lag p99 of
# 254 ms at every crossing with a 3 ms IQR. The deferred checkpoint is run once, after the measured
# phase, by `sqlite_wal_record` (`PRAGMA wal_checkpoint(TRUNCATE)`, whose result is recorded beside
# the `PASSIVE` row that names the frames it moved), and the run record states the policy, the WAL
# bytes it deferred and both checkpoint rows. A probe with 1000 pages against 0 is
# committed beside ticket 82's report (`runs/probe-checkpoint/`): the 254 ms stall follows the
# setting, not the rung.
SQLITE_WAL_AUTOCHECKPOINT_PAGES = 0
# The line `scoped_command` prepends and appends: the engine cgroup's own `memory.current` at the
# start of the invocation and its `memory.peak`, both read from inside it.
SCOPE_START_PREFIX = "scope_memory_start_bytes="
ENGINE_PEAK_PREFIX = "engine_memory_peak_bytes="
SEED_PEAK_PREFIX = "seed_memory_peak_bytes="
# The same scope's `cpu.stat` `usage_usec` at the start and end of the invocation: the measured
# phase's CPU time for an engine that is a separate process (flashdb's `flashdb-load`).
CPU_START_PREFIX = "scope_cpu_start_usec="
CPU_END_PREFIX = "scope_cpu_end_usec="
# How often the measured-phase sampler reads `memory.current` (see `MemoryPeakSampler`).
MEMORY_SAMPLE_INTERVAL_S = 0.05
# The cgroup a runner-invoked arm's engine lives in when the engine is NOT in the arm's own cgroup
# (postgres: the server runs in its own scope and the client is outside it).
ENGINE_CGROUP_ENV = "M1_ENGINE_CGROUP"
# The seeded `Stock.on_hand` every cell starts at (uniform: `on_hand_jitter=0`), which is what makes
# the oracle's `on_hand == initial - sum(delta)` check checkable without a second table.
DEFAULT_ON_HAND = 1_000_000

SPEC_SCHEMA = "flashdb.baseline.run-spec.v1"
# The pristine pre-load instance (ticket 82's controlled protocol): the dataset is built ONCE per
# arm, copied aside, and copied back before every measured arm-run, so an arm-run measures the
# SERVING path only. Refusing to re-seed inside the loop is what makes a 20 s run cost 20 s.
SNAPSHOT_SCHEMA = "flashdb.baseline.instance-snapshot.v1"
# The device floor's record: how much schedule lag and durable-commit latency this machine shows
# with NO engine in the path, measured with the arms' own pacer and window rule.
FLOOR_SCHEMA = "flashdb.baseline.device-floor.v1"
# The one database the postgres arm seeds and measures when the instance is restored from a
# snapshot: a fixed name is what lets the pre-load state be a filesystem copy of the whole cluster
# instead of a per-run `CREATE DATABASE` plus an 11-minute bulk load.
POSTGRES_DATABASE = "m1-controlled"
# What must NOT be running while an arm is measured (ticket 82: "no subagents, builds, or seed
# jobs during the measured phase"). The desktop applications are allowed; the measured run is
# paired within one machine state, so a steady desktop load is a controlled condition.
CONCURRENT_WORK_PATTERNS = ("cargo", "rustc", "flashdb-scale", "seed-inventory", "baseline_0b")

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


def trace_write_ingest(trace: Path) -> dict:
    """The write requests in a frozen trace and the payload bytes they carry.

    A write request is one carrying an `ops` block (the contract's atomic multi-write block); its
    payload is the trace line itself, i.e. the bytes the open-loop client sends for it. Every arm
    replays the SAME trace, so this is the engine-independent denominator of the write-amplification
    KPI rather than one engine's own encoding of the write.
    """
    write_requests = 0
    payload_bytes = 0
    with open(trace, encoding="utf-8") as fh:
        for line in fh:
            if not line.strip():
                continue
            blocks = json.loads(line).get("blocks") or {}
            if any(isinstance(block, dict) and "ops" in block for block in blocks.values()):
                write_requests += 1
                payload_bytes += len(line.rstrip("\n"))
    return {"write_requests": write_requests, "payload_bytes": payload_bytes, "trace": str(trace)}


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


def tree_sha256(root: Path) -> tuple[str, int, int]:
    """`(sha256, bytes, files)` over a directory tree: every relative path, size and byte.

    A restored instance is compared against its snapshot by this digest. Copying 1.7 GB back only
    proves a copy ran; the digest is what shows the bytes are the SEEDED ones, which is ticket 82's
    stop condition ("a snapshot restore cannot reproduce the seeded state").
    """
    digest = hashlib.sha256()
    total = 0
    files = 0
    for path in sorted(p for p in root.rglob("*") if p.is_file()):
        size = path.stat().st_size
        digest.update(f"{path.relative_to(root).as_posix()}\0{size}\0".encode())
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                digest.update(chunk)
        total += size
        files += 1
    return digest.hexdigest(), total, files


# ---------------------------------------------------------------------------
# The pre-load instance: build once, snapshot, restore before every arm-run
# ---------------------------------------------------------------------------
def postgres_data_dir() -> Path:
    """The cluster's data directory: the postgres arm's INSTANCE (the engine's whole state)."""
    return Path(os.environ.get("M1_PG_ROOT", "/var/tmp/m1-pg")) / "data"


def instance_path(name: str, workdir: Path, instance_root: Path | None = None) -> Path:
    """The live (mutable) pre-load instance of one arm.

    `instance_root` lets the controlled protocol keep ONE live instance per arm across many work
    directories: a snapshot is restored into it before every run, so the rung a run belongs to
    decides the work directory its records land in, never which bytes the engine read.
    """
    root = Path(instance_root) if instance_root else workdir
    if name in FLASHDB_ARMS:
        return root / f"instance-{name}"
    if name == "sqlite":
        return root / "sqlite.db"
    if name == "postgres":
        return postgres_data_dir()
    raise SystemExit(f"unknown arm {name!r}; expected one of {FLASHDB_ARMS + SQL_ARMS}")


def _remove(path: Path) -> None:
    if path.is_dir():
        shutil.rmtree(path)
    elif path.exists():
        path.unlink()


def snapshot_instance(name: str, workdir: Path, snapshot_dir: Path,
                      instance_root: Path | None = None) -> dict:
    """Copy the live pre-load instance aside and record the digest that identifies it."""
    live = instance_path(name, workdir, instance_root)
    if not live.exists():
        raise SystemExit(f"no live instance at {live}; run the instance build step first")
    snapshot_dir.mkdir(parents=True, exist_ok=True)
    target = snapshot_dir / (f"{name}.db" if live.is_file() else name)
    _remove(target)
    started = time.perf_counter()
    if live.is_dir():
        shutil.copytree(live, target, symlinks=True)
        digest, total, files = tree_sha256(target)
    else:
        shutil.copy2(live, target)
        digest, total, files = file_sha256(target), target.stat().st_size, 1
    record = {
        "schema": SNAPSHOT_SCHEMA,
        "arm": name,
        "live_path": str(live),
        "snapshot_path": str(target),
        "sha256": digest,
        "bytes": total,
        "files": files,
        "snapshot_s": round(time.perf_counter() - started, 3),
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
        "source": "the pre-load state built by `build-instance` (seeded; no request served)",
    }
    _write_json(snapshot_dir / f"{name}.json", record)
    print(f"snapshot {name}: {total} bytes, {files} files, sha256={digest}")
    return record


def restore_instance(name: str, workdir: Path, snapshot_dir: Path,
                     instance_root: Path | None = None) -> dict:
    """Put the snapshot back as the live instance and FAIL LOUDLY unless its digest matches."""
    meta_path = snapshot_dir / f"{name}.json"
    if not meta_path.exists():
        raise SystemExit(f"no snapshot for {name} at {meta_path}; run the snapshot step first")
    meta = _read_json(meta_path)
    live = instance_path(name, workdir, instance_root)
    source = Path(meta["snapshot_path"])
    if not source.exists():
        raise SystemExit(f"snapshot file {source} recorded by {meta_path} is missing")
    started = time.perf_counter()
    _remove(live)
    for sidecar in (Path(f"{live}-wal"), Path(f"{live}-shm")):
        sidecar.unlink(missing_ok=True)
    if source.is_dir():
        shutil.copytree(source, live, symlinks=True)
        digest, total, files = tree_sha256(live)
    else:
        shutil.copy2(source, live)
        digest, total, files = file_sha256(live), live.stat().st_size, 1
    if digest != meta["sha256"]:
        raise SystemExit(
            f"restore of {name} does NOT reproduce the seeded state: {live} digests to {digest}"
            f" but the snapshot is {meta['sha256']}; stopping instead of measuring something else")
    state = {
        "arm": name,
        "from": str(source),
        "to": str(live),
        "sha256": digest,
        "snapshot_sha256": meta["sha256"],
        "verified": True,
        "bytes": total,
        "files": files,
        "restore_s": round(time.perf_counter() - started, 3),
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
    }
    # The arm step quotes this record instead of re-describing the restore: the bytes it measured
    # are the bytes this digest covers.
    _write_json((Path(instance_root) if instance_root else workdir) / f"restore-{name}.json", state)
    return state


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

    def __init__(self, conn: sqlite3.Connection,
                 wal_autocheckpoint: int = SQLITE_WAL_AUTOCHECKPOINT_PAGES) -> None:
        # Autocommit at the Python level: the transaction is explicit, so a batch's boundaries are
        # the pacer's and not sqlite3's implicit ones.
        conn.isolation_level = None
        super().__init__(conn)
        self.wal_autocheckpoint = int(wal_autocheckpoint)
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
        for pragma in ("journal_mode=WAL", "synchronous=FULL",
                       f"wal_autocheckpoint={self.wal_autocheckpoint}"):
            self.cur.execute(f"PRAGMA {pragma}")

    def observed_durability(self) -> dict:
        """The connection's durability and WAL policy, read BACK from sqlite rather than assumed.

        `PRAGMA journal_mode=WAL` that cannot be applied returns the old mode and raises nothing,
        and the seeding path deliberately runs with `journal_mode=OFF, synchronous=OFF`, so the
        measured path depends on this restore actually having taken effect. The WAL
        auto-checkpoint is read back for the same reason: it decides whether the measured phase
        pays sqlite's checkpoint stall, so leaving it an unstated default would hide a parameter
        from the record. `journal_size_limit` is sqlite's own cap (or -1) and is recorded so the
        WAL cannot grow past a bound no reader was told about.
        """
        self.cur.execute("PRAGMA journal_mode")
        mode = str(self.cur.fetchone()[0]).lower()
        self.cur.execute("PRAGMA synchronous")
        sync = int(self.cur.fetchone()[0])
        self.cur.execute("PRAGMA wal_autocheckpoint")
        autocheckpoint = int(self.cur.fetchone()[0])
        self.cur.execute("PRAGMA journal_size_limit")
        journal_size_limit = int(self.cur.fetchone()[0])
        return {"journal_mode": mode, "synchronous": sync,
                "synchronous_name": "FULL" if sync == SQLITE_SYNCHRONOUS_FULL else str(sync),
                "wal_autocheckpoint_pages": autocheckpoint,
                "journal_size_limit": journal_size_limit}

    def require_measured_durability(self) -> dict:
        """Fail loudly unless the measured path ran under WAL + `synchronous=FULL` + the stated
        auto-checkpoint, so a parameter the report cites is the one the engine held."""
        observed = self.observed_durability()
        if (observed["journal_mode"], observed["synchronous"]) != ("wal",
                                                                  SQLITE_SYNCHRONOUS_FULL):
            raise SqlArmError(
                f"sqlite measured path is not WAL + synchronous=FULL: observed"
                f" journal_mode={observed['journal_mode']}"
                f" synchronous={observed['synchronous']}")
        if observed["wal_autocheckpoint_pages"] != self.wal_autocheckpoint:
            raise SqlArmError(
                f"sqlite measured path is not on the stated wal_autocheckpoint="
                f"{self.wal_autocheckpoint}: observed"
                f" {observed['wal_autocheckpoint_pages']}")
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


def sqlite_arm(path: str | Path = ":memory:", cache_kib: int = SQLITE_CACHE_KIB,
               create_schema: bool = True,
               wal_autocheckpoint: int = SQLITE_WAL_AUTOCHECKPOINT_PAGES) -> SqliteArm:
    """A sqlite arm on `path` (`:memory:` by default), page cache stated, schema created.

    `create_schema=False` opens an instance that ALREADY has the schema: a restored snapshot. The
    schema belongs to the pre-load state, so re-creating it there would be a setup step inside the
    measurement loop (and would fail loudly, which is how this was found).

    The connection accepts the arm's commit worker on another thread (`check_same_thread=False`):
    the open-loop pacer executes and commits one group at a time from ONE worker thread, and the
    read-back, the oracle and the checkpoint run on this thread after that worker has joined.
    """
    arm = SqliteArm(sqlite3.connect(str(path), check_same_thread=False), wal_autocheckpoint)
    arm.set_engine_memory(cache_kib)
    if create_schema:
        arm.create_schema()
    return arm


def sqlite_wal_record(arm: SqliteArm, db: Path) -> dict:
    """Record the WAL policy and, when the automatic checkpoint is deferred, run it here.

    Called after the measured phase and before the oracle: with `wal_autocheckpoint=0` the WAL keeps
    every frame the run wrote, so the state the run measured is exactly the state the checkpoint
    then writes back. The checkpoint is the maintenance sqlite would otherwise have paid on its
    committing thread, so it is recorded (WAL bytes, result, files) rather than done silently.

    The `TRUNCATE` row alone cannot evidence the move: on this build a successful truncating
    checkpoint reports `(0, 0, 0)` because it reads the state AFTER the truncation, and an
    already-empty WAL reports the same tuple. The deferred frames are therefore named first by a
    `PASSIVE` checkpoint, whose `(busy, frames_in_wal, frames_moved)` is recorded beside the
    `TRUNCATE` row, and the run stops unless that row shows every frame moved and the `TRUNCATE`
    left no WAL behind.
    """
    observed = arm.observed_durability()
    before = _sqlite_files(db)
    passive = None
    checkpoint = None
    if observed["wal_autocheckpoint_pages"] == 0:
        arm.cur.execute("PRAGMA wal_checkpoint(PASSIVE)")
        row = arm.cur.fetchone() or ()
        passive = {"pragma": "wal_checkpoint(PASSIVE)",
                   "result": [int(value) for value in row] if row else None,
                   "at": "after the measured phase, before the TRUNCATE checkpoint"}
        arm.cur.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        row = arm.cur.fetchone() or ()
        checkpoint = {"pragma": "wal_checkpoint(TRUNCATE)",
                      "result": [int(value) for value in row] if row else None,
                      "at": "after the measured phase, before the oracle"}
    after = _sqlite_files(db)
    if checkpoint is not None:
        _require_deferred_write_back(before.get("wal", 0), passive, after)
    return {"autocheckpoint_pages": observed["wal_autocheckpoint_pages"],
            "journal_size_limit": observed["journal_size_limit"],
            "wal_bytes_after_run": before.get("wal", 0),
            "passive_checkpoint": passive,
            "checkpoint": checkpoint,
            "files_after_checkpoint": after,
            "data_bytes_after_checkpoint": _db_bytes(db)}


def _require_deferred_write_back(wal_bytes_after_run: int, passive: dict,
                                files_after: dict) -> None:
    """Stop unless the deferred frames provably moved from the WAL into the database.

    `wal_bytes_after_run` says the WAL still held those bytes at the end of the measured phase. The
    `PASSIVE` row must then name those frames and move all of them into the database with `busy = 0`
    (this process is the only connection, so no reader can block a checkpoint), and the `TRUNCATE`
    must leave no WAL file. Anything else is a state the record cannot describe, and a record that
    cannot describe the write-back must not be written.
    """
    busy, frames_in_wal, frames_moved = (list(passive["result"] or ()) + [0, 0, 0])[:3]
    if wal_bytes_after_run > 0 and (busy or frames_in_wal == 0 or frames_moved != frames_in_wal):
        raise SqlArmError(
            f"the deferred checkpoint did not move every WAL frame: wal_bytes_after_run="
            f"{wal_bytes_after_run} but wal_checkpoint(PASSIVE) reported (busy, frames_in_wal,"
            f" frames_moved) = ({busy}, {frames_in_wal}, {frames_moved})")
    if files_after.get("wal", 0):
        raise SqlArmError(
            f"wal_checkpoint(TRUNCATE) left a WAL of {files_after['wal']} bytes; the record cannot"
            f" claim the deferred write-back happened")


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
    # `cpu.stat`'s `usage_usec` is read around the command for the CPU-per-request KPI. `grep | cut`
    # keeps the shell one line: an `awk` program's braces would collide with the f-string's fields.
    usage_usec = "grep '^usage_usec' $cg/cpu.stat 2>/dev/null | cut -d' ' -f2"
    script = (
        f'cg="/sys/fs/cgroup{cgroup}"; '
        f'echo "{SCOPE_START_PREFIX}$(cat $cg/memory.current 2>/dev/null || echo 0)"; '
        f'echo "{CPU_START_PREFIX}$({usage_usec})"; '
        f"{inner}; rc=$?; "
        f'peak=$(cat $cg/memory.peak 2>/dev/null || echo 0); '
        f'echo "{peak_prefix}$peak"; '
        f'echo "{CPU_END_PREFIX}$({usage_usec})"; '
        f"exit $rc"
    )
    return ["systemd-run", "--user", "--scope", "-q",
            "-p", f"MemoryMax={cap_mib}M", "-p", "MemorySwapMax=0", "bash", "-c", script]


def _peak_from_log(lines: list[str], prefix: str) -> int | None:
    for line in reversed(lines):
        if line.startswith(prefix):
            value = line[len(prefix):].strip()
            return int(value) if value.isdigit() else None
    return None


def _scope_cpu_usec(lines: list[str]) -> dict:
    """The `scoped_command` cgroup's `usage_usec` readings around the invocation."""
    return {"start_usec": _peak_from_log(lines, CPU_START_PREFIX),
            "end_usec": _peak_from_log(lines, CPU_END_PREFIX)}


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


def cgroup_cpu_usec(cgroup: Path | None = None) -> int | None:
    """A cgroup's cumulative `cpu.stat` `usage_usec` (None when the file is unreadable)."""
    path = cgroup if cgroup is not None else cgroup_self_path()
    if path is None:
        return None
    try:
        text = (path / "cpu.stat").read_text()
    except OSError:
        return None
    for line in text.splitlines():
        key, _, value = line.partition(" ")
        if key == "usage_usec" and value.strip().isdigit():
            return int(value.strip())
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


def run_paced_loop(
    due: list[float],
    rates: list[float],
    requests,
    batch: int,
    window_ms: int,
    execute_batch,
    open_loop: bool = False,
) -> dict:
    """The ONE pacer: replay `due`/`rates` through `execute_batch`, one window sample per second.

    `due[i]` is request `i`'s ready instant (microseconds from the start), `requests` yields the
    payloads in the same order, and `execute_batch(list_of_payloads)` returns the per-batch counters
    (`take`/`backorder`/`miss`/`errors`/`first_error`).

    `open_loop` selects which side of the commit the DISPATCH waits on, and nothing else:

    * `False` (closed loop, the device floor's shape): a group is executed and committed before the
      next one is dispatched, so a slow commit shows up as schedule lag on the next request.
    * `True` (open loop, `flashdb-load`'s shape and the SQL arms'): the pacer dispatches a request
      at its due instant into a ready queue and a worker executes and commits the group, acking on
      completion. The durable commit no longer holds the next dispatch back, so the schedule-lag
      rule scores the pacer's own wakeup for both engines and an engine's stall is scored where
      `flashdb-load` puts it, in the per-request service latency. A group is formed from the
      requests already ready when the worker picks them up, capped at `batch` and never held open
      to fill; one durable commit per group and the same `batch` cap in both shapes.

    `run_sql_arm` and the device floor both call this, so the machine's floor and the arms are
    measured by ONE pacer instead of two implementations that could drift apart.
    """
    if open_loop:
        counters, windows, groups, max_group, end_us = _open_loop_replay(
            due, rates, requests, batch, window_ms, execute_batch)
    else:
        counters, windows, groups, max_group, end_us = _closed_loop_replay(
            due, rates, requests, batch, window_ms, execute_batch)
    lines = [json.dumps(r, sort_keys=True) for r in windows.records]
    parsed = window_records(lines, window_ms)
    return {
        "requests_run": len(due),
        "groups": groups,
        "mean_group_size": len(due) / groups if groups else 0.0,
        "max_group_size": max_group,
        "elapsed_s": (end_us - due[0]) / 1e6,
        "counters": counters,
        "windows": parsed,
        "curve": summarize_curve(parsed, None),
        "windows_jsonl": lines,
    }


def _closed_loop_replay(due, rates, requests, batch, window_ms, execute_batch):
    """Dispatch the next group only after the previous one's commit returned (the floor's shape)."""
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
        state = execute_batch(pending)
        done = now_us()
        groups += 1
        max_group = max(max_group, j - i)
        for key in (TAKE, BACKORDER, MISS, "errors"):
            counters[key] += state[key]
        if counters["first_error"] is None:
            counters["first_error"] = state["first_error"]
        windows.batch([start - due[k] for k in range(i, j)],
                      [done - due[k] for k in range(i, j)], done - start, rates[i], state["errors"])
        i = j
    end_us = now_us()
    windows.finish(end_us, rates[-1])
    return counters, windows, groups, max_group, end_us


def _open_loop_replay(due, rates, requests, batch, window_ms, execute_batch):
    """Dispatch on the due schedule, execute and commit in a worker, ack on completion.

    The pacer thread does only what `flashdb-load`'s dispatcher does: wait for the due instant and
    hand the request over. The worker owns the connection, takes the requests already ready (at
    most `batch`), executes and commits them as ONE durable group, and stamps their completion. The
    lock covers bookkeeping only, never `execute_batch`, so a running commit cannot delay dispatch.
    """
    total = len(due)
    stream = iter(requests)
    windows = WindowSamples(window_ms)
    counters = {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": 0, "first_error": None}
    stats = {"groups": 0, "max_group": 0}
    t0 = time.perf_counter_ns()

    def now_us() -> float:
        return (time.perf_counter_ns() - t0) / 1000.0

    condition = threading.Condition()
    ready: list[tuple[int, float, dict]] = []  # (request index, dispatch us, payload)
    dispatched_all = False

    def worker() -> None:
        while True:
            with condition:
                while not ready and not dispatched_all:
                    condition.wait(0.05)
                if not ready:
                    return  # every request dispatched and every group acked
                pending = ready[:batch]
                del ready[:len(pending)]
            start = now_us()
            outcome = execute_batch([payload for _, _, payload in pending])
            done = now_us()
            with condition:
                windows.flush_through(done, rates[pending[0][0]])
                windows.batch([dispatched - due[index] for index, dispatched, _ in pending],
                              [done - due[index] for index, _, _ in pending],
                              done - start, rates[pending[0][0]], outcome["errors"])
                for key in (TAKE, BACKORDER, MISS, "errors"):
                    counters[key] += outcome[key]
                if counters["first_error"] is None:
                    counters["first_error"] = outcome["first_error"]
                stats["groups"] += 1
                stats["max_group"] = max(stats["max_group"], len(pending))

    thread = threading.Thread(target=worker, name="m1-commit-worker", daemon=True)
    thread.start()
    try:
        for index in range(total):
            _wait_until(t0, due[index])
            now = now_us()
            payload = next(stream)
            with condition:
                windows.flush_through(now, rates[index])
                ready.append((index, now, payload))
                condition.notify()
    finally:
        with condition:
            dispatched_all = True
            condition.notify_all()
        thread.join()
    end_us = now_us()
    windows.finish(end_us, rates[-1])
    return counters, windows, stats["groups"], stats["max_group"], end_us


def run_sql_arm(
    arm: SqlArm,
    manifest: dict,
    requests,
    batch: int = DEFAULT_BATCH,
    window_ms: int = DEFAULT_WINDOW_MS,
    engine_cgroup: Path | None = None,
    open_loop: bool = True,
) -> dict:
    """Replay the trace against one SQL arm and return its run record.

    `requests` is an ITERABLE of parsed requests, not a list: the pacer pulls only the batch in
    front of it, so a long trace costs no more memory than its batch.

    `engine_cgroup` is the cgroup the ENGINE ran in: this process's own for sqlite (embedded), the
    server's scope for postgres. Its `memory.current` is sampled across this measured phase only, so
    the reported peak is serving pressure and never the bulk load that preceded it.

    `open_loop` is the fairness fix the f70d91c report named (§9.3) and it defaults ON: flashdb's
    driver submits its durable barrier to a background worker and keeps dispatching, so scoring a
    SQL arm whose dispatch waits for its own commit against the same schedule-lag rule charges the
    synchronous arm for a difference in client shape, not in engine behaviour. Both shapes run one
    durable commit per group with the same `batch` cap; `pacing` in the record states which ran.
    """
    sampler = MemoryPeakSampler(engine_cgroup).start()
    due, rates = build_schedule(manifest)
    paced = run_paced_loop(due, rates, requests, batch, window_ms, arm.execute_batch,
                           open_loop=open_loop)
    engine_memory = sampler.stop()
    return {
        "arm": arm.name,
        "durability": arm.durability,
        "pacing": ("open loop: dispatch on the due schedule, one worker executes and commits each"
                   " group and acks on completion (flashdb-load's shape)" if open_loop else
                   "closed loop: the group's commit is acked before the next dispatch"),
        "batch": batch,
        "window_ms": window_ms,
        "lag_ceiling_us": LAG_CEILING_US,
        **paced,
        "engine_memory": engine_memory,
    }



# ---------------------------------------------------------------------------
# The KPI ladder's instruments (`docs/2_GOAL.md` §0)
# ---------------------------------------------------------------------------
KPI_SCHEMA = "flashdb.baseline.kpi.v1"


def _kpi(value, unit: str, method: str, reason: str | None = None) -> dict:
    """One KPI-ladder reading: its value, its unit, and the command or file it was read from.

    `value` is `None` only when the arm cannot supply the quantity, and `reason` then says what was
    absent. A missing instrument never becomes a zero: zero is a measurement (nothing was written),
    so the two states must not share a spelling.
    """
    field = {"value": value, "unit": unit, "method": method}
    if value is None:
        field["reason"] = reason or "the arm records no instrument for this quantity"
    return field


def _delta_field(before, after, *, unit: str, method: str, what: str) -> dict:
    """A counter's delta across the measured phase, or an absent reading naming why."""
    if before is None or after is None:
        return _kpi(None, unit, method,
                    f"{what} was not readable both before and after the measured phase"
                    f" ({before!r} -> {after!r})")
    if after < before:
        return _kpi(None, unit, method,
                    f"{what} went backwards ({before} -> {after}), so the difference is not a"
                    " measured-phase cost")
    return _kpi(after - before, unit, method)


def _sum_fields(*fields: dict, unit: str, method: str) -> dict:
    """The sum of readings that must ALL be present; one missing summand makes the total absent."""
    missing = [field for field in fields if field["value"] is None]
    if missing:
        return _kpi(None, unit, method, "; ".join(field["reason"] for field in missing))
    return _kpi(sum(field["value"] for field in fields), unit, method)


def _ratio(numerator: dict, denominator: dict, *, unit: str, method: str) -> dict:
    """A derived reading, undefined (never 0) when a side is absent or the denominator is 0."""
    if numerator["value"] is None:
        return _kpi(None, unit, method, f"numerator absent: {numerator['reason']}")
    if denominator["value"] is None:
        return _kpi(None, unit, method, f"denominator absent: {denominator['reason']}")
    if denominator["value"] == 0:
        return _kpi(None, unit, method,
                    "the denominator is 0, so the ratio is undefined and is never reported as 0")
    return _kpi(numerator["value"] / denominator["value"], unit, method)


def kpi_record(*, write_bytes: dict, ingest_bytes: dict, read_bytes: dict, requests: dict,
               cpu_us: dict, rss_peak_bytes: dict, instance_bytes: dict, recovery_s: dict,
               readings: dict) -> dict:
    """Assemble one arm-run's KPI-ladder readings from that arm's own named instruments.

    Each input is a `_kpi` field, and every ratio is derived only from those fields, so a number in
    the record cannot come from a second, unstated path. `readings` keeps the raw before/after
    values a verifier needs to audit the deltas.
    """
    return {
        "schema": KPI_SCHEMA,
        "write_bytes": write_bytes,
        "ingest_bytes": ingest_bytes,
        "write_amp": _ratio(write_bytes, ingest_bytes, unit="bytes written / byte ingested",
                            method="write_bytes / ingest_bytes"),
        "read_bytes": read_bytes,
        "requests": requests,
        "bytes_read_per_request": _ratio(read_bytes, requests, unit="bytes / request",
                                         method="read_bytes / requests"),
        "cpu_us": cpu_us,
        "cpu_us_per_request": _ratio(cpu_us, requests, unit="us / request",
                                     method="cpu_us / requests"),
        "rss_peak_bytes": rss_peak_bytes,
        "instance_bytes": instance_bytes,
        "rss_per_data_byte": _ratio(rss_peak_bytes, instance_bytes,
                                    unit="resident bytes / data byte",
                                    method="rss_peak_bytes / instance_bytes"),
        "recovery_s": recovery_s,
        "readings": readings,
    }


def _requests_total(windows: list[dict]) -> int:
    return sum(int(window.get("requests", 0)) for window in windows)


def _region_totals(windows: list[dict], keys: tuple[str, ...]) -> dict[str, int]:
    """Sum named per-region telemetry counters over the measured-phase windows."""
    totals = {key: 0 for key in keys}
    for window in windows:
        for region in (window.get("regions") or {}).values():
            for key in keys:
                totals[key] += int(region.get(key, 0) or 0)
    return totals


def _ingest_field(ingest: dict) -> dict:
    return _kpi(ingest["payload_bytes"], "bytes",
                "sum of the payload bytes of the trace's write requests (line bytes, newline"
                " excluded), where a write request is one carrying an `ops` block: trace.jsonl, the"
                " same frozen trace every arm replays")


def _requests_field(requests: int, windows_file: str) -> dict:
    return _kpi(requests, "requests",
                f"sum of the per-window `requests` over the measured phase ({windows_file})")


def _cpu_field(cpu: dict, method: str) -> dict:
    return _delta_field(cpu.get("start_usec"), cpu.get("end_usec"), unit="us", method=method,
                        what="cgroup cpu.stat usage_usec")


def _rss_field(rss_peak_bytes: int | None, method: str) -> dict:
    return _kpi(rss_peak_bytes, "bytes", method,
                "the record's engine_memory.peak_bytes is null: the memory sampler could not read"
                " the engine's cgroup")


def flashdb_kpi(windows: list[dict], *, ingest: dict, cpu: dict, rss_peak_bytes: int | None,
                instance_bytes: int | None, recovery_s: float | None,
                windows_file: str) -> dict:
    """flashdb's KPI from telemetry the arm already emits: its region counters and the trace.

    `bytes_written` is the encoded WAL op bytes the engine durably appended, `compaction_bytes` the
    merged tile output and `index_output_bytes` the derived index tile output, each attributed per
    region by `engine/src/metrics.rs`. The driver's `compact_bytes` is those same merges split by
    region, so it is not added a second time.
    """
    totals = _region_totals(windows, ("bytes_written", "compaction_bytes",
                                      "index_output_bytes", "bytes_read"))
    telemetry = f"the driver's --metrics-json region telemetry ({windows_file})"
    write_bytes = _kpi(
        totals["bytes_written"] + totals["compaction_bytes"] + totals["index_output_bytes"],
        "bytes",
        "sum over the measured-phase windows of per-region `bytes_written` (encoded WAL op bytes)"
        " + `compaction_bytes` (merged tile output) + `index_output_bytes` (derived index tile"
        f" output), from {telemetry}; an ordinary memtable publish's tile bytes are not attributed"
        " per region, so those lie outside this sum")
    read_bytes = _kpi(totals["bytes_read"], "bytes",
                      "sum over the measured-phase windows of per-region `bytes_read` (data-block"
                      f" bytes read from tiles while serving the region), from {telemetry}")
    cpu_us = _cpu_field(cpu, "cpu.stat usage_usec of the flashdb-load scope at the end minus at the"
                             " start of the invocation (the driver's whole measured phase)")
    return kpi_record(
        write_bytes=write_bytes, ingest_bytes=_ingest_field(ingest), read_bytes=read_bytes,
        requests=_requests_field(_requests_total(windows), windows_file), cpu_us=cpu_us,
        rss_peak_bytes=_rss_field(rss_peak_bytes,
                                  "the record's engine_memory.peak_bytes: exact `memory.peak` of"
                                  " the flashdb-load invocation's own scope, read inside it"),
        instance_bytes=_kpi(instance_bytes, "bytes",
                            "directory bytes of the instance the arm served, live WAL included"),
        recovery_s=_kpi(recovery_s, "s",
                        "wall time from the harness's subprocess start (the `systemd-run` scope"
                        " launch) to the driver's `startup_warm` line: the end of the open +"
                        " eager-warm phase, after which the first request is served on its due"
                        " schedule",
                        "the driver printed no `startup_warm` line, so no open/warm boundary was"
                        " observed"),
        readings={"region_totals": totals, "cpu_stat": cpu, "trace": ingest,
                  "windows_file": windows_file, "recovery_s": recovery_s})


def sqlite_kpi(windows: list[dict], *, io_before: dict, io_after: dict, ingest: dict, cpu: dict,
               rss_peak_bytes: int | None, data_bytes: int | None, windows_file: str) -> dict:
    """sqlite's KPI from the WAL and database file sizes plus the page arithmetic.

    Both readings are taken on the arm's own connection before and after the measured phase, while
    the post-run checkpoint has not yet run: with the automatic checkpoint deferred the WAL holds
    every frame the run wrote, so its size delta IS the phase's durable write.
    """
    files_before, files_after = io_before.get("files") or {}, io_after.get("files") or {}
    wal_delta = _delta_field(files_before.get("wal", 0), files_after.get("wal", 0), unit="bytes",
                             method="sqlite `-wal` file size after the measured phase minus its size"
                                    " before it (read on the arm's database file path)",
                             what="the sqlite -wal file size")
    page_delta = _delta_field(io_before.get("page_bytes"), io_after.get("page_bytes"),
                              unit="bytes",
                              method="`PRAGMA page_count * PRAGMA page_size` after the measured"
                                     " phase minus before it, read on the arm's connection",
                              what="the sqlite page_count * page_size reading")
    write_bytes = _sum_fields(
        wal_delta, page_delta, unit="bytes",
        method="sqlite bytes durably written during the measured phase: the `-wal` file's size"
               " delta plus the `PRAGMA page_count * PRAGMA page_size` growth; when an automatic"
               " checkpoint rewrites the WAL in place the file delta is a lower bound and the"
               " page growth covers the moved frames")
    return kpi_record(
        write_bytes=write_bytes, ingest_bytes=_ingest_field(ingest),
        read_bytes=_kpi(None, "bytes",
                        "no instrument: the SQL pacer emits no per-region byte telemetry, and sqlite"
                        " exposes no bytes-read counter through this harness",
                        "sqlite records no bytes-read counter; its page cache hides tile reads"),
        requests=_requests_field(_requests_total(windows), windows_file),
        cpu_us=_cpu_field(cpu, "cpu.stat usage_usec of this process's cgroup at the end minus at"
                               " the start of the measured phase (sqlite is embedded, so the pacer"
                               " shares the engine's cgroup)"),
        rss_peak_bytes=_rss_field(rss_peak_bytes,
                                  "the record's engine_memory.peak_bytes: the maximum of sampled"
                                  " cgroup memory.current across the measured phase"),
        instance_bytes=_kpi(data_bytes, "bytes",
                            "the arm's on-disk footprint at the end of the measured phase: the"
                            " database file plus its -wal and -shm sidecars"),
        recovery_s=_kpi(None, "s", "no instrument: sqlite has no separately measured open phase",
                        "sqlite recovers its WAL lazily on first access INSIDE the measured phase,"
                        " where the cost is charged to service latency; the harness measures no"
                        " separate open/warm boundary"),
        readings={"files_before": files_before, "files_after": files_after,
                  "page_bytes_before": io_before.get("page_bytes"),
                  "page_bytes_after": io_after.get("page_bytes"), "cpu_stat": cpu,
                  "trace": ingest, "windows_file": windows_file})


def _pg_block_bytes(io: dict) -> int | None:
    """The buffer manager's blocks reached (read + hit) times the cluster's block size."""
    read, hit, size = io.get("blocks_read"), io.get("blocks_hit"), io.get("block_size")
    if read is None or hit is None or not size:
        return None
    return (read + hit) * size


def postgres_kpi(windows: list[dict], *, io_before: dict, io_after: dict, ingest: dict, cpu: dict,
                 rss_peak_bytes: int | None, windows_file: str) -> dict:
    """postgres's KPI from `pg_stat_wal`, `pg_database_size()` and the buffer-manager blocks.

    All readings are taken on their own connection before and after the measured phase, so the
    instrument never joins the arm's measured transaction. `write_bytes` is the WAL the server
    generated plus the database's logical growth, each from the server's own cumulative counters.
    """
    wal_delta = _delta_field(io_before.get("wal_bytes"), io_after.get("wal_bytes"), unit="bytes",
                             method="`SELECT wal_bytes FROM pg_stat_wal` after the measured phase"
                                    " minus before it",
                             what="pg_stat_wal.wal_bytes")
    db_delta = _delta_field(io_before.get("database_bytes"), io_after.get("database_bytes"),
                            unit="bytes",
                            method="`pg_database_size(current_database())` after the measured phase"
                                   " minus before it",
                            what="pg_database_size(current_database())")
    write_bytes = _sum_fields(
        wal_delta, db_delta, unit="bytes",
        method="postgres bytes durably written during the measured phase: the"
               " `pg_stat_wal.wal_bytes` delta plus the `pg_database_size(current_database())`"
               " delta")
    read_bytes = _delta_field(
        _pg_block_bytes(io_before), _pg_block_bytes(io_after), unit="bytes",
        method="(`blks_read + blks_hit` from pg_stat_database for this database) x the cluster's"
               " block_size, after the measured phase minus before: bytes reached through the"
               " buffer manager, not device bytes",
        what="pg_stat_database blocks x block_size")
    instance_bytes = io_after.get("database_bytes")
    return kpi_record(
        write_bytes=write_bytes, ingest_bytes=_ingest_field(ingest), read_bytes=read_bytes,
        requests=_requests_field(_requests_total(windows), windows_file),
        cpu_us=_cpu_field(cpu, "cpu.stat usage_usec of the postgres server's own scope"
                               " (M1_ENGINE_CGROUP) at the end minus at the start of the measured"
                               " phase"),
        rss_peak_bytes=_rss_field(rss_peak_bytes,
                                  "the record's engine_memory.peak_bytes: the maximum of sampled"
                                  " server-scope memory.current across the measured phase"),
        instance_bytes=_kpi(instance_bytes, "bytes",
                            "`pg_database_size(current_database())` at the end of the measured"
                            " phase (logical size; the WAL is outside it)"),
        recovery_s=_kpi(None, "s", "no open/warm phase inside the arm",
                        "the postgres server is started outside the arm by"
                        " scripts/baseline-env.sh and the arm only opens a client connection, so"
                        " there is no engine open/warm phase to time"),
        readings={"wal_bytes_before": io_before.get("wal_bytes"),
                  "wal_bytes_after": io_after.get("wal_bytes"),
                  "database_bytes_before": io_before.get("database_bytes"),
                  "database_bytes_after": io_after.get("database_bytes"),
                  "blocks_read_before": io_before.get("blocks_read"),
                  "blocks_hit_before": io_before.get("blocks_hit"),
                  "blocks_read_after": io_after.get("blocks_read"),
                  "blocks_hit_after": io_after.get("blocks_hit"),
                  "block_size": io_after.get("block_size"), "cpu_stat": cpu, "trace": ingest,
                  "windows_file": windows_file})


def sqlite_io(arm: SqlArm, db: Path) -> dict:
    """The sqlite file and page readings the KPI is differenced from, read on the arm itself."""
    return {"files": _sqlite_files(db), "page_bytes": sqlite_page_bytes(arm)}


def sqlite_page_bytes(arm: SqlArm) -> int | None:
    """`PRAGMA page_count * PRAGMA page_size`: sqlite's logical database size."""
    try:
        arm.cur.execute("PRAGMA page_count")
        pages = arm.cur.fetchone()
        arm.cur.execute("PRAGMA page_size")
        size = arm.cur.fetchone()
        return int(pages[0]) * int(size[0])
    except (sqlite3.Error, TypeError, IndexError):
        return None


def postgres_io(database: str) -> dict:
    """The postgres counters the KPI is differenced from, on their own short-lived connection.

    A separate connection is used so the instrument cannot join the arm's measured transaction
    (psycopg opens one on its first statement); the counters are cumulative server statistics, so
    two reads around the phase are their delta.
    """
    with postgres_connect(database) as conn:
        with conn.cursor() as cur:
            cur.execute("SELECT wal_bytes FROM pg_stat_wal")
            row = cur.fetchone()
            wal_bytes = int(row[0]) if row and row[0] is not None else None
            cur.execute("SELECT pg_database_size(current_database())")
            row = cur.fetchone()
            database_bytes = int(row[0]) if row and row[0] is not None else None
            cur.execute("SELECT blks_read, blks_hit FROM pg_stat_database"
                        " WHERE datname = current_database()")
            row = cur.fetchone()
            blocks_read = int(row[0]) if row else None
            blocks_hit = int(row[1]) if row else None
            cur.execute("SHOW block_size")
            row = cur.fetchone()
            block_size = int(row[0]) if row and str(row[0]).isdigit() else None
    return {"wal_bytes": wal_bytes, "database_bytes": database_bytes,
            "blocks_read": blocks_read, "blocks_hit": blocks_hit, "block_size": block_size}


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


def seed_flashdb_instance(
    name: str,
    workdir: Path,
    bins: Path,
    seed_cap_mib: int = SEED_CAP_MIB,
    instance_root: Path | None = None,
) -> dict:
    """Build the pre-load (seeded) flashdb instance ONCE, inside its construction cap.

    Construction is heavier than serving (the seed memtable holds the whole dimension set), so this
    runs under `seed_cap_mib`; the measured load that follows runs under `engine_cap_mib`. Both are
    recorded. The seeding trace is the frozen one in the work directory, hashed here so every arm's
    instance can be shown to come from the same bytes.
    """
    instance = instance_path(name, workdir, instance_root)
    if instance.exists():
        shutil.rmtree(instance)
    instance.parent.mkdir(parents=True, exist_ok=True)
    seed_path = workdir / "seed.jsonl"
    seed_t0 = time.perf_counter()
    seeded = subprocess.run(
        scoped_command([str(bins / "flashdb-scale"), "seed-inventory", str(instance),
                        str(seed_path)], seed_cap_mib, SEED_PEAK_PREFIX),
        capture_output=True, text=True, env={**os.environ, **SEED_ENV})
    seed_s = time.perf_counter() - seed_t0
    if seeded.returncode != 0:
        raise RuntimeError(f"seed-inventory failed (rc={seeded.returncode}): {seeded.stderr[-2000:]}")
    counts = {key: int(value) for key, value in
              re.findall(r"(blocks|docs|customers|stocks)=(\d+)", seeded.stdout)}
    return {
        "arm": name,
        "seeded_counts": counts,
        "seed_s": seed_s,
        "seed_trace": str(seed_path),
        "seed_trace_sha256": file_sha256(seed_path),
        "construction_memory": {"cap_mib": seed_cap_mib,
                                "peak_bytes": _peak_from_log(seeded.stdout.splitlines(),
                                                             SEED_PEAK_PREFIX)},
        "instance_bytes": _dir_bytes(instance),
        "instance_files": _largest_files(instance),
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
    }


def stream_driver_log(stream, log, clock=time.perf_counter) -> float | None:
    """Copy the driver's streamed output into its log and time the open/warm boundary.

    Returns the instant the driver's `startup_warm` line ARRIVED - the end of the open +
    eager-warm phase, which is the recovery boundary the KPI ladder asks for - or `None` when the
    driver printed no such line (an older build, or `--dry-run`, which has no engine reads to warm).
    `clock` is injectable so the boundary can be tested without a driver.
    """
    warm_at = None
    for line in stream:
        log.write(line)
        if warm_at is None and line.startswith("startup_warm"):
            warm_at = clock()
    return warm_at


def run_flashdb_load(
    name: str,
    manifest: dict,
    workdir: Path,
    bins: Path,
    order_flow_requests: int,
    batch: int = DEFAULT_BATCH,
    window_ms: int = DEFAULT_WINDOW_MS,
    engine_cap_mib: int = ENGINE_CAP_MIB,
    instance_root: Path | None = None,
) -> dict:
    """Serve the trace through `flashdb-load` in the instance's own engine cap and parse its windows.

    The instance is the PRE-LOAD one (restored from the snapshot by the controlled protocol); this
    function serves and measures, and never seeds.
    """
    instance = instance_path(name, workdir, instance_root)
    if not instance.exists():
        raise SystemExit(f"no instance at {instance}; build or restore it before running the arm")
    trace = workdir / "trace.jsonl"
    cmd = flashdb_load_command(bins, instance, trace, manifest, batch, window_ms)
    driver_log = workdir / f"driver-{name}.log"
    t0 = time.perf_counter()
    # The driver's dump is written straight to its log: the measured process must not hold a whole
    # run's window samples in the instrument's memory. It is streamed line by line rather than
    # captured whole for one reason: the `startup_warm` line is the end of the open + eager-warm
    # phase, so its ARRIVAL time is the recovery boundary the KPI ladder asks for. `flashdb-load`
    # warns it on stderr (merged into stdout here) immediately before its timed loop starts.
    warm_at: float | None
    with open(driver_log, "w", encoding="utf-8") as log:
        proc = subprocess.Popen(scoped_command(cmd, engine_cap_mib, ENGINE_PEAK_PREFIX),
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                                bufsize=1)
        assert proc.stdout is not None
        warm_at = stream_driver_log(proc.stdout, log)
        proc.wait()
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
    instance_bytes = _dir_bytes(instance)
    kpi = flashdb_kpi(windows, ingest=trace_write_ingest(trace),
                      cpu=_scope_cpu_usec(lines), rss_peak_bytes=engine_peak,
                      instance_bytes=instance_bytes,
                      recovery_s=None if warm_at is None else warm_at - t0,
                      windows_file=f"windows-{name}.jsonl")
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
        "instance_bytes": instance_bytes,
        "instance_files": _largest_files(instance),
        "engine_cap_mib": engine_cap_mib,
        "engine_memory": {
            "peak_bytes": engine_peak,
            "start_bytes": _peak_from_log(lines, SCOPE_START_PREFIX),
            "samples": None,
            "method": "exact memory.peak of the flashdb-load invocation's own scope, read inside it",
            "scope": "measured phase only: the instance is BUILT in a separate construction scope",
        },
        "arm_scope_peak_bytes": None,
        "kpi": kpi,
        "windows": windows,
        "curve": summarize_curve(windows, None),
        "windows_jsonl": [json.dumps(w, sort_keys=True) for w in windows],
    }


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
    """Seed a fresh instance, then serve the trace under the engine's own cap (the M1 path).

    Two scopes, two budgets, both recorded: the dataset is BUILT under `seed_cap_mib` (construction
    is heavier than serving, slice 51's measured convention) and the measured load runs under
    `engine_cap_mib`. This is the one-shot path the ticket-80 runs used; the ticket-82 controlled
    protocol builds the instance once (`build-instance`) and calls `run_flashdb_load` on restored
    instances instead, so an arm-run costs its own 20 s and not a 7-12 minute seed.
    """
    seeded = seed_flashdb_instance(name, workdir, bins, seed_cap_mib)
    record = run_flashdb_load(name, manifest, workdir, bins, order_flow_requests, batch, window_ms,
                              engine_cap_mib)
    record["seed_s"] = seeded["seed_s"]
    record["construction_memory"] = seeded["construction_memory"]
    return record



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
    """The ticket's verdict for one arm, computed only from `summarize_curve`'s rows.

    `sustained`/`highest_sustained_rps` are the both-ceilings reading every stored verdict was
    scored with. The decided primary rule is the separate pair `sustained_primary`/
    `highest_primary_rps`, and the service ceiling that breached is reported (not a veto) as a
    window count.
    """
    sustained = [row for row in curve if row["sustained"]]
    highest_primary = highest_primary_sustained_rate(curve)
    return {
        "sustained": bool(sustained),
        "highest_sustained_rps": max((row["offered_rps"] for row in sustained), default=0.0),
        "sustained_primary": bool(highest_primary),
        "highest_primary_rps": highest_primary,
        "service_ceiling_reported_windows": sum(
            row.get("service_ceiling_reported_windows", 0) for row in curve),
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
# Building the pre-load instance once per arm (ticket 82's controlled protocol)
# ---------------------------------------------------------------------------
def postgres_connect(dbname: str):
    """A psycopg connection to the user-space cluster `scripts/baseline-env.sh` provides."""
    import psycopg  # imported here: only the postgres arm needs it

    return psycopg.connect(host=os.environ["M1_PGHOST"], port=os.environ["M1_PGPORT"],
                           user=os.environ["M1_PGUSER"], dbname=dbname, autocommit=True)


def seed_sqlite_instance(workdir: Path, manifest: dict, spec: dict, cache_kib: int,
                         instance_root: Path | None = None,
                         wal_autocheckpoint: int = SQLITE_WAL_AUTOCHECKPOINT_PAGES) -> dict:
    """Build sqlite's pre-load instance: schema + dimension rows, ONE bulk transaction.

    Seeding is setup, so it runs journal-less (`SqliteArm.begin_bulk_load`) and the measured
    durability is restored and READ BACK before the connection closes. The close checkpoints the
    WAL into the database file, which is what makes the snapshot a single stable file.
    """
    db = instance_path("sqlite", workdir, instance_root)
    for stale in (db, Path(f"{db}-wal"), Path(f"{db}-shm")):
        stale.unlink(missing_ok=True)
    db.parent.mkdir(parents=True, exist_ok=True)
    arm = sqlite_arm(db, cache_kib, wal_autocheckpoint=wal_autocheckpoint)
    customers, stock = seed_docs(manifest, spec["on_hand"])
    seed_t0 = time.perf_counter()
    arm.seed(customers, stock)
    seed_s = time.perf_counter() - seed_t0
    observed = arm.require_measured_durability()
    counts = {"customers": _scalar(arm, "SELECT count(*) FROM customer"),
              "stocks": _scalar(arm, "SELECT count(*) FROM stock")}
    arm.conn.close()
    return {
        "arm": "sqlite",
        "seeded_counts": counts,
        "seed_s": seed_s,
        "durability_observed": observed,
        "sqlite_cache_kib": cache_kib,
        "instance_bytes": _db_bytes(db),
        "sqlite_files": _sqlite_files(db),
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
    }


def seed_postgres_instance(manifest: dict, spec: dict, database: str = POSTGRES_DATABASE,
                           engine_cap_mib: int = ENGINE_CAP_MIB) -> dict:
    """Build postgres's pre-load instance: one named database, schema + rows, one transaction.

    The database NAME is fixed (not `m1_<arm>_<pid>`) on purpose: the cluster's data directory is
    the arm's instance, and a fixed name is what lets the pre-load state be a filesystem copy of the
    whole cluster instead of a re-seed per arm-run.
    """
    with postgres_connect("postgres") as admin:
        admin.execute(f'DROP DATABASE IF EXISTS "{database}"')
        admin.execute(f'CREATE DATABASE "{database}"')
    conn = postgres_connect(database)
    conn.autocommit = False
    try:
        arm = PostgresArm(conn)
        arm.create_schema()
        customers, stock = seed_docs(manifest, spec["on_hand"])
        seed_t0 = time.perf_counter()
        arm.seed(customers, stock)
        seed_s = time.perf_counter() - seed_t0
        counts = {"customers": _scalar(arm, "SELECT count(*) FROM customer"),
                  "stocks": _scalar(arm, "SELECT count(*) FROM stock")}
        data_bytes = _scalar(arm, "SELECT pg_database_size(current_database())")
    finally:
        conn.close()
    return {
        "arm": "postgres",
        "database": database,
        "seeded_counts": counts,
        "seed_s": seed_s,
        "data_bytes": data_bytes,
        "postgres_settings": {"shared_buffers": "see run record", "fsync": "on",
                              "synchronous_commit": "on"},
        "engine_cap_mib": engine_cap_mib,
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
    }


def build_instance(name: str, manifest: dict, spec: dict, workdir: Path, bins: Path,
                   instance_root: Path | None, seed_cap_mib: int, engine_cap_mib: int,
                   sqlite_cache_kib: int, database: str,
                   wal_autocheckpoint: int = SQLITE_WAL_AUTOCHECKPOINT_PAGES) -> dict:
    """Build one arm's pre-load instance and CHECK it holds the dataset the manifest describes.

    The check is what ties the two seeding paths together: flashdb is seeded from the frozen
    `seed.jsonl` and the SQL arms from `seed_docs(manifest)`, so an equal customer/stock count on
    every arm is the evidence that all three were seeded from the same rows.
    """
    if name in FLASHDB_ARMS:
        record = seed_flashdb_instance(name, workdir, bins, seed_cap_mib, instance_root)
    elif name == "sqlite":
        record = seed_sqlite_instance(workdir, manifest, spec, sqlite_cache_kib, instance_root,
                                      wal_autocheckpoint)
    elif name == "postgres":
        record = seed_postgres_instance(manifest, spec, database, engine_cap_mib)
    else:
        raise SystemExit(f"unknown arm {name!r}")
    counts = record.get("seeded_counts") or {}
    expected = {"customers": int(manifest["n_customers"]),
                "stocks": int(manifest["n_sku"]) * int(manifest["n_loc"])}
    for key, want in expected.items():
        if key in counts and counts[key] != want:
            raise SystemExit(f"{name} seeded {counts[key]} {key} but the manifest describes"
                             f" {want}; the pre-load instance is not the frozen dataset")
    record["expected_counts"] = expected
    return record


def cmd_build_instance(args: argparse.Namespace) -> int:
    """Build (seed) the pre-load instance of each requested arm; no request is served here."""
    workdir = Path(args.workdir).resolve()
    manifest, spec, _trace = _load_workdir(workdir)
    instance_root = Path(args.instance_root).resolve() if args.instance_root else None
    records = {}
    for name in args.arms:
        records[name] = build_instance(name, manifest, spec, workdir, Path(args.bins),
                                       instance_root, args.seed_cap_mib, args.engine_cap_mib,
                                       args.sqlite_cache_kib, args.database,
                                       args.sqlite_wal_autocheckpoint)
        print(f"build-instance {name}: seed_s={records[name]['seed_s']:.1f}"
              f" bytes={records[name].get('instance_bytes') or records[name].get('data_bytes')}")
    path = Path(args.out) if args.out else workdir / "instance-build.json"
    _write_json(path, records)
    if args.snapshot_dir:
        for name in args.arms:
            snapshot_instance(name, workdir, Path(args.snapshot_dir).resolve(), instance_root)
    return 0


def cmd_snapshot(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    instance_root = Path(args.instance_root).resolve() if args.instance_root else None
    for name in args.arms:
        snapshot_instance(name, workdir, Path(args.snapshot_dir).resolve(), instance_root)
    return 0


def cmd_restore(args: argparse.Namespace) -> int:
    workdir = Path(args.workdir).resolve()
    instance_root = Path(args.instance_root).resolve() if args.instance_root else None
    for name in args.arms:
        state = restore_instance(name, workdir, Path(args.snapshot_dir).resolve(), instance_root)
        print(f"restore {name}: {state['bytes']} bytes from {state['from']}"
              f" (sha256 verified, {state['restore_s']:.1f} s)")
    return 0


# ---------------------------------------------------------------------------
# The device floor: how much lag this machine shows with NO engine in the path
# ---------------------------------------------------------------------------
def run_floor_bench(bench: str, due: list[float], rates: list[float], batch: int,
                    window_ms: int, target: Path) -> dict:
    """One floor bench, measured by the arms' own pacer and window rule.

    `idle` paces the schedule and does nothing per request, so its schedule lag IS the machine's
    own wakeup/scheduler floor. `fsync` writes a small record and fsyncs once per request, so its
    per-request service latency is the device's durable-commit floor for the same one-commit-per-
    request durability every arm runs at these rates.
    """
    requests = ({"id": index} for index in range(len(due)))
    if bench == "idle":
        def execute(_batch) -> dict:
            return {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": 0, "first_error": None}
        closer = None
    elif bench == "fsync":
        target.parent.mkdir(parents=True, exist_ok=True)
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        payload = json.dumps({"bench": "fsync", "record": "x" * 256}).encode()

        def execute(batch_) -> dict:
            for _ in batch_:
                os.write(fd, payload)
                os.fsync(fd)
            return {TAKE: 0, BACKORDER: 0, MISS: 0, "errors": 0, "first_error": None}

        def closer() -> None:
            os.close(fd)
    else:
        raise SystemExit(f"unknown floor bench {bench!r}; expected idle or fsync")
    try:
        paced = run_paced_loop(due, rates, requests, batch, window_ms, execute)
    finally:
        if closer is not None:
            closer()
    (target.parent / f"windows-floor-{bench}.jsonl").write_text(
        "\n".join(paced["windows_jsonl"]) + "\n", encoding="utf-8")
    paced["windows_file"] = f"windows-floor-{bench}.jsonl"
    paced["file_bytes"] = target.stat().st_size if target.exists() else 0
    return paced


def cmd_floor(args: argparse.Namespace) -> int:
    """Measure the device floor (idle schedule lag + fsync commit latency), no engine involved."""
    out = Path(args.out).resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    manifest = {"segments": [{"offered_rate": args.rate, "count": args.requests}],
                "total_requests": args.requests}
    due, rates = build_schedule(manifest)
    benches = {}
    for bench in ("idle", "fsync"):
        benches[bench] = run_floor_bench(bench, due, rates, args.batch, args.window_ms,
                                        out.parent / f"floor-{bench}.dat")
    payload = {
        "schema": FLOOR_SCHEMA,
        "rate_rps": args.rate,
        "requests": args.requests,
        "window_ms": args.window_ms,
        "batch": args.batch,
        "lag_ceiling_us": LAG_CEILING_US,
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
        "machine": machine_state(),
        "engine": "none: an idle pacer and a file fsync, no storage engine in the path",
        "benches": {name: {key: value for key, value in paced.items() if key != "windows_jsonl"}
                    for name, paced in benches.items()},
    }
    _write_json(out, payload)
    for name, bench in benches.items():
        row = bench["curve"][0]
        print(f"floor {name}: lag p99 {row['lag_p99_us']} us, service p99 {row['p99_us']} us,"
              f" rejected {row['invalid_windows']}/{row['windows']} windows,"
              f" achieved {row['achieved_rps']:.1f} rps")
    return 0


# ---------------------------------------------------------------------------
# Machine state: what the schedule-lag rule is sensitive to, recorded at every boundary
# ---------------------------------------------------------------------------
def _read_text(path: str) -> str | None:
    try:
        return Path(path).read_text().strip()
    except OSError:
        return None


def _power_profile() -> str | None:
    try:
        done = subprocess.run(["powerprofilesctl", "get"], capture_output=True, text=True,
                              timeout=5)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() or None


def _top_cpu_processes(limit: int = 6) -> list[dict]:
    """The busiest CPU consumers at this instant, so the state is recorded rather than promised."""
    try:
        done = subprocess.run(["ps", "-eo", "pcpu=,comm=,args=", "--sort=-pcpu"],
                              capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return []
    rows = []
    for line in done.stdout.splitlines()[1:]:
        parts = line.split(None, 2)
        if len(parts) < 2:
            continue
        rows.append({"pcpu": float(parts[0]), "comm": parts[1],
                     "args": (parts[2] if len(parts) > 2 else "")[:160]})
        if len(rows) >= limit:
            break
    return rows


def _concurrent_work() -> list[dict]:
    """Processes that would invalidate the measured phase (builds, extra harnesses, seed jobs).

    The desktop applications (firefox, gnome-shell, the `pi` agent) are NOT on this list: an arm is
    compared only against itself and its paired arms within one machine state, so a steady desktop
    load is a controlled condition, while a compiler, a second harness or a seed job is not.
    """
    found = []
    try:
        done = subprocess.run(["ps", "-eo", "pid=,ppid=,stat=,args="], capture_output=True,
                              text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return found
    # This process and its ANCESTORS are the instrument itself (the harness, the runner's payload,
    # the orchestrating shell). Everything else that matches is a second job on the box.
    own = set()
    pid = os.getpid()
    while pid and pid not in own:
        own.add(pid)
        try:
            stat = Path(f"/proc/{pid}/stat").read_text()
            pid = int(stat.rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            break
    for line in done.stdout.splitlines():
        parts = line.split(None, 3)
        if len(parts) < 4:
            continue
        if parts[2].startswith("Z") or int(parts[0]) in own:
            continue
        args = parts[3]
        for pattern in CONCURRENT_WORK_PATTERNS:
            if pattern in args:
                found.append({"pid": int(parts[0]), "pattern": pattern, "args": args[:160]})
                break
    return found


def machine_state() -> dict:
    """AC, power profile, EPP, effective frequency, load and the busiest processes.

    The governor NAME is not a statement about this platform: on this intel_pstate laptop it reads
    `powersave` even when the EPP hint and the effective frequency are both at maximum, which is why
    AC, the power profile, the EPP hint and `scaling_cur_freq` are recorded together instead.
    """
    cpu_dirs = sorted(Path("/sys/devices/system/cpu").glob("cpu[0-9]*"))
    ac = [line for path in sorted(Path("/sys/class/power_supply").glob("A*"))
          for line in [_read_text(str(path / "online"))] if line is not None]
    loadavg = _read_text("/proc/loadavg") or ""
    return {
        "at": datetime.datetime.now().isoformat(timespec="seconds"),
        "ac_online": ac,
        "power_profile": _power_profile(),
        "governor": _read_text("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        "epp": sorted({_read_text(str(cpu / "cpufreq/energy_performance_preference")) or "n/a"
                       for cpu in cpu_dirs}),
        "scaling_cur_freq_khz": sorted({_read_text(str(cpu / "cpufreq/scaling_cur_freq")) or "n/a"
                                        for cpu in cpu_dirs}),
        "scaling_max_freq_khz": _read_text("/sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq"),
        "nproc": os.cpu_count(),
        "loadavg": loadavg,
        "top_cpu": _top_cpu_processes(),
        "concurrent_work": _concurrent_work(),
    }


def cmd_machine_state(args: argparse.Namespace) -> int:
    """Append one machine-state record; `--phase` names the boundary it was taken at."""
    out = Path(args.out).resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    state = machine_state()
    state["phase"] = args.phase
    state["arm"] = args.arm
    state["rung"] = args.rung
    with open(out, "a", encoding="utf-8") as fh:
        fh.write(json.dumps(state, sort_keys=True) + "\n")
    if state["concurrent_work"]:
        print(f"machine-state {args.phase}: CONCURRENT WORK {state['concurrent_work']}",
              file=sys.stderr)
        return 3
    print(f"machine-state {args.phase}: loadavg={state['loadavg'].split()[0]}"
          f" epp={','.join(state['epp'])} freq_khz={','.join(state['scaling_cur_freq_khz'])}")
    return 0


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
    seed_record = prepare_seed_trace(workdir / "seed.jsonl", manifest, args.seed_trace,
                                     args.on_hand)
    _write_json(workdir / "manifest.json", manifest)
    _write_json(workdir / "spec.json", {
        "schema": SPEC_SCHEMA,
        "on_hand": args.on_hand,
        "batch": args.batch,
        "window_ms": args.window_ms,
        "lag_ceiling_us": LAG_CEILING_US,
        "order_flow_requests": order_flows,
        "trace_sha256": manifest["trace_sha256"],
        "seed_trace": seed_record,
        # The exact invocation that produced this work directory's trace/spec; the runner adds its
        # own `runner` block (argv, caps, lock) after `prepare`, so a committed run names the
        # command that made it instead of a plausible alternative.
        "argv": [Path(sys.argv[0]).name, *sys.argv[1:]],
    })
    print(f"trace {workdir / 'trace.jsonl'}: requests={manifest['total_requests']}"
          f" order_flow={order_flows} seed_bytes={seed_record['bytes']}"
          f" seed_sha256={seed_record['sha256']}"
          f" sha256={manifest['trace_sha256']}")
    return 0


def prepare_seed_trace(seed_path: Path, manifest: dict, seed_trace: str | None,
                       on_hand: int = DEFAULT_ON_HAND) -> dict:
    """Provide the seeding trace and record the digest every arm's instance is built from.

    `seed_trace` re-uses a FROZEN trace instead of regenerating one: ticket 82 fixes the pre-load
    dataset once (`runs/m1-10x/rung-50/seed.jsonl`), and every rung and every block of the controlled
    protocol is seeded from those bytes. The file is hard-linked where possible (it is 1.4 GB and a
    controlled run has fifteen work directories) and its digest is recorded in the spec, so "the
    same rows" is a check rather than a claim. The one-line-per-document dialect is also how the
    frozen trace is checked against this work directory's manifest dimensions.
    """
    if seed_trace:
        source = Path(seed_trace).resolve()
        if not source.exists():
            raise SystemExit(f"--seed-trace {source} does not exist")
        seed_path.unlink(missing_ok=True)
        try:
            os.link(source, seed_path)
            method = "hard link"
        except OSError:
            shutil.copyfile(source, seed_path)
            method = "copy"
        # The row COUNT the trace holds is checked where it can be checked cheaply and exactly:
        # against the instance each arm built (`build_instance` compares the seeded doc counts with
        # this manifest's dimensions). What is recorded here is the digest, so every rung and every
        # block of a controlled run can be shown to have used one frozen pre-load dataset.
        return {"source": str(source), "method": method,
                "bytes": seed_path.stat().st_size, "sha256": file_sha256(seed_path)}
    blocks = write_seed_trace(seed_path, manifest, on_hand)
    return {"source": "regenerated from the manifest's own seed and dimensions",
            "method": "generate", "blocks": blocks, "bytes": seed_path.stat().st_size,
            "sha256": file_sha256(seed_path)}


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
    """Serve one arm over the prepared trace and write its run record.

    `--restored` is ticket 82's controlled path: the pre-load instance has already been put back by
    the `restore` step, so this step SEEDS NOTHING and measures the serving path only. Without it
    the step behaves exactly as the ticket-80 runs did (build a fresh instance, then serve it).
    """
    workdir = Path(args.workdir).resolve()
    manifest, spec, trace = _load_workdir(workdir)
    name = args.arm
    instance_root = Path(args.instance_root).resolve() if args.instance_root else None
    if args.restored and instance_root is None:
        # The restored instance is the ONLY instance this run may use; if it lives under a shared
        # root, the arm has to be told, or it would silently seed a fresh one in the workdir and
        # measure that instead.
        raise SystemExit("--restored requires --instance-root: the restore writes the instance"
                         " outside this work directory")
    if name in FLASHDB_ARMS:
        if args.restored:
            record = run_flashdb_load(name, manifest, workdir, Path(args.bins),
                                      spec["order_flow_requests"], spec["batch"],
                                      spec["window_ms"], args.engine_cap_mib, instance_root)
        else:
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
        db = instance_path(name, workdir, instance_root)
        if args.restored:
            # The database IS the restored snapshot: deleting and re-seeding it would replace the
            # controlled pre-load state with a construction step again.
            if not db.exists():
                raise SystemExit(f"no restored sqlite instance at {db}")
            arm = sqlite_arm(db, args.sqlite_cache_kib, create_schema=False,
                             wal_autocheckpoint=args.sqlite_wal_autocheckpoint)
            seed_s = 0.0
        else:
            for stale in (db, Path(f"{db}-wal"), Path(f"{db}-shm")):
                stale.unlink(missing_ok=True)
            arm = sqlite_arm(db, args.sqlite_cache_kib,
                             wal_autocheckpoint=args.sqlite_wal_autocheckpoint)
            customers, stock = seed_docs(manifest, spec["on_hand"])
            seed_t0 = time.perf_counter()
            arm.seed(customers, stock)
            seed_s = time.perf_counter() - seed_t0
        # Read the durability back BEFORE the measured phase: the seeding path runs under
        # journal_mode=OFF / synchronous=OFF and a PRAGMA that cannot be applied fails silently.
        observed_durability = arm.require_measured_durability()
        # The KPI's before-side, read on the arm itself and immediately before the measured phase:
        # the KPI's write bytes are the WAL and page deltas across this phase only.
        io_before = sqlite_io(arm, db)
        cpu_before = cgroup_cpu_usec()
        record = run_sql_arm(arm, manifest, iter_requests(trace), spec["batch"],
                             spec["window_ms"], engine_cgroup=cgroup_self_path())
        cpu_after = cgroup_cpu_usec()
        io_after = sqlite_io(arm, db)
        record["seed_s"] = seed_s
        record["sqlite_cache_kib"] = args.sqlite_cache_kib
        record["durability_observed"] = observed_durability
        record["engine_cap_mib"] = args.engine_cap_mib
        record["arm_scope_peak_bytes"] = cgroup_peak_bytes()
        record["arm_scope_peak_scope"] = ("this process's capped scope over the whole arm step"
                                          " (bulk load + measured phase)")
        record["kpi"] = sqlite_kpi(record["windows"], io_before=io_before, io_after=io_after,
                                   ingest=trace_write_ingest(trace),
                                   cpu={"start_usec": cpu_before, "end_usec": cpu_after},
                                   rss_peak_bytes=record["engine_memory"]["peak_bytes"],
                                   data_bytes=sum(io_after["files"].values()),
                                   windows_file=f"windows-{name}.jsonl")
        # The WAL policy is a measured-phase parameter, and with the automatic checkpoint deferred
        # the checkpoint itself runs here: after the run, before the oracle, and recorded.
        record["sqlite_wal"] = sqlite_wal_record(arm, db)
        record["sqlite_files"] = _sqlite_files(db)
        record["data_bytes"] = _db_bytes(db)
        if args.skip_oracle:
            record["oracle"] = {"status": "DEFERRED", "checks": [],
                                "reason": "--skip-oracle: the restored instance is byte-identical in"
                                          " every block, so the inventory is verified once per arm"}
        else:
            add_verify_index(arm)
            record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                          spec["order_flow_requests"], spec["on_hand"])
    elif name == "postgres":
        record = run_postgres_arm(name, manifest, spec, iter_requests(trace), trace,
                                  args.engine_cap_mib, restored=args.restored,
                                  database=args.database, verify=not args.skip_oracle)
    else:
        raise SystemExit(f"unknown arm {name!r}; expected one of {FLASHDB_ARMS + SQL_ARMS}")
    record["trace_sha256"] = manifest["trace_sha256"]
    record["instance_root"] = str(instance_root) if instance_root else str(workdir)
    record["restored"] = bool(args.restored)
    if args.restored:
        record["restore"] = _read_json(instance_root / f"restore-{name}.json")
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
    # `run_flashdb_probes` is a GENERATOR: calling it costs nothing and the replay happens as
    # `oracle_flashdb` consumes it. The timer therefore wraps the consumption, not the call -- a
    # timer around the call alone records 0.0 s and says nothing about the read-back's cost.
    replayed = run_flashdb_probes(name, manifest, workdir, bins, trace_orders)
    result = oracle_flashdb(replayed, manifest, {
        "take": record["counters"]["take"],
        "errors": record["counters"]["errors"],
        "first_error": record["counters"]["first_error"],
    }, spec["on_hand"])
    record["oracle_s"] = time.perf_counter() - probe_t0
    return result


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


def run_postgres_arm(name: str, manifest: dict, spec: dict, requests, trace: Path,
                     engine_cap_mib: int = ENGINE_CAP_MIB, restored: bool = False,
                     database: str = POSTGRES_DATABASE, verify: bool = True) -> dict:
    """The postgres arm against the user-space cluster `scripts/baseline-env.sh` provides.

    The engine here is the SERVER, in its own capped scope; this client is outside it. The runner
    passes that scope's cgroup as `M1_ENGINE_CGROUP`, so the client can sample the SERVER's
    `memory.current` across the measured phase instead of charging the instrument's memory to the
    engine.

    `restored` selects ticket 82's controlled path: the cluster's data directory has already been
    put back from the snapshot, so the pre-load database exists and nothing may be created, seeded
    or dropped here. Without it the arm builds its own per-run database, exactly as the ticket-80
    runs did.
    """
    dbname = database if restored else f"m1_{name}_{os.getpid()}"
    if not restored:
        with postgres_connect("postgres") as admin:
            admin.execute(f'DROP DATABASE IF EXISTS "{dbname}"')
            admin.execute(f'CREATE DATABASE "{dbname}"')
    try:
        conn = postgres_connect(dbname)
        conn.autocommit = False
        arm = PostgresArm(conn)
        seed_s = 0.0
        if not restored:
            arm.create_schema()
            customers, stock = seed_docs(manifest, spec["on_hand"])
            seed_t0 = time.perf_counter()
            arm.seed(customers, stock)
            seed_s = time.perf_counter() - seed_t0
        engine_cgroup = Path(os.environ[ENGINE_CGROUP_ENV]) if os.environ.get(
            ENGINE_CGROUP_ENV) else None
        # The KPI's before-side, read on its own connection so the instrument never joins the arm's
        # measured transaction; the server scope's cpu.stat gives the engine's own CPU time.
        io_before = postgres_io(dbname)
        cpu_before = cgroup_cpu_usec(engine_cgroup)
        record = run_sql_arm(arm, manifest, requests, spec["batch"], spec["window_ms"],
                             engine_cgroup=engine_cgroup)
        cpu_after = cgroup_cpu_usec(engine_cgroup)
        io_after = postgres_io(dbname)
        record["seed_s"] = seed_s
        record["database"] = dbname
        record["durability"] = PostgresArm.durability
        record["engine_cap_mib"] = engine_cap_mib
        # The server scope's LIFETIME peak (bulk load + measured phase) is patched by the runner
        # after this client step; it is an arm-scope number, never the serving peak.
        record["arm_scope_peak_bytes"] = None
        record["arm_scope_peak_scope"] = ("the postgres server scope's lifetime (bulk load +"
                                          " measured phase); the client runs outside it")
        record["kpi"] = postgres_kpi(record["windows"], io_before=io_before,
                                      io_after=io_after, ingest=trace_write_ingest(trace),
                                      cpu={"start_usec": cpu_before, "end_usec": cpu_after},
                                      rss_peak_bytes=record["engine_memory"]["peak_bytes"],
                                      windows_file=f"windows-{name}.jsonl")
        if verify:
            add_verify_index(arm)
            record["oracle"] = oracle_sql(arm, manifest, record["counters"],
                                          spec["order_flow_requests"], spec["on_hand"])
        else:
            record["oracle"] = {"status": "DEFERRED", "checks": [],
                                "reason": "--skip-oracle: the restored instance is byte-identical in"
                                          " every block, so the inventory is verified once per arm"}
        record["postgres_settings"] = {
            "synchronous_commit": _setting(conn, "synchronous_commit"),
            "fsync": _setting(conn, "fsync"),
            "shared_buffers": _setting(conn, "shared_buffers"),
            "server_version": _setting(conn, "server_version"),
            "data_directory": _setting(conn, "data_directory"),
            "unix_socket_directories": _setting(conn, "unix_socket_directories"),
        }
        # A restored run must measure the cluster whose data directory this harness restored. A
        # `M1_PG_ROOT` that reaches the restore but not the server starts a DIFFERENT cluster, and
        # the run would then report one cluster's latency under the other one's footprint.
        if restored:
            expected = postgres_data_dir().resolve()
            served = Path(record["postgres_settings"]["data_directory"]).resolve()
            if served != expected:
                raise SystemExit(
                    f"the server is serving {served} but this run restored {expected}; check that"
                    " M1_PG_ROOT reaches both the harness and the server")
        # The arm's data:memory ratio is reported from the server's own catalog, not from a guess.
        record["data_bytes"] = _scalar(arm, "SELECT pg_database_size(current_database())")
        conn.close()
    finally:
        if not restored:
            with postgres_connect("postgres") as admin:
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


def saved_window_rows(lines: list[str], window_ms: int) -> list[dict]:
    """The reducer's rows for a saved window JSONL, whichever dialect the arm wrote.

    flashdb's `windows-<arm>.jsonl` IS `cold_0b.window_records`'s output, while the SQL arms' file is
    the pacer's raw `WindowSamples` snapshot (no per-window `completed_delta`), so those rows go
    through the same reducer the stored curve was computed with. The reducer's `completed_delta` is
    the marker: no writer emits it before the reducer does.
    """
    rows = [json.loads(line) for line in lines if line.strip()]
    if rows and all("completed_delta" in row for row in rows):
        return rows
    return window_records(lines, window_ms)


def recompute_records(root: Path) -> list[dict]:
    """Re-derive every saved arm-run's curve from its own window JSONL, without re-running the arm.

    The window samples (`windows-<arm>.jsonl`) are the record's evidence, so a record scored before
    the lag-only decision can be re-read under the decided primary rule from the saved windows
    alone: `sustained` recomputes exactly as recorded (the harness invoked every arm without a
    service ceiling, so the service reading travels in each window's `service_ceiling_breached`),
    and `sustained_primary` is the reading the record did not carry. The recorded columns are
    returned beside the recomputed ones so a caller can show they agree instead of asserting it.

    `root` is either one arm-run workdir (`run-*.json` in it) or a crossing tree
    (`b*/rung-*/run-*.json`).
    """
    root = root.resolve()
    candidates = sorted(set(root.glob("run-*.json")) | set(root.glob("*/rung-*/run-*.json")))
    rows: list[dict] = []
    for path in candidates:
        record = _read_json(path)
        if "curve" not in record or "arm" not in record:
            continue  # `run-spec.json` and the like are not arm-run records
        windows_file = path.parent / str(record.get("windows_file", ""))
        if not windows_file.is_file():
            raise SystemExit(f"{path} names no readable windows file: {windows_file}")
        windows = saved_window_rows(
            windows_file.read_text(encoding="utf-8").splitlines(),
            int(record.get("window_ms") or DEFAULT_WINDOW_MS))
        recorded = {float(row["offered_rps"]): row for row in record.get("curve", [])}
        for row in summarize_curve(windows, None):
            prior = recorded.get(float(row["offered_rps"]), {})
            rows.append({
                "workdir": str(path.parent.relative_to(root)) if path.parent != root else ".",
                "arm": record.get("arm"),
                "offered_rps": row["offered_rps"],
                "achieved_rps": row["achieved_rps"],
                "recorded_achieved_rps": prior.get("achieved_rps"),
                "lag_p99_us": row["lag_p99_us"],
                "recorded_lag_p99_us": prior.get("lag_p99_us"),
                "service_p99_us": row["p99_us"],
                "recorded_service_p99_us": prior.get("p99_us"),
                "sustained": row["sustained"],
                "recorded_sustained": prior.get("sustained"),
                "sustained_primary": row["sustained_primary"],
                "service_ceiling_reported_windows": row["service_ceiling_reported_windows"],
            })
    if not rows:
        raise SystemExit(f"no run records or window samples under {root}")
    return rows


RECOMPUTE_FIELDS = (("achieved_rps", "recorded_achieved_rps"),
                    ("lag_p99_us", "recorded_lag_p99_us"),
                    ("service_p99_us", "recorded_service_p99_us"),
                    ("sustained", "recorded_sustained"))


def cmd_recompute(args: argparse.Namespace) -> int:
    """Print the recomputed curve readings beside the recorded ones; write nothing by default."""
    rows = recompute_records(Path(args.root))
    mismatched = [row for row in rows
                  if any(row[new] != row[old] for new, old in RECOMPUTE_FIELDS)]
    print("| workdir | arm | offered rps | achieved rps (recomputed / recorded) |"
          " lag p99 us (recomputed / recorded) | service p99 us (recomputed / recorded) |"
          " sustained | sustained_primary | service reported |")
    print("|---|---|---|---|---|---|---|---|---|")
    for row in rows:
        print(f"| {row['workdir']} | {row['arm']} | {row['offered_rps']:.0f}"
              f" | {row['achieved_rps']:.2f} / {row['recorded_achieved_rps']}"
              f" | {row['lag_p99_us']} / {row['recorded_lag_p99_us']}"
              f" | {row['service_p99_us']} / {row['recorded_service_p99_us']}"
              f" | {'yes' if row['sustained'] else 'no'}"
              f" | {'yes' if row['sustained_primary'] else 'no'}"
              f" | {row['service_ceiling_reported_windows']} |")
    print(f"{len(rows)} rows re-derived from their window samples;"
          f" {len(mismatched)} differ from the recorded curve")
    if args.out:
        Path(args.out).write_text(json.dumps(rows, sort_keys=True, indent=2) + "\n",
                                  encoding="utf-8")
    return 0 if not mismatched else 1


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
    prep.add_argument("--seed-trace", default=None,
                      help="re-use this frozen seeding trace (hard-linked, digest recorded) instead"
                           " of regenerating one from the manifest")
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
    arm.add_argument("--sqlite-wal-autocheckpoint", type=int,
                     default=SQLITE_WAL_AUTOCHECKPOINT_PAGES,
                     help="sqlite `PRAGMA wal_autocheckpoint` in pages, stated and recorded per run"
                          " (0 defers the checkpoint to the post-run `wal_checkpoint(TRUNCATE)`)")
    arm.add_argument("--skip-verify", action="store_true",
                     help="defer the flashdb read-back to the `verify` step (outside the engine cap)")
    arm.add_argument("--restored", action="store_true",
                     help="the pre-load instance was restored from its snapshot: seed nothing, just"
                          " serve (ticket 82's controlled path)")
    arm.add_argument("--instance-root", default=None,
                     help="directory holding the live instance the arm serves (default: workdir);"
                          " required with --restored")
    arm.add_argument("--database", default=POSTGRES_DATABASE,
                     help="the postgres database the restored instance holds")
    arm.add_argument("--skip-oracle", action="store_true",
                     help="defer the SQL inventory oracle (the restored state is the same in every"
                          " block, so it is verified once per arm and not once per block)")
    arm.set_defaults(func=cmd_arm)

    build = sub.add_parser("build-instance",
                           help="build (seed) the pre-load instance of each arm; serves nothing")
    build.add_argument("--workdir", required=True)
    build.add_argument("--arms", type=_arm_names, default=list(ARM_NAMES))
    build.add_argument("--bins", default=str(REPO_ROOT / "target" / "release"))
    build.add_argument("--instance-root", default=None)
    build.add_argument("--snapshot-dir", default=None,
                       help="copy the instance aside right after building it")
    build.add_argument("--engine-cap-mib", type=int, default=ENGINE_CAP_MIB)
    build.add_argument("--seed-cap-mib", type=int, default=SEED_CAP_MIB)
    build.add_argument("--sqlite-cache-kib", type=int, default=SQLITE_CACHE_KIB)
    build.add_argument("--sqlite-wal-autocheckpoint", type=int,
                       default=SQLITE_WAL_AUTOCHECKPOINT_PAGES)
    build.add_argument("--database", default=POSTGRES_DATABASE)
    build.add_argument("--out", default=None)
    build.set_defaults(func=cmd_build_instance)

    snap = sub.add_parser("snapshot", help="copy each arm's pre-load instance aside (with a digest)")
    snap.add_argument("--workdir", required=True)
    snap.add_argument("--arms", type=_arm_names, default=list(ARM_NAMES))
    snap.add_argument("--snapshot-dir", required=True)
    snap.add_argument("--instance-root", default=None)
    snap.set_defaults(func=cmd_snapshot)

    restore = sub.add_parser("restore",
                             help="put each arm's snapshot back and verify its digest")
    restore.add_argument("--workdir", required=True)
    restore.add_argument("--arms", type=_arm_names, default=list(ARM_NAMES))
    restore.add_argument("--snapshot-dir", required=True)
    restore.add_argument("--instance-root", default=None)
    restore.set_defaults(func=cmd_restore)

    floor = sub.add_parser("floor",
                           help="measure the device floor (idle schedule lag + fsync latency)")
    floor.add_argument("--out", required=True)
    floor.add_argument("--rate", type=int, default=200)
    floor.add_argument("--requests", type=int, default=4_000)
    floor.add_argument("--window-ms", type=int, default=DEFAULT_WINDOW_MS)
    floor.add_argument("--batch", type=int, default=DEFAULT_BATCH)
    floor.set_defaults(func=cmd_floor)

    state = sub.add_parser("machine-state",
                           help="record the machine state at an arm-run boundary")
    state.add_argument("--out", required=True)
    state.add_argument("--phase", required=True)
    state.add_argument("--arm", default=None)
    state.add_argument("--rung", default=None)
    state.set_defaults(func=cmd_machine_state)

    verify = sub.add_parser("verify", help="run a flashdb arm's deferred inventory read-back")
    verify.add_argument("--workdir", required=True)
    verify.add_argument("--arm", required=True)
    verify.add_argument("--bins", default=str(REPO_ROOT / "target" / "release"))
    verify.set_defaults(func=cmd_verify)

    report = sub.add_parser("report", help="write the scoreboard from the arm records")
    report.add_argument("--workdir", required=True)
    report.add_argument("--arms", type=_arm_names, default=None)
    report.set_defaults(func=cmd_report)

    rec = sub.add_parser("recompute",
                         help="re-derive saved curve flags from their window samples (read-only)")
    rec.add_argument("--root", required=True,
                     help="one arm-run workdir, or a tree of b*/rung-*/ run records")
    rec.add_argument("--out", default=None, help="also write the recomputed rows as JSON here")
    rec.set_defaults(func=cmd_recompute)

    args = ap.parse_args(argv)
    try:
        return args.func(args)
    except OracleViolation as violation:
        print(f"ORACLE VIOLATION: {violation}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
