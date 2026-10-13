# 56 - overlapped durability-preserving WAL barrier (wave5 WP-E)

Contract: work package E of `.scratch/52-program/issues.md`. Add a durability-preserving
overlapped/async WAL barrier to the group-commit path so the load driver can append group N+1 while
group N's `fdatasync` is in flight, then rework the driver and re-measure. Acceptance: durability
preserved (tests); if stage 1 is sound, rung 50 meets the lag ceiling (lag p99 <= 10 ms, no
lag-rejected window) without regressing throughput/write amp; otherwise report honestly why with
per-group evidence. Ticket 55 (`.scratch/55-lag-stall-attribution/issues.md`) named the primary
cause of the rung-50 lag breach as a cascade: one write group's WAL `fdatasync` tails to 71-498 ms
on the shared device, the synchronous single-writer pacer blocks behind it, and the NEXT group is
issued that late, batching the accumulated requests and breaching the ceiling. This ticket removes
that cascade without weakening durability; it does not change the pre-registration or the docs.

Instance/traces (unchanged from ticket 55): `/tmp/cold44/per_region-mixed` (8.17x; 1,643,933,528 B
data / 192 MiB cap) for iteration and `/tmp/cold51/per_region-mixed` (12.25x) for the headline
confirmation. Trace `adversarial.jsonl` (seed 42, sha256 bf0a5170…, 8000 lines), policy
`declared.policy.json` (default 4096/10/2), ceilings `--ceiling-us 10000 --lag-ceiling-us 10000`.
Each run cold in `systemd-run --user --scope MemoryMax=192 MiB` (MemorySwapMax=0), one process at a
time; the instance is a `cp -a` copy under `/tmp/cold56/` and the release binary is the one build
copied to `/tmp/cold56/flashdb-load` (sha256 `4344fe8efc438e5d…`), run only from that copy.

## Design (implemented at 083da49 + 413176c)

The barrier is overlapped, not weakened: an append is still covered by an `fdatasync` before it is
acknowledged, and an `fdatasync` failure fails the whole group closed (no ack) and poisons the WAL
so a later open requires recovery. The only change is WHO waits and WHEN: the single pacer no longer
blocks on the barrier inline; a single FIFO background worker runs `fdatasync` on a cloned WAL fd
while the pacer appends the next group.

- `Wal::sync_handle()` clones the WAL fd (so the worker has a stable handle across rotation) and
  `ensure_writable`; `Wal::poison()` records a sync failure so a later write/open fails closed.
- `Engine` owns a `BarrierWorker` (one FIFO thread) and a `BarrierResult { fsync_us, completed_at }`
  per completed barrier. `submit_group()` appends+applies then enqueues the barrier and returns
  `true` (read-only groups submit nothing and return `false`). `pending_barriers()` /
  `MAX_PENDING_BARRIERS = 64` bound the in-flight queue; `wait_group_barrier()` /
  `try_take_group_barrier()` drain in submission order; `wal_valid_len()` reports the live WAL size
  for maintenance. Every state-mutating entry point (`build_tile`, `publish_tile`, `compact`,
  `maybe_compact_shape`, `compaction_job`, `apply_compaction_job`, `commit_block`, `commit_batch`,
  `rotate_wal`) asserts no barrier is in flight so no snapshot/rotation races an outstanding fsync.
- `replay::submit_requests_grouped(engine, requests)` runs a request group in order (guards see
  earlier effects), submits one covering barrier, and returns `(barrier_submitted, per_request_results)`.
- The load driver's `--group-requests` path defers each write group's per-request samples and acks
  until its barrier completes (`PendingGroup`), so the group's completion time is the worker's
  `completed_at` (service latency includes the barrier; schedule lag, measured at dispatch, does
  not). Read-only groups ack immediately. Barriers are drained oldest-first when the in-flight cap
  is reached, when WAL maintenance must run, at each window boundary (so the window's stats and the
  compaction/apply paths see a fully-drained engine), and defensively at end of run.

Rejected alternatives:

- **Ack before the covering `fdatasync` (true async group commit).** Rejected: weakens durability —
  a crash after ack but before the barrier loses acknowledged writes. The whole point of the group
  commit in ticket 23 was one barrier BEFORE ack; this ticket moves the wait off the pacer, never
  past the ack.
- **One barrier per request, worker-drained.** Rejected: changes the measured group-commit protocol
  and amortization; the barrier cadence is what ticket 23 measured.
- **Unbounded in-flight barriers.** Rejected: memory must stay bounded; `MAX_PENDING_BARRIERS=64`
  caps it and the driver drains oldest-first at the cap.
- **Barrier worker per group.** Rejected: unbounded threads; one FIFO worker preserves ordering and
  keeps the fd-clone count constant.

## Durability argument

Every acknowledged write is still appended to the WAL and covered by an `fdatasync` that has
RETURNED before the ack is recorded. The driver only counts a group's successful requests as
`completed` inside `drain_one_pending_group`, which runs only after `wait_group_barrier` returns the
worker's `Ok(BarrierResult)`. A barrier that fails returns `Err`, counts the whole group as errors,
records `first_error`, and propagates the error out of the driver (which then stops dispatching
because the engine is poisoned). So there is no silent ack on a failed sync. Ordering is preserved
by the FIFO worker and the oldest-first drain: group N+1's completion is never observed before
group N's. Rotation/recovery stay correct because every mutating path refuses to run while a barrier
is in flight (assert), so a rotated/compacted snapshot is always a prefix whose barrier has already
returned; recovery never has to reason about a half-fsynced segment.

## Tests (new)

- `engine/src/engine.rs`: `overlapped_barrier_sync_error_fails_closed_and_poisons_the_wal`,
  `overlapped_barrier_failure_is_observed_in_submission_order` (an injected fsync failure poisons
  the WAL, fails the whole group with no ack, and the failure is observed in FIFO order).
- `engine/tests/group_commit.rs`: `overlapped_barriers_drain_in_order_and_recover` (two groups
  submitted back-to-back, drained in order, reopen+replay recovers both),
  `submit_requests_grouped_reports_barrier_and_per_request_results` (read-only group submits no
  barrier and returns per-request results; write group submits one barrier).

## Gates (verbatim; run at 413176c + this doc)

1. `cargo test --workspace --offline` — 284 passed; 0 failed; 0 ignored.
2. `cargo build --workspace --offline` — 0 warnings.
3. `cd harness && uv run --with pytest pytest -q` — 132 passed in 4.72s.
4. `git diff --check` — silent (exit 0).

## Measurement

Full-run command (baseline flags, unchanged from ticket 55, plus `--group-requests 64`):

```
flashdb-load <instance> adversarial.jsonl \
  --rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 \
  --metrics-json --peak-report --ceiling-us 10000 --lag-ceiling-us 10000 \
  --region-mode per_region --policy-file <policy> --group-requests 64 \
  --checkpoint-bytes 8388608 --rotate-bytes 8388608 --compact-bytes 67108864
```

The no-compact control is the same minus the `--checkpoint/--rotate/--compact` flags and with only
`--rate-rps 50` over the first 2000 lines.

### Before/after (rung 50, the acceptance rung)

| run | lag-rej | svc-rej | lag p99 med/max (us) | svc p99 med (us) |
|---:|---:|---:|---:|---:|
| cold44 baseline (sync) | 9/40 | 37/40 | 968 / 286,900 | 16,527 |
| cold44 after (overlapped) | 5/40 | 38/40 | 3,260 / 135,165 | 15,569 |
| cold44 after no-compact | 4/40 | 31/40 | 1,272 / 172,260 | 13,742 |
| c51 baseline (sync) | 2/40 | 37/40 | 2,040 / 480,001 | 13,695 |
| c51 after (overlapped) | 2/40 | 38/40 | 1,720 / 174,192 | 14,443 |

### Before/after (whole run: throughput + write amp + per-request over-ceiling count)

| run | late_total | lag-rej total | svc-rej total | achieved_rps | compact_bytes | compact_merges | elapsed_ms |
|---:|---:|---:|---:|---:|---:|---:|---:|
| cold44 baseline (sync) | 649 | 30 | 72 | 106 | 56,758,538 | 38 | 75,148 |
| cold44 after (overlapped) | 299 | 22 | 74 | 106 | 56,758,538 | 38 | 75,163 |
| c51 baseline (sync) | 1,497 | 27 | 72 | 106 | 56,758,271 | 38 | 75,172 |
| c51 after (overlapped) | 616 | 19 | 74 | 106 | 56,758,273 | 38 | 75,271 |

- **Throughput:** unchanged (achieved 106 rps, elapsed 75.1-75.3 s on both instances).
- **Write amp:** unchanged by construction and by measurement (identical `compact_bytes_total` and
  `compact_merges=38`; the compaction thresholds are the same).
- **The targeted cascade is removed:** `late_total` (per-request lags over 10 ms) falls 649 -> 299
  (cold44) and 1,497 -> 616 (c51), and the run's lag-rejected windows fall 30 -> 22 and 27 -> 19.
- **Service ceiling:** essentially flat (72 -> 74 rejected windows), still dominated by the
  durability barrier itself (the ~5-15 ms `fdatasync` against a 10 ms ceiling), the pre-existing
  tension recorded in tickets 54/55, not a regression introduced here.

## Verdict

**Stage 1 (the engine overlapped barrier) is sound and durability-preserving**, and the driver
rework removes the fsync-tail cascade that ticket 55 named as the rung-50 blocker. The per-group
proof: a group whose `fdatasync` tails to 335,540 us now dispatches with 56 us of lag
(`cold44 no-compact` window 18 `seq=24 fsync_us=335540 completion_us=353463 due_dispatch_us=56`);
the 104,701 us and 192,495 us tails on c51 land the same way (`window 15 seq=8` 77 us lag,
`seq=37` 12,589 us lag). The 400 ms-level next-group lag of the synchronous baseline no longer
appears.

**But rung 50 does NOT meet the lag ceiling**, so the acceptance is honest FAIL on the
"rung 50 meets lag p99 <= 10 ms" criterion. The remaining breaches are NOT the fsync tail; they are
two residual mechanisms, isolated with per-group evidence:

1. **Synchronous cold point reads on the request path (dominant).** The order flow executes its
   `cust` lookup and stock guard read synchronously inside each write group's dispatch, and the
   read shapes are synchronous too; a cold read blocks the single pacer and cascades exactly as the
   fsync tail did. Proof (`cold44 no-compact` window 18): `seq=25` (read) `completion_us=60186`
   blocks the pacer, then `seq=26 requests=3 due_dispatch_us=40339`, `seq=27 requests=4
   due_dispatch_us=61363`, `seq=28 requests=9 due_dispatch_us=172260` — the 172 ms lag group has
   `append_us=100 apply_us=20 fsync_us=6266`, i.e. its own write work is trivial; the lag was
   inherited from the pacer being blocked by the preceding cold reads. This is the same "secondary,
   run-dependent slow read" mechanism ticket 55 saw when the compaction worker streamed tiles; here
   it is the order-flow probes themselves, present with compaction OFF.

2. **Window-boundary barrier drain (secondary, introduced by the driver rework).** The driver
   drains every in-flight barrier at each window boundary so the window's stats and the
   compaction/apply paths see a drained engine; if a slow barrier is still in flight at the 1 s
   boundary, that drain blocks the pacer and the blocking lands as the NEXT window's first-group
   lag. Proof (c51 window 15 -> 16): window 15 `seq=37 fsync_us=192495 completion_us=192922` is
   still in flight when the window closes, so `drain_all_pending_groups` waits ~174 ms and window
   16 `seq=1 requests=9 due_dispatch_us=174192`. This moves a fraction of the fsync tail to the
   boundary instead of the next group — a large improvement over the synchronous baseline (which
   blocked inline on every slow group), but not zero.

Breach attribution across the three after-runs (11 lag-rejected windows total): ~7 are cold-read
cascades, ~4 are boundary-drain (including one marginal 12.8 ms case). Even with the boundary drain
removed, rung 50 would still fail on the cold-read cascade alone (e.g. cold44 no-compact windows
16/18/28), so the boundary drain is not the only blocker.

## Judging views (per the adopted §7.1)

- **Sustained-by-lag-ceiling-only:** NOT sustained at rung 50 (5/40 cold44, 2/40 c51). Honest FAIL.
- **Sustained-by-both-ceilings:** NOT sustained at rung 50 in either baseline (the service ceiling
  is breached by the durability barrier itself). Unchanged from the baseline; the service ceiling
  remains the pre-existing blocker recorded in tickets 54/55 and is judged separately, not the
  primary metric here.

## Residual / next steps (not implemented here)

- **Overlap the request-path reads** (or move the `cust`/stock probes off the pacer) so a cold read
  does not cascade into schedule lag. This is a different mechanism from the fsync barrier and is
  the remaining dominant blocker; it needs its own work package.
- **Make the window-boundary drain non-blocking** (record schedule-lag samples at dispatch, service
  samples at barrier completion, and let a rare cross-boundary barrier attribute only its service
  sample to the completion window). This removes the boundary-drain residual but, per the
  attribution above, does not by itself meet the lag ceiling.
- **Pre-registration reconciliation (open author item, NOT changed here):** the pre-registration's
  text that treats a service-ceiling-invalid window as invalidating the run conflicts with the
  adopted §7.1 decision that the primary metric is judged on the LAG ceiling with service latency
  reported separately. Left as a flagged reconciliation for the author.
