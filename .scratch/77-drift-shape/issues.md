# 77 — WP-W: close the T4 evaluability gap with a non-scan drift shape

## Contract

Work package W of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave21-driftshape`
(isolated worktree, branch `wave21/driftshape`, based on integrated master `8a4ea20`). Do not cd
to `/home/z/vibe/flashdb`. One flashdb process at a time under `MemoryMax=128MiB`;
`flock /tmp/wave21-measure.lock -c '<full command>'` serialises every arm-run on the shared box;
box load recorded at every boundary. Do NOT edit `.scratch/15-objective-v0/pre-registration.md`,
and do NOT change any pre-registered threshold (the 2-window T4 target, the 10% band, or any
ceiling).

T4 (pre-registered, verbatim): *A drift claim is supported only if the learner recovers to within
10% of the pre-drift throughput within 2 windows while `flashdb-static-derived` does not (or
recovers measurably slower).*

Ticket 73 proved T4 UNDERIVABLE with the only drift shape the harness exposed: the post-drift mix
is `scan` weight x10, which cuts the write share 40% -> ~11% (the fsync-bound bottleneck) and
leaves an `order_no` index already demanded by the win mix's 30% scans, so post-drift capacity >=
pre-drift capacity and no recovery event occurs. The pre-registration also names the other shapes
— "a shape's weight multiplied, or one key's popularity spiked" — so the gap is an instrument gap,
not a verdict.

## Objective

Add a configurable drift SHAPE to `harness/flashdb_harness/cold_0b.py` (at least a write-weighted
drift; the key-popularity spike is a secondary candidate), keep the existing scan-drift behaviour
selectable, re-test T4, and report the honest verdict. A capacity-reducing drift shape must exist
and be selectable; T4 is either evaluated (recovery windows for learned vs static-derived, with
the throughput series and seed count) or honestly proven still-underivable with evidence.

## Measurement contract (verbatim, inherited from the dispatch)

- `cp -a` copy of `/tmp/cold51/per_region-mixed` (12.25x) into a fresh workdir; build once; copy
  the release binaries; run only the copy; one process at a time.
- First validate evaluability: find a drift rate below pre-drift capacity for the new shape; if
  none exists, prove it.
- Then run learned + static-derived (+ declared if cheap), >= 3 seeds + a repeat.
- Every measurement command wrapped in `flock /tmp/wave21-measure.lock -c '<full command>'`;
  record which runs were lock-protected and the box load.
- Judging: SS7.1 lag-only primary; report both lag-only and both-ceilings.
- Gates, verbatim: `cargo build --workspace --offline` (0 warnings);
  `cargo test --workspace --offline` (totals); `cd harness && uv run --with pytest pytest -q`;
  `git diff --check` (silent).

## Design (checkpoint before implementation)

The drift-plan builder already generalises to any WIN_MIX key:

```
drifted = dict(WIN_MIX)
drifted[drift_shape] = drifted[drift_shape] * drift_factor
```

so a write-spike is reachable by exposing the existing `drift_shape`/`drift_factor` knobs on the
CLI instead of adding a parallel trace path. The harness currently hard-codes the defaults in the
`main()` call site (`plan_workload(wl, ...)` passes only `drift_rate`), so the only gap is two
arguments.

Change 1 (minimum, write-spike):
- `--drift-shape {scan,write}` default `scan` (preserves ticket 73's behaviour exactly).
- `--drift-factor INT` default `10` (so `scan x10` remains the default and `write x10` is
  selectable; the factor is a stated shape knob, not a threshold).
- Thread both into `plan_workload`.

For `--drift-shape write`, the post-drift mix becomes `{stock:20, customer:10, scan:30,
write:400}` (total weight 460): write share 40% -> ~87%. Mechanism expectation: each `order_flow`
write carries a cold `Customer` email find plus a cold `Stock` patch read plus the WAL puts, so
raising the write share raises the cold-read+fsync work per offered request — the opposite
direction from the scan drift. This is the capacity-reducing hypothesis to test, not an assumed
result.

Change 2 (discovered during evaluability probing, recorded before the T4 run): the drift plan
hard-codes `pre=2` pre-drift segments (4,000 requests). Ticket 73 residual (c) already flags that
this is shorter than the learned arm's demand-gated index burn-in at >= 800 rps, so learned's
pre-drift baseline is polluted at exactly the rates where a capacity-reducing drift saturates.
To re-test T4 honestly the pre-drift segment count must be configurable:
- `--drift-pre-segments INT` default `2` (preserves the ticket-73 scan-drift trace exactly);
  the post-drift stays at 4 segments, so the default total stays 6 segments as before.
- Thread it into `plan_workload`. This is a harness trace-length knob, not a threshold; the
  2-window target, the 10% band, and every ceiling are untouched.

Key-popularity spike (stretch, only if cheap): the generator already has `set_key_spike` (a
flash-sale hot-SKU knob) but `build_trace` never exposes it, and a hot-key drift is not obviously
capacity-reducing on this schema (a hot key warms up, which removes cold reads). It is therefore
NOT the primary shape; if it is added it must be a separate, explicit shape and must be measured,
not assumed.

The pre-registration and all thresholds stay untouched. The T4 judging remains on throughput
(per-window pre-drift vs post-drift achieved rps), with the learner's internal
`recovery_windows` reported separately exactly as ticket 73 did.

## Change log

(recorded incrementally; see git log)
