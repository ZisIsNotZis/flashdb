# 71 - WP-S: the decisive SS7.1 lag-only evaluation (after startup-warm + merge-oscillation fixes)

## Contract

Work package S of the program. Integrated master (this worktree HEAD, `853633a`) carries both the
WP-R1 startup-warm fix (ticket 69: eager index build+adopt + bounded synchronous `warm_startup` pass
before the timed windows) and the WP-R2 merge-oscillation fix (ticket 70: re-merge minimum-payback
hysteresis in `Engine::select_region_run`). The layout axis gives a reproducible +43-57%
achieved-throughput win; the question this package answers is whether, with BOTH fixes integrated,
**rung 50 now reaches 0 lag-rejected windows on the win workload** so the win becomes a clean §7.1
lag-only SUSTAINED result, and to report T1/T2/T3 with the best available evidence.

Single writer in `/home/z/vibe/worktrees/flashdb/wave17-eval` (isolated worktree, branch
`wave17/eval`, based on integrated master). One process at a time under the memory cap; box load is
recorded at every boundary.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3, both judging
views, the harness's both-ceilings `0/0` sustain artifact flagged, any new defect, the slice-39
zero-gain status, and a frank statement of what the evidence does and does not support for the
pre-registered thesis. Pilot-grade labels only; a 10-seed pre-registered verdict is claimed only if
actually run (it will not be).

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
metric, so the lag-only numbers are re-derived from the per-rung `lag_ceiling_rejected_windows` and
`error_windows` fields, not quoted from the harness verdict. A "0/0" (zero service-rejected AND
zero lag-rejected) is what the harness needs to print `sustained=true`; the lag-only reading needs
only the second zero.

## Measurement protocol (exact commands)

Release binaries built once from HEAD `853633a` and the run only uses `target/release` binaries.
Instance `cp -a` copies of `/tmp/cold51/per_region-mixed` (12.25x = 1,643,925,811 B / 128 MiB).
The harness `flashdb_harness/cold_0b.py` runs every arm under
`systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0` (the harness does this per
arm); `--cap-mib 128` names the same cap.

One harness invocation per seed (it runs the 3 arms x 2 workloads sequentially, one arm process at
a time), wrapped in `flock /tmp/wave17-measure.lock` so no other measurement lane overlaps the lag
ceiling:

```
cd harness && flock /tmp/wave17-measure.lock uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave17-eval/<seed> \
  --report .scratch/71-eval-decisive/<seed>.md \
  --json   .scratch/71-eval-decisive/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>
```

- Full ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10;
  rung 400 = 5).
- Seeds committed: **42, 43, 44, 45, 46 (5 independent) + 42 repeat (noise band)**; extend to
  seeds 47+ if wall-clock allows. (Ticket 68's protocol used 42/43/44 + 42-repeat; this is the
  decisive run, so more independent seeds are preferred within the wall-clock budget.)
- Box load (`uptime`) recorded before and after each seed invocation, plus a process snapshot at
  run start.

Gates (verbatim, run at final HEAD; see bottom):

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q` — summary.
4. `git diff --check` — silent (exit 0).

## Report checklist (what this ticket must contain, honestly)

1. **T1 (win)**: under SS7.1 lag-only primary, is any arm rung-50 (or higher) lag-sustained now?
   The rung-50 lag-rej table per arm per seed. Does learned beat declared by >15% in the sustained
   load level — if 0 breaches is reached, give the sustained levels; if not, give the
   achieved-throughput comparison (+43-57%?) and the lag-valid fraction, with seed count and noise
   band.
2. **T2 (adversarial)**: learned vs declared rung-50 lag; reorg ratio (<=1.5x?); index builds on
   the 0-scan region.
3. **T3 (win reorg ratio)**: median and max; does the single-region blowup still recur?
4. Both judging views; flag the harness's both-ceilings `0/0` artifact.
5. A FRANK overall statement: what the evidence does and does not support for the pre-registered
   thesis (T1/T2/T3/T4), at the achieved seed count, under the adopted SS7.1 reading. New defects;
   slice-39 zero-gain status.

## Plan (committed before the long run)

1. Build release binaries once from HEAD; run the four gates.
2. Run the measurement seeds serially under `flock /tmp/wave17-measure.lock`, recording load at
   every boundary and `cp -a`-copying the base instance per arm (harness does this).
3. Parse the per-seed JSON into the report tables (rung-50 lag-rej per arm per seed; per-rung
   achieved; reorg bytes; index builds; both judging views).
4. Write this ticket's full report, append the one-paragraph dispatch entry to
   `.scratch/52-program/issues.md`, commit, and report the hash + honest pilot-grade verdict.
