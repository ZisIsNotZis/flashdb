# 45 - latency model: schedule from arrivals, measure per request

Independent verification of `a1b809f` found the driver's `--group-requests` pacer consumed
future-scheduled requests and reported a group-level elapsed time as a per-request latency.

## Defects (against a1b809f)

1. `engine/src/bin/flashdb-load.rs:552,559` - `if due_us <= now_us { ... end = (index+size).min(total) }`
   takes `size` requests as soon as the FIRST is due, so future-scheduled requests are consumed and
   the driver runs ahead of the offered schedule (the old "128% achieved" artifact).
2. `engine/src/bin/flashdb-load.rs:699-703` - one `group_us = started.elapsed()` sample assigned to
   every member (`latencies.push(group_us)`), so the "per-request" p50/p99 is the GROUP's elapsed
   time (~1.6 ms/member at group 64), inflating the bound by ~N.
3. `engine/src/bin/flashdb-load.rs:769` - `--ceiling-us` compared against the p99 of that inflated
   group quantity, while the pre-registration's primary metric is a LAG rule (lag p99 <= 10 ms) and
   a separate hardcoded `LAG_SATURATION_US` (line 83) drove only the saturation flag; the validity
   rule and the pre-registration did not name the same quantity.
4. The group bursts (`559`) make schedule lag spike at group boundaries (per-rung lag_p99
   111,851 / 339,308 / 408,615 / 1,026,708 us), so the lag statistic was contaminated by dispatch.

## Chosen model (arrivals, per-request measurement)

- A request is READY at its scheduled due time. `ready_group` (`ready_group` fn) forms a group only
  from requests ALREADY READY (due <= now), capped at `size` and never straddling an offered-rate
  boundary, and dispatches as soon as one is ready; it is never held open to fill. The schedule is
  advanced to the next unissued request's due time (`end_due_us`), so no future request is consumed.
- Each member's latency = group completion minus ITS OWN due time; each member's lag = issue minus
  its own due. Samples are kept PER REQUEST and percentiled (p50/p95/p99) separately.
- The group's elapsed time (completion minus its first member's due) is `group_elapsed_us`, a
  diagnostic only, never presented as a per-request latency.
- Two named ceilings per window: `--ceiling-us` bounds the per-request service p99,
  `--lag-ceiling-us` (default 10000, the pre-registration's own number) bounds the per-request
  schedule-lag p99. `reject_reason` names which; both reach the JSON.
- Durable invariants from `744caaa` kept: attributed rate <= offered within `RUNG_RATE_TOLERANCE`,
  every completion attributed to the rung it was offered under (rate-boundary cap), trailing window
  flushed, per-window `invalid`/reject fields in the JSON.

## New field names (exact)

Driver `--metrics-json` window: `service_p50_us`, `service_p95_us`, `service_p99_us` (per-request
service; legacy aliases `p50_us`/`p95_us`/`p99_us` carry the same values),
`lag_p50_us`, `lag_p95_us`, `lag_p99_us`, `group_elapsed_us`, `early_dispatches`, `reject_reason`,
`service_ceiling_us`, `service_ceiling_breached`, `lag_ceiling_us`, `lag_ceiling_breached`
(plus existing `invalid`, `window_us`).

Driver CLI: `--lag-ceiling-us N` (default 10000); `--ceiling-us N` is now explicitly the
service-latency ceiling.

`load_summary`: `early_dispatches`, `service_ceiling_rejected_windows`,
`lag_ceiling_rejected_windows`, `ceiling_rejected_windows` (windows over either).

`cold_0b` curve row: the above service/lag percentiles, `group_elapsed_us`, `early_dispatches`,
`service_ceiling_rejected_windows`, `lag_ceiling_rejected_windows`, `ceiling_rejected_windows`,
`reject_reasons` (dict of reason -> count). Result JSON gains `lag_ceiling_us`.

## Tests

`harness/tests/test_cold_0b.py` (fail against a1b809f - verified by stashing the two source files):

- `test_t1_per_request_latency_is_not_the_group_elapsed_time` - a 64-member window with a 108,282 us
  group and 1,600 us per-request service; asserts the rung p50 is the per-request value, strictly
  below the group value, and p99 <= group value.
- `test_t2_the_driver_never_consumes_a_request_before_its_due_time` - `early_dispatches` reaches the
  window record, the curve and the summary; a non-zero value stays visible.
- `test_t3_service_and_lag_ceilings_report_different_reasons` - a lag-ceiling window and a
  service-ceiling window report different `reject_reason` and land in different counters.

`engine/src/bin/flashdb-load.rs` unit tests: `ready_group_never_consumes_a_future_request`,
`ready_group_stops_at_the_offered_rate_boundary`, `reject_reason_names_which_ceiling_failed`,
`lag_ceiling_flag_parses_and_defaults` (plus updated JSON-schema tests).

## Gates (verbatim)

`cargo build --workspace --offline`:
```
   Compiling flashdb-engine v0.1.0 (/home/z/vibe/flashdb/engine)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.98s
```
(zero warnings; the only warnings in the tree are pre-existing `unused_mut` in
`engine/tests/manifest_engine.rs`, another lane's file, surfaced by `cargo test` only.)

`cargo test --workspace --offline`:
```
test result: ok. 113 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.86s
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.17s
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.17s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.43s
test result: ok. 33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.97s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.24s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.38s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.39s
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.29s
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.41s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
23 test binaries, 263 passed, 0 failed.

`cd harness && uv run --with pytest pytest -q`:
```
129 passed in 9.47s
```

`git diff --check`: silent (exit 0).

Smoke run (debug driver, 300-request adversarial slice, `--group-requests 64`, window 200 ms):
`early_dispatches=0` on every window; per-request `p50_us` 819/1247/566 vs `group_elapsed_us`
8043/22879/7477; `reject_reason` `service_ceiling+lag_ceiling` on the window that breached both.

## Residual risk

- Dispatching as soon as one request is ready means groups are size 1 whenever the engine keeps up,
  so the group-commit fsync sharing that `--group-requests` modelled now only appears when the
  driver has fallen behind. That is the arrived-load semantics the task asked for; it lowers the
  measured group-commit throughput relative to the old (run-ahead) runs, so old and new per-rung
  numbers are not comparable.
- The per-request service latency is measured from the request's ARRIVAL (due time), so it includes
  queueing behind earlier members; the schedule lag is reported separately, and the two sum.
- `--ceiling-us`/`--lag-ceiling-us` verdicts were only smoke-tested on a tiny WAL, not on the cold
  8.17x instance: no scored curve was rerun here.
