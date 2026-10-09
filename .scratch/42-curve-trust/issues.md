# 42 - curve trust: why per-rung achieved exceeded offered, and the fix

Scope: make the capacity curve trustworthy enough to quote per-rung numbers. Touches
`engine/src/bin/flashdb-load.rs`, `harness/flashdb_harness/cold_0b.py`, `harness/tests/`.

## 1. Diagnosis (against the code AND the recorded run data)

Run inspected: `.scratch/39-0b-measurable/adversarial.json`, arm `adversarial/declared`
(rates `[50,100,200,400]`, `--segment-requests 2000`, `--group-requests 64`, `--window-ms 1000`).

Method: each candidate mechanism is paired with the code path that would produce it and a
signature in the recorded windows.

Published table (OLD estimator, `median(Δcompleted / nominal_window)`):

| offered | window deltas seen | OLD median | OLD % |
|---|---|---:|---:|
| 50  | 64 or 0            | 64.0  | 128% |
| 100 | 128 or 64          | 128.0 | 128% |
| 200 | 192 or 256         | 192.0 | 96%  |
| 400 | 320/448/384        | 384.0 | 96%  |

The engine kept up: `elapsed_ms=74878` vs the offered span `75000 ms` (40+20+10+5 s), and overall
`achieved_rps=107` vs the offered mean `106.7`. So the >100% rows are a measurement artifact, not
extra throughput.

**(b) grouped driver commits `size` requests as soon as the FIRST is due. CONFIRMED - primary.**
`flashdb-load.rs` HEAD:530-532 takes `end = index + size` and `window_rate = rate_for(index)`;
HEAD:680 advances the schedule one request-interval per member *after* the whole group is issued.
At offered `R` a group spans `G/R s`; with `G=64, W=1 s` a 50 rps window holds at most one group,
so its Δ is 64 or 0 and the median is `G/W = 64 = 128%` of 50 (and 2 groups at 100 rps -> 128).
This is the mechanism that produces the 128% figure.

**(a)/(c) rung-boundary attribution. CONFIRMED - secondary.**
HEAD:531 lets a group cross a rate boundary and HEAD:532 labels it by its START rate; the window's
`rate_rps` is the last group's rate and windows close on wall time (HEAD:692). Recorded evidence:
window 38 is labelled `rate=50` with `completed=2048`, i.e. the group spanning indices 1984..2047
carried 48 requests that are OFFERED at 100 rps but were credited to the 50 rps rung.

**(c') the window rate itself.** A window that spans a rung boundary is reported under one rate
(the last group's). Real, folded into (a).

**(d) pacer timer granularity. NOT SUPPORTED.** The burst pacer sleeps in `<= 1000 us` chunks
(`flashdb-load.rs` HEAD:685-688), two orders under the 20 ms period at 50 rps; every one of the 71
recorded windows has `lag_p99_us=0`. Granularity does not create the over-report.

**Additional defect found while re-scoring.** The trailing partial window is never flushed: the
last group's completions were counted by `load_summary` but absent from the window stream
(declared: last window `completed=7552`, summary `completed=8000` -> 448 completions unattributed).
Fixed with the rest.

## 2. Fix

Driver `flashdb-load.rs`:
- clamp a group to the offered-rate run so it never straddles a rung (`end` loop) - attribution.
- close a window at a rung boundary (guarded by `!latencies.is_empty()`) so a window never mixes
  two offered rates; flush the trailing window (`index >= total` = `flashdb-load.rs:731`).
- emit `window_us`, `lag_p99_us`, `invalid`, `ceiling_us`, `ceiling_breached` on the `--metrics-json`
  line (`WindowLine`). Before, `invalid`/`lag_p99_us` were only on the human `window=` line, so
  `window_records`' `obj.get("invalid", False)` was always false for JSON runs.
- `load_summary` now carries `ceiling_rejected_windows`.

Harness `cold_0b.py`:
- `window_records` parses the new fields and keeps `completed_delta` and per-window `window_s`.
- `summarize_curve`: `achieved = attributed_completions / max(offered_span, wall_span)` where
  `offered_span = Σrequests / rate`. The offered span is a FLOOR on the denominator, so achieved
  <= offered by construction; a saturated rung (wall time > offered span) reads below offered.
- `sustained` now requires: within `SUSTAINED_TOLERANCE` of offered, no error window, no pacer-
  rejected (`invalid`) window, no ceiling-rejected window, and `not rate_exceeded`.
- new `ceiling_rejected_windows` per rung; `invalid`/`lag`/`ceiling` honoured.

### Tolerance
`RUNG_RATE_TOLERANCE = 0.02` (2%). The offered span is a mathematical floor on the denominator, so
achieved cannot legitimately exceed offered; 2% only absorbs f64/rounding and the clock granularity
of the wall-time term. It is far below the 5% sustained band and the measured ~10-20% noise band, so
`rate_exceeded` can only trip on a genuine bookkeeping bug (completions > offered) - never on a
legitimate measurement. A tripped guard forces `sustained=False`.

## 3. Ceiling policy (STEP 3)
The pre-registration validity rule needs a latency ceiling; the published runs carried none.
- `cold_0b.main` `--ceiling-us` is now **required** (`required=True`): a scored run with no ceiling
  exits 2 with `error: the following arguments are required: --ceiling-us`. No permissive default.
- The driver carries `ceiling_us` and its per-window `ceiling_breached` on the JSON line and
  `ceiling_rejected_windows` in `load_summary`. (The driver keeps `--ceiling-us` optional because
  other harnesses build on it; the *scored* path is `cold_0b`.)
- The report states the ceiling and **how many windows it rejected**, in total and per arm, and
  each curve row carries `ceiling_rejected_windows`.

## 4. Regression test
`harness/tests/test_cold_0b.py`:
- `test_group_bursts_cannot_inflate_a_rung_above_its_offered_rate` - a synthetic 2000-request 50 rps
  rung of 31x64 + 16 bursts over 40 one-second windows (the recorded shape). Under the OLD estimator
  the median is 64.0 = 128%; the new estimator returns 50.0 (100%) and `rate_exceeded` is False.
- `test_a_rung_that_exceeds_its_offered_rate_fails_closed` - completions > offered trips
  `rate_exceeded` and fails `sustained`.
- `test_json_window_invalid_flag_reaches_the_curve` - an `invalid` JSON window invalidates the rung.
- `test_a_scored_run_requires_a_latency_ceiling` - missing `--ceiling-us` raises `SystemExit`.

**Verification that it fails on the old logic:** with only `cold_0b.py` reverted to HEAD
(`git stash push -- harness/flashdb_harness/cold_0b.py`), the regression test fails with
`AssertionError: assert 64.0 == 50.0` (the 128% median); restored, the suite passes. New logic
verified: **126 passed on master** (this slice reported 119; the 7 extra tests were added by later
slices, not by a defect in this work).

### Synthetic driver run (not a cold arm)
`/tmp/cold33/per_region-mixed` no longer exists, so no cold arm could be re-run (under the 10-minute
budget). A cheap synthetic driver run (1280 trivial stock-miss requests, `--rate-rps 50,100`,
`--segment-requests 640`, `--group-requests 64`, `--window-ms 1000`, `--ceiling-us 100000000`)
reproduces exactly the artifact and the fix:

| offered | NEW achieved | NEW % | sustained | OLD median | OLD % |
|---:|---:|---:|:--:|---:|---:|
| 50  | 50.0  | 100% | yes | 63.9  | 128% |
| 100 | 100.0 | 100% | yes | 127.9 | 128% |

Re-scoring the recorded slice-39 window streams with the NEW estimator (declared): 50->100%,
100->100%, 200->100%, 400->100%, all sustained (old: 128/128/96/96). This is a re-score of old
windows, not a re-run: it does not include the group clamp / window split / trailing flush.

## 5. Gates (verbatim)
- `cargo test --workspace --offline`: 259 passed, 0 failed across 23 targets
  (e.g. `113 passed; 0 failed`, `13 passed; 0 failed`, `8 passed; 0 failed`, ...).
- `cargo build --workspace --offline`: `Finished dev profile`; 0 warning lines.
- `cd harness && uv run --with pytest pytest -q`: `126 passed` (this slice reported `119 passed in
  2.48s`; the drift is later-added tests, not a defect in this work).
- `git diff --check`: silent.

## 6. Residual risk
- No cold re-run: the cold instance is gone, so the fix is proven by unit tests + one synthetic
  driver run, not by the real arm. The re-scored table is an estimator-only replay of old windows.
- `achieved` uses the offered span as a floor, so a saturated arm that still completes every
  request (open loop) can read ~100% in the achieved column; saturation is carried by the `invalid`
  (lag) flag and `sustained`. A reader must not read the achieved column alone.
- The rung-boundary window split changes window boundaries (more, shorter windows), so `windows`
  counts and `run_window_count` differ slightly from published runs.
- Requiring `--ceiling-us` breaks any scripted invocation that omitted it (intended fail-closed).
