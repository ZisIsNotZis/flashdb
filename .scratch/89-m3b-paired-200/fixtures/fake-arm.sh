#!/usr/bin/env bash
# TEST FIXTURE ONLY — stands in for `target/release/flashdb-load` when `paired-200.sh --simulate`
# runs. It writes one driver-shaped window JSON object and the driver's `load_summary` line so the
# real header parser, the real scorer and the real manifest see the format they will see from the
# box. It never touches the engine and is never produced by an evidence run.
#
#   fake-arm.sh <variant> <pair> <out-file> <loadavg-file>
#
# FAKE_ERRORS   the `errors` count the arm reports (default 0)
# FAKE_COMPLETED requests completed in the window (default 200 = sustained at 200 rps)
# FAKE_LOSE_AFTER if set to "<pair>-<variant>" (e.g. 3-declared), rewrite the fake loadavg file
#                above the trigger after this arm finishes — the box "leaves the quiet window".
set -u
variant=$1
pair=$2
out=$3
loadavg_file=$4
errors=${FAKE_ERRORS:-0}
completed=${FAKE_COMPLETED:-200}
requests=${FAKE_REQUESTS:-200}

cat >> "$out" <<EOF
{"achieved_rps":200.0,"completed":$completed,"compact_bytes":0,"early_dispatches":0,"errors":$errors,"group_elapsed_us":5000,"groups":4,"groups_over_service_ceiling":0,"invalid":false,"lag_ceiling_breached":false,"lag_ceiling_us":10000,"lag_p50_us":100,"lag_p95_us":500,"lag_p99_us":1000,"max_group_size":50,"mean_group_size":50.0,"min_lag_us":0,"p50_us":1000,"p95_us":2000,"p99_us":3000,"rate_rps":200.0,"reject_reason":null,"requests":$requests,"service_ceiling_breached":false,"service_ceiling_us":null,"service_p50_us":1000,"service_p95_us":2000,"service_p99_us":3000,"window":0,"window_us":1000000}
EOF
cat >> "$out" <<EOF
load_summary requests=$requests completed=$completed errors=$errors else_used=0 late_total=0 early_dispatches=0 windows=1 invalid_windows=0 service_ceiling_rejected_windows=0 lag_ceiling_rejected_windows=0 ceiling_rejected_windows=0 last_valid_rate_rps=200 saturated_at_rps=none commit_batch=1 group_requests=64 elapsed_ms=1000 achieved_rps=200 compact_bytes_total=0 compact_merges=0 first_error="none"
EOF
if [ -n "${FAKE_LOSE_AFTER:-}" ] && [ "$pair-$variant" = "$FAKE_LOSE_AFTER" ]; then
    echo "5.00 5.00 5.00 1/100 1234" > "$loadavg_file"
fi
