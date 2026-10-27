# 22 — write capacity is one-fsync-per-request (~150 rps); needs group/batch commit

Opened 2026-10-01 from the first real capacity curve (offered-load pacer, objective v1's primary
metric: "can it hold up under high load").

## Measurement (reproducible, two runs)

Method: seeded inventory engine (85k dimension rows), a 3,762-request write-only trace replayed
open-loop at 800-6,400 rps by `flashdb-load`, with a device baseline probed in the same session
by writing 512 B and `fsync`ing 100 times in the engine's directory.

| observation | value |
|---|---|
| device fsync, p50 (probe, same session/dir) | **5,107 / 6,660 us** (two runs) |
| engine `commit` p50 (per-pattern telemetry) | **6,025 us** - i.e. exactly one fsync |
| all read/probe patterns (`U:Customer.email` 6.3 us, `P:Customer` 3.1 us, ...) | microseconds |
| achieved write throughput at 800/1,600/3,200/6,400 rps offered | **~135-150 rps in every case** |
| schedule lag | grows without bound (19-28 s by mid-run) |
| read-only trace | p50 8 us, no saturation at 4,000 rps |

So: **the write path is durability-bound, not CPU- or device-bandwidth-bound**, and the offered
rate does not change it - every request costs one barrier, so capacity = 1 / fsync_latency.

Two methodology notes that must travel with these numbers:
- An earlier run of the same shape reported a 41 us commit p50. I cannot reproduce it; the device
  probe (which was broken at the time: it truncated and synced without writing, reporting 0 us)
  cannot confirm the device state then. Treat it as an unexplained outlier, not as a reference.
- `docs/2_GOAL.md`/`6_ARCHITECTURE.md` already specify the fix in principle ("one WAL barrier on the
  foreground", the group-commit scheduler contract); it is unimplemented.

## Required work

A **batch/group commit** path: submit N blocks, perform the per-block atomicity and uniqueness
validation over the batch's *projected post-state* (the same rule as today: published state plus
the batch's own net effect, so intra-batch interactions stay legal), then append every WAL record
and take **one** barrier for the whole batch, and only then ack every block in it.

Constraints:
- Per-block atomicity is unchanged: either a block's effects are durable and acked, or nothing of
  it is visible. A conflict in one block must not silently commit the others - state the rule
  (whole-batch rejection is acceptable and simplest; per-block rejection with a durable ack of the
  rest is a stronger contract and must be stated if chosen).
- Uniqueness must be evaluated against the batch's post-state, so "block A releases value v and
  block B takes it" inside one batch is legal, exactly as inside one block today.
- The WAL record framing, CRC, recovery, rotation and the republish guard must keep working with
  multiple records appended per barrier; recovery must still fail closed.
- The API shape is the author's to confirm in principle, but the measured problem is unambiguous:
  publish it as a new method with the existing single-block one kept (so callers choose).
- Acceptance: measured achieved throughput at a fixed offered rate with an N-block batch, reported
  against the ~150 rps one-fsync baseline in the same session, with the device fsync p50 stated
  alongside; a batch of N should approach min(device_parallelism, fsync_cost x 1) - i.e. throughput
  should scale with N until something else binds, and the report must name what binds next.
- Also required: the pacer must be able to offer a *batch* workload (the trace already groups
  requests; the harness can mark which requests form one commit batch) so the comparison is
  apples-to-apples.

## Result (implementation, 2026-10-02)

Landed `Engine::commit_batch(&[(block_id, ops)])` (one WAL barrier for many blocks) and a
`--commit-batch B` mode on the pacer. Conflict rule: **whole-batch rejection** - any unique
conflict rejects the whole batch before a record is appended, so `BatchOutcome::Conflict`
means nothing durable (WAL unchanged, no CSN advance). Justification: a unique violation is a
terminal business error, not an `else`, and the batch is one durability unit, so absorbing the
losing block would make the durable set depend on evaluation order. A batch is byte-equivalent
in the WAL to the same blocks committed one at a time (same records, order, framing); only the
barrier count differs.

Measurement: 3,762-request write-only trace (order_flow mix), fresh engine per run, fixed
offered 6,400 rps, device fsync probed in the same dir/session.

| B | device fsync p50 | achieved rps | commit p50 (one barrier) | late (lag > 10 ms) |
|---|---|---|---|---|
| 1 | 5,061 us | **143** | 5,057 us | 3,760 / 3,762 |
| 8 | 5,664 us | **1,199** | ~5.6 ms | 3,746 / 3,762 |
| 64 | 5,089 us | **6,378** | 7.18 ms | 0 / 3,762 |

B=1 reproduces the prior one-fsync ~150 rps ceiling. Per-batch latency stays ~one device
fsync (5-7 ms) at every B, so B=64's 64-blocks-per-barrier is ~64x cheaper than 64 serial
barriers; achieved write throughput scales 143 -> 1,199 -> 6,378 rps (x44.6). What binds next:
at the fixed 6,400 rps offer, B>=64 is offered-rate-bound (`late_total=0`); raising the offer to
25,600 rps on a 20,000-request write trace shows the device barrier is still the binder at
B=64 (7,813 rps) and B=256 (21,396 rps), while B=1024 holds 25,902 rps with no lag - so the
barrier is amortized but not yet displaced, and CPU/pacer scheduling has not been reached.
Bytes per block are unchanged (the batch appends exactly the records a sequence of single
commits would).

## Result (2026-10-01, slice 22)

`Engine::commit_batch(&[(block_id, ops)]) -> BatchOutcome` with **whole-batch conflict rejection**
(nothing durable on a conflict; a unique violation is a terminal business error, and absorbing only
the losing block would make the durable set depend on evaluation order). Validation runs over the
batch's projected post-state, so intra-batch release+reuse is legal and an intra-batch duplicate is
a conflict. Dedup blocks are excluded from the overlay so a retry cannot wrongly reject a batch.
Per-block CSN/dedup/watermark advance exactly as single commits.

**Durable bytes are byte-identical to the same blocks committed one at a time** (tested for N=1 and
N=4): same records, same order, same framing, one barrier. So nothing about durability was relaxed
to win the throughput.

Measured (3,762-request write-only trace, fresh engine per run, offered 6,400 rps, device fsync
probed in the same directory/session):

| batch B | device fsync p50 | achieved rps | per-batch p50 | lag>10 ms |
|---|---|---|---|---|
| 1 | 5,061 us | **143** | 5,057 us | 3,760 |
| 8 | 5,664 us | **1,199** | ~5.6 ms | 3,746 |
| 64 | 5,089 us | **6,378** | 7.18 ms | 0 |

Per-batch latency stays ~one device fsync (5-7 ms) at every B, versus B x fsync if serialized
(64 blocks would be ~326 ms). At a 25,600 rps offer, B=64 gives 7,813, B=256 gives 21,396 and
B=1024 holds 25,902 rps with no lag, i.e. the barrier is amortized but not yet displaced and no
CPU ceiling has been isolated.

## Guard semantics under batching (must not be relaxed)

The batch path above validates UNIQUENESS over the batch post-state. But a request's *guards* (e.g.
`where on_hand >= qty` on a stock decrement) are evaluated before the commit, against the published
state. If several requests are batched and every guard is evaluated pre-batch, two requests could
each pass a guard that a sequential execution would have failed on the second one - i.e. batching
would introduce oversell. Sequential execution is the reference semantics: a guard sees everything
committed before it, including earlier requests.

So wiring batching into the request path requires: requests within a batch are evaluated **in
order against the batch's projected state** (published + earlier requests in the batch), exactly as
sequential commits would, with the barrier deferred. Any relaxation of that is a product-semantics
change the author must decide, not an implementation detail. Until then, batch throughput numbers
describe the engine's commit path, not the request path's semantics.

## Correction (2026-10-01): the group is NOT atomic across a crash, and that is correct

Slice 23's task text demanded that a crash before `end_group` lose the WHOLE group with no prefix
surviving. The implementation does the opposite - recovery replays every complete, CRC-valid frame
appended before the barrier - and the worker said so plainly rather than claiming the requirement
was met.

On review, **the requirement was wrong, not the implementation**. For the request path the group is
a set of INDEPENDENT requests batched to share one barrier. A durable prefix after a crash is
exactly what the sequential reference semantics produce: a sequence of single commits would also
have made the earlier blocks durable and lost the later ones. Nothing in the group was acknowledged,
so no acknowledged work is lost, and a client retry is idempotent through its business unique key
(the author's idempotency contract). Forcing whole-group atomicity would have made the batched path
*less* faithful to sequential execution, and it would have required a group-end marker that recovery
must insist on.

So the contract was corrected at the API (`end_group`'s doc comment now states the real semantics
and explicitly disclaims whole-group atomicity), and the true statement is: each block is atomic,
the group is not. A genuine atomic multi-block group remains a possible future feature with an
explicit end marker, and nothing in the current design depends on it.
