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
