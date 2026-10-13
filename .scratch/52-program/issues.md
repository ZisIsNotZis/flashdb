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
