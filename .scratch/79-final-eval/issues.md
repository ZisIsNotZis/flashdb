# 79 - final combined post-integration evaluation (setup dispatch)

## Objective

Final-revision proof of the thesis metrics on the fully integrated master (tilecap + driftshape +
rewarm repair). This record is the setup dispatch: it fixes the measured revision, the release
binaries, and the fresh cold 12.25x instance the measurement will use. The measurement itself
(arms, seeds, tables, T1/T2/T3/T4 verdicts) is recorded in this ticket by the eval worker.

## Branch / HEAD

- Branch: `wave23/final-eval`
- Worktree: `/home/z/vibe/worktrees/flashdb/wave23-final-eval`
- Base master HEAD: `48b0488af5d9db1c2252c727e9389bfb06ecdc09`

## Instance

Fresh cold copy (not reused): `cp -a /tmp/cold51/per_region-mixed /tmp/wave23-eval/per_region-mixed`.
Size: 1,643,925,811 B (12.25x vs 128 MiB cap).

## Protocol

- Measure ONLY the release binaries built in this worktree (`target/release/flashdb-load`,
  `target/release/flashdb-scale`, `target/release/flashdb-replay`), one process at a time, against
  `/tmp/wave23-eval/per_region-mixed`.
- Record `uptime`/loadavg and cgroup peak with each run; every measurement wrapped in
  `flock /tmp/wave21-measure.lock -c '<full command>'`.
- Do NOT edit `.scratch/15-objective-v0/pre-registration.md`.

## AC

- `cargo build --workspace --release --offline` completes with 0 warnings in this worktree.
- Fresh cold instance copy exists at `/tmp/wave23-eval/per_region-mixed` (1,643,925,811 B).
- This dispatch header committed on `wave23/final-eval`; dispatch entry appended to
  `.scratch/52-program/issues.md`.
