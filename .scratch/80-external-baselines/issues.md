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
<payload>` under `flock /tmp/m1-measure.lock`, one at a time. The postgres arm starts the server in
its own named scope (`m1-pg-postgres-<pid>.scope`) and runs the CLIENT outside it: the cap covers
the SERVER (the engine) only, not the client. This corrects an earlier sentence in this record that
said the scope covered "the server as well as the client"; it never did, and the measurement
record's per-arm memory table below already states the boundary correctly.

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

## Measurement record (WP1.4): the ladder, the memory boundary, and the regime gap

Authored by the measurement worker on `wave/m1-baselines`. No pre-registered threshold was changed,
no engine code was touched, nothing was pushed. The report and its raw JSON are
`.scratch/80-external-baselines/report.md` and `report.json`; the raw run evidence is under
`runs/m1-measure/` (the ladder) and `runs/m1-10x/` (the regime run).

### The measured memory boundary (a method change, recorded here)

The first ladder attempt put the harness AND the engine inside one 128 MiB scope per arm and
OOM-killed the flashdb arms at the pre-registered rung sizes: the instrument (the parsed trace, the
read-back verification) was spending the engine's budget. The boundary is now stated explicitly, in
the shape the published T1-T4 runs already used (`cold_0b` ran `flashdb-load` under the cap and the
Python harness outside it):

| arm | what the 128 MiB cap covers | engine-side memory control |
|---|---|---|
| flashdb-declared / flashdb-learned | `flashdb-load` only, in its own `systemd-run --scope` cgroup; the harness, the pacer and the read-back are outside it | the cgroup cap; the instance is built under a separate 192 MiB construction cap (slice 51's measured convention: seeding peaks ~500 B/doc, serving ~75 MiB) |
| sqlite | the whole arm process, because sqlite's engine is EMBEDDED in it (driver + pacer + engine share the cgroup) | `PRAGMA cache_size = -8192` (8 MiB) states the engine's own page cache; both the pragma and the process cap are recorded per run |
| postgres | the server process tree only, in its own named scope (`m1-pg-postgres-<pid>.scope`); the client stays outside | the cgroup cap plus the disclosed minimal config (`shared_buffers=16MB`, `fsync=on`, `synchronous_commit=on`), echoed from the server's own `SHOW` in every run record |

A consequence worth stating: postgres's data directory moved from `/tmp` (tmpfs, where `fsync` is a
no-op, and where its pages were charged to the client's cgroup) to `/var/tmp/m1-pg` (ext4), so its
durability setting is real and its memory is its own.

### Seeding is setup, not workload

Seeding was rewritten so that it cannot be mistaken for the measured path: dimension rows are
STREAMED (they were materialised as Python lists, which is why a 10^5-row instance could not fit the
cgroup), loaded in ONE transaction per arm, and sqlite's seed runs with
`journal_mode=MEMORY`/`synchronous=OFF` before the arm restores `journal_mode=WAL`/`synchronous=FULL`
for every request it measures. Measured effect: the 220k-row sqlite seed went from >4m30s (one
durable commit per row) to 5.2 s. Postgres seeds in one transaction, which psycopg 3 pipelines.
`seed_s` and the construction scope's own peak are recorded on every arm.

### Verification is verification, not workload

The flashdb read-back (`build_probe_trace` + `flashdb-replay` + `oracle_flashdb`) is now a separate
`verify` step that runs after the measured arm and outside the engine's cgroup, with the same checks
and the same fail-loud behaviour. It was also made streaming on both sides (the probe file is written
as it is generated; the replay's stdout is consumed line by line), and its probes are ordered
movements -> customers -> cells so a per-cell check can be folded and released as its cell arrives.
Memory is now proportional to the trace, not to the dataset. For sqlite/postgres the oracle stays in
the arm (it is a handful of SQL statements) but gains a `(sku, loc)` index on `stock_movement`
created AFTER the replay ends, so the verification's cost is not the product of cells and movements
and cannot lengthen the measured path.

Evidence for the moment this was needed: an in-scope read-back peaked at ~1.4 KB of cgroup memory
per verified cell (45k probes reached 127 MiB of 128 MiB), and the replay runs at 1.7k-10.7k
probes/s, so a 10^7-cell read-back is 16 min-1.6 h of verification per flashdb arm.

### The ladder (harness check) — PROVISIONAL regime

`scripts/m1-ladder-run.sh` runs `50 x [1,2,4,8]` rps, 20 s of offered load per rung, all four arms,
one process at a time under `flock /tmp/m1-measure.lock`, each engine in its own capped scope, with
the machine's load average recorded before and after every rung. The instance is
`customers=30000, skus=30000, locs=4` (120k cells + 30k customers = 150k docs; flashdb 18.6-23.8 MB across rungs,
sqlite 11.8 MB, postgres 21.2 MB). That is 0.09-0.19x of the 128 MiB cap, i.e. NOT the
pre-registered >=10x regime; every number from it is PROVISIONAL and is reported as a harness check,
never as the T5/T6/T7 verdict.

### The regime gap, with the numbers that establish it

The pre-registered floor is data >= 10x the memory budget = 1.28 GiB at the 128 MiB cap. The
committed instrument could not reach it, and the reasons are measured, not argued:

1. In-scope verification cost: ~1.4 KB of cgroup memory per verified cell and 1.7k-10.7k probes/s.
   At 10.8M cells that is 16 min-1.6 h per flashdb arm-run and 2.4 GB of Python dicts if the oracle
   keeps per-cell state (both addressed by the streaming read-back above, the time is not).
2. In-scope trace parse: ~7 KB of Python objects per request, so a long trace is a memory decision
   about the harness (fixed by streaming the trace).
3. Construction memory: `flashdb-scale seed-inventory` peaks ~500 B/doc in the memtable, so the
   dataset must be BUILT under a larger transient cap (192 MiB) than the one it is served under
   (128 MiB) — the same two-cap convention slice 51 published.
4. SQL oracle complexity: the per-cell arithmetic is O(cells x movements) without a `(sku, loc)`
   index (fixed above by the post-hoc verification index).

### Files changed by this work package

| file | change |
|---|---|
| `harness/flashdb_harness/baseline_0b.py` | streamed dimension rows and seeding transaction; `scoped_command` (engine in its own cap); `verify` step; streamed trace and read-back; sqlite `cache_size`; per-arm `data_bytes`, `seed_s`, `seed_peak_bytes`, `engine_cap_mib` |
| `scripts/m1-baseline-run.sh` | per-arm engine boundaries (flashdb engine scope, sqlite process scope, postgres server scope), `--skip-verify`, postgres peak patched from the server's own cgroup |
| `scripts/m1-ladder-run.sh` | the ladder runner (rungs, per-rung trace, load-average boundaries) |
| `scripts/m1-ladder-score.py` | T5/T6/T7, footprint and regime arithmetic from the raw run records; a tri-state T5/T7 `verdict` so a JSON-only reader cannot take a contended rung's throughput pass as PASS |
| `scripts/m1-latency-figure.py` | the M1 comparison figure (`figure.svg`): lag p99 vs offered rate with sustained markers, drawn directly from the per-rung `run-<arm>.json` records |

### The regime run (the pre-registered 1x rung at >=10x)

`runs/m1-10x/rung-50/`: 200,000 customers, 2.65M skus x 4 locs = 10.6M stock cells (10.8M docs),
one frozen trace of 1,000 requests offered at 50 rps (`sha256 859b99a1…`), seed 42, theta 1.0, B=64,
one process at a time, each engine in its own 128 MiB cgroup.

| arm | achieved rps | lag p99 us | service p99 us | rejected windows | sustained | engine cgroup peak | data bytes (x cap) | oracle |
|---|---|---|---|---|---|---|---|---|
| flashdb-declared | 49.95 | 40,901 | 29,230 | 12 (lag_ceiling) | no | 128 MiB (at cap) | 1,683,903,919 (12.55x) | PASS (12 checks) |
| sqlite | 50.00 | 1,969,377 | 2,006,844 | 1 (lag_ceiling) | no | 128 MiB (at cap) | 546,123,232 (4.07x) | PASS (12 checks) |
| postgres | 50.00 | 41,827 | 27,705 | 3 (lag_ceiling) | no | 128 MiB (at cap) | 1,103,779,519 (8.22x) | PASS (12 checks) |

This is the re-run of 2026-10-17 12:30-13:47 through the locked runner (argv, caps and lock in
`spec.json`; load average at every boundary in `loadavg.jsonl`); the numbers above replace the earlier
run's. **The rung was contended: loadavg 6.14 at the flashdb arm's start, and no arm sustained it** —
flashdb on 12 of 20 windows, sqlite on 1 (a 1.97 s host stall), postgres on 3. The earlier run of the
same seed and trace (`8fcf807`, load average not recorded — the provenance defect this cycle fixed)
had flashdb at 1 rejection and both baselines clean. `report.md` §3.1, §6 and §7 carry the comparison
and what it does and does not decide.

- **T5 at the regime: (a) PASS, (b) NOT ROBUSTLY DETERMINED.** flashdb-declared 49.95 rps against the
  slower baseline's 50.00 → deficit 1.001 against the pre-registered 1.5x requirement (2x is the fail
  line), and it reproduces in both runs; but no arm sustained this rung in the committed run while
  both baselines sustained it in the earlier one, so the sustained half of the reading is undecided by
  either run alone and needs a quiet-box repeat. **T7 inherits both halves** (the declared arm is the
  one measured; `flashdb-learned` is the same wiring in M1 and was not run at 10x).
- **T6 at the regime: UNVERIFIED.** It needs a ladder at 10x; measured costs make that impossible in
  this package's 4 h wall-clock cap — flashdb seeding 353.7-416.1 s per arm-run (the seed also
  reached its 512 MiB construction cap), a whole flashdb arm step 51 min 15 s on the committed
  boundaries (12:36:16 → 13:27:31; seed + 20 s measured phase + probe write and read-back), and
  postgres seeding 650.1-686.9 s per arm-run. Eight flashdb arm steps at 10x plus four of each SQL
  arm is 29,276 s ≈ 8 h of measured cost by the committed boundaries, not an opinion.
- **flashdb's read-back at 10x is no longer deferred: it PASSED** in the re-run (12 checks;
  10,600,000 cells read back with `on_hand == initial + sum(delta)` and `on_hand >= 0`, 200,000
  customers, 694 order numbers, 0 request errors), which closes the one acceptance criterion the
  earlier regime run missed. Cost: the flashdb arm step spans 51 min 15 s on the committed
  `loadavg.jsonl` boundaries, of which the recorded seed is 416.133 s and the measured phase 20.007
  s; the read-back's own time is the residue and is not separately recorded — the run record's
  `oracle_s 6.53e-06` is a pre-fix timer artifact, not a measurement (`report.md` §4).

### Verification cost fixes made for this run

The read-back was the run's dominant cost and is now bounded: the SQL oracle's per-cell arithmetic is
one `GROUP BY` aggregate joined to `stock` (a correlated subquery per stock row took tens of minutes at
10.6M cells and was observed live in `pg_stat_activity`), the cell scan is a streamed cursor rather
than `fetchall`, sqlite's bulk load uses `journal_mode=OFF` (a `MEMORY` journal at 10^7 rows is the
whole database in RAM: the first 10x sqlite attempt died with rc=137 at the 128 MiB cap), and flashdb's
verification streams both the probe file and `flashdb-replay`'s stdout. The `(sku, loc)` verification
index is created after the measured replay ends, so it cannot lengthen the measured path.

## Fix record (independent review FAIL → this cycle)

The independent review of the first M1 report (bound to `8fcf807`) returned **FAIL** on three
acceptance claims and one measurement's provenance. This section is the current record of what was
fixed; the report it applies to is `.scratch/80-external-baselines/report.md` and its `report.json`.

### Defects and their fixes

| # | defect | fix | evidence |
|---|---|---|---|
| P1.1 | the SQL window reducer rejected only on `lag_ceiling`/`errors`, while `flashdb-load` also rejects on 3 consecutive lag-growth windows — so the arms were not scored by the same rule, contrary to M1's acceptance | the growth clause (`LAG_GROWTH_WINDOWS`, `WindowSamples.advance_growth`, `saturated`, `lag_saturation` in `reject_reason`) is now part of the reducer, and the per-window `late_requests`/`late_growth_windows`/`saturated` inputs are recorded in `windows-<arm>.jsonl` | `harness/tests/test_baseline_0b.py::test_lag_growth_is_rejected_exactly_where_flashdb_load_rejects_it` binds the reducer to a transcription of the driver's rule window by window; the clause fired on real data (run A rung 200 flashdb-declared: 4 `lag_ceiling` + 2 `lag_saturation`) |
| P1.2 | T6's multiple used flashdb's ACHIEVED rate at 400 rps (a level it did not sustain) against sqlite's sustained rate from a different rung; the dead `sustained_verdict()` was never called | T6 now reports the pre-registered comparison at the top rung (not evaluable there: no baseline sustained 400) AND the sustained-vs-sustained reading (declared 0.50x, learned 1.00x); `sustained_verdict()` is replaced by the used `sustained_level()` | `scripts/m1-ladder-score.py` `t6()`, `report.json` `verdicts[].t6`, report §6 |
| P1.3 | sqlite's WAL/`synchronous=FULL` was asserted from the setter, never observed, although the seeding path runs journal-less and a failed `PRAGMA` is silent | `SqliteArm.observed_durability`/`require_measured_durability` read both pragmas back after the bulk load and fail loudly; run B records `{"journal_mode": "wal", "synchronous": 2}` | three tests (read-back, post-seed restore, failed-restore negative control); `runs/m1-10x/rung-50/run-sqlite.json` `durability_observed` |
| P1.4 | run B's provenance was asserted: no `--seed-cap-mib` in the runner, no argv in `spec.json`, no lock evidence, no load average | the runner takes `--seed-cap-mib`, writes its argv/cwd/caps/lock into `spec.json`, and records the load average at every prepare/arm/report boundary; run B was re-run through it and the records regenerated, never hand-edited | `runs/m1-10x/rung-50/spec.json` `runner`, `loadavg.jsonl` |
| P1.5 | the memory comparison mixed lifecycle points: flashdb's peak was measured-phase, sqlite's and postgres's were scope lifetimes including the bulk load, and `engine_cap_mib` was absent for the SQL arms | the serving peak is measured over the measured phase only — a per-invocation scope for flashdb, a 20 Hz `memory.current` sampler for sqlite and postgres — with the arm-scope lifetime peak and the construction peak recorded separately; `engine_cap_mib` on every arm | `report.md` §5, `run-<arm>.json` `engine_memory` / `arm_scope_peak_bytes` / `construction_memory` |
| P2 | per-arm group size at 400 unattributed; four numbers with no source or a wrong value; `oracle_flashdb` had no test and no negative control; `issues.md` and the runner's header contradicted `report.md` about the postgres scope; the budget was quoted as 3 h here and 4 h there; the "synchronous SQL pacer is the main reason" claim was unsupported | per-arm group sizes tabled at 400 with the equal-durability statement (postgres 11.19 vs 1.04-1.37); every quoted number re-derived from the records; 8 parametrised negative controls for `oracle_flashdb` on a synthetic replay; both scope statements corrected (the cap covers the SERVER, the client is outside); the budget is 4 h everywhere; the causal claim softened to what the run shows | report §2, §4, §5; `tests/test_baseline_0b.py`; this file; `scripts/m1-baseline-run.sh` |

The regime rung was re-measured under the locked runner (`report.md` §9 quotes the command) and the
ladder was re-measured as well, because the pre-fix SQL windows cannot be re-scored: they carry no
per-window late-request count, so whether the growth clause would have rejected them is not derivable
from the committed bytes. In the re-measured windows the clause changed no SQL verdict (every SQL
rejection is a `lag_ceiling` one).

### Second cycle (report correction after the re-review, no new measurement)

| defect | fix | where |
|---|---|---|
| **P1:** §5 claimed the serving peak was "measured over the measured phase only — the bulk load is not charged to it", false for both SQL arms at the regime | `start_bytes` / `peak − start` table: both SQL arms entered the measured phase at 99.98% of the cap, so their serving peaks are carried-over bulk-load memory, not measured-phase growth, and sqlite's serving peak is numerically its arm-scope lifetime peak | `report.md` §5 |
| T6's declared shortfall stated as "2x" | declared shortfall is 6x (3.0/0.5); the learned one is 3x (3.0/1.0) | `report.md` §6 T6 |
| a 12:49:34 → 13:27:31 / 37 min 57 s timing no committed record contains | replaced with the committed arm-step boundaries (12:36:16 → 13:27:31 = 51 min 15 s), the recorded `seed_s 416.133` / `elapsed_s 20.007`, and the `oracle_s 6.53e-06` pre-fix timer artifact; the ~8 h ladder cost re-derived from the same boundaries | `report.md` §4, §6, §7; this file |
| "this kernel ignores a write to `memory.peak` — verified" | withdrawn; no committed artefact records that probe | `report.md` §5 |
| `report.json.unverified` carried 4 of §7's 8 items | all 8, mirroring §7 in the same order | `scripts/m1-ladder-score.py`, `report.json` |
| run B's T5/T7 `pass: true` read as PASS in JSON | tri-state `verdict` (`pass` / `fail` / `not-robustly-determined`) per T5 arm and per T7, both runs | `scripts/m1-ladder-score.py`, `report.json` |
| load-average claims beyond the readings ("other tenants throughout", 6.1-8.3, 2.7-8.3) | §3.1 and §7 state the bracketing readings (6.14 at the arm step's start, 3.39 at its end) and that the measured phase's own load is unrecorded; run B's span is 2.7-6.1, run A's 2.2-4.9 | `report.md` §3.1, §7; `report.json` |
| "Durability, observed rather than asserted" overclaimed for flashdb | retitled to the per-arm truth; flashdb's `durability_observed` is `null` and its setting is the declared `--group-requests 64` argv | `report.md` §5 |
| the M1 deliverable named "a comparison table + figure" had no figure | `figure.svg` and its generator, drawn from the committed per-rung records | `scripts/m1-latency-figure.py`, `figure.svg` |
| the declared 100 rps / learned 200 rps pair read as two T6 readings | labelled run-to-run variance of the same declared wiring (2x on the sustained level on this shared box) | `report.md` §6 |

### Still open (named, not hidden)

- **A quiet-box repeat of the regime rung.** The committed run was contended (§3.1 of the report) and
  no arm sustained the rung there; the earlier run had both baselines clean. T5/T7's sustained reading
  is not robustly determined until that repeat exists. The command is in `report.md` §9 and the seed
  and trace are deterministic (sha256 `859b99a1…`).
- **A ladder at 10x** for T6 — unchanged, and still the expensive one (~8 h of measured cost on the
  committed arm-step boundaries).
- **The pre-registered 10-seed aggregation** — unchanged; both runs are one seed.
