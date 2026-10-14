# 52 - program dispatch record: wave1b resume plan

## Dispatch record

- Timestamp: 2026-10-13T18:02:21+08:00
- Mode: Automatic Execution Mode, entered on the user's explicit instruction "按照要求自动推进" received this session.
- Milestone: the pre-registered falsification evaluation of the thesis (metrics T1/T2/T4 plus T3 and the primary sustained-load metric) on a >=10x data:memory instance; per STATUS section 9.
- Action: dispatched the staged resume plan as workflow script `.scratch/52-program/wave1b.js` (a revision of `wave1.js`), base master `93740b6`.
- Isolation: five pre-created git worktrees/branches, one per lane:
  - `wave1/instr` -> `/home/z/vibe/worktrees/flashdb/wave1-instr` (per-window latency instrumentation; ticket 48)
  - `wave1/coloc` -> `/home/z/vibe/worktrees/flashdb/wave1-coloc` (family co-location key encoding; ticket 50)
  - `wave1/tenx` -> `/home/z/vibe/worktrees/flashdb/wave1-tenx` (>=10x cold instance; ticket 51)
  - `wave1/decisions` -> `/home/z/vibe/worktrees/flashdb/wave1-decisions` (adopted decision records)
  - `wave1/tails` -> `/home/z/vibe/worktrees/flashdb/wave1-tails` (latency-tail attribution; ticket 49)
- Staging rationale: the four lanes above run in parallel; the timing-sensitive tail-attribution lane runs alone after them so concurrent builds do not contaminate its latency measurements.
- Integration plan: after the lanes settle, one integrator worker merges the five branches into master and runs the combined gates; then fresh independent reviewers cover each package and the integrated revision, per the project's Proof and review rules.
- Bounds: workflow timeout 4 hours; each lane instructed to commit incrementally and state unfinished work.

## Integration record: wave1b merge and combined gates

- Timestamp: 2026-10-13T18:51:02+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `7fc453a` (one-file program-ticket commit on top of base `93740b6`)

Merge commits (in order, `git merge --no-ff`, all clean, no conflicts):
- `wave1/instr` -> `78ae0ae` (per-window latency instrumentation; ticket 48)
- `wave1/coloc` -> `a2ec47c` (family co-location key encoding; ticket 50)
- `wave1/tenx` -> `0965b20` (>=10x cold instance; ticket 51)
- `wave1/decisions` -> `e02b802` (adopted 2026-10-09 decision records)
- `wave1/tails` -> `601c2b4` (latency-tail attribution; ticket 49)

Control scripts commit:
- `bef4f14` "52: wave1b orchestration control scripts" (adds `.scratch/52-program/wave1b.js`, `.scratch/52-program/wave1-integrate-review.js`)

Conflict resolution: none. The five branch file sets are disjoint and merged cleanly with the ort strategy.

Integrated revision hash (code under gate): `bef4f14847e2735d08c1c875a262e0e6c6ff1188` (HEAD after merges + control-scripts commit at integration)
Final HEAD hash at review recording: `38e924253a709a67aa1d41b098b946e0d1d1ccf0` (later than the integrated revision)

Combined gates (run at integrated revision):

1. `cargo build --workspace --offline` — finished with 0 warnings.
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.98s
```

2. `cargo test --workspace --offline` — all test result lines:
```
test result: ok. 117 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.90s   (src/lib.rs)
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s    (src/bin/flashdb-load.rs)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s     (src/bin/flashdb-replay.rs)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s     (src/bin/flashdb-scale.rs)
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s     (tests/batch_commit.rs)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s     (tests/constraints.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s     (tests/family_colocation.rs)
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.22s     (tests/group_commit.rs)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s     (tests/manifest_engine.rs)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s     (tests/merge_threshold.rs)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s    (tests/metrics.rs)
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.60s   (tests/multi_tile.rs)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s     (tests/order_flow.rs)
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.33s     (tests/partitioned_publish_cap.rs)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.08s     (tests/read_requests.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s     (tests/region_compaction.rs)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s     (tests/region_partition.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s     (tests/replay_trace.rs)
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s     (tests/request_commit.rs)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.25s     (tests/scale_bin.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.26s     (tests/tile_format.rs)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s     (tests/tile_layout.rs)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.54s    (tests/tile_read.rs)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s     (src/main.rs)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s     (Doc-tests flashdb_engine)
```
Total: 272 passed; 0 failed; 0 ignored.

3. `cd harness && uv run --with pytest pytest -q` — summary line:
```
132 passed in 7.67s
```
(uv emitted a non-failing warning that `VIRTUAL_ENV=/home/z/.venv` differs from the project env path `.venv`; tests still passed.)

4. `git diff --check` — silent (exit 0).

No failures, no conflicts.

## wave1b independent review outcomes

Integrated HEAD: `183a0be140757a5784e196fbff22ca7b7d005044` (`git rev-parse HEAD`, 2026-10-13T18:58:52+08:00).

- **rev-instr** — `VERDICT: CANNOT-VERIFY` — blocking findings: none (no P0/P1 by static inspection; CANNOT-VERIFY because the required pytest gate and cargo test/build gates could not be executed — reviewer had no shell tool).
- **rev-coloc** — `VERDICT: CANNOT-VERIFY` — blocking findings: none (no P0/P1; CANNOT-VERIFY because `cargo test --offline -p flashdb-engine --test family_colocation` and `cargo test --workspace --offline` could not be executed — reviewer had no shell tool).
- **rev-tenx** — `VERDICT: PASS` — blocking findings: none.
- **rev-decisions** — `VERDICT: PASS` — blocking findings: none (finding 1 is a P1 stale reorganisation figure in STATUS.md §3, explicitly labelled non-blocking).
- **rev-tails** — `VERDICT: PASS` — blocking findings: none.
- **rev-integration** — `VERDICT: CANNOT-VERIFY` — blocking findings: 1 (full workspace build + Rust test + Python suite at `183a0be` unexecuted — no shell tool, so the decisive build/test/reproducibility gate is unrun). Non-blocking P2: `52-program/issues.md` "Final HEAD hash" labels `bef4f14` as final while the true final HEAD is `183a0be`.

## wave1b review corrections applied (2026-10-13T19:06:10+08:00)

| file | change | source |
|---|---|---|
| `.scratch/STATUS.md` | §3 "Whole run" reorganisation line: `299,390,660 B over 63 merges` → `317,915,511 B over 64 merges` | `.scratch/47-latency-trusted-curve/curve.json` (commit `9bae6fe`) summary `compact_bytes_total: 317915511`, `compact_merges: 64` |
| `docs/glossary.md` | "External baseline" parenthetical: `(sqlite, postgres)` → `(sqlite, postgres, lmdb, rocksdb)` | `.scratch/15-objective-v0/pre-registration.md` B1 (adopted 2026-10-09) |
| `.scratch/52-program/issues.md` | integration record: reworded the stale "Final HEAD hash … `bef4f14`" line to distinguish the integrated revision (code under gate) `bef4f14` from the then-current HEAD at review recording `38e9242` | `git rev-parse HEAD` = `38e924253a709a67aa1d41b098b946e0d1d1ccf0` |
| `.scratch/49-latency-tail-attribution/issues.md` | (a) `--compact-bytes 0 -> omit` source citation: `cold_0b.py:846` → `cold_0b.py:883-886`; (b) overstated bound `(4.2x-28x)`/`>= 4.2x` → `(4.17x-28x)`/`>= 4.17x` (no measured number changed) | (a) `harness/flashdb_harness/cold_0b.py` lines 883-886; (b) ticket's rung-400 example 590,578/141,402 = 4.18x |
| `.scratch/52-program/wave1-verify-fix.js` | added (untracked control script) via `git add` | task instruction |

## wave2 WP-B dispatch record

- Timestamp: 2026-10-13 (resumed after the 30-minute wall; commits ae1df85).
- Action: dispatched work package B "make load-path compaction latency-safe" to the
  single-writer `wave2/tailfix` worktree. Contract, diagnosis, decision, change, tests,
  gates, before/after table and verdict are in `.scratch/53-compaction-tail-latency/issues.md`.
- Result: commits `a59f703` (off-path compaction build + lazy apply) and `ae1df85`
  (read+write WAL handle). 274 Rust tests and 132 Python tests green; 0 build warnings;
  `git diff --check` silent. Acceptance: the 3x rung-200 svc-p99 threshold is met
  (11.0x below the ticket-49 baseline); the "no window rejected" rung is NOT met because
  the ticket-49 un-attributed residual (cold read page-ins / allocator / OS jitter) still
  rejects windows with compaction off the path — reported, not overstated.

## Integration record: wave2/tailfix

- Timestamp: 2026-10-13T20:26:16+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `e11bf6ceb2d5a4b92e2fb9be5701f05093d76500`
- Merge: `git merge --no-ff wave2/tailfix` (off-request-path compaction fix; independently verified PASS at `e9e2ae5`)
  - Merge commit: `112f96a90418298e176a28bfe60e21075ce737eb`
  - Parents: `e11bf6ceb2d5a4b92e2fb9be5701f05093d76500` + `e9e2ae59d8e631f10deab991eca25ba3233c495f`
  - Conflict resolution: none (clean ort merge; no conflicts).
- Final HEAD after merge: `112f96a90418298e176a28bfe60e21075ce737eb`
- Next lane worktree: `git worktree add -b wave3/residual /home/z/vibe/worktrees/flashdb/wave3-residual master`
  - Worktree path: `/home/z/vibe/worktrees/flashdb/wave3-residual`
  - Branch: `wave3/residual`
  - HEAD: `112f96a90418298e176a28bfe60e21075ce737eb`

Combined gates (run at merged master `112f96a`):

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.56s
```

2. `cargo test --workspace --offline` — totals: 277 passed; 0 failed; 0 ignored.
(117 lib + 18 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 8 group_commit + 6 manifest_engine + 7 merge_threshold +
14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
132 passed in 4.77s
```
(uv emitted a non-failing warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv`; tests still passed.)

4. `git diff --check` — silent (exit 0).

## wave3 WP-C dispatch record

- Timestamp: 2026-10-13T20:51:50+08:00.
- Action: dispatched work package C "separate and remove the residual latency tail" to the
  single-writer `wave3/residual` worktree. Contract, diagnosis, decision, tests, gates,
  before/after table and verdict are in `.scratch/54-residual-tail/issues.md`.
- Result: commits `c36b9c3` (per-window proc/io + trim_heap instrumentation) and `6ec1ea3`
  (group-barrier fdatasync timing + tests). Diagnosis: the ticket-49 residual is dominated by the
  per-group WAL `fdatasync` (6.7 ms p50 / 13.4 ms p99 on this device, 119–654 ms/window), NOT cold
  page-ins (read-only 2.7–4.3 ms; warm read_bytes=0 still 9.3–21.7 ms), NOT allocator (≤ 5 ms) and
  NOT scheduler jitter. It is the equal-durability barrier, irreducible without weakening
  durability, so no latency fix was applied: the ticket states the smallest honest blocker and
  proposes (not implements) how the pre-registration can treat the 10 ms service ceiling.
  279 Rust tests and 132 Python tests green; 0 build warnings; `git diff --check` silent.

## Integration record: wave3/residual

- Timestamp: 2026-10-13T21:16:00+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `af86df186d6c49a4e5c3c8816a85b48c43e19d62`
- Merge: `git merge --no-ff wave3/residual` (residual-tail attribution + honesty repair; independently verified PASS at `55c26ae`)
  - Merge commit: `5fbc229e1f1e7a65d175ce10a2c4eff6ecd9e30a`
  - Parents: `af86df186d6c49a4e5c3c8816a85b48c43e19d62` + `55c26aeccef23c8c2da6b2e8fe97be0c81779ef5`
  - Conflict resolution: one content conflict in `.scratch/52-program/issues.md` (both branches appended a section after the same line). Resolved by keeping both `Integration record: wave2/tailfix` and `wave3 WP-C dispatch record` in chronological order; no content dropped.
- Control scripts committed separately: `6cc177a` "52: wave2/wave3 orchestration control scripts" (3 untracked *.js under `.scratch/52-program/`).
- Final HEAD before this record: `6cc177abca879dc70370b0b3d1eba00d0777dbe3`
- Next lane worktree: `git worktree add -b wave4/lag /home/z/vibe/worktrees/flashdb/wave4-lag master`
  - Worktree path: `/home/z/vibe/worktrees/flashdb/wave4-lag`
  - Branch: `wave4/lag`
  - HEAD: `6cc177abca879dc70370b0b3d1eba00d0777dbe3`

Combined gates (run at merged master `6cc177a`):

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.60s
```

2. `cargo test --workspace --offline` — totals: 279 passed; 0 failed; 0 ignored.
(117 lib + 19 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 9 group_commit + 6 manifest_engine + 7 merge_threshold +
14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 5.57s
```

4. `git diff --check` — silent (exit 0).

## wave4 WP-D dispatch record

- Timestamp: 2026-10-13T21:45:00+08:00.
- Action: dispatched work package D "attribute and remove the primary-metric (lag-ceiling)
  stalls" to the single-writer `wave4/lag` worktree. Contract, diagnosis, decision, tests,
  gates, before/after table and verdict are in `.scratch/55-lag-stall-attribution/issues.md`.
- Result: commit `ed51975` (per-group phase + OS-view instrumentation + `--dry-run` host-stall
  control; no latency fix). Diagnosis with per-group numbers: the ~hundreds-of-ms single-request
  stall is a cascade — one write group's WAL `fdatasync` tails to 71-498 ms on the shared device
  (present with compaction AND WAL maintenance fully off, e.g. c51-writes-only 9/40 lag-rej at
  rung 50), and the synchronous single-writer pacer blocks behind it, so the NEXT group is issued
  that late and breaches the lag ceiling. The compaction worker's concurrent tile I/O is a
  secondary, run-dependent slow-read source (cold44 9/40 -> 1/40 with it off) that does not
  reproduce on the 12.25x confirmation. No honest engine/driver change removes the device fsync
  tail without weakening durability, so no latency fix was applied: the ticket states the bounded
  honest blocker and proposals. 280 Rust tests and 132 Python tests green; 0 build warnings;
  `git diff --check` silent.

## Integration record: wave4/lag

- Timestamp: 2026-10-13T21:58:05+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `8766c59c2a3f0259b96ea92190e45b593457bdc6`
- Merge: `git merge --no-ff wave4/lag` (observational lag-stall instrumentation, no engine latency change; independently verified PASS at `9cc224f`)
  - Merge commit: `e36704ff1f36ac2ac7d491e77cf6b7421333b69e`
  - Parents: `8766c59c2a3f0259b96ea92190e45b593457bdc6` + `9cc224f8e8990f8f1130139063855ce2e1aaf7bf`
  - Conflict resolution: one content conflict in `.scratch/52-program/issues.md` (both branches appended a section after the same anchor line). Resolved by keeping both `Integration record: wave3/residual` (master) and `wave4 WP-D dispatch record` (wave4/lag) in chronological order; no content dropped.
- Final HEAD after merge (integrated revision under gate): `e36704ff1f36ac2ac7d491e77cf6b7421333b69e`
- Next lane worktree: `git worktree add -b wave5/overlap /home/z/vibe/worktrees/flashdb/wave5-overlap master`
  - Worktree path: `/home/z/vibe/worktrees/flashdb/wave5-overlap`
  - Branch: `wave5/overlap`
  - HEAD: `e36704ff1f36ac2ac7d491e77cf6b7421333b69e`

Combined gates (run at merged master `e36704f`):

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.95s
```

2. `cargo test --workspace --offline` — totals: 280 passed; 0 failed; 0 ignored.
(117 lib + 20 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 9 group_commit + 6 manifest_engine + 7 merge_threshold +
14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 4.19s
```

4. `git diff --check` — silent (exit 0).

## wave5 WP-E dispatch record

- Timestamp: 2026-10-13T22:42:00+08:00.
- Action: dispatched work package E "overlapped durability-preserving WAL barrier" to the
  single-writer `wave5/overlap` worktree. Contract, design, durability argument, tests, gates,
  before/after table and verdict are in `.scratch/56-overlapped-barrier/issues.md`.
- Result: commits `083da49` (engine stage 1) and `413176c` (load driver stage 2). The overlapped
  barrier is durability-preserving (ack only after the covering `fdatasync` returns; a sync
  failure fails the group closed and poisons the WAL) and removes the fsync-tail cascade that
  ticket 55 named as the rung-50 blocker: `late_total` falls 649->299 (cold44) and 1,497->616
  (c51), and per-group proof shows a 335 ms `fdatasync` now dispatching with 56 us of lag.
  Throughput (106 rps) and write amp (identical compact bytes / 38 merges) are unchanged. Rung 50
  still does NOT meet the lag ceiling (5/40 cold44, 2/40 c51) because two residual mechanisms
  remain, neither the fsync tail: synchronous cold point reads on the request path (order-flow
  cust/stock probes, dominant) and the window-boundary barrier drain (secondary). Honest FAIL on
  that criterion; the primary blocker is a new, separately-scoped mechanism.

## Integration record: wave5/overlap

- Timestamp: 2026-10-13T22:55:08+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `56faf205380526f2ac6ec31942cd7ca94051721a`
- Merge: `git merge --no-ff wave5/overlap` (durability-preserving overlapped WAL barrier; independently verified PASS at `1cc629a`)
  - Merge commit: `539de6e9fa79eea2fee4cec380f6306974660baf`
  - Parents: `56faf205380526f2ac6ec31942cd7ca94051721a` + `1cc629a2c20b1e967dc69f9f360aa7764d4dda9b`
  - Conflict resolution: one content conflict in `.scratch/52-program/issues.md` (both branches appended a section after `## wave4 WP-D dispatch record`). Resolved by keeping both `Integration record: wave4/lag` (master) and `wave5 WP-E dispatch record` (wave5/overlap) in chronological order; no content dropped.
- Final HEAD after merge (integrated revision under gate): `539de6e9fa79eea2fee4cec380f6306974660baf`
- Next lane worktree: `git worktree add -b wave6/reads /home/z/vibe/worktrees/flashdb/wave6-reads master`
  - Worktree path: `/home/z/vibe/worktrees/flashdb/wave6-reads`
  - Branch: `wave6/reads`
  - HEAD: `539de6e9fa79eea2fee4cec380f6306974660baf`

Combined gates (run at merged master `539de6e`):

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.59s
```

2. `cargo test --workspace --offline` — totals: 284 passed; 0 failed; 0 ignored.
(119 lib + 20 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 4.20s
```

4. `git diff --check` — silent (exit 0).

## wave6 WP-F dispatch record

- Timestamp: 2026-10-13T23:40:00+08:00.
- Action: dispatched work package F "non-blocking window boundary + cold-read optimization +
  entity-first co-location" to the single-writer `wave6/reads` worktree. Contract, design, tests,
  gates, before/after table and verdict are in `.scratch/57-cold-reads-and-colocation/issues.md`.
- Result: commits `8cf9988` (engine fixes) + `ec47e25`/`76231d2` (non-blocking boundary) +
  `44205d1` (newest-first get) + `5d90b93` (key-encoding selector). The window-boundary barrier
  drain is now non-blocking and contributes **zero** lag breaches (`boundary=0` on all runs); the
  two WP-E engine bugs are fixed (cap-reached leaves the group open for retry; end_group refuses
  while a barrier is in flight). Rung 50 STILL does NOT meet the lag ceiling (cold44 4-6/40, c51
  4/40): the remaining cause is read-attributed cold point reads under shared-device durability
  I/O. Newest-first `get()` wins only -2.5% request-path bytes; entity-first co-location wins
  -16.6% reads (4.03→3.36 blocks/req) but regresses compaction write amp ~12x (22→265 MB), so its
  rung-50 tail is worse (15/40). Honest FAIL on the primary criterion; pre-registration ceiling
  reconciliation left as the flagged OPEN AUTHOR ITEM.

## Integration record: wave6/reads

- Timestamp: 2026-10-13T23:54:50+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `adba22785cc621ba65d562574b7a1068ab027655`
- Merge: `git merge --no-ff wave6/reads` (non-blocking boundary drain, newest-first read, key-encoding selector + entity-first co-location arm; independently verified PASS at `17de3a3`)
  - Merge commit: `e3a4804c928e229d4b90b5a4207595a4dbc5fa60`
  - Parents: `adba22785cc621ba65d562574b7a1068ab027655` + `17de3a3db6e4f7af9778dfeaa12bba4400aea76a`
  - Conflict resolution: one content conflict in `.scratch/52-program/issues.md` (both branches appended a section after `## wave5 WP-E dispatch record`). Resolved by keeping both `Integration record: wave5/overlap` (master) and `wave6 WP-F dispatch record` (wave6/reads) in chronological order; no content dropped.
- Final HEAD after merge (integrated revision under gate): `e3a4804c928e229d4b90b5a4207595a4dbc5fa60`

Combined gates (run at merged master `e3a4804`):

1. `source $HOME/.cargo/env; cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.90s
```

2. `cargo test --workspace --offline` — totals: 289 passed; 0 failed; 0 ignored.
(122 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 6.19s
```

4. `git diff --check` — silent (exit 0).

## SSOT refresh record (waves 2-6)

- Timestamp: 2026-10-14T00:01:33+08:00
- Actor: SSOT (current-state) writer worker.
- Scope: refresh the current-state SSOT after waves 2-6. Changed files and the source ticket for each change:

| file | change | source ticket |
|---|---|---|
| `.scratch/STATUS.md` | rewrote to current facts: test counts 289 Rust / 132 Python; off-request-path compaction (`a59f703`+`ae1df85`, repair `f4539e4`); overlapped durability-preserving WAL barrier (`083da49`+`413176c`, `MAX_PENDING_BARRIERS=64`, ack-after-covering-fdatasync, fail-closed); non-blocking window-boundary drain (`ec47e25`+`76231d2`, `boundary=0`); rung-50 lag-rej cold44 6/40, c51 4/40, ef 15/40; device fsync 6.7/13.4 ms idle, 71-498 ms under load, reads 4.03 blocks/req; entity-first co-location −16.6% reads (4.03→3.36) but ~12x write amp (21 MB→265 MB) and worse tail; whole-run reorganisation 20.9-23.3 MB over 26-27 merges (was ~318 MB over 64); added the OPEN primary-vs-service ceiling reconciliation to §7 with its three options; removed the stale delegation note and compressed the §6 defect changelog | 53, 54, 55, 56, 57 |
| `docs/engine.md` | foreground-commit section now states the overlapped barrier: append+apply before the covering `fdatasync`, ack only after it returns, memtable visible before durable, fail-closed, `MAX_PENDING_BARRIERS=64`; removed the stale "memtable applied after fsync" and "2 ms / 128 blocks flush" lines | 56 (design), 57 (cap/boundary fixes) |
| `docs/objective.md` | latency statement now marks the lag-only primary as the adopted §7.1 decision and flags the open reconciliation with `pre-registration.md`'s either-ceiling validity rule | 54, 55, 56, 57 (residual proposals + open item) |

- Not changed: `.scratch/15-objective-v0/pre-registration.md` (author-owned) and tickets 53-57 (verification records).

## wave7 WP-G dispatch record

- Timestamp: 2026-10-14T00:53:00+08:00.
- Action: dispatched work package G "take the request-path reads off the pacer" to the
  single-writer `wave7/readsoverlap` worktree. Contract, diagnosis, rejected alternatives, design,
  tests, gates, before/after table and verdict are in `.scratch/58-overlapped-reads/issues.md`.
- Result: commits `94c3027` (checkpoint) + `67c541f` (read-ahead tile warming) + `009ce3d`
  (pace the warm to avoid a startup read burst). A background
  read-ahead worker pre-reads the tile blocks for the next 64 requests (paced 4 ms/request), so the
  pacer's authoritative reads are page-cache hits and the cold page-in happens off the pacer;
  durability/ordering/read correctness are untouched (warming only). 291 Rust tests and 132 Python
  tests green; 0 build warnings; `git diff --check` silent. Rung 50 is sustained by the lag ceiling
  on c51 (0/40) and cold44 run B (0/40); cold44 run A is 1/40 with the single breach attributed to
  the write-path WAL append under a device fsync stall (not a read). The primary-vs-service ceiling
  reconciliation remains the OPEN AUTHOR ITEM.

## Integration record: wave7/readsoverlap

- Timestamp: 2026-10-14T00:56:33+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `5b4b78d85de0ee74b66c7cc467af7a40b91d22e4`
- Merge: `git merge --no-ff wave7/readsoverlap` (read-ahead tile warming takes cold point reads off the pacer; WP-G independently verified PASS at `22eef3e`)
  - Merge commit: `908c1eb597d42b7a9db6d7113990280d1d8a310e`
  - Parents: `5b4b78d85de0ee74b66c7cc467af7a40b91d22e4` + `22eef3e37923807f8f584c469d2c78bb2d4278a0`
  - Conflict resolution: none. The branch's file set is disjoint from master's current tree and merged cleanly with the ort strategy.
- Integration cleanup (minimal, behavior-preserving): the merged branch's test module carried an unused `use super::*;` import (`engine/src/readahead.rs:183`) that `cargo test` flagged on a fresh lib-test compile. Removed that one line in commit `49285d3a043376ed3cbfebd92d98a882217c042b` ("52: drop unused readahead test import (0-warning cargo test)") so `cargo test` recompiles with 0 warnings. No behavior change; does not disturb WP-G's PASS.
- Final HEAD after integration (this record commit): `git rev-parse HEAD` on master (reported to the orchestrator).
- Next lane worktree: `git worktree add -b wave8/eval /home/z/vibe/worktrees/flashdb/wave8-eval-pilot master`
  - Worktree path: `/home/z/vibe/worktrees/flashdb/wave8-eval-pilot`
  - Branch: `wave8/eval`
  - HEAD at creation: `908c1eb597d42b7a9db6d7113990280d1d8a310e` (the merge commit, before the cleanup/record commits)
  - After this record commit, `wave8/eval` was fast-forwarded to integrated master so the pilot is based on the final integrated HEAD (see note below).

Combined gates (run at HEAD `49285d3a043376ed3cbfebd92d98a882217c042b`, i.e. merge + cleanup; 0 warnings throughout):

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```

2. `cargo test --workspace --offline` — totals: **291 passed; 0 failed; 0 ignored**
   (124 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 4.09s
```

4. `git diff --check` — silent (exit 0).

No failures, no conflicts.

## wave8 WP-H dispatch record (eval pilot)

- Timestamp: 2026-10-14T01:40:00+08:00.
- Action: dispatched work package H "T1/T2/T4 evaluation PILOT under the fixed engine" to the
  single-writer `wave8/eval` worktree. Contract, exact commands, per-arm/per-rung tables, both
  judging views, T1-T4 directions with seed counts and noise band, readiness gaps, and the full
  campaign cost estimate are in `.scratch/59-eval-pilot/issues.md` (plan committed `8a3c059` before
  any long run; results and the four JSON/`md` artifacts committed after).
- Result: NOT a 10-seed verdict. 13 arm-runs = 2 access seeds (42/43) per (arm, workload) on the
  12.25x c51 instance, win declared seed-42 repeated. Under heavy external box load (llama-server
  92-95% CPU + a parallel 6-way evaluation, load avg 5-16): (a) the win (T1) workload is
  unmeasurable — no arm sustains rung 50 because the 30% movement-scan share does cold reads on the
  pacer (the read-ahead worker warms only unique-prefix lookups); (b) on adversarial-uniform the
  learned arm LOSES the rung-50 lag ceiling to declared (8-9/40 vs 0/40) while writing 3.6-4.4x
  declared's reorganisation bytes — the T2 failure direction; (c) win T3 ratio 0.19-2.71x across 3
  runs (below 3x but non-reproducible); (d) run-to-run throughput spread 38-71% (win) vs 1%
  (adversarial). The thesis is blocked on the win scan path and a quiet-box 10-seed run, not yet
  derivable.

## Integration record: wave8/eval

- Timestamp: 2026-10-14T01:47:19+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `29d7658e6751d34db3dc0ee169aac10bce669d62`
- Merge: `git merge --no-ff wave8/eval` (WP-H eval-pilot ticket + artifacts; docs only)
  - Merge commit: `59b01bc9f0f1366d2fbe9e0f0e9283c1e2c0b093`
  - Parents: `29d7658e6751d34db3dc0ee169aac10bce669d62` + `d1fa28391d2ad5b8f7f7a75b0d2bbd79009e2cc5`
  - Conflict resolution: none. The branch's file set is disjoint from master's current tree and merged cleanly with the ort strategy.
- Final HEAD after integration (this record commit): `git rev-parse HEAD` on master (reported to the orchestrator).
- Lane worktrees created from integrated master (`59b01bc`):
  - `git worktree add -b wave9/scanwarm /home/z/vibe/worktrees/flashdb/wave9-scanwarm master`
    - Path: `/home/z/vibe/worktrees/flashdb/wave9-scanwarm`, branch `wave9/scanwarm`, HEAD `59b01bc9f0f1366d2fbe9e0f0e9283c1e2c0b093`
  - `git worktree add -b wave9/churn /home/z/vibe/worktrees/flashdb/wave9-churn master`
    - Path: `/home/z/vibe/worktrees/flashdb/wave9-churn`, branch `wave9/churn`, HEAD `59b01bc9f0f1366d2fbe9e0f0e9283c1e2c0b093`

Combined gates (run at merge `59b01bc9f0f1366d2fbe9e0f0e9283c1e2c0b093`; 0 warnings throughout):

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```

2. `cargo test --workspace --offline` — totals: **291 passed; 0 failed; 0 ignored**
   (124 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
132 passed in 4.18s
```

4. `git diff --check` — silent (exit 0).

No failures, no conflicts.

## wave9 WP-I dispatch record (scan read-ahead)

- Timestamp: 2026-10-14T02:08:00+08:00.
- Action: dispatched work package I "make the win workload's SCAN reads part of the read-ahead warm
  set" to the single-writer `wave9/scanwarm` worktree. Contract, diagnosis, rejected alternatives,
  design, change, before/after tables, both judging views and verdict are in
  `.scratch/60-scan-readahead/issues.md`.
- Result: the scan-warm extension is implemented and sound (memoized per-snapshot full-prefix warm of
  `P|StockMovement`, commits `d951522` checkpoint + `6695493` implementation), but it does NOT move
  the win lag ceiling: rung-50 lag-rej is 38/38 before vs 39/40 after (box noise). The measured
  exact remaining cause is that the win scan is CPU-bound on the pacer, not page-in-bound — the run
  has a 98.9% page-cache hit rate before AND after (the scan re-reads the same ~4.8 MB entity every
  ~67 ms and self-warms), while the inline `execute_request` read phase is ~103-117 s over the
  ~117-125 s run (~89-94% of wall time) and unchanged by warming. Removing that inline CPU cost needs
  an `order_no` index (write-path change) or an overlapped scan (breaks the authoritative-reads-stay-
  inline invariant), both out of scope for a read-ahead package and recorded for the next work
  package. No point-lookup regression mechanism (scan warm is inert for adversarial shapes) and no
  read/write amplification blowup (+2.3% device reads). 293 Rust / 132 Python tests green; 0 build
  warnings; `git diff --check` silent.

## wave9 WP-J dispatch record (learner churn fix)

- Timestamp: 2026-10-14T02:41:00+08:00.
- Action: dispatched work package J "stop the learner churning on the adversarial-uniform workload
  (T2)" to the single-writer `wave9/churn` worktree. Contract, quantified diagnosis, design +
  rejected alternatives, change, tests, verbatim gates, before/after tables, both judging views,
  acceptance verdict and residual items are in `.scratch/61-learner-churn/issues.md` (checkpoint
  committed before implementing; the fix and the six final-config run artifacts committed after).
- Change (harness only, no engine code): the learner's cost evidence is now per-request and
  median-scored (load-invariant and spike-robust), and the switch/drift thresholds are raised to
  50% / 200% above the measured noise floor. Result (pilot-grade, 2 seeds + 1 repeat, 12.25x c51,
  shared box load avg 4-5): the pre-registration's disqualifying rung-50 lag churn is gone
  (learned 0-1/40 vs the pre-fix 8-9/40, matching declared), learned's adversarial reorg fell from
  3.6-4.4x to 0.19-0.84x declared on 2 of 3 runs (one 2.40x outlier from a late-rung
  device-saturation drift episode — the exact remaining cause, labelled not-resolved); on win the
  learner still adapts (12/12 regions non-default both seeds, decision count > 0). Gates: 291 Rust
  + 134 Python tests green, cargo build 0 warnings, `git diff --check` silent.

## Integration record: wave9/scanwarm + wave9/churn

- Timestamp: 2026-10-14T02:53:20+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `e0545aa8dc55e7464c4b12bbba09919bd08d0bb7`
- Merge 1: `git merge --no-ff wave9/scanwarm` (WP-I scan read-ahead; engine code + dispatch record)
  - Merge commit: `dc812bf2e6477151cb0decf1df682615a20c3e66`
  - Parents: `e0545aa8dc55e7464c4b12bbba09919bd08d0bb7` + `de22471d2c12f8af6ad988715b7982343705cc70`
  - Conflict resolution: `.scratch/52-program/issues.md` had a content conflict (master's wave8/eval integration record and the branch's wave9 WP-I dispatch record both appended at the same EOF location). Resolved by keeping both sections in chronological order (wave8 record first, then WP-I dispatch record).
- Merge 2: `git merge --no-ff wave9/churn` (WP-J learner-churn fix; harness code + dispatch record + run artifacts)
  - Merge commit: `66e43c133f33ba9d6f5317a690af33ee2a9b58a5`
  - Parents: `dc812bf2e6477151cb0decf1df682615a20c3e66` + `ff6aa8cca66fc02328a42c9c1e07b39d60912aa1`
  - Conflict resolution: `.scratch/52-program/issues.md` again conflicted at the same EOF location (integrated master's two existing sections vs the branch's wave9 WP-J dispatch record). Resolved by appending the WP-J dispatch record after the existing sections.
- Final HEAD after integration: `66e43c133f33ba9d6f5317a690af33ee2a9b58a5`
- Re-evaluation worktree created from integrated master:
  - `git worktree add -b wave10/eval /home/z/vibe/worktrees/flashdb/wave10-eval master`
    - Path: `/home/z/vibe/worktrees/flashdb/wave10-eval`, branch `wave10/eval`, HEAD `66e43c133f33ba9d6f5317a690af33ee2a9b58a5`

Combined gates (run at merge `66e43c133f33ba9d6f5317a690af33ee2a9b58a5`; 0 warnings throughout):

1. `cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.58s
```

2. `cargo test --workspace --offline` — totals: **293 passed; 0 failed; 0 ignored**
   (126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
134 passed in 4.12s
```

4. `git diff --check` — silent (exit 0).

No failures.

## wave10 WP-K dispatch record (T1/T2 re-run after WP-I + WP-J)

- Timestamp: 2026-10-14T03:40:00+08:00.
- Action: dispatched work package K "re-run the T1/T2 evaluation on the integrated engine after the
  WP-I scan-readahead fix and the WP-J learner-churn fix" to the single-writer `wave10/eval`
  worktree (isolated worktree, branch `wave10/eval`, based on integrated master `66e43c1`).
  Contract, exact commands, per-arm/rung tables, both judging views, T1/T2 verdicts with seed
  counts and noise band, residual gaps and cost estimate are in `.scratch/62-eval-rerun/issues.md`;
  plan committed (`c821716d`) before any long run.
- Result (pilot-grade, 3 independent access seeds 42/43/44 + seed 42 repeated, 12.25x c51, shared
  box load avg 4.6-8.8): **T1 still underivable** (no arm lag-sustains rung 50 on win, 39-40/40
  lag-rej — the scan gap is not closed); **T2 rung-50 lag churn fixed** (learned 0-1/40 = declared
  on all 4 runs) but **T2 reorg churn not robustly fixed** (learned/declared ratio 0.13-2.88x,
  median 1.16x; both seed-42 runs exceed 1.5x via a single-region `P|Customer` oscillation); win
  seed44 churns `P|Customer` to 8.37x declared during rung 50 (T3 FAIL). The harness's both-ceilings
  `T2 PASS` is a 0/0 artifact and must not be quoted. Gates: 293 Rust + 134 Python tests green,
  cargo build 0 warnings, `git diff --check` silent.

## Integration record: wave9 lanes + wave10 re-eval

- Timestamp: 2026-10-14T03:49:59+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `e625e00a112c17e6bb63598886c67d92cc212fec` (already contains
  the wave8/eval docs plus both wave9 lane merges)
- Merge: `git merge --no-ff wave10/eval` (WP-K T1/T2 re-run; ticket `.scratch/62-eval-rerun` +
  run artifacts)
  - Merge commit: `bbab411d0481c836f10ab179d318f34eb8d41232`
  - Parents: `e625e00a112c17e6bb63598886c67d92cc212fec` + `9ad38cf63b335097d4b81bb4092c52399c95f040`
  - Conflict resolution: `.scratch/52-program/issues.md` had a content conflict (master's
    "Integration record: wave9/scanwarm + wave9/churn" section and the branch's "wave10 WP-K
    dispatch record" both appended after the same anchor, the wave9 WP-J dispatch record). Resolved
    by keeping both sections in chronological order (wave9 integration record first, then the WP-K
    dispatch record); no content dropped.
- Final HEAD after integration: `bbab411d0481c836f10ab179d318f34eb8d41232`
- SSOT worktree created from integrated master:
  - `git worktree add -b wave11/ssot /home/z/vibe/worktrees/flashdb/wave11-ssot master`
    - Path: `/home/z/vibe/worktrees/flashdb/wave11-ssot`, branch `wave11/ssot`, HEAD
      `bbab411d0481c836f10ab179d318f34eb8d41232`

Combined gates (run at merge `bbab411d0481c836f10ab179d318f34eb8d41232`; 0 warnings throughout):

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s
```

2. `cargo test --workspace --offline` — totals: **293 passed; 0 failed; 0 ignored**
   (126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   14 metrics + 33 multi_tile + 7 order_flow + 1 partitioned_publish_cap + 5 read_requests +
   3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
   3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
134 passed in 4.72s
```

4. `git diff --check` — silent (exit 0).

No failures.

## wave12 WP-M dispatch record (learned secondary access path)

- Timestamp: 2026-10-14T07:28:00+08:00.
- Action: dispatched work package M "add the learned secondary access path keyed on the observed
  `order_no` predicate (`P|StockMovement`) — the axis WP-K found missing" to the single-writer
  `wave12/accesspath` worktree (branch `wave12/accesspath`, based on integrated master `65d8d3b`).
  Contract, Stage-1 deterministic headroom, design, invariants argument, rejected alternatives,
  Stage-2 before/after tables (both judging views) and residuals are in
  `.scratch/64-learned-access-path/issues.md`; Stage-1 plan committed (`3fc776e`) before any build.
- Result: Stage-1 GO (30,000 docs / 4.78 MB per scan -> 1 doc / ~70 KB). Stage-2 implemented
  (engine `index.rs` derived `U|StockMovement.order_no` tile + read-path cross-check fallback;
  driver off-path build/adopt/retire; harness 12-candidate indexed axis for the movement region) and
  committed (`69d48f1`, `4f8d8c2`, `b28e86e`, `64cf940`, `9290822`, `6ef3f56`, `a8570fc`). The
  learner **commits** the index (`P|StockMovement = 4096/200/index_order_no=true`) and
  `static-derived` freezes it too. Win seed-42 ×2 (noise band), cap 128 MiB, lambda 1.0: achieved
  throughput **declared 72-73 -> learned/static 107 rps** and ~100% achieved at rungs 100/200/400
  (vs declared 85%/43%/21%), service p99 at 100+ rps ~4-36 ms -> ~9-197 us. Strict sustained rps
  stays 0 for every arm under both the §7.1 lag-only view (1-4 fdatasync-floor lag breaches per rung
  even with the index, plus rung-50 burn-in churn) and the pre-registered both-ceilings view; the
  index is necessary but not sufficient — the remaining binding constraint is the per-group
  `fdatasync` durability floor, a different (device-bound) axis. Gates: 299 Rust + 138 Python tests
  green, cargo build 0 warnings, `git diff --check` silent. Residuals and the exact cause of the
  remaining 0 are recorded in the ticket.

## Integration record: wave12/accesspath

- Timestamp: 2026-10-14T07:37:21+08:00
- Actor: integrator worker (single writer on master)
- Base at integration start: master `65d8d3baba98fa2312ac760a3163c376b18c39cd`
- Merge: `git merge --no-ff wave12/accesspath` (WP-M learned secondary access path; engine index
  tile + driver off-path build/adopt/retire + harness indexed axis + ticket
  `.scratch/64-learned-access-path` + run artifacts)
  - Merge commit: `2c1c719de9db378b3bfb01a20a95c757325ad30a`
  - Parents: `65d8d3baba98fa2312ac760a3163c376b18c39cd` + `5fdcb2209d1d713dd924e3fd071e10daf0f265c6`
  - Conflict resolution: none (clean `ort` merge; no conflicting files).
- Final HEAD after integration: `2c1c719de9db378b3bfb01a20a95c757325ad30a`
- Evaluation worktree created from integrated master:
  - `git worktree add -b wave13/eval /home/z/vibe/worktrees/flashdb/wave13-eval master`
    - Path: `/home/z/vibe/worktrees/flashdb/wave13-eval`, branch `wave13/eval`, HEAD
      `2c1c719de9db378b3bfb01a20a95c757325ad30a`

Combined gates (run at merge `2c1c719de9db378b3bfb01a20a95c757325ad30a`; 0 warnings throughout):

1. `cargo build --workspace --offline` — 0 warnings:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.02s
```

2. `cargo test --workspace --offline` — totals: **299 passed; 0 failed; 0 ignored**
   (126 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
   3 family_colocation + 11 group_commit + 6 manifest_engine + 7 merge_threshold +
   15 metrics + 33 multi_tile + 7 order_flow + 5 order_no_index + 1 partitioned_publish_cap +
   5 read_requests + 3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit +
   2 scale_bin + 3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale +
   0 main + 0 doc-tests)

3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
138 passed in 4.15s
```

4. `git diff --check` — silent (exit 0).

No failures.

## Dispatch record: wave14/churn2 (WP-O learner burn-in churn)

- Timestamp: 2026-10-14T09:08:00+08:00
- Actor: WP-O worker (single writer, isolated worktree)
- Base: integrated master `04b27b256efb6d6e085523bf705297400ce6208b` (wave12/accesspath merge).
- Worktree: `git worktree add -b wave14/churn2 /home/z/vibe/worktrees/flashdb/wave14-churn2 master`
  (HEAD `04b27b2`, clean at start).
- Task: remove the learner-side rung-50 lag churn (WP-M's `index_order_no` axis toggles/built on a
  0-scan region and its 12-candidate movement burn-in), so learned matches the frozen layout's lag
  while keeping the +40-51% win. Fix = demand-gated, one-shot sticky index axis (harness-side
  `layout_learner.py` only; engine unchanged).
- Ticket + artifacts: `.scratch/66-learner-burnin-churn/` (contract, quantified diagnosis, design +
  rejected alternatives, before/after tables, both judging views, verdict, residual items).
- Final HEAD after the work: `66:` commits on `wave14/churn2` (checkpoint `8eb92e0`, code `3604161`,
  results/ticket update — see the final commit hash in the ticket gates section).
- Headline (pilot, 2 seeds + 1 repeat, 12.25x c51, shared box): win learned rung-50 lag-rej 5/7/3
  vs static-derived 3/3/5 (pre-change 25-29 vs 3-4) with the index committed (1 build, 0 toggles);
  aggregate win throughput declared 68-74 -> learned/static 107 rps (+44.6% to +57.4%); adversarial
  learned builds 0 index (pre-change 3) and rung-50 lag-rej 0/40 = declared; adversarial reorg ratio
  median 1.36x (pre-change 2.51x). Gates: 299 Rust + 140 Python, build 0 warnings, diff --check silent.

## wave14 WP-P dispatch record (request-path write stall)

- Timestamp: 2026-10-14T09:00:00+08:00 (measurement window 08:29-08:59).
- Action: dispatched work package P "remove the request-path device durability stall" to the
  single-writer `wave14/writestall` worktree. Contract, diagnosis, design, rejected alternatives,
  invariants argument, change, tests, gates, before/after tables, both judging views, acceptance
  verdict and residuals are in `.scratch/67-requestpath-write-stall/issues.md` (checkpoint
  `618b760`; implementation `98ba12e` + `16303ef`).
- Result: the grouped WAL append is now deferred onto the existing FIFO barrier worker (append +
  `fdatasync` in one job, `Wal::encode_frame` + `group_frames` + `begin_group_deferred`); the
  pacer never blocks on the device `write_all`, and an ack still requires the covering `fdatasync`
  (tests: `deferred_append_buffers_on_the_pacer_and_lands_on_the_worker`,
  `end_group_refuses_a_deferred_append_group`, strengthened
  `overlapped_barrier_sync_error_fails_closed_and_poisons_the_wal`). Per-group proof: a 129.6 ms
  `fdatasync` now dispatches the NEXT group with 70 us of lag (was a ~54-74 ms cascade); across the
  three win after-runs static-derived sees 12 fsyncs >100 ms and 23 >50 ms with ZERO fsync-attributed
  lag breaches. A supervisor-approved scope extension (same commit set) also warms the WP-M
  `order_no` index tile in the read-ahead view (`tile_read_view` + `request_read_lookups` +
  `OrderNoIndex::tile`), removing the mid-rung cold-index page-ins the write fix exposed.
  Measurement (win, 12.25x c51, seeds 42/43 + 42 repeat, shared box with the concurrent churn2
  lane): static-derived rung-50 lag-rej 2-4/40 -> **2/40** in all three after-runs, and the two
  remaining windows are the read-ahead worker's STARTUP WARM-UP (windows 0-1, `reads=N boundary=0`),
  not the write path and not the durability floor; declared ~39-40/40 (full scan) and learned
  ~25-26/40 (the WP-O churn) are unchanged. Throughput and write amp unchanged. Honest verdict:
  write-path acceptance MET; rung-50 lag-rej=0 NOT reached (residual 2/40 warm-up transient), both
  judging views reported, pre-registration untouched. Gates: 302 Rust + 138 Python tests green,
  cargo build 0 warnings, `git diff --check` silent.

## Integration record: wave14/churn2 + wave14/writestall

- Timestamp: 2026-10-14T09:27:46+08:00
- Actor: integrator (single writer, master)
- Merge 1: `git merge --no-ff wave14/churn2` -> commit `2b5f73941ae99c0830901a9ca9b55e118b3291c0`
  (branch tip `3bd4656169c96ab0ce84dbfa3fb5363234de1174`). No conflict.
- Merge 2: `git merge --no-ff wave14/writestall` -> commit `b7958bcca92f9cedecde72c5da6e47d70709d53d`
  (branch tip `c551ecc869de961f6b6f9e6997798ddab631515f`). One content conflict in the shared
  `.scratch/52-program/issues.md` (both branches appended their dispatch record at the same tail
  anchor after "No failures."). Resolved minimally by keeping both sections in order (churn2 then
  writestall), no content dropped or rewritten.
- Re-evaluation worktree: `git worktree add -b wave15/eval /home/z/vibe/worktrees/flashdb/wave15-eval master`
  (HEAD `b7958bcca92f9cedecde72c5da6e47d70709d53d`).
- Combined gates (master `b7958bc`):
  1. `cargo build --workspace --offline` — Finished, 0 warnings (exit 0).
  2. `cargo test --workspace --offline` — 302 passed; 0 failed (exit 0).
  3. `cd harness && uv run --with pytest pytest -q` — 140 passed (exit 0).
  4. `git diff --check` — silent (exit 0).

## wave15 WP-Q dispatch record (final SS7.1 lag-only evaluation)

- Timestamp: 2026-10-14T09:30:00+08:00 (plan committed) -> 2026-10-14T10:13:00+08:00 (measured;
  final record committed at the hash named in the ticket).
- Action: dispatched work package Q "the final SS7.1 lag-only evaluation after the churn +
  write-stall fixes" to the single-writer `wave15/eval` worktree (integrated master `b7958bc`,
  both WP-O `wave14/churn2` and WP-P `wave14/writestall` merged). Contract, exact commands,
  protocol, both judging views, per-arm/rung tables, T1/T2/T3 verdicts, noise band and residual
  gaps are in `.scratch/68-eval-final/issues.md`; raw artifacts are the committed
  `seed42/43/44/seed42-repeat.{json,md}` in the same directory.
- Scope/result: win (T1) + adversarial (T2), arms declared/learned/static-derived, full ladder
  50/100/200/400 rps, 3 independent seeds (42/43/44) + a 42 repeat, one process at a time under
  `MemoryMax=128MiB`, box load recorded at every boundary (loadavg 3.9-9.1). Headline (pilot,
  4 runs, NOT 10-seed): rung-50 lag-rej=0 is NOT reached under the §7.1 lag-only primary —
  win declared 39-40/40, learned 3-13/40, static-derived 2-3/40 (startup warm-up windows 0-2 +
  a `U|Stock` merge-oscillation blowup on seed42); the +43-57% achieved-throughput layout win is
  preserved (learned/static 107 rps vs declared 68-75) but does not convert to a positive lag-
  sustained score; adversarial learned matches declared (0/40, 0 index builds, reorg ratio median
  1.34x); win reorg ratio median 2.41x with the single-region blowup recurring (75 MB `U|Stock` on
  seed42 vs 0 MB on the same-trace repeat). Gates: 302 Rust + 140 Python tests green, cargo build
  0 warnings, `git diff --check` silent.

## wave16 WP-R dispatch record (startup warm-up)

- Timestamp: 2026-10-14T10:38:55+08:00 (checkpoint committed) -> 2026-10-14T11:19:50+08:00 (measured;
  final record at the hash named in the ticket).
- Action: dispatched work package R1 "eagerly warm the win scan + its index at run start" to the
  single-writer `wave16/startwarm` worktree (branch based on integrated master `e78d036`). Contract,
  diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables,
  both judging views, acceptance verdict and residuals are in `.scratch/69-startup-warm/issues.md`;
  raw artifacts are the committed `seed42/43/44/seed42-repeat.{json,md}` in the same directory.
- Scope/result: win workload, arms declared/learned/static-derived, full ladder 50/100/200/400 rps,
  3 independent seeds (42/43/44) + a 42 repeat, one process at a time under `MemoryMax=128MiB`, box
  load recorded at every boundary (loadavg 5.1-10.2). Headline (pilot, 4 runs, NOT 10-seed): the
  read-ahead STARTUP warm-up is eliminated — static-derived windows 0-2 are clean in all four runs
  (first-3 lag p99 110-4,407 us, all `total=0`) via eager index build+adopt + a bounded synchronous
  `warm_startup` pass (`scan_entries=30000 index_entries=30000` every run); strict rung-50
  lag-rej=0 is reached on seed42-repeat (0/40) but not across all seeds (static-derived 3/2/1/0),
  the residual 1-3/40 being mid-run compaction-adjacent cold reads + device fdatasync tails
  (windows 28-36), not the startup warm-up. Throughput preserved (learned/static ~105-107 rps vs
  declared 69-72, +43-57%). Gates: 303 Rust + 140 Python tests green, cargo build 0 warnings,
  `git diff --check` silent.
- Serialization note (added after the runs): all four WP-R runs were UN-lock-protected (they
  finished before the shared-box `flock /tmp/wave16-measure.lock` rule); the concurrent
  `wave16/oscillation` lane contaminates the lag ceiling, so the WP-R lag numbers are
  direction-only under contention. The deterministic startup-warm evidence is unaffected; the
  decisive serial rung-50 verdict belongs to WP-S after integration.

## wave16 WP-R2 dispatch record (bound the single-region merge oscillation)

- Timestamp: 2026-10-14T10:50:00+08:00 (diagnosis committed) -> 2026-10-14T11:47:00+08:00
  (measured; final record committed at the hash named in the ticket).
- Action: dispatched work package R2 "bound the single-region merge oscillation so one region
  cannot blow up the reorganisation and the lag" to the single-writer `wave16/oscillation`
  worktree (based on integrated master, HEAD `1b13c8b` after the fix). Contract, diagnosis,
  design + rejected alternatives, change, tests, verbatim gates, before/after tables, both
  judging views, acceptance verdict and residuals are in `.scratch/70-merge-oscillation/issues.md`;
  raw artifacts are the committed `seed42/43/44/seed42-repeat.{json,md}` in the same directory.
- Scope/result: re-merge minimum-payback hysteresis in `Engine::select_region_run` (first merge
  keeps the slice-27 absolute-count gate; a region is re-admitted only after `merge_min_tiles`
  NEW tiles since its post-merge floor), one engine file + one new/updated regression test. Win
  (T1) + adversarial (T2), arms declared/learned/static-derived, full ladder 50/100/200/400 rps,
  3 independent seeds (42/43/44) + a 42 repeat, one process at a time under `MemoryMax=128MiB`,
  box load recorded at every boundary (loadavg 4.9-8.7). Headline (pilot, 4 runs, NOT 10-seed):
  the `U|Stock` blowup is bounded and same-trace-consistent (5.23 MB / 1 merge on BOTH seed42 and
  the repeat, vs 75.2 MB / 8 vs 0 MB pre-fix); win T3 learned/declared median 0.39x (0.22-0.92x)
  with no single-run blowup; learned win rung-50 lag-rej 3/6/3/4 vs static-derived 4/5/5/4
  (pre-fix seed42 was 13/40); learned throughput 107 rps on all four runs (no regression).
  Gates: 303 Rust + 140 Python tests green, cargo build 0 warnings, `git diff --check` silent.

## Integration record: wave16/startwarm + wave16/oscillation

- Timestamp: 2026-10-14T12:05:27+08:00 (integration committed).
- Action: single-writer integrator merged `wave16/startwarm` (`--no-ff`, merge commit
  `3e064fb3264330826a47c8cf4f63a7cd3d9c5419`) and `wave16/oscillation` (`--no-ff`, merge commit
  `fa21cef3d5604c7d4c20db11a18976c39b171a34`) into `master`. One conflict: both branches appended
  dispatch records at the end of this file; resolved by keeping both sections in order
  (WP-R startup warm-up, then WP-R2 merge oscillation). Engine code areas were disjoint
  (`wave16/startwarm`: `engine/src/bin/flashdb-load.rs` + `engine/src/readahead.rs`;
  `wave16/oscillation`: `engine/src/engine.rs` + `engine/tests/merge_threshold.rs`), so no engine
  code conflict.
- Final evaluation worktree created: `git worktree add -b wave17/eval
  /home/z/vibe/worktrees/flashdb/wave17-eval master`; HEAD `fa21cef3d5604c7d4c20db11a18976c39b171a34`.
- Combined gates (verbatim):
  - `cargo build --workspace --offline` — Finished, 0 warnings.
  - `cargo test --workspace --offline` — 304 Rust tests passed, 0 failed.
  - `cd harness && uv run --with pytest pytest -q` — 140 passed.
  - `git diff --check` — silent.

## wave17 WP-S dispatch record (the decisive SS7.1 lag-only evaluation)

- Timestamp: 2026-10-14T12:07:46+08:00 (plan committed `1461662`) -> 2026-10-14T13:20:24+08:00
  (8 runs measured; final record committed at the hash named in the ticket).
- Action: dispatched work package S "the decisive SS7.1 lag-only evaluation after the
  startup-warm + merge-oscillation fixes" to the single-writer `wave17/eval` worktree (based on
  integrated master, fast-forwarded to `853633a` before the plan commit). Contract, exact
  commands, per-arm/rung tables, both judging views, T1/T2/T3 verdicts with seed counts and noise
  band, the overall thesis statement and residual gaps are in `.scratch/71-eval-decisive/issues.md`;
  raw artifacts are the committed `seed42/43/44/45/46/47/48/seed42-repeat.{json,md}` in the same
  directory. Scope/result: win (T1) + adversarial (T2), arms declared/learned/static-derived,
  full ladder 50/100/200/400 rps, 7 independent seeds (42-48) + a 42 repeat, one flashdb process
  at a time under `MemoryMax=128MiB` and `flock /tmp/wave17-measure.lock`, box load recorded at
  every boundary (loadavg 3.1-12.1; first 4 seeds noisier). Headline (pilot, 8 runs, NOT 10-seed):
  rung-50 lag-rej=0 is still NOT reached under §7.1 lag-only (static-derived 1-3/40, learned
  4-6/40, declared 39-40/40), BUT the two fixes + serial measurement lift the first positive
  lag-only sustained rung to **rung 100** — static-derived 0/20 lag-rej in 8/8 seeds, learned
  0/20 in 6/8, declared never sustained — which is the first positive lag-only sustained rung in
  any multi-seed sample. The +43-57% achieved-throughput layout win is preserved
  (learned/static 106-107 rps vs declared 70-75, +42.7-52.9%); adversarial learned matches
  declared (median 1/40 vs 2.5/40, 0 index builds, reorg ratio median 0.928x <= 1.5x); win T3
  reorg ratio median 0.934x with no single-run blowup (`U|Stock` 5.13-5.23 MB / 1 merge in all 8
  seeds, max single-window actuation 11.08-11.37 MB). Gates: 304 Rust + 140 Python tests green,
  cargo build 0 warnings, `git diff --check` silent.

## wave18 scribe-repair dispatch record (ticket 67 + 70 doc-accuracy corrections)

- Timestamp: 2026-10-14T12:26:46+08:00.
- Action: dispatched a doc-only scribe repair to the single-writer `wave18/scribe` worktree
  (branch `wave18/scribe`, based on master `853633a`). Two independent-verifier findings fixed in
  place, no code change and no load re-measurement: (A) ticket 67's write-amplification variance
  was re-attributed from "a single ~69-72 MB merge" to the true single-region merge oscillation
  (seed43new `U|Customer` 6 merges of 11.37-11.52 MB = 69.0 MB; seed42repeatnew `P|Customer`
  7 merges of 10.17-10.41 MB = 72.1 MB; largest single merge ~11.5 MB), with the seed43new
  `P|Order` +5.9 MB / `U|StockMovement` +2.3 MB growth offset by `P|Stock` −4.2 MB; (B) ticket 70's
  `after seed43` win-learned reorg was corrected to the driver's summary totals (18,256,145 B /
  6 merges, including the 11,372,547 B final drained merge), giving learned/declared 0.594x and a
  win T3 median of 0.5758x (still <=3x), the max single merge 11.37 MB, plus the gate-header HEAD
  (`d63e294`) and the seed43 declared 39/39 cell. Full numbers and gates are in
  `.scratch/72-scribe-repairs/issues.md`; repair commits `1e42fdf9849d4a4aef201359b4235976a8887080`
  and `90a256d8d9f986b44a1f35ec78655c1c3566d763`.

## Integration record: wave18/scribe

- Timestamp: 2026-10-14T12:37:12+08:00 (integration committed).
- Action: single-writer integrator merged `wave18/scribe` (`--no-ff`, merge commit
  `e1184b4f07104b629a4d709c9db3ee9560ce9a7c`) into `master`. Docs-only merge (4 files under
  `.scratch/`: `.scratch/52-program/issues.md`, `.scratch/67-requestpath-write-stall/issues.md`,
  `.scratch/70-merge-oscillation/issues.md`, and new `.scratch/72-scribe-repairs/issues.md`),
  no conflicts, no code change and no load re-measurement.
- Combined gates (verbatim):
  - `cargo build --workspace --offline` — Finished, 0 warnings.
  - `cargo test --workspace --offline` — 304 Rust tests passed, 0 failed.
  - `cd harness && uv run --with pytest pytest -q` — 140 passed.
  - `git diff --check` — silent.

## Integration record: wave17/eval

- Timestamp: 2026-10-14T13:35:31+08:00 (integration committed).
- Action: single-writer integrator merged `wave17/eval` (`--no-ff`, merge commit
  `0c1b4c5b98f54186a133f7d8c4fdba689f2442c8`) into `master`. Docs-only merge carrying the WP-S
  ticket 71 + artifacts (17 files under `.scratch/71-eval-decisive/`, plus the dispatch entry in
  this file), no code change and no load re-measurement. One conflict: both `wave17/eval` and
  `master` (via `wave18/scribe`) appended sections at the same spot after the wave16 integration
  record; resolved by keeping both in order (wave17 WP-S dispatch record, then wave18 sections).
- T4 worktree created: `git worktree add -b wave19/drift
  /home/z/vibe/worktrees/flashdb/wave19-drift master`; HEAD
  `0c1b4c5b98f54186a133f7d8c4fdba689f2442c8`.
- Combined gates (verbatim):
  - `cargo build --workspace --offline` — Finished, 0 warnings.
  - `cargo test --workspace --offline` — 304 Rust tests passed, 0 failed.
  - `cd harness && uv run --with pytest pytest -q` — 140 passed.
  - `git diff --check` — silent.

## wave19 WP-U dispatch record (measure the pre-registered T4 drift-recovery threshold)

- Timestamp: 2026-10-14T13:41:18+08:00 (plan committed `20eb720`) -> 2026-10-14T14:00:58+08:00
  (9 arm-runs measured; final record committed at the hash named in the ticket).
- Action: dispatched work package U "measure T4 (drift recovery) on the >=10x instance" to the
  single-writer `wave19/drift` worktree (based on integrated master `0c1b4c5`). Contract, exact
  commands, evaluability proof, per-window tables, both judging views, the T4 verdict with seed
  count and noise band, and residual gaps are in `.scratch/73-t4-drift/issues.md`; raw artifacts
  are the committed `val300..val1500` and `s43_600`/`s44_600` `.json`/`.md` in the same
  directory. Scope/result: drift workload (`scan` weight x10, win mix -> 81% scans), arms
  declared (300 rps probe only) / learned@1.0 / static-derived, explicit drift rates 300/400/500/
  600/800/1000/1500 rps, seeds 42 (all rates) + 43/44 (600 rps), one flashdb process at a time
  under `MemoryMax=128MiB` and `flock /tmp/wave19-drift.lock`, box loadavg 4.7-6.1 recorded at
  every boundary. Headline (pilot, 3 seeds x 7 rates, NOT 10-seed): **T4 is UNDERIVABLE** — the
  integrated WP-M order_no index (already demanded by the win mix's 30% scans) plus the drift's
  write-share cut 40% -> ~11% (the fsync-bound bottleneck) make the post-drift mix strictly no
  harder than the pre-drift mix: static-derived pre 1485 vs post 1500 rps at 1500 offered, and no
  post-drift throughput drop at any tested rate, so the pre-registered "recover to within 10% of
  pre-drift throughput within 2 windows" event never occurs. The harness's internal region-level
  re-exploration signal fires on an unrelated region (U|Customer, 6 windows) and still exceeds
  the 2-window target, but that is not the throughput T4 the pre-registration wrote. Gates: 304
  Rust + 140 Python tests green, cargo build 0 warnings, `git diff --check` silent.

## Integration record: wave19/drift

- Timestamp: 2026-10-14T14:13:07+08:00 (integration committed).
- Action: single-writer integrator merged `wave19/drift` (`--no-ff`, merge commit
  `bcb7de1b4f28d57ca0eacfee5970a1fa90d78941`) into `master`. Docs-only merge carrying the WP-U
  ticket 73 + artifacts (19 files under `.scratch/73-t4-drift/`, plus the dispatch entry in this
  file and the `.scratch/STATUS.md` T4 verdict update), no code change and no load re-measurement.
  One conflict: both `wave19/drift` and `master` (via `wave17/eval`) appended sections at the
  same spot after the wave16 integration record; resolved by keeping both in order (wave17
  integration record, then wave19 WP-U dispatch record).
- Wave20 SSOT worktree created: `git worktree add -b wave20/ssot
  /home/z/vibe/worktrees/flashdb/wave20-ssot master`; HEAD
  `bcb7de1b4f28d57ca0eacfee5970a1fa90d78941`.

## wave20 WP-V dispatch record (final SSOT refresh to the T1-T4 state)

- Timestamp: 2026-10-14T14:20:17+08:00.
- Action: dispatched work package V "refresh the current-fact SSOT to the completed T1/T2/T3/T4
  state" to the single-writer `wave20/ssot` worktree (branch `wave20/ssot`, based on integrated
  master after the wave19/drift merge). Doc-only: `.scratch/STATUS.md` was rewritten in place to
  the adopted §7.1 lag-only primary reading and the ticket 71/73 re-derived thresholds — rung 100
  lag-sustained (static-derived 8/8, learned 6/8 seeds; declared never), rung 50 0/40 NOT reached
  (residual: compaction-adjacent cold reads + device fsync tail), +42.7-52.9% throughput (median
  +48.6%, outside the 2.48% noise band), T2 reorg median 0.928x (<=1.5x), T3 win reorg median
  0.934x (<=3x) with no single-region blowup, T4 UNDERIVABLE per ticket 73; the engine-latency
  facts (compaction off-path, overlapped barrier + deferred append, non-blocking boundary,
  read-ahead + eager startup warm; 304 Rust / 140 Python tests) and the open author items (§7.1
  ceiling reconciliation, tile-cap fix §7.4 adopted-unimplemented, T4/residual gaps) were stated
  with ticket paths. Change log and ticket sources in `.scratch/74-ssot-final/issues.md`.
  Gates: cargo build 0 warnings, 304 Rust + 140 Python tests green, `git diff --check` silent.

## Integration record: wave20/ssot

- Timestamp: 2026-10-14T14:33:03+08:00 (integration committed).
- Action: single-writer integrator merged `wave20/ssot` (`--no-ff`, merge commit
  `94efd536b72320eed550725e88e4bce3261b74e1`) into `master`. Docs-only merge carrying the
  verified SSOT refresh (`.scratch/STATUS.md` rewrite + ticket 74), no code change and no load
  re-measurement. One conflict: both `wave20/ssot` and `master` (via `a8094be`) appended sections
  at the same spot after the wave19 integration record; resolved by keeping both in order (wave19
  integration record, then wave20 WP-V dispatch record).
- Merge commit: `94efd536b72320eed550725e88e4bce3261b74e1`.
- Final master HEAD: the follow-up commit carrying this record (see `git rev-parse HEAD`;
  reported in the dispatch run).
- Combined gates (verbatim):
  - `cargo build --workspace --offline` — Finished, 0 warnings.
  - `cargo test --workspace --offline` — 304 Rust tests passed, 0 failed.
  - `cd harness && uv run --with pytest pytest -q` — 140 passed.
  - `git diff --check` — silent.

## wave21 WP-W dispatch record (drift shape for T4)

- Timestamp: 2026-10-14T15:23:30+08:00.
- Action: dispatched work package W "close the T4 evaluability gap with a non-scan drift shape"
  to the single-writer `wave21/driftshape` worktree (branch `wave21/driftshape`, based on
  integrated master `8a4ea20`). Code+measurement package: harness gains `--drift-shape
  {scan,write}`, `--drift-factor`, and `--drift-pre-segments` (defaults preserve the ticket-73
  scan-spike trace exactly; scan drift stays selectable), plus tests (142 Python tests). The
  write-spike drift (`write` weight x10, write share 40% -> ~87%) is capacity-reducing: on the
  12.25x c51 instance static-derived pre-drift 1460 rps -> post-drift ~1260 rps at 1500 offered,
  so rate 1450 sits below pre-drift capacity and above post-drift capacity (the recovery event T4
  needs now exists). T4 re-tested at 1450 rps with pre=6 segments (drift boundary 12,000
  requests): 3 seeds (42/43/44) + seed-42 repeat, learned@1.0 + static-derived per run,
  `flock /tmp/wave21-measure.lock` on every run, box loadavg 4.48-8.85. **Honest T4 verdict: now
  EVALUABLE and measured NOT SUPPORTED** — the learner does not recover and static-derived does
  not recover measurably slower: the learner never triggers drift re-exploration
  (`recovery_windows = {}` in all 8 arm-runs), its post-drift steady throughput median 1266.5 rps
  is indistinguishable from static-derived's 1292.0 rps (-2.0% inside the ~8% same-seed repeat
  noise), and the write-spike floor is policy-independent (the page-size/merge/index knobs cannot
  reduce the write-heavy mix cost). The pre->post drop (~11%) is real (outside the ~8% noise);
  pilot-grade, 3 seeds + 1 repeat, NOT the 10-seed matrix. Ticket + artifacts in
  `.scratch/77-drift-shape/`; gates: cargo build 0 warnings, 304 Rust + 142 Python tests green,
  `git diff --check` silent.
