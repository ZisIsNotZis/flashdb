# 48 - close two measurement gaps: group statistics, absolute min lag, group-over-ceiling count

Branch `wave1/instr`, based on master `93740b6`. Three gaps the slice-47 verifier could not resolve
because the fields did not exist: per-window group formation, the absolute minimum lag (percentiles
are clamped), and whether a p99 tail is one group's serial work or many groups' stalls.

## Fields added (exact names)

`flashdb-load --metrics-json` window line (and the human `window=` line):

| field | meaning |
|---|---|
| `groups` | groups dispatched this window |
| `mean_group_size` | `requests / groups` (0.0 when no group dispatched); mean size is re-derivable |
| `max_group_size` | largest group dispatched this window |
| `min_lag_us` | ABSOLUTE minimum per-request schedule lag (>= 0) |
| `groups_over_service_ceiling` | groups whose elapsed exceeded `service_ceiling_us` (0 with no `--ceiling-us`) |

Existing `group_elapsed_us` (max group elapsed) kept unchanged. `cold_0b.py` `window_records` parses
all five; `summarize_curve` aggregates them into the rung row (`groups`=sum, `max_group_size`=max,
`mean_group_size`=`sum requests / sum groups`, `min_lag_us`=min over windows,
`groups_over_service_ceiling`=sum). The per-arm capacity-curve table shows them per rung under
columns `groups | grp mean | grp max | min lag | grp>ceil`.

## Invariants kept (744caaa / 965adf4)

Unchanged code paths; all prior tests still pass: bounded rung rate + 2% tolerance
(`RUNG_RATE_TOLERANCE`, `rate_exceeded`), rung attribution that never straddles an offered-rate
change (`ready_group`), trailing-window flush (`index >= total`), per-window `invalid`/`reject_reason`,
`early_dispatches`. Only new counters were added to the existing window emission.

## Tests

`harness/tests/test_cold_0b.py` (3 new, all fail against pre-change `cold_0b.py` - verified by
checking out `HEAD:harness/flashdb_harness/cold_0b.py` and rerunning: 3 failed, KeyError):

- `test_group_statistics_are_consistent_and_bounded` - per-window requests sum to the summary total;
  `mean_group_size == requests/groups`; `1 <= mean <= group cap`; `max_group_size <= cap`; rung mean
  = total requests / total groups.
- `test_min_lag_is_present_and_non_negative` - `min_lag_us` present, non-negative, and carried to the
  rung row.
- `test_a_group_over_the_service_ceiling_is_counted` - a window whose largest `group_elapsed_us`
  exceeds the ceiling reports `groups_over_service_ceiling >= 1`; the rung sums it.

`engine/src/bin/flashdb-load.rs`: `metrics_json_line_carries_group_statistics_and_min_lag` (round-trips
the line and checks `mean_group_size == requests/groups`); the frozen-name test updated with the new
keys in sorted order.

## End-to-end driver run (synthetic dir WAL, two windows)

```
$ ./target/debug/flashdb-load /tmp/instr48b tests/fixtures/mixed_seed42_20.jsonl \
    --rate-rps 200 --window-ms 100 --metrics-json --group-requests 4 --ceiling-us 50
window 0 requests 19 groups 18 mean 1.0555555555555556 max 2 min_lag 56 grp_elapsed 7082 grp_over_ceil 18 svc_p99 7082
window 1 requests 1  groups 1  mean 1.0                max 1 min_lag 589 grp_elapsed 993  grp_over_ceil 1  svc_p99 993
```
sum requests = 20 = summary total; `mean == requests/groups`; `groups_over_service_ceiling` counts
the 18 (and 1) groups slower than the 50 us ceiling.

## Gates (verbatim)

`cargo test --workspace --offline` - every `test result:` line `ok ... 0 failed`; total:

```
passed=265 failed=0
```

`cargo build --workspace --offline`:

```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.05s
```
warnings: 0.

`cd harness && uv run --with pytest pytest -q`:

```
132 passed in 10.70s
```

`git diff --check`: silent (exit 0).

## Residual risk

- `min_lag_us` and the rung `min lag` are minima over the clamped per-request lag samples (the same
  samples the percentiles use); a genuinely early dispatch (negative raw lag) would show as
  `early_dispatches`, not here. Samples are not retained, so a rung min is a min of per-window minima.
- `groups_over_service_ceiling` counts against the SERVICE ceiling (`--ceiling-us`); with no ceiling
  given it is 0 and the group elapsed cannot be judged against a bound.
- `mean_group_size` is a JSON float; at very large request counts the emitted value is a rounded
  double, but the exact quotient is always re-derivable as `requests / groups`.
- Single synthetic run + unit tests; no scored 0b run in this slice, so the fields are proven on the
  wire, not on a cold capped instance.
