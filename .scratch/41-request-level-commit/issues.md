# 41 — the commit group is not a validation unit; make uniqueness per request

Opened 2026-10-06 from `STATUS.md` §4 defect 8 and `docs/contracts.md` ("A commit group is not a
validation unit"). The contract's validation unit is the REQUEST; the implementation's was the
group.

## Scope

`Engine::commit_batch` was all-or-nothing across its whole input: one unique conflict rejected the
entire call (`BatchOutcome::Conflict`), so `flashdb-load --commit-batch B` put the blocks of B
independent requests into one call and one conflicting request rejected its B-1 unrelated
neighbours. A request's verdict therefore depended on which other requests shared its group.

This ticket changes the commit-group API to carry the request grouping and resolves uniqueness per
request, in input order, against the published state plus the effects of the earlier ACCEPTED
requests in the same group. It does not touch the deferred-barrier path (`--group-requests`,
`replay::execute_requests_grouped`), which already commits each request's blocks in order and
already reports a conflict per request.

Touch set: `engine/src/engine.rs`, `engine/src/bin/flashdb-load.rs`, `engine/tests/` (new
`request_commit.rs`, updated `batch_commit.rs` / `group_commit.rs`), this file. No `docs/` or
`harness/` change.

## Confirmed contract (author, 2026-10-06; not re-litigated)

- A block is the atomicity unit.
- Blocks inside one request have declared relations: `needs` = sequence, no edge = parallel,
  else = conditional fallback.
- Uniqueness is evaluated on the projected post-state, never per operation. A block (or request)
  that deletes owner A and adds owner B for the same unique prefix is legal.
- The unit of a uniqueness verdict is the REQUEST. A commit group is NOT a validation unit.
- A request whose projected post-state leaves more than one live owner for a touched unique prefix
  is **dropped entirely**: none of its blocks is appended, none consumes a CSN, and it reports a
  conflict. The other requests commit normally, each block consuming its own CSN in input order.
- Invariant after the call: the durable state has at most one live owner per unique prefix.
- Durability is unchanged: one barrier for the group, and a crash may leave a valid prefix.

## Change

- `Engine::commit_batch` now takes `&[Vec<(Vec<u8>, Vec<Op>)>]` (a slice of requests, each a slice
  of its blocks) and returns `io::Result<Vec<RequestOutcome>>` (one `RequestOutcome` per request,
  in input order). `RequestOutcome::{Committed(Vec<Outcome>), Conflict(Conflict)}` replaces the
  two-variant all-or-nothing `BatchOutcome`.
- Validation keeps a running `live` map per touched unique prefix: the published owners, then the
  net effect of every earlier ACCEPTED request. A request is accepted only if every prefix it
  touches has at most one live owner after its own overlay; otherwise it is pushed as
  `Conflict` and skipped, leaving no trace. Accepted requests assign each of their new blocks its
  own CSN in input order and join the append plan; retries keep their existing CSN and contribute
  no overlay. One `fdatasync` covers the whole accepted set (unchanged durability).
- `flashdb-load --commit-batch B` now builds one single-block request per write request and counts
  the per-request outcome: an accepted request is one `completed`, a dropped request is one `error`
  reported at its own request index, not a group failure.
- Doc comments in `engine.rs` and `engine/tests/batch_commit.rs` that asserted whole-batch
  rejection now state the per-request contract.

## Acceptance criteria

Tests added in `engine/tests/request_commit.rs`, all asserting the DURABLE post-state by reopening
the engine from the WAL:

- t1 `swap_a_value_between_two_rows_in_one_block_commits`
- t2 `delete_then_reinsert_the_same_value_with_a_new_handle_commits`
- t3 `earlier_request_releases_a_value_a_later_request_takes`
- t4 `one_conflicting_request_leaves_its_neighbours_committed_without_a_csn_gap`
- t5 `two_requests_inserting_the_same_value_first_in_input_order_wins`

No regression of the existing tests, notably the slice-23 group-commit and prefix-durability tests
and the existing unique-conflict tests (`batch_commit.rs` updated to the new request API, intent
preserved).

## Evidence (verbatim)

### `cargo test --workspace --offline`

```
     Running unittests src/lib.rs (target/debug/deps/flashdb_engine-0910e1580e762a65)
test result: ok. 113 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.69s
     Running unittests src/bin/flashdb-load.rs (target/debug/deps/flashdb_load-06d3ec55e1b908a2)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/bin/flashdb-replay.rs (target/debug/deps/flashdb_replay-20f916240367c5f6)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/bin/flashdb-scale.rs (target/debug/deps/flashdb_scale-10d52ff57e671403)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/batch_commit.rs (target/debug/deps/batch_commit-03056c8007e40d8a)
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.17s
     Running tests/constraints.rs (target/debug/deps/constraints-e23a3b90346d060a)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running tests/group_commit.rs (target/debug/deps/group_commit-d36441161ab821a9)
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
     Running tests/manifest_engine.rs (target/debug/deps/manifest_engine-06ebda6922a63508)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
     Running tests/merge_threshold.rs (target/debug/deps/merge_threshold-21565556f97531d6)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
     Running tests/metrics.rs (target/debug/deps/metrics-11288ad273514e7a)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
     Running tests/multi_tile.rs (target/debug/deps/multi_tile-d60892b0f0265621)
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 7.17s
     Running tests/order_flow.rs (target/debug/deps/order_flow-51396f14d21d3d9b)
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
     Running tests/read_requests.rs (target/debug/deps/read_requests-c7e5984dfdce89de)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.66s
     Running tests/region_compaction.rs (target/debug/deps/region_compaction-bbd4896429af8ffe)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/region_partition.rs (target/debug/deps/region_partition-97bee90982ae7afa)
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/replay_trace.rs (target/debug/deps/replay_trace-f9acff68f1299fa9)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.22s
     Running tests/request_commit.rs (target/debug/deps/request_commit-bcbe5d5144aa6ec0)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.04s
     Running tests/scale_bin.rs (target/debug/deps/scale_bin-2eaf99fb6bcaaabf)
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.16s
     Running tests/tile_format.rs (target/debug/deps/tile_format-a2d98d9006f07ed3)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
     Running tests/tile_layout.rs (target/debug/deps/tile_layout-3b15ba3aef1f31df)
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
     Running tests/tile_read.rs (target/debug/deps/tile_read-7ffff97a0246b3bd)
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.66s
     Running unittests src/main.rs (target/debug/deps/nvme_bench-62654cd5e60d264d)
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Total: **255 passed; 0 failed** across all binaries.

### `cargo build --workspace --offline`

```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.67s
```

Zero warnings.

### `git diff --check`

Silent (exit 0, no whitespace errors).

## Residual risk

- `RequestOutcome` replaces `BatchOutcome` and `commit_batch`'s signature changed; any out-of-tree
  caller must be updated. Within this workspace the only callers are `flashdb-load` and the tests,
  both updated.
- The per-request path scans each touched unique prefix once per group (cached across accepted and
  dropped requests alike), so validation cost is unchanged in shape; the load driver's batch
  workload uses one distinct prefix per request and was not re-measured for throughput in this
  ticket (correctness-only change; one barrier for the group is preserved).
- A dropped request's reserved handle in `flashdb-load --commit-batch` is not reused within the
  run; handles are opaque and only applied ops advance the watermark, so this is not a correctness
  issue.
