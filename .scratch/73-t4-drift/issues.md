# 73 - WP-U: measure the pre-registered T4 (drift recovery) threshold on the >=10x instance

## Contract

Work package U of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave19-drift`
(isolated worktree, branch `wave19/drift`, based on integrated master `0c1b4c5`). One process at a
time under the memory cap; box load recorded at every boundary; `flock /tmp/wave19-drift.lock`
serialises every arm-run. Do not edit `.scratch/15-objective-v0/pre-registration.md`.

T4 (pre-registered, verbatim): *A drift claim is supported only if the learner recovers to within
10% of the pre-drift throughput within 2 windows while `flashdb-static-derived` does not (or
recovers measurably slower).*

The harness `flashdb_harness/cold_0b.py` supports `--workloads drift`: 6 segments at ONE fixed
offered rate (2 pre-drift segments of the win mix `{stock:20,customer:10,scan:30,write:40}`, then
4 drifted segments with `scan` weight x10 -> 81% scans), `--drift-rate` explicit or `auto`
(`auto` = `--drift-rate-fraction` of the pre-flight declared arm's highest SUSTAINED level; on this
box the declared arm never sustains either ceiling so `auto` falls back to the 50 rps reference and
is NOT usable here). `--drift-rate` is therefore set explicitly, below the pre-drift (win) mix's
capacity so at least one arm is not saturated at the pre-drift level.

## Pre-drift (win) capacity — already measured (ticket 71, 8 seeds on the same c51 instance)

Achieved fraction at the ladder top (learned/static hold ~100% at 400 rps; the layout win is the
WP-M `order_no` index which both indexed arms carry):

| arm | 50 | 100 | 200 | 400 |
|---|---:|---:|---:|---:|
| declared | 50.0 | 85.0 | 85.0 | 80.5 (saturates ~85 rps) |
| learned@1.0 (seed42) | 50.0 | 100.0 | 199.9 | 399.4 |
| static-derived (seed42) | 50.0 | 100.0 | 199.9 | 399.6 |

So the pre-drift (win) mix capacity of the two indexed arms is >= ~400 rps achieved (rung 400 is
achieved ~100%, with a few lag-ceiling rejections at the very top). Any drift rate <= ~350 rps
leaves learned/static not saturated pre-drift; declared saturates above ~85 rps and is therefore
NOT part of the T4 baseline at an evaluable rate (it has no index and cannot recover the scan
spike), only reported for completeness.

## Evaluability gate (this package's first step)

T4 is evaluable ONLY if, at the chosen drift rate, the post-drift (81% scan) mix is ABOVE the arm's
post-drift capacity while the pre-drift (win) mix is BELOW its pre-drift capacity — i.e. the drift
causes a real throughput drop that a recovery can be measured against. If no rate exists below the
pre-drift capacity that still makes the post-drift mix saturate, T4 is UNDERIVABLE (nothing to
recover) and is reported as such, proven with the measured pre/post capacity curve, NOT faked.

Decision rule (recorded before seeing the result):
1. Validation drift run at **300 rps** (below the ~400 rps indexed win capacity), seed 42, arms
   `declared learned static-derived`, one harness invocation.
2. Read the per-window achieved throughput around the drift boundary (request index 4000, the end
   of the 2nd of 6 segments): pre-drift achieved vs post-drift achieved.
3. If learned/static show a post-drift throughput drop (post-drift achieved measurably below
   offered while pre-drift achieved ~ offered), 300 rps is the evaluable rate; proceed.
4. If learned/static hold the rate post-drift (no drop), probe 350 and 400 rps the same way to find
   the post-drift capacity. If even 400 rps shows no post-drift drop (post-drift capacity >= 400),
   report T4 UNDERIVABLE with the measured capacity curve as proof.

## Measurement protocol (exact commands)

Build once from HEAD, copy the binaries, run only the copy (the harness re-verifies no rebuild by
byte-comparing `target/release/flashdb-load` against the pinned copy before each invocation):

```
source $HOME/.cargo/env
cargo build --offline --release --bin flashdb-load --bin flashdb-scale --bin flashdb-replay
mkdir -p /tmp/wave19-drift-bin
cp target/release/flashdb-load /tmp/wave19-drift-bin/
sha256sum target/release/flashdb-load /tmp/wave19-drift-bin/flashdb-load
```

Then, per seed (one harness invocation runs the arms sequentially, one process at a time, under the
harness's own `systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0`):

```
cd harness && flock /tmp/wave19-drift.lock uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold51/per_region-mixed \
  --workdir /tmp/wave19-drift/<seed> \
  --report .scratch/73-t4-drift/<seed>.md \
  --json   .scratch/73-t4-drift/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift \
  --arms declared learned static-derived \
  --drift-rate <R> --seed <seed>
```

- `reference-rate 50` (unused by drift), `segment_requests 2000` (6 segments = 12,000 requests:
  4,000 pre-drift, 8,000 post-drift), `--window-ms 1000`.
- Seeds: **42, 43, 44 (3 independent) + 42 repeat (noise band)** = 4 harness invocations.
- Arms per invocation: declared, learned@1.0, static-derived (declared first — the learner's
  per-window reorg budget is derived from declared's observed rate; static-derived is then frozen
  from the learner's own burn-in sweep).
- Box load recorded at every boundary via `uptime`; no other `flashdb-load/scale/replay` process
  runs concurrently (checked at each boundary).

## T4 readout (how the per-window numbers are computed)

From each `<seed>.json` arm record's `windows[]`: each window carries `requests` (offered in the
window), `completed` (cumulative), `completed_delta`, `window_s` (actual wall time),
`achieved_rps = completed_delta/window_s`, and the lag/service ceiling flags. The drift boundary is
the window whose cumulative offered count first crosses 4,000 (end of the 2nd pre-drift segment).
The pre-drift throughput is the summed completions over pre-drift windows divided by their summed
wall time; the post-drift recovery window count is the number of windows after the boundary until
the arm's rolling achieved throughput is back within 10% of that pre-drift throughput. The learner's
internal `recovery_windows` (region-level re-exploration) is reported alongside as the harness's own
T4 signal, but the pre-registered T4 is judged on THROUGHPUT, so the window-level numbers above are
the primary evidence.

## Acceptance (verbatim from the task)

1. Is the drift arm evaluable? pre-drift + post-drift capacity, chosen drift rate as a fraction of
   pre-drift capacity.
2. T4: per-window throughput around the drift point for learned vs static-derived; does learned
   recover to within 10% of pre-drift throughput within 2 windows; does static-derived fail to (or
   recover measurably slower)? Window-level numbers, seed count, noise band.
3. Both judging views (SS7.1 lag-only primary; both-ceilings).
4. Honest overall T4 verdict + residual/gap.
5. Gates verbatim: `cargo build --workspace --offline` (0 warnings); `cargo test --workspace
   --offline` (totals); `cd harness && uv run --with pytest pytest -q`; `git diff --check` (silent).
   Commit; report hash and the honest T4 verdict (pilot-grade; NOT a 10-seed claim unless run).
