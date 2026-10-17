# 80 - external baseline scoreboard (T5-T7), M1 of the roadmap

## Objective

Make the pre-registered external claim evaluable. Deliver one request-level harness that drives
flashdb (`declared`, `learned`) and the external baselines (`sqlite`, `postgres`) through the same
frozen trace at matched durability in the same memory-capped regime, and report T5/T6/T7 with the
conditions named on every number.

Owning milestone: M1 in `docs/roadmap.md`. Authoritative contract:
`.scratch/15-objective-v0/pre-registration.md` §"External baselines and thresholds".

## Environment facts (verified this session)

- Docker daemon is up, but `docker pull` cannot reach Docker Hub (`registry-1.docker.io` dial
  timeout) and no images are local. Chinese mirrors (`mirrors.aliyun.com`, `mirrors.tuna.tsinghua.edu.cn`)
  are reachable over HTTP; PyPI/GitHub/Docker Hub are not.
- Consequence: run postgres and sqlite **natively**, installed from the aliyun apt mirror, instead of
  in containers. This is a method change below product design, recorded here. Memory capping is by
  `systemd-run --user --scope -p MemoryMax=...`, the same mechanism the flashdb runs already use.
- `psycopg` 3.3.6 (binary) is present in the uv cache. `python3-lmdb`, `librocksdb-dev`, `liblmdb-dev`
  are candidates in the apt mirror for a later slice; M1's threshold gate names only sqlite/postgres.

## Scope

In scope:
- Install and smoke-test `postgresql` and `sqlite3` from the aliyun apt mirror (no host-wide tuning).
- A Python request-level baseline harness under `harness/flashdb_harness/` that reuses the existing
  deterministic generator dialect and the existing offered-load pacing/window/latency rule.
- Track adapters for sqlite and postgres that execute the four request shapes with the inventory
  correctness oracle.
- A measurement run at 1x and the highest common offered level, and a T5/T6/T7 report.
- A work record in this ticket, plus a frozen trace manifest with its sha256.

Out of scope:
- lmdb/rocksdb (a later M1 extension after the threshold gate passes), L2 co-location, drift, the
  10-seed matrix, any change to the pre-registered thresholds, any flashdb engine change.
- Docker images (unreachable), paid services, external compute.

## Authoritative inputs

- `harness/flashdb_harness/generator.py` — the four request shapes and the deterministic Rng/Zipf.
- `harness/flashdb_harness/load_profile.py` — offered-load segment manifest and drift injection.
- `harness/flashdb_harness/cold_0b.py` `build_trace` — the JSONL dialect and the pacer contract.
- `engine/src/request.rs` — the adapter's accepted request/block semantics (the oracle's reference).
- `.scratch/15-objective-v0/pre-registration.md` §B1/T5/T6/T7 — the thresholds.
- `.scratch/STATUS.md` §6 — the measurement discipline (one process at a time, flock, cgroup peak).

## Work packages (first level)

| WP | Owner | Deliverable | Acceptance |
|---|---|---|---|
| 1.1 env | worker | postgres + sqlite installed and smoke-tested; exact versions and config recorded | `psql -c 'select 1'` and `sqlite3 ':memory:' 'select 1'` succeed; server data dir is per-run, not host-global |
| 1.2 adapters | worker | sqlite and postgres track adapters executing order_flow / stock_lookup / customer_lookup / movements_scan with matched durability (sqlite WAL + `synchronous=FULL`; postgres `synchronous_commit=on`) | a small trace yields the inventory oracle: `on_hand = on_hand_initial - sum(delta)`, `on_hand >= 0`, no duplicate `order_no`, movement count = committed take blocks |
| 1.3 driver | worker | a runner that reuses the generator trace + offered-load manifest, applies the same window / lag-ceiling / service-ceiling rule, and reports achieved rps per arm and rung | byte-identical trace sha256 across arms; per-window rejection reasons named; one process at a time, `MemoryMax` and `cgroup_peak` recorded |
| 1.4 measure | worker | a run at 1x and the highest common offered level for flashdb declared, sqlite, postgres | raw JSON/MD artefacts committed; T5/T6/T7 computed from the run, conditions stated |
| 1.5 review | reviewer (fresh) | an independent verdict on the harness, the oracle, the equal-durability claim, and the T5/T6/T7 arithmetic | PASS / CANNOT-VERIFY / FAIL with defect evidence; no self-review |

## Acceptance for M1 (the project gate)

- Every external arm runs the same frozen trace (sha256 recorded) and passes the inventory oracle.
- Equal durability is stated per arm and the differences disclosed, not hidden.
- The same latency rule is applied to every arm when computing its sustained level.
- T5, T6, T7 are each reported with the offered levels, the observed rps, the noise/conditions, and a
  verdict; unexecuted criteria are disclosed as unverified, never claimed as passed.
- The independent review is bound to the final commit.

## Budget

- Workers: 4 (WP1.1-WP1.4), depth 1, one writer per worktree/branch.
- Review: 1 fresh reviewer + at most 2 fix cycles.
- No-progress attempts: 2 per work package.
- Wall-clock: 4 hours for the measurement + report; stop and report incomplete at the cap.

## Stop conditions

- apt cannot install postgres/sqlite from the mirror → stop and report; do not switch to an
  unverified source.
- The oracle cannot be made to pass for a baseline → record the defect and the failing trace; do not
  publish a T5/T6/T7 number from a failing arm.
- Any criterion that would require changing a pre-registered threshold → stop and escalate to the
  author; never change it.

## Build record (WP1.1-WP1.3): the request-level baseline harness

Authored by the build worker on `wave/m1-baselines`. Nothing pre-registered was changed, no engine
code was touched, nothing was pushed. Closes WP1.1 (env), WP1.2 (adapters) and WP1.3 (driver) for
the three arms the M1 gate names.

### Deliverables

| file | what it is |
|---|---|
| `harness/flashdb_harness/baseline_0b.py` | the harness: trace+manifest, the SQL arms, the pacer, the flashdb arm, the oracle, the scoreboard |
| `harness/tests/test_baseline_0b.py` | trace determinism, dialect identity, schedule accumulation, the lag/error rule, the oracle PASS and two fail-loudly cases |
| `scripts/baseline-env.sh` | user-space postgres 18.6 from the aliyun apt mirror (download, extract, initdb, config, start/stop), no root, idempotent |
| `scripts/m1-baseline-run.sh` | the one-line run: `flock`, build, prepare once, each arm in its own `MemoryMax` scope, report |
| `.scratch/80-external-baselines/runs/{m1-build,m1-build-else}/` | the committed run evidence (see "Artefacts") |

### Commands actually run (from `/home/z/vibe/flashdb`)

```
scripts/baseline-env.sh setup && scripts/baseline-env.sh start   # M1_PG_ROOT=/tmp/m1-pg
scripts/m1-baseline-run.sh \
    --workdir .scratch/80-external-baselines/runs/m1-build \
    --seed 42 --requests 2000 --rate 50 --customers 200 --skus 500 --locs 4 --batch 64 \
    --cap-mib 128
scripts/m1-baseline-run.sh \
    --workdir .scratch/80-external-baselines/runs/m1-build-else \
    --requests 200 --on-hand 2          # forces the guard's else/backorder branch on every arm
cd harness && uv run --with pytest pytest -q
```

Each arm therefore ran as `systemd-run --user --scope -p MemoryMax=128M -p MemorySwapMax=0 bash
<payload>` under `flock /tmp/m1-measure.lock`, one at a time. The postgres arm starts and stops its
server **inside** the same scope, so the cap covers the server as well as the client.

### The frozen trace

- `sha256 = 7a56a3d6a19b7396ea60a91b669a32349b3d57f8defd96222e6db3151accf3e2`, 2000 requests, one
  segment at 50 rps, 200 customers / 500 sku / 4 loc, seed 42, theta 1.0, `on_hand=1_000_000`.
- The same parameters produce byte-identical bytes: `prepare` re-run into a second directory printed
  the same sha256, and `tests/test_baseline_0b.py` proves both the per-seed identity and that the
  bytes are `generator.Generator.requests()` unchanged.
- Else-branch coverage run: `sha256 = 99afa54ecf2caea95467fa52fcb4463ab07cd8c424d7605e93ca477accc58946`,
  200 requests, same dimensions, `on_hand=2` (the trace does not depend on `on_hand`, only the
  outcome does).

### Durability, one disclosed batch size

`B = 64` for every arm (`cold_0b`'s shipped `--group-requests`), one durable commit per batch:

| arm | durability setting | measured mean group size |
|---|---|---|
| flashdb-declared | `--group-requests 64`: one WAL barrier per group, acked after the barrier | 1.00 |
| sqlite | `journal_mode=WAL`, `synchronous=FULL`, one `COMMIT` per batch | 1.00 |
| postgres | `fsync=on`, `synchronous_commit=on` (config below), one `COMMIT` per batch | 1.00 |

At 50 rps the batch ceiling never binds: every arm formed 2000 groups of one request, so at this
rung the durability is one durable commit per request on **all three** arms and the batch is
disclosed but inert. A later, higher rung must report the ceiling it actually reaches.

Postgres config, verbatim (written by `scripts/baseline-env.sh`, from `env.md`): `shared_buffers =
16MB`, `max_connections = 32`, `work_mem = 4MB`, `maintenance_work_mem = 16MB`, `fsync = on`,
`synchronous_commit = on`, `wal_level = minimal`, `max_wal_senders = 0`, `unix_socket_directories =
'/tmp/m1-pg/socket'`, `port = 55432`, `listen_addresses = ''`. It is chosen so a shared-buffer-sized
engine fits the same 128 MiB cgroup as flashdb; it is minimal, not tuned, and is not postgres's
default. The run record echoes the server's own `SHOW` for `fsync`, `synchronous_commit` and
`shared_buffers` (`server_version = 18.6 (Ubuntu 18.6-3build2)`).

### Oracle result per arm (2,000-request trace)

All three arms PASS. The oracle fails loudly at the first violation (non-zero exit, the offending
cell/order named) and no violation occurred.

| arm | oracle | what was checked |
|---|---|---|
| flashdb-declared | PASS | 2000 cells read back via `stock_lookup`, 200 customers via `customer_lookup` (proves no skipped take), 1366 order numbers via `movements_scan`; `on_hand == initial + sum(delta)` per cell, `on_hand >= 0`, at most one movement per order, every movement's `order_no` matches, movement count == 1366 committed takes, 0 request errors |
| sqlite | PASS | the 12 checks over the database: per-cell arithmetic, `on_hand >= 0`, no duplicate `order_no` across `"order"`/`backorder`, movement count == takes, order count == takes, backorder count == fallbacks, take+backorder+miss == 1366 order_flows, no skipped take, every movement's `-delta`/sku equals its order line, `sum(delta) == -sum(qty)`, `sum(on_hand) == cells*initial - taken qty` |
| postgres | PASS | identical check list to sqlite |

Else-branch run (200 requests, `on_hand=2`), all three arms PASS with 51 takes + 92 backorders = 143
order_flows, so the guard's `else` path and the backorder accounting are exercised end to end, not
merely present.

**Disclosed asymmetry:** the adapter has no `Order`/`Backorder` read shape, so for the flashdb arm
"no duplicate `order_no`" cannot be re-queried. It is enforced at write time (unique on both
entities plus the adapter's cross-entity check) and any refusal would be a request error, which the
oracle requires to be zero; for the SQL arms it is also queried after the fact. This limit is
recorded in the run record (`oracle.order_no_uniqueness`).

### Windows (2,000-request trace, 40 windows of 1 s, official rule: lag p99 <= 10,000 us and no errors)

| arm | achieved rps | achieved/offered | lag p50 / p95 / p99 us | service p50 / p99 us | rejected windows | sustained | cgroup peak |
|---|---|---|---|---|---|---|---|
| flashdb-declared | 49.98 | 99.96% | 114 / 158 / 1766 | 3828 / 11752 | 0 | yes | 40.0 MiB |
| sqlite | 50.00 | 100.0% | 16 / 140 / 1687 | 1585 / 9052 | 0 | yes | 36.6 MiB |
| postgres | 50.00 | 100.0% | 18 / 1014 / 6288 | 3561 / 26211 | 0 | yes | 81.4 MiB |

All three fit the 128 MiB cap with room (peaks are the scope's own `memory.peak`, read from inside
the cgroup). The run records carry the per-window samples in the driver's own dialect
(`windows-<arm>.jsonl`), scored by `cold_0b.summarize_curve` for every arm, so the lag rule is one
implementation rather than three.

### pytest

`cd harness && uv run --with pytest pytest -q` → **153 passed in 16.84s** (7 of them new:
`tests/test_baseline_0b.py`). Coverage gap disclosed: the postgres and flashdb arms need the cluster
and the Rust binaries, so they are exercised by the runner and evidenced above, not by pytest; the
SQL shape logic they share *is* unit-tested through the sqlite arm on an in-memory database,
including both order_flow branches and two fail-loudly cases (a corrupted `on_hand`, a duplicate
order number).

### The invocation the measurement worker should use

```
cd /home/z/vibe/flashdb && scripts/m1-baseline-run.sh \
    --workdir .scratch/80-external-baselines/runs/m1-measure \
    --requests 2000 --rate <rung> --customers 200 --skus 500 --locs 4 --batch 64 --cap-mib 128
```

It holds `/tmp/m1-measure.lock` for the whole run, builds the binaries, emits the trace once,
re-hashes it per arm, runs each arm in its own capped scope and writes
`manifest.json`, `spec.json`, `windows-<arm>.jsonl`, `run-<arm>.json` and `scoreboard.md`. Add
`--arms sqlite,postgres` to skip flashdb, or `--skip-build` when `target/release` is already
current. `.scratch/80-external-baselines/runs/.gitignore` keeps the regenerable state (trace,
engines, sqlite file, driver logs) out of git.

### Environment facts this build added

- `psycopg` must be pinned: `uv run --with 'psycopg[binary]==3.3.6'`. Plain `psycopg` is the
  pure-Python build and cannot find `libpq` (it is only in the extracted prefix, and
  `ctypes.util.find_library` does not consult `LD_LIBRARY_PATH`); `UV_PRERELEASE=allow` also makes an
  unpinned solve pick `3.0b1`. `psycopg[binary]==3.3.6` is in the uv cache.
- `crates.io` and `static.crates.io` are unreachable, so `cargo build --offline` cannot resolve
  `serde`. The runner therefore builds with the aliyun crates mirror by flags
  (`--config source.crates-io.replace-with=...`, `sparse+https://mirrors.aliyun.com/crates.io-index/`),
  touching no user or host cargo config. `scripts/baseline-env.sh` likewise downloads through the
  machine's already-configured aliyun apt mirror.

### Not done here (later work packages)

- No ladder, no T5/T6/T7 verdict: this run proves correctness and the pacer at one rung (50 rps).
- `flashdb-learned` is accepted as an arm name and runs the same declared-default wiring (M1 wires no
  learner controller); it was not run.
- The SQL pacers execute a batch synchronously (the commit completes before the next dispatch);
  `flashdb-load --group-requests` submits its barrier to a background worker and can overlap. At 50
  rps with one request per group this is off the critical path, but it is a real difference and a
  higher rung must state it.
- First observation for the measurement phase: postgres's lag p99 already reaches ~1.5-6 ms at 50 rps
  (service p99 9-26 ms across runs), so postgres is likely to be the first arm rejected by the 10 ms
  lag rule at a higher rung. That is a property of the disclosed minimal config, not a defect in the
  harness, and it must be reported rather than tuned away.
