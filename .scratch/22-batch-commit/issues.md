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
- `docs/objective.md`/`design.md` already specify the fix in principle ("one WAL barrier on the
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
