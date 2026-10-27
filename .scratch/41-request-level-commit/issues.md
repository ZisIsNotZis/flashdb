# 41 — the commit group is not a validation unit; make uniqueness per request

Opened 2026-10-06 from `STATUS.md` §4 defect 8 and `docs/7_INTERFACE.md` ("A commit group is not a
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

## Review of 4a348d7 (2026-10-09): P0 half-applied accept, fixed

### Correction to the change description

The defect this ticket really was lived in `Engine::commit_batch`, reached by the
`--commit-batch` path. The `--group-requests` deferred-barrier path
(`replay::execute_requests_grouped`) already executed each request sequentially against the
immediately applied serving state and already reported a conflict per request; it never had this
defect and is unchanged. So all of the P0 work below is in `commit_batch` and its tests.

### P0 finding (verbatim from the independent review of 4a348d7)

> a request can be accepted with only half its blocks applied, producing two live owners for one
> unique prefix in the durable state. In `commit_batch` the ops of EVERY non-retry block are folded
> into the request's uniqueness overlay, but a block whose `block_id` was already used by an earlier
> block OF THE SAME REQUEST is later converted to `AlreadyCommitted` and excluded from `pending`
> (the `seen` check around lines 2640-2652, populated only after the slot-plan loop). The overlay
> therefore contains an effect that is never applied, so validation accepts a request that only
> half-applies. Counterexample: seed `PutUnique{P,h1}`; then `commit_batch` with ONE request of two
> blocks sharing block_id `b'x'`: first `PutUnique{P,h2}`, second `DelUnique{P,h1}`. Overlay =
> `{h2 live, h1 dead}` so `leaves_two_owners` is false and the call is ACCEPTED, returning
> `Committed` for block 1 and `AlreadyCommitted` for block 2, while only block 1 is appended.
> Durable and recovered state then has BOTH h1 and h2 live for P, and `unique_lookup` fails with
> `InvalidData` dual/master owners. This is a regression: the pre-change code checked the
> same-request duplicate BEFORE accumulating the overlay.

Reproduced against the pre-fix tree: `commit_batch` returned
`Ok([Committed([Committed { csn: 2 }, AlreadyCommitted { csn: 2 }])])` (the new a1 test panicked on
`unwrap_err()`).

### Disposition (chosen, fail-closed)

A repeated `block_id` INSIDE one request is now rejected as `io::ErrorKind::InvalidInput`,
"duplicate block_id within one request", **before any overlay fold and before any append**,
consuming no CSN and leaving no state. Rationale: block ids are keys in the request's `blocks`
object, so they are unique by construction for any request that came through the request surface
(and `replay.rs`'s `UniqueValue` rejects duplicate object keys, so a duplicate can only come from a
programmatic caller). The engine refuses what it cannot order. Retry dedup ACROSS calls
(`self.dedup`) and ACROSS the group's requests (`seen`) is unchanged.

Structural fix so overlay and pending cannot disagree by construction:

- the duplicate check runs in the slot-plan loop, so every `Slot::New` block is distinct within its
  request;
- the now-dead same-request `seen` branch in the CSN-assignment loop is removed (a `Slot::New`
  block is always appended; a block already committed this group/call is a `Slot::Retry`);
- a comment states the invariant ("`overlay` is the net unique effect of exactly the blocks this
  request will append") and a `debug_assert_eq!(folded, pending.len() - pending_before, ...)` pins it -
  that assertion fails on the pre-fix code (folded = 2, appended = 1).

### New tests and relabelling (`engine/tests/request_commit.rs`)

Added, each asserting the DURABLE post-state via reopen:

- a1 `duplicate_block_id_within_one_request_is_refused_and_changes_nothing` - the review's exact
  counterexample; asserts `InvalidInput`, no CSN consumed, no WAL growth, and exactly one live owner
  for P after reopen. Discriminating: it fails on the pre-fix code.
- a2 `repeated_block_id_across_two_requests_replays_the_first_and_costs_no_csn` - a `block_id`
  repeated in a LATER request of the group replays the first occurrence and costs no CSN.
- a3 `one_request_deleting_then_adding_an_owner_for_the_same_prefix_commits` - overlay/pending
  agreement: one request that deletes owner h1 and adds owner h2 for the same prefix still commits
  and both blocks are durable with their own CSNs.

Relabelled (kept, not deleted): t1/t2/t3 are now marked in a code comment as **regression pins**,
because an independent reconstruction showed the pre-change implementation produced their durable
assertions too. They guard against future regressions but do NOT evidence the per-request unit;
only t4, t5 and a1 discriminate it.

### P2 fixes

- p1 `engine.rs` `commit_batch` doc: the crash-prefix sentence no longer claims the invariant holds
  for a torn prefix. It now states the invariant holds at REQUEST granularity after a successful
  call, and that a prefix torn inside a request was never validated as a state and follows the
  pre-existing block-atomicity rule (docs/6_ARCHITECTURE.md I6: a block is all-or-nothing, a request is
  not).
- p2 `engine/src/replay.rs`: the `execute_requests_grouped` doc said a crash before the barrier
  "can lose the whole group"; it now says a crash can leave a durable PREFIX of the group's records
  (recovery replays every complete, CRC-valid frame), matching the `Engine::end_group` rule.
- p3 the relabelling above, recorded here and in the test file's code comment.

### Gates (verbatim, post-fix tree)

`cargo test --workspace --offline`:

```
test result: ok. 113 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.77s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.17s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.22s
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.59s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.47s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.58s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.88s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Total: **258 passed; 0 failed** (was 255; +3 new tests).

`cargo build --workspace --offline` (after `cargo clean -p flashdb-engine`, so it really compiled):

```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.75s
```

Zero warnings. (The `cargo test` run emits two pre-existing "variable does not need to be mutable"
warnings in the untouched `engine/tests/manifest_engine.rs` at lines 48 and 77; they are not part
of the build gate and are not introduced by this change.)

`git diff --check`: silent (exit 0, no whitespace errors).

### Residual risk

- The fix changes the verdict for a request that repeats a `block_id` inside itself: from
  `Ok(Committed/AlreadyCommitted)` to `Err(InvalidInput)`. No request produced by the request
  surface can hit this (a JSON object cannot repeat a key; `replay.rs` rejects duplicate keys), so
  only a programmatic caller passing a hand-built request can observe the change - which is the
  intended fail-closed behaviour.
- The `debug_assert_eq!` invariant check is debug-only; release builds rely on the structural
  argument (duplicate refused up front, retries skip the fold).
- p1/p2 are comment corrections only; the durable block-atomicity behaviour was already as now
  documented (docs/6_ARCHITECTURE.md I6), so no runtime change accompanies them.
