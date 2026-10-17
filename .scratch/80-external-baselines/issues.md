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
