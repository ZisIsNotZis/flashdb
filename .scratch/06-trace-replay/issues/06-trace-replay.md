# 06-trace-replay — deterministic inventory JSONL execution

Status: claimed
Owner: parent agent; isolated worker owns the read-only request adapter at run `d6490691-d85e-404f-aee2-da57c3559c55`.
Base revision: `d54378e` (2026-09-28).

## Objective and bounds

Execute the four exact request shapes emitted by `harness/flashdb_harness/generator.py` against the v0 WAL+memtable prototype, from frozen JSONL bytes, and check per-block outcomes and inventory invariants. This is an experimental replay path, **not** the general request grammar, an LSM/tile implementation, or production concurrency.

## Dependencies and acceptance

1. Read-only shapes: Stock composite-unique lookup, Customer email lookup, StockMovement order_no scan with deterministic order/limit. Unknown shapes are rejected, not guessed. Worker in isolated worktree, then parent review/integration.
2. A bounded JSONL replay entry point consumes the generator output and exposes per-request structured outcomes and a trace SHA-256. Malformed lines report line number; request failures are distinguished from malformed input. No silent skips.
3. Frozen corpus is generated from a declared seed/configuration. A test compares replay outcomes against a simple independent inventory oracle for stock nonnegativity, movements/order/backorder exclusivity, dedup and recovery; test data includes hot-key repeats and insufficient stock.
4. Gates: `cargo test --workspace --offline`, `cd harness && uv run --with pytest pytest`, `git diff --check`, scoped review. Where implementation cannot support a generated request shape, fail loudly and record the limit rather than claiming corpus coverage.

## Known boundary

Engine is single-writer and all currently implemented reads are memtable-only. Read bindings are not WAL-journalled; replay after a completed read cannot promise the original observation. No tile/compaction/manifest, cgroup scale or layout-learning comparison until the next stage. Device benchmark 0a is reduced-load reference, not clean-idle calibration.

## Progress — 2026-09-28

- Read adapter worker commit `235afae` integrated as `48c70ee`; streaming `scan_primary` avoids cloning every version; movement top-100 holds at most 100 rows but still traverses the entire P prefix. Fresh read-only reviewer verdict OK with one P2: direct API serialized oversized request before enforcing size limit. Parent replaced it with a bounded streaming serialization sink and added a 128 KiB read-shape rejection test.
- First actual 20-line generator corpus (`Generator(42,20)`) SHA-256 `929822594483301f5951e98746c60932d7bfd82408780555a231e9d0a28c5264`. `harness/tests/test_generator.py` locks the corpus bytes/hash; Rust `engine/src/replay.rs` streams JSONL with 64 KiB line bound, line-numbered errors, callback outcomes and hash only on complete success. A Rust integration test bootstraps Stock/Customer, checks inventory counts and branch outcomes independently, scans, WAL reopen and dedup replay.
- First full-corpus test failed at line 18 (`unsupported find fields`). Root cause: worker's hand-written movement request placed `order`/`limit` inside `StockMovement`, but actual generator puts them next to `StockMovement` under `find`. Corrected validator and test fixture to match actual generator byte shape; full corpus now runs without skips. This proves why representative hand-written samples were insufficient.
- Read-only reviewer on `48c70ee` gave OK-with-notes; P2 oversized direct request allocation was corrected with a bounded `serde_json::to_writer` sink before intent serialization, with a 128 KiB rejection regression. Its static review missed the actual movement wire shape; full-corpus execution exposed and fixed it. Reviewer did not run tests.
- Replay API streams line-bounded JSONL and callbacks, SHA-256 only on complete success; CLI `flashdb-replay <existing-wal> <trace.jsonl>` emits per-request JSONL plus final corpus hash, never creates/truncates a WAL. The bootstrap is supplied by tests or a separately prepared WAL, not silently inferred from the trace. Test replays all 20 generator records, checks independent stock counts, all three read cases, Order/Backorder exclusivity, WAL recovery/dedup, and CLI output/hash. Checks at current working revision: `cargo test --workspace --offline -q` = 32 unit + 7 order-flow + 3 read + 2 replay tests; `cd harness && uv run --with pytest pytest -q` = 8 passed; `git diff --check` passed. Final parent replay review pending.

## Next action

Commit the replay path and inspect its scoped diff with a fresh reviewer. Then decide whether to provide a supported bootstrap command or keep this CLI explicitly requiring a pre-seeded WAL. No user decision pending; no layout performance claim before tile/compaction exists.
