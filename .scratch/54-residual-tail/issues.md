# 54 - separate and remove the residual latency tail (wave3 residual)

Contract: ticket-53 (`.scratch/53-compaction-tail-latency/issues.md`) moved the load-path
compaction build off the request thread, but its own acceptance was honest: no rung is fully
sustained because the ticket-49 "residual" — the median per-window svc p95 that survives with
compaction AND WAL maintenance removed (ticket-49 `neither`: 11,839/16,963/39,511/41,102 us at
rungs 50/100/200/400) — was never attributed. Ticket-49's candidate list was cold-cache read
page-ins (1.64 GB data under a 192 MiB cap), the allocator (`trim_heap`/`malloc_trim(0)` once per
window on the request thread), and OS scheduler jitter. This ticket measures which sub-mechanism
that residual actually is, fixes it if it is an engine/driver defect, and otherwise states the
smallest honest blocker. The pre-registration is not changed.

Instance: `/tmp/cold44/per_region-mixed` (8.17x; 1,643,933,528 B data / 192 MiB cap) for iteration
and `/tmp/cold51/per_region-mixed` (12.25x) for the headline confirmation. Trace
`adversarial.jsonl` (seed 42, sha256 bf0a5170…, 8000 lines: 2408 stock + 2436 customer + 3156
order-flow writes), policy `declared.policy.json` (default 4096/10/2), ceilings
`--ceiling-us 10000 --lag-ceiling-us 10000`. Each run cold in `systemd-run --user --scope
MemoryMax=192 MiB` (warm probe: 2048 MiB), one process at a time. Frozen drivers under
`/tmp/cold54/` (not committed): `09a0998a…` (proc/trim instrumentation) and `eef1bce2…` (adds the
group-barrier fdatasync counter); the headline runs use `eef1bce2…`.

## Instrumentation added (committed at c36b9c3 + 6ec1ea3, before any fix)

One `window_stall` line per window on stderr (stdout's frozen metrics-JSON schema is unchanged):

- `minflt`/`majflt` — `/proc/self/stat` fields 10/12, differenced (soft/hard page faults).
- `rchar`/`read_bytes`/`wchar`/`write_bytes` — `/proc/self/io`, differenced; `read_bytes` is the
  bytes the block layer actually fetched from storage (a page-cache miss), so `read_bytes`/`rchar`
  separates a cold page-in from a cache-hit read without touching the read path.
- `trim_us` — elapsed time of the request-thread `trim_heap()` call at the window boundary.
- `group_fsync_us` — sum of the per-group WAL `fdatasync` elapsed time (`Engine::end_group` →
  `Wal::sync`), accumulated in `Engine::group_fsync_us` and drained by `Engine::take_group_fsync_us`
  once per window.
- `max_latency_us`/`max_lag_us`/`max_group_us` — the window's absolute single-request stall maxima.

## Measured diagnosis (cold44 8.17x; units us unless noted)

`group_fsync_us` = the whole window's WAL `fdatasync` sum; `trim_us` = the one boundary call;
`read_bytes` = actual storage fetches. `svc p95 med` = median of the windows' per-window p95
(ticket-49 convention).

### Config 1 baseline — compaction + WAL maintenance on (the shipped WP-B regime), 192 MiB

| rung | ach | svc p50 | svc p95 med | svc p99 med | lag p99 med/max | svc-rej | lag-rej | group_fsync med/max | trim max | read_bytes med |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 50 | 1,281 | 12,085 | 16,923 | 2,691/695,429 | 39/40 | 9/40 | 119,071/791,437 | 4,979 | 1,122,304 |
| 100 | 100 | 2,198 | 13,780 | 21,537 | 9,337/76,665 | 20/20 | 10/20 | 267,289/352,112 | 1,339 | 2,207,744 |
| 200 | 200 | 12,438 | 53,235 | 80,464 | 48,424/114,429 | 10/10 | 10/10 | 493,151/585,327 | 1,679 | 3,842,048 |
| 400 | 393 | 19,947 | 135,401 | 157,901 | 106,452/333,992 | 5/5 | 5/5 | 615,810/653,522 | 2,118 | 7,532,544 |

### Config 2 readonly — SAME trace, writes filtered out (4844 reads), compaction/WAL off, 192 MiB

Reads alone, fully cold: median svc p95 2,968/2,653/4,263 us (rungs 50/100/200), median svc p99
4,726/4,660/6,566 us, `read_bytes` 983 KB–3.4 MB/window, `majflt=0`, `trim_us` max 184 us.
3/65 windows invalid, all from rare 300 ms-level scheduler spikes with NO corresponding `read_bytes`
increase (3 windows: max_group 360/23/299 ms vs read_bytes 0.9/1.6/1.8 MB — normal).

### Config 3 warm — full trace, cap 2048 MiB (whole 1.64 GB instance fits; cgroup peak 1.72 GB)

`read_bytes=0` in EVERY window (no page-ins). Median svc p95 9,307/10,294/14,152/21,650 us,
`group_fsync` med 112,917/244,548/500,682/739,015 us.

### Config 4 neither — full trace, compaction off + WAL maintenance off, 192 MiB (ticket-49 residual)

Median svc p95 11,150/12,371/17,659/34,128 us (reproduces ticket-49 `neither` 11,839/16,963/
39,511/41,102 within session drift), `group_fsync` med 124,994/253,281/516,005/653,117 us,
`trim_us` max 116–265 us, `majflt=0`, `read_bytes` 1.1–7.8 MB/window.

### cold51 confirmation (12.25x, baseline, compaction + WAL on)

| rung | svc p95 med | svc p99 med | lag p99 med/max | svc-rej | lag-rej | group_fsync med | trim max | read_bytes med |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 12,046 | 19,192 | 2,193/303,956 | 38/40 | 7/40 | 127,808 | 1,263 | 1,142,784 |
| 100 | 10,508 | 13,834 | 3,836/165,672 | 20/20 | 3/20 | 242,799 | 1,595 | 2,097,152 |
| 200 | 20,044 | 27,744 | 12,810/53,834 | 10/10 | 9/10 | 503,517 | 1,982 | 4,067,328 |
| 400 | 34,806 | 96,306 | 23,827/226,220 | 6/6 | 6/6 | 601,397 | 2,121 | 7,012,352 |

## Attribution (the dominant sub-mechanism, with numbers)

**Dominant: the per-group WAL `fdatasync` (the deferred-barrier durability point), on the request
thread.**

- `group_fsync_us` is 119–654 ms PER WINDOW (baseline) / 113–739 ms (warm), and it grows with the
  write rate exactly as the tail does. At rung 50 there are ~20 write-groups/window, so the barrier
  is ~5–7 ms per group — the same-device fsync probe measures `device_fsync_p50_us=5176`,
  `device_fsync_p99_us=10528` (baseline). The whole window's fdatasync sum is 1–2 orders of
  magnitude larger than every other candidate.
- **Cold read page-ins are NOT dominant.** `readonly` (fully cold, `read_bytes` up to 3.4 MB/window)
  gives median svc p95 2.7–4.3 ms. `warm` (read_bytes=0, the whole instance cached) STILL gives
  median svc p95 9.3–21.7 ms. The page-in share is the cold-minus-warm gap: ~2 ms at rung 50
  (11.2 vs 9.3), i.e. ~15–20% of the residual; the other ~80% persists with zero page-ins.
- **Allocator is NOT dominant.** `trim_us` max is 4,979 us in the baseline (compaction's transient
  buffers) and 116–265 us in `neither`; it runs once per window at the boundary (after the window's
  requests, before `window_start` is re-seeded) and is ≤ 1/20th of the fdatasync sum.
- **No page-fault storm.** `majflt=0` in every window: the read path uses `pread`, not mmap, so a
  cold tile read is an ordinary `read()`/`pread()` (counted in `read_bytes`), never a major fault.
- **Scheduler jitter** exists as rare 300 ms-level spikes (3/65 readonly windows) with no matching
  I/O, but it is not the median tail.

What remains unseparated: the fdatasync's OWN tail (why a few barriers reach 10–15 ms) is the
shared device (`device_fsync_p99_us=10528`), not separable from the engine; and the rare
multi-hundred-ms scheduler spikes (no I/O correlate) are un-attributed to any engine code.

## Decision + rejected alternatives

The dominant mechanism is the required durability barrier, NOT an engine/driver defect, so this
ticket applies NO latency fix (the honest "irreducibly blocked" branch): the residual is
characterized and the blocker stated, with a proposal for how the pre-registration can treat it.

Rejected alternatives:

- **(a) Defer the fdatasync (background fsync / async group commit, ack before the barrier).**
  Rejected: it would weaken durability — a crash after ack but before the barrier loses
  acknowledged writes, violating the pre-registration's "all at equal durability: same group-commit
  batch-size distribution". The `fdatasync` per group IS the durability point.
- **(b) Hold groups open to batch more writes per barrier (a `commit_delay`).** Rejected: at
  rungs 50/100 writes are ~25/12 ms apart, so any delay long enough to catch a second write adds
  more latency than the ~5 ms barrier it saves, and at rung 50 it catches nothing. It would also
  change the slice-47 latency-trust model ("never held open to fill").
- **(c) Reduce cold read page-ins / trim_heap.** Rejected as the fix: both are already small
  (~2 ms page-in share; ≤ 5 ms boundary trim) and neither is the dominant term, so fixing them
  would not move the metric.
- **(d) Read-path bloom/readahead tuning.** Rejected: the read path alone measures 2.7–4.3 ms
  (readonly), under the 10 ms ceiling, so it is not the blocker.

Chosen: keep the engine unchanged; record the characterization and the smallest honest statement
below, and propose (not implement) the pre-registration treatment.

## Smallest honest statement

The two lowest rungs fail the **service** ceiling (10 ms), not the pre-registration's own lag
ceiling: rung 50 lag p99 median is 2,193–2,691 us and rung 100 lag p99 median 3,836–9,337 us, while
service p99 median is 16.9/21.5 ms (baseline) — because a write request waits for its group's single
WAL `fdatasync`, measured at 5,176 us p50 / 10,528 us p99 on this device, and at low offered load
group commit cannot batch writes (one write per group). That barrier is the equal-durability cost,
not an engine/driver defect, so no honest code change removes it without weakening durability.

## Proposal for the pre-registration (not implemented)

1. Judge the primary sustained-load metric on the **lag ceiling alone** (`lag p99 <= 10 ms`, the
   pre-registration's own number), and report the service-latency distribution as the efficiency
   measure the pre-registration already calls it — decoupling the service ceiling from window
   rejection. A fixed 10 ms service ceiling is below the device's own fdatasync p99 (10.5 ms), so
   it is unsatisfiable by any engine that must `fdatasync` per write group on this box.
2. If the service ceiling must reject windows, make it **relative to the measured device fdatasync**
   (e.g. `N × device_fsync_p99`), so it scales with the device instead of an absolute number.
3. Alternatively, state the durability floor explicitly: under group commit with per-group
   `fdatasync`, the minimum service latency floor is the device fsync p99, and score the engine
   against that stated floor rather than a fixed ceiling.

## Tests

- `engine/tests/group_commit.rs::group_fsync_accounting_attributes_the_barrier_and_resets` — an
  empty group takes no barrier (counter stays 0), and the accumulator resets exactly after a read.
- `engine/src/bin/flashdb-load.rs::proc_counters_since_differences_and_never_underflows` — the
  per-window counter differencing never underflows (saturating).
All existing Rust and Python tests stay green (gates below).

## Before/after table

There is no latency fix, so "before" == "after": the shipped WP-B regime (compaction + WAL
maintenance on, 192 MiB, cold44) is the `baseline` table above, and the cold51 confirmation matches
it. The honest acceptance is therefore the `OR` branch (characterization + blocker), not a
sustained-rung claim.

## Acceptance verdict

**Irreducibly blocked (honest), not a fake pass.** No rung sustains the 10 ms SERVICE ceiling
because a write group's WAL `fdatasync` (5.2 ms p50 / 10.5 ms p99 on this device) cannot be
amortized at low offered load. The pre-registration's PRIMARY lag ceiling is met at rung 50
(lag p99 median 2.2–2.7 ms) and borderline at rung 100 (lag p99 median 3.8–9.3 ms). The blocker is
the durability barrier, not an engine/driver defect, not allocator/scheduler jitter, not cold reads.
Throughput and write amp are unchanged (no code change).

## Residual / unseparated items

- The fdatasync's own tail (barriers reaching 10–15 ms) is the shared device's fsync distribution;
  not separable from the engine.
- Rare 300 ms-level scheduler/disk spikes (3/65 readonly windows) have no I/O correlate and remain
  un-attributed.
- `trim_us` is up to ~5 ms in the compaction-on baseline (transient merge buffers); minor and not
  dominant, left unchanged.

## Gates (verbatim; run at 6ec1ea3)

`cargo test --workspace --offline` — 279 passed; 0 failed; 0 ignored.

`cargo build --workspace --offline` — 0 warnings.

`cd harness && uv run --with pytest pytest -q` — `132 passed in 5.74s`.

`git diff --check` — silent (exit 0).

## Raw artifacts (not committed)

`/tmp/cold54/`: `flashdb-load` (frozen, eef1bce2…), `adversarial.jsonl`,
`adversarial-readonly.jsonl`, `declared.policy.json`, `run.sh`/`run2.sh`/`analyze.py`,
`{baseline,readonly,warm,neither,c51-baseline}.{stdout,stderr}` and their `-instance` copies.
