#!/usr/bin/env bash
# Paired A/B: flashdb-declared (no index) vs flashdb-indexed (order_no index on P|StockMovement).
# Same frozen snapshot, same trace, same 200 rps rung, one process at a time in a 128 MiB cgroup.
set -euo pipefail
cd "$(dirname "$0")/../.."
R=.scratch/83-index-probe/runs
SNAP=.scratch/82-controlled-m1/runs/snapshot/flashdb-declared
TRACE=.scratch/82-controlled-m1/runs/b2/rung-200/trace.jsonl
POL=.scratch/83-index-probe/policy/indexed.policy.json
mkdir -p "$R"
for pair in 1 2 3; do
  for variant in declared indexed; do
    inst=$R/live-$variant
    rm -rf "$inst"
    cp -a --reflink=auto "$SNAP" "$inst"
    args=(target/release/flashdb-load "$inst" "$TRACE" --rate-rps 200 --window-ms 1000
          --metrics-json --group-requests 64 --lag-ceiling-us 10000 --peak-report)
    if [ "$variant" = indexed ]; then args+=(--policy-file "$POL"); fi
    out=$R/p$pair-$variant
    systemd-run --user --scope -q -p MemoryMax=128M -p MemorySwapMax=0 \
      bash -c "$(printf '%q ' "${args[@]}")" > "$out.out" 2> "$out.err" || true
    echo "done p$pair-$variant"
  done
done
