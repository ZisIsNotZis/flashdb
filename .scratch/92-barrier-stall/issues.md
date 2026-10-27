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
