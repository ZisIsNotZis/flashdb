#!/usr/bin/env bash
# F-M1 / ticket 83: measure the index defect as WORK PER REQUEST on the
# append-heavy instance. No rate is measured here - no --rate-rps, no throughput,
# no paired 200 rps block (that is M3 clause (b), deferred for want of a quiet box).
#
#   arm BEFORE: pre-fix code, worktree at f584ecc (commit before WP-1 8640a80),
#               build_order_no_index(dir, &tile_read_view, name)  -> tiles only
#   arm AFTER:  HEAD, engine/tests/index_defect_mechanism.rs        -> merged view
#
# The pre-fix arm is EXPECTED to fail its non-emptiness assertion: that failure is
# the regression proof. Everything is debug-profile; only counts and byte totals
# are read out, never wall-clock.
set -euo pipefail
cd "$(dirname "$0")/../.."
L=.scratch/84-index-defect-mechanism/logs
mkdir -p "$L"

echo "== AFTER (HEAD $(git rev-parse --short HEAD)) ==" | tee "$L/head.out"
cargo test --test index_defect_mechanism -- --nocapture 2>&1 | tee -a "$L/head.out"

echo "== BEFORE (pre-fix f584ecc) ==" | tee "$L/pre-fix.out"
WT=.worktrees/pre-fix-84
git worktree remove --force "$WT" 2>/dev/null || true
git worktree add --detach "$WT" f584ecc
cp .scratch/84-index-defect-mechanism/pre-fix-probe.rs "$WT/engine/tests/index_defect_mechanism.rs"
( cd "$WT" && cargo test --test index_defect_mechanism -- --nocapture ) 2>&1 | tee -a "$L/pre-fix.out" || true
git worktree remove --force "$WT"

echo "== full suite ==" | tee "$L/workspace-tests.out"
cargo test --workspace 2>&1 | tee -a "$L/workspace-tests.out"
