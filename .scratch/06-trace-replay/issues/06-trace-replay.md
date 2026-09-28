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

## Next action

Integrate and inspect the read-adapter worker; add the replay entry point and oracle on top of its dispatcher. Record exact revision, checks, and findings here. No user decision pending.
