# 92 — the ~0.5 s barrier-start stall at 200 rps, and the WAL preallocation candidate

Contract: none yet from root; opened 2026-10-27 by the node that owns the roadmap, from the M4 ranked
fix list (`docs/6_ARCHITECTURE.md` §7 #2 and #4) and the box-free work the roadmap schedules while M5
waits on the quiet window (`docs/0_ROADMAP.md` §Current milestone, WP-14).

## What was re-read, and the two stale facts it corrects

The M4 report said "no per-phase timestamp exists" and "ticket 82 did not keep stderr". Both are false
against the tree and the record:

- The per-group phase timer has existed since slice 55/56: `GroupPhaseReport` in
  `engine/src/bin/flashdb-load.rs` prints `group_phase window=… seq=… requests=… due_dispatch_us=…
  append_us=… apply_us=… fsync_us=… maintain_us=… completion_us=…` and `window_stall …` to stderr.
- The M1 stderr was kept as
  `.scratch/82-controlled-m1/runs/b{1..5}/rung-200/driver-flashdb-declared.log`.
  `grep -c 'group_phase\|window_stall' .scratch/82-controlled-m1/runs/b2/rung-200/driver-flashdb-declared.log`
  → 3,977.

Re-reading the five 200 rps blocks changes the 573 ms attribution:

| block | window 10 max group (us) | stall group's own `fsync_us` | stall group's `append_us` | `maintain_us` | lag p99 (us) | rejected? |
|---|---:|---:|---:|---:|---:|---|
| b1 | 475,967 | 10,672 (window stall) | ≤ 70 | 0 | 5,690 | no |
| b2 | 573,458 | 20,903 | 28 | 0 | 30,430 | **yes** |
| b3 | 583,421 | 36,354 | 11 | 0 | 33,747 | **yes** |
| b4 | 404,334 | 8,988 | ≤ 86 | 0 | 917 | no |
| b5 | 431,950 | 19,841 | ≤ 114 | 0 | 1,078 | no |

The large group is in **window 10 of every block**, not only the two the lag rule rejected. Its own
`fdatasync` is 9-36 ms, max append over the whole block is 114 us, and maintenance/compaction bytes are
zero. The falsifier "a single fdatasync itself taking ~570 ms → device/environment" therefore **does
not fire**; the ~0.4-0.55 s unaccounted time is the barrier job's **queue wait before the worker starts
it**, which `GroupPhaseReport` has no field for. In b2 window 10, ten single-request groups complete
within ~30 ms of one another after waiting 540-573 ms, each with a ~3 ms own fsync — the shape of a
queue draining after the worker was not running, not of one slow sync.

## The two candidates

1. **Missing timestamp** (instrument): stamp each `WalAppendSyncJob` at submission and again when the
   worker starts it; report `barrier_wait_us = start - submit`. This decides whether the worker was
   idle (scheduler/device, category (c)) or busy with an earlier job (the queue is then bounded by
   `MAX_PENDING_BARRIERS = 64`, so the answer is arithmetic).
2. **WAL segment growth** (fix #2, the mechanism candidate): the WAL is appended one frame at a time to
   a growing file (`engine/src/wal.rs` has no `posix_fallocate`/chunked `set_len`; `set_len` is used
   only to truncate on recovery). At 200 rps one frame per request, the event recurs at the same point
   in every 20 s block, consistent with crossing a filesystem allocation boundary. The proposed switch
   preallocates a chunk and appends inside it, off by default.

## What WP-14 must produce

- `group_phase` carries `barrier_wait_us`, so queue wait + append + fsync + apply + dispatch has no
  unexplained residual for the stall group.
- The M1 rung-200 trace replayed at 200 rps reproduces a window-10 stall in at least 3 of 5 blocks.
  This is a **work-per-request count**, not a rate, so foreign load does not invalidate it; no
  throughput number is claimed from the replay.
- Preallocation on/off on the same replay: queue wait removed, or the remaining cause named.
- The switch off leaves `file_len == valid_len` and every pre-existing WAL test unchanged; on, the file
  is really allocated (`metadata().blocks()*512 >= chunk*3/4`, not a sparse hole), fills in chunks, and
  `open_or_recover` replays exactly the appended payloads and truncates the zero tail.
- The pinned-flag workspace suite is green, and the log states its loadavg on line 1.

## Reproduce the re-read

```sh
cd /home/z/vibe/flashdb
grep -h "window_stall" .scratch/82-controlled-m1/runs/b{1,2,3,4,5}/rung-200/driver-flashdb-declared.log \
  | awk '{for(i=1;i<=NF;i++){if($i~/^window=/)w=$i; if($i~/^stall_completion_us=/)c=$i; if($i~/^stall_fsync_us=/)f=$i} print w,c,f}' \
  | sort -t= -k2 -nr | head
python3 - <<'PY'
import json
for b in ['b1','b2','b3','b4','b5']:
    ws=[json.loads(l) for l in open(f'.scratch/82-controlled-m1/runs/{b}/rung-200/windows-flashdb-declared.jsonl')]
    w=ws[10]
    print(b, w['lag_p99_us'], w['group_elapsed_us'], w['reject_reason'])
PY
```

## WP-14 measurement (2026-10-27, worktree `wp14-barrier-stall` @ `0b1f038`+edits)

Instrument and replay are committed with this ticket: `run.sh` (the command that
produced every log below), `summarize.py` (the parser that prints the tables),
`logs/replay-*.out` (raw driver stderr+stdout per block, line 1 = the block's
loadavg) and `logs/summary-*.out`. The replay is a **work-per-request counter**,
not a rate: no throughput number is claimed and none is printed by this ticket.

### The instrument

`group_phase` now carries `barrier_wait_us` (and `window_stall` carries
`stall_barrier_wait_us` plus `stall_residual_us`). The engine stamps each
`WalAppendSyncJob` at submission and again when the worker starts it; the
difference is carried on `BarrierResult`/`BarrierCompletion` as `wait_us` and
never touches the append/sync order or the durability contract (the worker still
appends then `fdatasync`s each group FIFO, and a request is acked only after its
own covering barrier completes). `residual_us` is
`completion_us - (due_dispatch + barrier_wait + append + fsync + apply + maintain)`,
i.e. the pacer's dispatch-to-submit work plus read execution for a read-only
group. On every barrier-owning group that stalled below, it is < 800 us.

### Replay protocol

The frozen M1 rung-200 trace (`82-controlled-m1/runs/b2/rung-200/trace.jsonl`,
sha256 `6472cf46…b8f93f`) replayed at 200 rps, 1 s windows, `--group-requests 64`,
`--lag-ceiling-us 10000`, onto a fresh `cp -a --reflink=auto` of the frozen
`snapshot/flashdb-declared` per block (the original is never mutated). Two
conditions, each with the preallocation switch off and on (16 MiB chunk, the
postgres segment size behind the WP-6 candidate):

- `cap128` — under the M1 arm's own `MemoryMax=128M` scope (`summary-cap128.out`);
- `nocap` — no cgroup memory cap (`summary-nocap.out` off, `summary-nocap-on.out` on).

The box was not quiet during any block (loadavg 6-13 in the logs; M1 was 0.33).
That is deliberate: the ticket states the stall is a work-per-request count.

### Attribution: the queue wait is the preceding barriers' own fdatasync

Per block, the maximum `completion_us` group and its split (us), no memory cap,
preallocation **off**:

| block | loadavg | window | completion | barrier_wait | append | fsync | apply | residual |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| b1 | 7.79 | 12 (read-only max) | 35,778 | 0 | 0 | 0 | 0 | 35,643 |
| b2 | 7.27 | 4 | 222,351 | 204,921 | 10 | 17,009 | 7 | 344 |
| b3 | 6.00 | 3 | 297,524 | 286,115 | 24 | 10,679 | 12 | 623 |
| b4 | 7.11 | 3 | 209,784 | 190,076 | 23 | 19,232 | 8 | 428 |
| b5 | 7.80 | 3 | 188,970 | 182,803 | 23 | 5,389 | 11 | 680 |

The stall is the queue wait; append is 10-24 us. The wait is not a worker that
was idle: walking the `group_phase` lines backwards from the stalled group (they
are emitted in FIFO completion order), the immediately preceding barriers' own
append+fdatasync already sum to the wait — e.g. b3: 9 prior barriers, 331,417 us
of their own service, against a 290,258 us wait; b2: 13 barriers / 213,493 us
against 204,921 us; b4: 10 / 200,850 against 195,662 us; b5: 6 / 197,565 against
183,276 us. The fdatasync burst is 5-19 ms per barrier on this box (18-37 ms in
the on-arm), the FIFO queue grows to 6-13 barriers, and `MAX_PENDING_BARRIERS`
(64) is the bound; the driver keeps draining, so the wait drains with it. No
single fdatasync is 300-600 ms (the M1 falsifier still does not fire) — the sum
over the queue is.

This reproduces the M1 shape: queue wait hundreds of ms, own append tens of us,
own fdatasync tens of ms, apply us. Four of five no-cap off blocks show a max
group completion > 100 ms (b2-b5); b1's max is a read-only group at 35.8 ms.

### The window is not 10 here

The stall lands in **windows 2-4**, not window 10, on every run above. It is a
burst of slow `fdatasync`s that builds the queue, and when it arrives depends on
the box: at loadavg 6-13 it arrives ~2-4 s into the 20 s run; on M1's quiet box
(0.33) it arrived ~10 s in. The window index is when the device stalled, not a
property of the barrier mechanism, so the literal "window 10" reading is **not
reproduced** on this loaded box; the stall itself (>100 ms max group, queue-wait
dominated, no residual) is, in 4 of 5 off blocks and 5 of 5 on blocks.

### Preallocation verdict: rejected as the mechanism

Same replay, 16 MiB real-allocated chunks appended inside, no memory cap:

| block | completion | barrier_wait | append | fsync | residual | prior barriers' service vs wait |
|---|---:|---:|---:|---:|---:|---|
| b1 | 253,251 | 234,002 | 22 | 18,476 | 676 | 10 / 275,390 vs 247,777 |
| b2 | 253,246 | 231,652 | 25 | 21,193 | 280 | 9 / 250,958 vs 247,861 |
| b3 | 390,920 | 355,204 | 30 | 35,000 | 0 | 11 / 418,754 vs 385,605 |
| b4 | 401,080 | 363,780 | 23 | 36,706 | 399 | 13 / 401,067 vs 395,426 |
| b5 | 370,564 | 349,525 | 35 | 20,397 | 530 | 11 / 391,707 vs 364,923 |

**The queue wait does not disappear with preallocation on — it is the same
mechanism, and in this run larger.** Append stays 22-35 us in both arms, so the
WAL never spends the stall inside the write; the queue is built by `fdatasync`
service time (18-37 ms here), which preallocation cannot change. Under the 128M
cap the same verdict holds: on b5 the max group is a 142,406 us wait behind 7
barriers' 148,329 us of service. The switch is kept **off by default**; it is a
layout knob, not a fix for this stall. The remaining cause is named: a burst of
slow WAL `fdatasync`s (device/kernel latency, category (c) environment) queues
FIFO barriers, and the group's latency is the sum of the barriers ahead of it.

### The switch itself (off-by-default proof)

The live driver's WAL after each block (line `wal_after` in every replay log):

- off: `len=6,900,595 blocks=13,480` (6.90 MB, `len == valid_len` by the unit test);
- on: `len=16,777,216 blocks=32,768` — exactly 16 MiB, `blocks*512 = 16,777,216`,
  no sparse hole.

Unit tests (all green, `cargo test -p flashdb-engine`):
`wal::tests::prealloc_off_keeps_file_len_equal_to_valid_len`,
`wal::tests::prealloc_reserves_really_allocated_chunks` (grows by whole chunks and
checks `metadata().blocks()*512 >= chunk*3/4`),
`wal::tests::prealloc_recovery_replays_exactly_and_truncates_zero_tail` (replays
exactly the appended payloads and truncates the zero tail),
`engine::tests::prealloc_engine_appends_in_chunks_and_recovers_exactly`,
`engine::tests::barrier_completion_reports_queue_wait`.

Pinned workspace suite at this revision (line 1 = loadavg):
`logs/workspace-pinned.out` — `cargo --config
'source.crates-io.replace-with="aliyun"' ... test --workspace --offline`
returned exit 0 with **345 passed, 0 failed** (28 test binaries), loadavg
8.15 8.25 9.39 at start.

### Reproduce

```sh
cd <worktree>
cargo build --release --bin flashdb-load
BLOCKS=5 ARMS="off on"                .scratch/92-barrier-stall/run.sh   # cap128 (summary-cap128.out)
SUMMARY=summary-nocap.out     LOG_TAG=-nocap     CAP_MIB=0 ARMS=off BLOCKS=5 .scratch/92-barrier-stall/run.sh
SUMMARY=summary-nocap-on.out  LOG_TAG=-nocap-on  CAP_MIB=0 ARMS=on  BLOCKS=5 .scratch/92-barrier-stall/run.sh
```
