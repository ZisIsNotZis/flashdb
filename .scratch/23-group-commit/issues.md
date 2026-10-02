# 23 — deferred-barrier group commit for the REQUEST path (in-order visibility)

Opened 2026-10-02 from slice 22. Slice 22 landed `Engine::commit_batch`
(one WAL barrier for many blocks, whole-batch uniqueness rejection) and measured
the engine's *commit* path. But a request's **guards** (`where on_hand >= qty`)
are evaluated by the caller before committing, against the published state. If a
group of requests evaluated every guard against the pre-group state, two stock
decrements could both pass a guard sequential execution would fail on the second
— oversell. Group commit for the request path must therefore keep in-order
visibility and defer only the barrier.

## Design (implemented)

**Deferred barrier with immediate application.** Inside a group each block is
WAL-appended and applied to the memtable IMMEDIATELY, so every later probe/guard
in the group sees it exactly as a sequential commit would; the single `sync` is
deferred to `end_group`. Nothing in the group is acknowledged before that barrier.

API (`engine/src/engine.rs`):

- `Engine::begin_group() -> io::Result<()>` opens a group; a second call fails.
- `Engine::commit_block_grouped(block_id, ops) -> io::Result<Outcome>` —
  identical to `commit_block` except the `fdatasync` is deferred. Uniqueness is
  still evaluated against the current (already group-updated) state, so a
  conflict is a **per-block** `Outcome::Conflict`, never a whole-group rejection;
  later grouped commits continue. Fails closed outside a group.
- `Engine::end_group() -> io::Result<()>` takes the group's single barrier.
  `end_group` twice fails closed. On a partial append failure inside the group
  the WAL writer is poisoned exactly as on the single-block path, so this sync
  fails and the group is never acknowledged; recovery truncates the torn tail.
- `Engine::group_open() -> bool` lets the request executor route its block
  through the grouped primitive; every *other* `commit_block`/`commit_batch`
  caller still fails closed during a group.

The durability rule is stated at the API: the barrier is the group's ONLY
durability point, the group is one durability unit (coarser than per-block), a
crash before the barrier can lose the whole group, and because no member was acked
no acknowledged work is lost. Every other mutating path (`commit_block`,
`commit_batch`, `build_tile`, `publish_tile`, `rotate_wal`, `compact`,
`maybe_compact_shape`) refuses while a group is open, so a forgotten `end_group`
fails closed instead of binding durable state to un-barriered bytes.

Batch executor (`engine/src/replay.rs`):
`execute_requests_grouped(engine, &[Value]) -> io::Result<Vec<io::Result<RequestResult>>>`
executes the requests in input order inside one group. A per-request error (a
unique conflict, a malformed request) is reported at its own index and does not
abort the group; later requests still execute against the earlier effects.

Pacer (`engine/src/bin/flashdb-load.rs`): `--group-requests B` offers the real
adapter flow in groups of B requests through the grouped executor and reports the
`take: failed` (else-branch) count, so the measurement shows how many guards
actually fired.

## Tests (`engine/tests/group_commit.rs` + one engine unit test)

- in-order visibility: two 3-unit takes from a 3-unit row — the second must take
  its `else` branch and the row ends at 0, never negative;
- byte-identical WAL to the same requests executed one at a time;
- N grouped blocks consume N CSNs;
- a conflict in the middle of a group is per-block (reported, nothing durable)
  and the blocks after it still commit;
- a torn tail before the barrier loses the whole group on recovery; a complete
  barrier replays all of it;
- the republish guard and rotation keep working after a group;
- grouped commit outside a group / `end_group` twice / nested `begin_group` /
  every other mutating path during a group all fail closed;
- (`engine.rs` unit test) `end_group` after a failed append poisons the writer
  like the single-block path, and recovery truncates the torn frame.

## Measurement (real inventory workload through the grouped executor)

Method: deterministic seed trace (`harness/flashdb_harness/seed.py`, 85,000
dimension rows) + flow trace (`generator.py` MIX: order_flow 70, stock_lookup 15,
customer_lookup 10, movements_scan 5), 4,000 requests, `on_hand=4` so the stock
guard actually fires. A fresh seeded engine per group size. Offered open-loop at
a fixed 6,400 rps by `flashdb-load --group-requests B --window-ms 100`; the device
fsync p50 is probed in the same directory/session. Driver:
`scripts/group-commit-run.sh`.

| B | device fsync p50 | achieved rps | per-group p50 | guard-fail (else) | errors | late |
|---|---|---|---|---|---|---|
| 1 | 6,415 us | **184** | ~5.9 ms | 1,227 / 4,000 | 0 | 3,998 |
| 8 | 5,983 us | **893** | ~7.2 ms | 1,227 / 4,000 | 0 | 3,992 |
| 64 | 6,425 us | **4,232** | ~15 ms | 1,227 / 4,000 | 0 | 3,360 |

At B=1 the request path reproduces the one-fsync-per-request ceiling (~184 rps
for this mixed read+write trace; ~1,050 commits). Scaling with B is real
(184 → 893 → 4,232, ×23) but the per-group latency at B=64 is already ~15 ms, not
one fsync (~6 ms): the group's requests execute serially (single writer), so the
adapter's per-request CPU — JSON parse, the Customer/Stock/Order/Backorder
lookups, the 5% movements scan, WAL encode, memtable apply — accumulates inside
the group alongside the barrier.

Crucially the else-branch count is **identical at every B** (1,227): the same
requests failed the same guards in the same order regardless of grouping, so the
grouped path is exercising the in-order semantics, not replaying successful puts.

## What binds next

Group size stops helping the moment the group's serially-executed per-request
CPU is a larger term than the single barrier. Probe at a fixed 51,200 rps offer
(same trace, fresh seeded engine per size, device fsync p50 ~6.0 ms):

| B | achieved rps | per-group barrier count |
|---|---|---|
| 64 | **3,699** | 63 |
| 256 | **6,534** | 16 |
| 1024 | **8,253** | 4 |

At B=1024 the whole 4,000-request trace pays ~4 barriers (~24 ms of ~484 ms), so
the barrier is no longer the binder — and throughput is only ~2.2x the B=64
number, not 16x larger. Since group latency is `cpu_per_request * B + fsync`, the
throughput asymptote is `1 / cpu_per_request` (~8-10k req/s for this trace). The
binder is now **per-request CPU on the single writer**: JSON parse, the
Customer/Stock/Order/Backorder lookups, the 5% full `StockMovement` scan, WAL
encode and memtable apply. The next levers are cheaper per-request work or
concurrent execution (sharding the single writer), not a larger barrier group —
larger groups lift throughput only by amortizing the barrier over CPU that grows
linearly with B.

This is a request-path finding, not a commit-path one: slice 22's synthetic
write batches (no reads, no guards) at B=64 reached 6,378 rps on the same device,
where here B=64 holds ~4.2k because the real adapter's guard/lookup/scan work is
executed inside the group ahead of the barrier.

## Result (implementation, 2026-10-02)

Landed the deferred-barrier group API (`begin_group` / `commit_block_grouped` /
`end_group` / `group_open`), the in-order batch executor
`replay::execute_requests_grouped`, and the pacer `--group-requests B` offering
the real adapter flow in groups. In-order visibility is preserved (identical
else count at every B), the durable bytes are byte-identical to the same requests
committed one at a time, and every fail-closed path is covered. Measured
184 → 893 → 4,232 rps for B = 1, 8, 64 at a fixed 6,400 rps offer on the real
inventory flow (device fsync p50 ~6.0-6.4 ms, 1,227 of 4,000 requests took the
guard's `else` branch at every B), with per-request CPU — not the barrier — the
next binder above B=64.
