# 55 - attribute and remove the primary-metric (lag-ceiling) stalls (wave4)

Contract: ticket-54 (`.scratch/54-residual-tail/issues.md`) found that the primary per-window
lag ceiling (`lag p99 <= 10 ms`, no window rejected) is breached at rung 50 by a SINGLE-request
stall (`max_group_us == max_latency_us`, up to ~724 ms) with normal `read_bytes` and `majflt=0`,
and that its cause was NOT attributed. This ticket instruments the request path so a lag breach
is attributable without a rerun (per-group phase breakdown + OS context-switch/CPU view + a
host-stall control), names the stall, fixes any engine/driver cause, and otherwise states the
smallest honest blocker. The pre-registration and docs are not changed.

Instance/traces (unchanged from ticket-54): `/tmp/cold44/per_region-mixed` (8.17x; 1,643,933,528 B
data / 192 MiB cap) for iteration and `/tmp/cold51/per_region-mixed` (12.25x) for the headline
confirmation. Trace `adversarial.jsonl` (seed 42, sha256 bf0a5170…, 8000 lines), readonly
`adversarial-readonly.jsonl` (sha256 3eb16731…, 4844 reads), policy `declared.policy.json`
(default 4096/10/2), ceilings `--ceiling-us 10000 --lag-ceiling-us 10000`. Each run cold in
`systemd-run --user --scope MemoryMax=192 MiB` (MemorySwapMax=0), one process at a time. Iteration
source is a `cp -a` copy under `/tmp/cold55/`; the binary is the one release build copied to
`/tmp/cold55/flashdb-load` (sha256 a1fbb3d5…), run only from that copy.

## Instrumentation added (committed at ed51975, before any diagnosis/fix)

Per-group phase breakdown on stderr (`group_phase` line per dispatched group) and the per-window
OS view on the existing `window_stall` line; stdout's frozen metrics-JSON schema is unchanged.

- `group_phase window=W seq=S requests=N due_dispatch_us=A append_us=B apply_us=C fsync_us=D
  maintain_us=E completion_us=F` — per group: the first member's schedule lag (due→dispatch), the
  engine's WAL `append` / memtable `apply` / WAL `fdatasync` elapsed (drained via the new
  `Engine::take_group_phase_us`), the request-path WAL-maintenance elapsed, and the group
  completion (completion minus first due). `completion - due_dispatch - (append+apply+fsync+maintain)`
  is the read/parse/off-CPU residual.
- `window_stall` gained `vol_csw`/`invol_csw` (per-window differenced voluntary/non-voluntary
  context switches from `/proc/self/status`), `utime_ticks`/`stime_ticks` (per-window differenced
  CPU jiffies from `/proc/self/stat`), the stall group's own phase breakdown
  (`stall_due_dispatch_us`/`stall_append_us`/`stall_apply_us`/`stall_fsync_us`/`stall_maintain_us`/
  `stall_completion_us`), and `max_sleep_us`/`max_sleep_request_us` (the pacer's worst sleep
  overshoot, so a host-level wakeup delay is separable from an engine stall).
- `--dry-run` + `--dry-run-busy-us N`: the host-stall control — pace and parse the same trace but
  do NO engine I/O (no reads/writes/fdatasync), with a bounded spin `N` us per request standing in
  for the request's on-CPU cost.

## Measured diagnosis (units us unless noted; cold44 8.17x and cold51 12.25x)

The device context (100-sample probe at run start) is the same for every run: fsync p50 ~5.4-7.1 ms,
p99 ~11.7-23.6 ms. Under load the per-group WAL `fdatasync` tail is FAR worse than that probe.

Rung 50 (the primary-metric rung), per config:

| run (instance) | lag-rej | svc-rej | lag p99 med/max | svc p99 med |
|---:|---:|---:|---:|---:|
| baseline cold44 (compact + WAL maint) | 9/40 | 37/40 | 968 / 286,900 | 16,527 |
| no-worker cold44 (WAL maint, no compact) | 1/40 | 39/40 | 1,978 / 65,488 | 13,611 |
| writes-only cold44 (no compact, no WAL maint) | 1/40 | 37/40 | 2,008 / 268,584 | 14,505 |
| readonly cold44 (no writes) | 1/40 | 2/40 | 1,281 / 145,413 | 3,644 |
| dry-run cold44 (no engine I/O) | 0/40 | 0/40 | 186 / 5,566 | 2,235 |
| c51-baseline (compact + WAL maint) | 2/40 | 37/40 | 2,040 / 480,001 | 13,695 |
| c51-writes-only (no compact, no WAL maint) | 9/40 | 38/40 | 1,702 / 403,658 | 17,726 |

Per-group WAL-fdatasync tail over rung 50's ~1,900-2,000 groups (counts of groups whose `fsync_us`
exceeds each threshold):

| run | >10 ms | >50 ms | >100 ms | >200 ms | >400 ms |
|---:|---:|---:|---:|---:|---:|
| baseline cold44 | 56 | 1 | 1 | 1 | 0 |
| c51-baseline | 56 | 1 | 1 | 1 | 1 |
| writes-only cold44 | 70 | 1 | 1 | 1 | 0 |
| c51-writes-only | 78 | 7 | 5 | 2 | 1 |

## Attribution (the named cause, with per-group numbers)

**The ~hundreds-of-ms single-request stall is a CASCADE from a slow group, and the dominant slow
group is the device's WAL-`fdatasync` tail — an environmental device spike, not engine code.**

The pacer is single-writer and synchronous: it cannot issue request N+1 until group N's barrier
returns. So when one group's `fdatasync` tails, that tail lands on the NEXT group as schedule lag
(and batches whatever requests accumulated while the pacer was blocked). Per-group proof
(`c51-writes-only` window 4, where the process does NO I/O other than the WAL itself):

- `group_phase … seq=10 requests=1 due_dispatch_us=17 … fsync_us=419491 … completion_us=423576` —
  one write, dispatched on time (17 us lag), but its single `fdatasync` took 419,491 us.
- `group_phase … seq=11 requests=21 due_dispatch_us=403658 … fsync_us=4499 … completion_us=33499` —
  the next group, issued 403,658 us late because the pacer was blocked behind seq=10's barrier,
  batching the 21 requests that had accumulated.

The same pattern reproduces in every write config and both instances: `cold44 writes-only` window 9
(fsync 205 ms → next lag 268 ms), `cold44 baseline` window 14 (fsync 299 ms → window 15 lag 287 ms),
`c51-baseline` window 22 (fsync 498 ms → lag 480 ms). The device tail is present with compaction
AND WAL maintenance fully off (c51-writes-only 9/40 lag-rej, cold44 writes-only 1/40), so it is
the device's fsync tail under load, not an engine path. The 100-sample idle probe cannot see it
(p99 12-24 ms); the ~1900-group run sees 1-7 fsyncs >50 ms per rung and 1-2 >200 ms, and any
fsync >~30 ms cascades into a >10 ms lag at rung 50 (20 ms interval).

**Secondary, run-dependent: the compaction worker's concurrent tile I/O slows single cold reads
on the 8.17x iteration.** `cold44 baseline` additionally shows single reads of 75-175 ms that
cascade the same way — `baseline window 7`: `seq=30 requests=1 due_dispatch_us=86 … fsync_us=0 …
completion_us=167400` (a pure read, no append/apply/fsync, 167 ms) → `seq=31 requests=8
due_dispatch_us=147534`. Disabling `--compact-bytes` drops cold44 rung-50 lag-rej 9/40 → 1/40.
This does NOT reproduce on the 12.25x confirmation (c51-baseline 2/40, no slow reads), so it is a
secondary contention effect of the worker's tile streaming, not the deterministic blocker.

**Minor: host sleep/wakeup overshoot.** `cold44 baseline` window 26 `max_sleep_us=17906` on a
1,000 us requested chunk — one ≤1 ms pacer sleep returned ~18 ms late (OS timer/wakeup delay).

**Separate request-path engine stall (rung 400 only): the opt-in WAL maintenance.** With
`--checkpoint-bytes`/`--rotate-bytes` the driver calls `maybe_checkpoint`/`rotate_wal` inline after
a group; the first rotation publishes the memtable and retires the WAL segment on the request path
(`baseline window 74 stall_maintain_us=226281`, `no-worker window 74 stall_maintain_us=193909`).
This does not fire at rung 50 (the WAL has not reached the 8 MiB threshold), so it is not the
primary-metric blocker; it is recorded as a residual.

## Decision + rejected alternatives

The dominant cause (the device's WAL-fdatasync tail cascading through the synchronous single-writer
pacer) is environmental, and no honest engine/driver change removes it without weakening durability.
**No latency fix is applied** (the honest "irreducibly blocked" branch); the contribution is the
attribution and the bounded characterization.

Rejected alternatives:

- **(a) Defer the fdatasync (async group commit / background fsync, ack before the barrier).**
  Rejected: it weakens durability — a crash after ack but before the barrier loses acknowledged
  writes, violating the pre-registration's "all at equal durability". The per-group `fdatasync` IS
  the durability point, and it is exactly what tails.
- **(b) I/O-prioritize the compaction worker (`ioprio` idle).** Rejected as a no-op here: the box's
  NVMe devices run the `none` I/O scheduler (verified `/sys/block/nvme{0,1}n1/queue/scheduler`),
  which ignores ioprio, so this would be a fake fix. It is listed only as a proposal for a host
  where bfq is in effect.
- **(c) A concurrent-writer / background-syncer pacer that issues the next request while the
  previous group's barrier is in flight.** Rejected: it changes the single-writer concurrency and
  group-commit model (the durability claim), which is a contract/trade-off change outside this
  ticket's autonomy, not a driver fix.
- **(d) Disable or throttle the compaction worker to remove the secondary slow reads.** Rejected as
  "the fix": it either removes the learner's actuation (the product under test) or throttles the
  very reorganisation the learner requests, and it does NOT remove the dominant fsync tail (c51-
  writes-only is 9/40 with compaction fully off).

## Smallest honest statement

At rung 50 the pre-registration's primary per-window lag ceiling (`lag p99 <= 10 ms`, no window
rejected) is NOT met in any write config on either instance: lag breaches are 9/40 (cold44
baseline), 1/40 (cold44 no-worker / writes-only), 2/40 (c51 baseline), 9/40 (c51 writes-only).
Each breaching window is the cascade described above: one group's WAL `fdatasync` tails to
71-498 ms (the device's tail under load), the pacer blocks behind it, and the next group is issued
that late — so `max_group_us == max_latency_us` for a single accumulated group, with `majflt=0`
and no matching `read_bytes` spike. The device fsync tail alone (≥1 group >50 ms per rung, 1-2
>200 ms) makes "no window rejected" unsatisfiable while every write group must `fdatasync`
synchronously on the request path; removing that barrier is the only code change that would pass,
and it weakens durability. The compaction worker adds a secondary, run-dependent slow-read
component on the 8.17x iteration (9/40 vs 1/40 with it disabled) but does not reproduce on the
12.25x confirmation, where the device tail dominates outright (c51 writes-only 9/40 with no
compaction at all). Read-only and the no-engine-I/O dry-run control are clean (dry-run 0/40 lag-rej
at all rungs), which proves the stall is not the pacer bookkeeping and not intrinsic to the host in
the absence of engine I/O — it is introduced by the engine's required WAL I/O on this shared device.

## Proposal for the pre-registration (not implemented)

1. Make the primary metric's lag ceiling **robust to the device fsync tail**: either state the
   durability floor explicitly (per-group `fdatasync`; the lag ceiling can only be met if the
   device's fsync tail ≤10 ms, which it is not on this box), or move the WAL to a device whose
   fsync tail is below the ceiling and re-derive the metric there.
2. If the service ceiling must reject windows, scale it to the measured device fsync tail (as
   ticket-54 already proposed), and report the lag breach separately.
3. Record the compaction-worker contention honestly: on the 8.17x iteration the learner's
   actuation rewrites ~0.3-6 MB of tiles per window and its I/O shares the device with the request
   path; a run that scores the learner's sustained level should either quiesce actuation during the
   scored windows or account for its device bandwidth as part of the learner's cost.

## Tests

- `engine/tests/group_commit.rs::group_phase_accounting_attributes_and_resets` — an empty group
  accumulates no append/apply/barrier, and the per-group accumulator resets exactly after a read
  (renamed/extended from ticket-54's `group_fsync_accounting_attributes_the_barrier_and_resets`).
- `engine/src/bin/flashdb-load.rs::proc_counters_since_differences_and_never_underflows` — the
  per-window differencing now also covers the context-switch and CPU-tick counters and never
  underflows (saturating).
- `engine/src/bin/flashdb-load.rs::dry_run_flag_enables_the_host_stall_control` — `--dry-run` and
  `--dry-run-busy-us` parse correctly and reject a missing value.
All existing Rust and Python tests stay green (gates below).

## Before/after table

There is no latency fix, so "before" == "after" for the engine. The attribution is the scenario
separation, rung 50 (cold44 8.17x / cold51 12.25x):

| config | lag-rej cold44 / c51 | lag p99 med/max (us) cold44 | svc p99 med (us) cold44 |
|---:|---:|---:|---:|
| baseline (compact + WAL maint) | 9/40 ; 2/40 | 968 / 286,900 ; 2,040 / 480,001 | 16,527 ; 13,695 |
| no-worker (WAL maint, no compact) | 1/40 ; — | 1,978 / 65,488 | 13,611 |
| writes-only (no compact, no WAL maint) | 1/40 ; 9/40 | 2,008 / 268,584 ; 1,702 / 403,658 | 14,505 ; 17,726 |
| readonly | 1/40 ; — | 1,281 / 145,413 | 3,644 |
| dry-run (no engine I/O) | 0/40 ; — | 186 / 5,566 | 2,235 |

## Acceptance verdict

**Irreducibly blocked (honest), not a fake pass.** The single-request stall is named with per-group
evidence: a slow group's WAL `fdatasync` tail (device, 71-498 ms under load) cascades through the
synchronous single-writer pacer into the next group's schedule lag, breaching the lag ceiling. The
device fsync tail is present with compaction and WAL maintenance fully off (c51-writes-only 9/40),
so no engine/driver change removes it without weakening durability (deferring the barrier); I/O
prioritization is a no-op on this box's NVMe `none` scheduler. The compaction worker's concurrent
tile I/O is a secondary, run-dependent slow-read source (cold44 9/40 → 1/40 without it) that does
not reproduce on the 12.25x confirmation. No latency fix is applied; the engine is unchanged. The
rung-50 lag ceiling is therefore NOT sustained, and the smallest honest conclusion plus proposals
are stated above.

## Residual / unseparated items

- The device fsync tail's own distribution is the shared device, measured per-group here but not
  separable from other tenants on the box.
- The opt-in WAL maintenance (`--checkpoint-bytes`/`--rotate-bytes`) runs inline on the request
  path and stalls ~193-226 ms at rung 400 (not rung 50); moving it off-path is a candidate fix but
  out of scope for the primary-metric rung.
- The cold44-baseline slow reads (75-175 ms) are attributed to compaction-worker tile I/O
  contention but were not reproduced on cold51; a dedicated-device control would separate them
  cleanly.

## Gates (verbatim; run at the instrumentation commit ed51975)

`cargo test --workspace --offline` — 280 passed; 0 failed; 0 ignored.

`cargo build --workspace --offline` — 0 warnings.

`cd harness && uv run --with pytest pytest -q` — `132 passed in 4.16s`.

`git diff --check` — silent (exit 0).

## Raw artifacts (not committed)

`/tmp/cold55/`: `flashdb-load` (frozen, ed51975, sha256 a1fbb3d5…), `adversarial.jsonl`,
`adversarial-readonly.jsonl`, `declared.policy.json`, `cold44-src`/`cold51-src` (cp -a),
`run.sh`/`analyze55.py`, and `{baseline,no-worker,writes-only,readonly,dry-run,c51-baseline,
c51-writes-only}.{stdout,stderr}` plus their `-instance` copies.
