# 73 - WP-U: measure the pre-registered T4 (drift recovery) threshold on the >=10x instance

## Contract

Work package U of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave19-drift`
(isolated worktree, branch `wave19/drift`, based on integrated master `0c1b4c5`). One process at a
time under the memory cap; box load recorded at every boundary; `flock /tmp/wave19-drift.lock`
serialises every arm-run. Do not edit `.scratch/15-objective-v0/pre-registration.md`.

T4 (pre-registered, verbatim): *A drift claim is supported only if the learner recovers to within
10% of the pre-drift throughput within 2 windows while `flashdb-static-derived` does not (or
recovers measurably slower).*

The harness `flashdb_harness/cold_0b.py` supports `--workloads drift`: 6 segments at ONE fixed
offered rate (2 pre-drift segments of the win mix `{stock:20,customer:10,scan:30,write:40}`, then
4 drifted segments with `scan` weight x10 -> 81% scans). `--drift-rate` is set explicitly (see
Evaluability gate below; `auto` is unusable on this box because the declared arm never sustains
either ceiling, so the pre-flight `highest_sustained_rate` is 0 and `auto` falls back to the 50 rps
reference).

## Pre-drift (win) capacity — already measured (ticket 71, 8 seeds on the same c51 instance)

Achieved fraction at the WP-S ladder top (learned/static hold ~100% at 400 rps; the layout win is
the WP-M `order_no` index which both indexed arms carry):

| arm | 50 | 100 | 200 | 400 |
|---|---:|---:|---:|---:|
| declared | 50.0 | 85.0 | 85.0 | 80.5 (saturates ~85 rps) |
| learned@1.0 (seed42) | 50.0 | 100.0 | 199.9 | 399.4 |
| static-derived (seed42) | 50.0 | 100.0 | 199.9 | 399.6 |

declared has no index and saturates ~85 rps on the win mix; it is therefore NOT a T4 subject at any
evaluable rate and is reported only where cheap.

## Evaluability gate (decision recorded before seeing the result)

T4 is evaluable ONLY if, at the chosen drift rate, the post-drift (81% scan) mix is ABOVE the arm's
post-drift capacity while the pre-drift (win) mix is BELOW its pre-drift capacity — i.e. the drift
causes a real throughput drop that a recovery can be measured against. If no rate exists below the
pre-drift capacity that still makes the post-drift mix saturate, T4 is UNDERIVABLE (nothing to
recover) and is reported as such, proven with the measured pre/post capacity curve, NOT faked.

Decision rule: probe drift rates 300 -> 400 -> 500 -> 600 -> 800 -> 1000 -> 1500 rps (seed 42,
learned + static-derived, declared only in the first probe because it saturates and is expensive),
read the per-window achieved throughput around the drift boundary (request index 4000), and look
for a post-drift drop. If none exists up to the pre-drift capacity, report T4 UNDERIVABLE with the
capacity curve as proof.

## Measurement protocol (exact commands)

Build once from HEAD, copy the binaries, run only the copy (the harness re-verifies no rebuild by
byte-comparing `target/release/flashdb-load` against the pinned copy before each invocation):

```
source $HOME/.cargo/env
cargo build --offline --release --bin flashdb-load --bin flashdb-scale --bin flashdb-replay
mkdir -p /tmp/wave19-drift-bin
cp target/release/flashdb-load /tmp/wave19-drift-bin/
sha256sum target/release/flashdb-load /tmp/wave19-drift-bin/flashdb-load
# 47c0e5a333881b97dde44c0ce1403ec9bd0f2ddf4f1195a232d097aaf58fabbe  (identical to WP-S HEAD 853633a)
```

Then, per seed (one harness invocation runs the arms sequentially, one process at a time, under the
harness's own `systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0`):

```
cd harness && flock /tmp/wave19-drift.lock uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave19-drift/<tag> \
  --report /home/z/vibe/worktrees/flashdb/wave19-drift/.scratch/73-t4-drift/<tag>.md \
  --json   /home/z/vibe/worktrees/flashdb/wave19-drift/.scratch/73-t4-drift/<tag>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift \
  --arms declared learned static-derived \
  --drift-rate <R> --seed <seed>
```

- `segment_requests 2000` (6 segments = 12,000 requests: 4,000 pre-drift, 8,000 post-drift),
  `--window-ms 1000`.
- Runs executed (serial, one arm process at a time, `flock /tmp/wave19-drift.lock`; box loadavg
  4.7-6.1 throughout the 13:39-14:01 measurement window): seed42@300 (3 arms incl declared), then
  seed42@400/500/600/800/1000/1500 and seed43@600, seed44@600 (learned + static-derived only).
- The learner ran without the declared-derived reorg budget in the learned+static runs (declared
  excluded there); `--learner-cooldown-windows 3` stayed active and reorg stayed small
  (0-28 MB), so the absent budget did not materialise as churn.

## T4 readout (how the per-window numbers are computed)

From each `<tag>.json` arm record's `windows[]`: each window carries `requests` (offered in the
window), `completed_delta`, `window_s` (actual wall time), `achieved_rps =
completed_delta/window_s`, and the lag/service ceiling flags. The drift boundary is the window
whose cumulative offered count first crosses 4,000 (end of the 2nd pre-drift segment). Pre-drift
throughput = summed completions over pre-drift windows / their summed wall time; post-drift is the
same over post-drift windows (the single transition window that straddles 4,000 is excluded). The
pre-registered T4 is judged on THROUGHPUT; the learner's internal `recovery_windows` (region-level
re-exploration) is reported separately as the harness's own signal and is NOT the throughput T4.

## Results

### 1. Is the drift arm evaluable? — NO (proven with the capacity curve)

Per-arm achieved throughput around the drift boundary (pre-drift vs post-drift, same offered rate):

| run | arm | pre-drift achieved | post-drift achieved | drop? |
|---|---:|---:|---:|:--:|
| seed42@300 | learned@1.0 | 304.2 rps (11 w) | 300.6 rps (27 w) | no |
| seed42@300 | static-derived | 299.6 rps (13 w) | 300.5 rps (26 w) | no |
| seed42@400 | learned@1.0 | 399.7 rps (9 w) | 400.9 rps (20 w) | no |
| seed42@400 | static-derived | 400.2 rps (9 w) | 400.9 rps (20 w) | no |
| seed42@500 | learned@1.0 | 499.5 rps (6 w) | 500.9 rps (16 w) | no |
| seed42@500 | static-derived | 498.2 rps (7 w) | 501.1 rps (16 w) | no |
| seed42@600 | learned@1.0 | 597.7 rps (5 w) | 601.0 rps (13 w) | no |
| seed42@600 | static-derived | 597.8 rps (6 w) | 601.0 rps (13 w) | no |
| seed42@800 | learned@1.0 | 408.3 rps (4 w, burn-in) | 888.6 rps (9 w) | no |
| seed42@800 | static-derived | 788.9 rps (4 w) | 801.5 rps (10 w) | no |
| seed42@1000 | learned@1.0 | 603.4 rps (5 w, burn-in) | 1001.9 rps (5 w) | no |
| seed42@1000 | static-derived | 994.0 rps (3 w) | 1003.5 rps (8 w) | no |
| seed42@1500 | learned@1.0 | 371.3 rps (4 w, burn-in) | ~1400 rps (saturates) | no (pre is burn-in) |
| seed42@1500 | static-derived | 1485.4 rps (2 w) | 1500.0 rps (5 w) | no |
| seed43@600 | learned@1.0 | 598.4 rps (5 w) | 601.1 rps (14 w) | no |
| seed43@600 | static-derived | 598.0 rps (6 w) | 601.4 rps (13 w) | no |
| seed44@600 | learned@1.0 | 595.1 rps (5 w) | 602.1 rps (14 w) | no |
| seed44@600 | static-derived | 597.0 rps (6 w) | 601.4 rps (13 w) | no |

Whole-run achieved fraction (per arm, the harness curve):

| run | learned@1.0 | static-derived |
|---|---:|---:|
| seed42@300 | 300.0 (1.000) | 300.0 (1.000) |
| seed42@400 | 400.0 (1.000) | 400.0 (1.000) |
| seed42@500 | 500.0 (1.000) | 500.0 (1.000) |
| seed42@600 | 599.9 (1.000) | 600.0 (1.000) |
| seed42@800 | 799.9 (1.000) | 799.7 (1.000) |
| seed42@1000 | 999.8 (1.000) | 999.6 (1.000) |
| seed42@1500 | 1408.4 (0.939) | 1495.0 (0.997) |
| seed43@600 | 600.0 (1.000) | 600.0 (1.000) |
| seed44@600 | 600.0 (1.000) | 600.0 (1.000) |

**Pre-drift capacity:** static-derived (the clean arm, index adopted at open) holds 1485 rps at
1500 offered on the win mix (2 clean windows) and ~994 rps at 1000 — capacity >= ~1485 rps.
learned's pre-drift is clean only at <= 600 rps (at >= 800 rps the 2-segment pre-drift is shorter
than its demand-gated index burn-in, so its pre-drift numbers at 800/1000/1500 are the burn-in
transient, not capacity).

**Post-drift capacity:** static-derived holds 1500 rps at 1500 offered on the 81%-scan mix
(post >= pre in every run); learned holds 600 at 600, 1002 at 1000, and saturates at ~1400 at
1500. So post-drift capacity >= pre-drift capacity for both arms.

**Chosen drift rate as a fraction of pre-drift capacity:** not applicable — every probed rate up to
and past the pre-drift capacity (1500 rps) leaves the post-drift mix at least as fast as the
pre-drift mix. There is no rate R with `pre-drift capacity > R > post-drift capacity`, so the
recovery event T4 needs never occurs.

**Why (mechanism, consistent with the data):** the drift is `scan` weight x10. The order_no index
(WP-M) is already demanded by the win mix's 30% scan share, so both indexed arms carry it before
the drift; meanwhile the drift cuts the write share from 40% to ~11%, and writes are the
fsync-bound bottleneck on this device. More cheap index scans + fewer fsync writes makes the
post-drift mix strictly no harder (measured: static pre 1485 -> post 1500 rps at 1500 offered).

### 2. T4 — per-window throughput around the drift point

No arm shows a post-drift throughput drop at any tested rate, so there is no "recover to within
10% of pre-drift throughput" event to time. Window-level numbers around the drift boundary at the
cleanest rate (seed42@600, drift boundary = window 5 for learned, window 6 for static):

learned@1.0 seed42@600 (rps per window): w0 20.0, w1 101.0, w2 80.3, w3 1819.0, w4 1522.7,
**w5 621.8 (drift)**, w6 600.9, w7 601.5, w8 601.3, w9 601.1, w10 600.4 ... w18 598.3 — post-drift
steady ~600 rps, no dip.

static-derived seed42@600: w0 595.3, w1 588.2, w2 601.0, w3 573.1, w4 631.3, w5 598.5,
**w6 614.9 (drift)**, w7 601.9, w8 599.9 ... w19 600.7 — post-drift steady ~600 rps, no dip.

The same holds at 300/400/500/800/1000 rps (seed42) and at 600 rps for seeds 43/44.

**Seed count and noise band:** 3 independent seeds (42, 43, 44) at 600 rps plus seed42 across 7
rates (300-1500) = 9 drift arm-runs total. The WP-S measured throughput noise band is ~2.5% (same
trace repeat); the observed post-drift/pre-drift gap is 0% (post >= pre) at every clean rate, so
the "no drop" conclusion is far outside noise. The learned arm at >= 800 rps is excluded from the
pre-drift comparison because its 2-segment pre-drift is shorter than its index burn-in (a
measurement-limit, stated, not hidden).

### 3. Both judging views

- **SS7.1 lag-only (primary):** the lag ceiling rejects a scattered handful of windows on every
  arm (learned 7-12/windows, static 7-11/windows across runs — the shared-box noise + device
  fsync tail named in WP-S), but this does not affect throughput (achieved fraction 1.000 at
  every rate <= 1000). Under the lag-only reading the two indexed arms sustain the whole drift run
  at every tested rate; declared (no index) never sustains anything.
- **Both ceilings (harness / pre-registration):** `sustained=false` at every rate for every arm,
  because the service ceiling rejects essentially every window (svc_rej = window count in all but
  the first windows) — the per-group `fdatasync` device floor (6.6 ms p50 / 13.4 ms p99 vs a fixed
  10 ms). The open SS7.1 primary-vs-service reconciliation is unchanged and not edited.

The harness's own T4 line (from the 300 rps run) prints
**`NOT MEETABLE WITH CURRENT PARAMETERS — learner recovered in 6 windows`**: that is the learner's
INTERNAL region-level `recovery_windows` (`U|Customer: 6`, a cost-deviation re-exploration on a
region unrelated to the scan drift, with the throughput never moving), not the pre-registered
throughput-recovery quantity. The pre-registered T4 is judged on throughput and is UNDERIVABLE, not
"recovered in N windows" and not "not meetable".

### 4. Honest overall T4 verdict

**T4 is UNDERIVABLE (pilot-grade), not faked.** With the integrated WP-M order_no index, the
pre-registered scan-spike drift does not produce a throughput drop for either indexed arm at any
offered rate from 300 rps up to and past the pre-drift capacity (~1485 rps): the index is already
demanded by the win mix's 30% scans, and the drift simultaneously cuts the write share 40% -> 11%
(the actual fsync-bound bottleneck). Post-drift capacity >= pre-drift capacity (measured: static
pre 1485 vs post 1500 rps at 1500 offered; no drop in 9 arm-runs over 3 seeds x 7 rates). Because
no offered rate exists below the pre-drift capacity that still makes the post-drift mix saturate,
the "recover to within 10% of pre-drift throughput within 2 windows" event never occurs, and T4 has
no baseline to judge. This is the slice-38 "above-capacity" evaluability failure inverted: slice
38's rate was above the pre-drift capacity; now the drift direction itself is the wrong direction
for a recovery test (it removes load, it does not add it).

The harness's internal T4 signal (region re-exploration time) does fire on an unrelated region
(`U|Customer`) and reports 6 windows, which still exceeds the pre-registered 2-window target
(`min_samples x drift_reexplore_candidates = 6` with the published defaults) — but that is the
region-re-exploration reading, not the throughput reading the pre-registration actually wrote.

**Residual/gap:** (a) the drift shape is hardcoded to `scan x10` in the harness (no CLI knob), so
only this one drift direction was measured; a write-spike or a key-popularity-spike drift (as the
pre-registration's "or one key's popularity spiked" names) is not reachable with the current
harness and might still produce a recovery event. (b) the drift-workload static-derived is derived
from the learner's sweep over the FULL pre+post run (the harness's `max_rate` filter cannot split
the two segments at a single fixed rate), so it is not a strictly pre-drift-frozen control — this
does not affect the underivability conclusion (learned also shows no drop) but is stated. (c) the
pre-drift segment is only 2 segments (4000 requests), shorter than learned's index burn-in at
>= 800 rps, so learned's high-rate pre-drift is not a clean baseline. (d) 3 seeds + 7 rates =
9 arm-runs, not 10 seeds; shared box (loadavg 4.7-6.1); pilot-grade.

## Gates (verbatim, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.04s
```
2. `cargo test --workspace --offline` — totals: **304 passed; 0 failed; 0 ignored**
(129 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
+ 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics + 33 multi_tile + 7 order_flow
+ 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction +
6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout
+ 12 tile_read + 0 flashdb-replay + 0 flashdb-scale + 0 nvme_bench + 0 doc-tests). The integrated
tree has one more test than either pre-merge lane (ticket 69's +1 lib warm_startup test and ticket
70's +1 merge_threshold test both present).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
140 passed in 4.38s
```
4. `git diff --check` — silent (exit 0).
