# 68 - WP-Q: the final SS7.1 lag-only evaluation after the churn + write-stall fixes

## Contract

Work package Q of the program. Integrated master (`b7958bc`) carries both the WP-O learner-churn
fix (ticket 66: demand-gated, one-shot sticky `index_order_no` axis) and the WP-P write-stall fix
(ticket 67: the grouped WAL append is deferred onto the barrier worker, and the WP-M index tile is
warmed in the read-ahead view). The layout axis gives a +40-51% throughput win; the question this
package answers is whether, with both fixes integrated, **rung 50 now reaches 0 lag-rejected
windows on the win workload** so the win becomes a clean §7.1 lag-only SUSTAINED result, and to
report T1/T2/T3 with the best available evidence.

Single writer in `/home/z/vibe/worktrees/flashdb/wave15-eval` (isolated worktree, branch
`wave15/eval`, based on integrated master). One process at a time under the memory cap; box load is
recorded at every boundary.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3, both judging
views, the harness's both-ceilings `0/0` sustain artifact flagged, any new defect, the slice-39
zero-gain status, and a frank statement of what the evidence does and does not support for the
pre-registered thesis. Pilot-grade labels only; a 10-seed pre-registered verdict is claimed only if
actually run (it will not be).

## Measurement protocol (exact commands)

Release binaries are built once from HEAD `b7958bc` and the run only uses `target/release`
binaries. Instance `cp -a` copies of `/tmp/cold51/per_region-mixed` (12.25x = 1,643,925,811 B /
128 MiB). The harness `flashdb_harness/cold_0b.py` runs every arm under
`systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0` (the harness does this per
arm); `--cap-mib 128` names the same cap.

One harness invocation per seed (it runs the 3 arms x 2 workloads sequentially, one arm process at
a time):

```
cd harness && uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave15-eval/<seed> \
  --report .scratch/68-eval-final/<seed>.md \
  --json   .scratch/68-eval-final/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>
```

- Full ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10;
  rung 400 = 5).
- Seeds: **42, 43, 44 (3 independent) + 42 repeat (noise band)**; more if wall-clock allows.
- Box load (`uptime`) recorded before and after each seed invocation.

Gates (verbatim, run at final HEAD after the measurement; see bottom):

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q` — summary.
4. `git diff --check` — silent (exit 0).

## Judging views (both reported; neither edited)

- **§7.1 lag-only (primary).** A rung is lag-sustained iff its achieved rate is within 5% of the
  offered rate, it has zero error windows, and **zero schedule-lag-ceiling rejected windows**
  (`lag_ceiling_rejected_windows == 0`). The service ceiling is reported separately as the
  efficiency measure and does not veto the lag-only reading.
- **Both ceilings (the harness's built-in `sustained` / T1 verdict).** A rung is `sustained` only
  if BOTH the service ceiling and the lag ceiling reject zero windows. The harness's `sustained`
  field and its T1 threshold are computed this way.

**Flagged artifact:** the harness's `sustained`/`highest_sustained_rate` and the T1 threshold table
are the both-ceilings reading. Under the adopted §7.1 lag-only primary they are NOT the primary
metric, so the lag-only numbers are re-derived below from the per-rung `lag_ceiling_rejected_windows`
and `error_windows` fields, not quoted from the harness verdict. A "0/0" (zero service-rejected AND
zero lag-rejected) is what the harness needs to print `sustained=true`; the lag-only reading needs
only the second zero.

## Results

(To be filled after the runs.)

### Box load at every boundary

| boundary | loadavg (1/5/15) |
|---|---|

### T1 (win) — rung-50 lag-only verdict per arm

(To be filled.)

### T2 (adversarial) — rung-50 lag vs declared, reorg ratio, index builds

(To be filled.)

### T3 (win) — learned/declared reorg ratio, single-region blowup

(To be filled.)

## Residual gaps

(To be filled.)
