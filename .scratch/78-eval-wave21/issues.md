# 78 - WP-X: the combined post-wave21 evaluation (rung-50 residual + T4 + T1/T2/T3 regression)

## Contract

Work package X of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave22-eval`
(isolated worktree, branch `wave22/eval`, based on integrated master `ce66f3e` carrying the
wave21/tilecap fix (ticket 75) and the wave21/driftshape write-spike drift shape (ticket 77)).
Do NOT cd to `/home/z/vibe/flashdb`. One flashdb process at a time under
`MemoryMax=128MiB`; `flock /tmp/wave21-measure.lock -c '<full command>'` serialises every
measurement command on the shared box; box load recorded at every boundary. Do NOT edit
`.scratch/15-objective-v0/pre-registration.md`, and do NOT change any pre-registered threshold
(the 2-window T4 target, the 10% band, any ceiling, the 15%/5%/3x numbers).

This is the **combined confirmation run** on the fully integrated tree. Ticket 71 re-derived
T1/T2/T3 (rung-100 lag-only sustain, rung-50 residual = compaction-adjacent cold reads + device
fsync tail) and ticket 77 re-derived T4 with the new write-spike drift shape (EVALUABLE, measured
NOT SUPPORTED). This package re-runs all of it in one pass against the integrated HEAD to confirm
the combined result, then records the per-arm/rung tables, both judging views, T1-T4 verdicts with
seed counts and noise band, an overall statement, and the residual.

**Acceptance (verbatim from the task):** the honest §7.1 lag-only verdict on T1/T2/T3/T4, per-arm
per-rung lag-only tables (is rung 50 sustained now? rung 100?), T1 throughput, T2 adversarial,
T3 win reorg, T4 with the new drift shape, both judging views, a frank overall statement at the
achieved seed count, new-defect status, and slice-39 status. Pilot-grade labels only; a 10-seed
pre-registered verdict is claimed only if actually run (it will not be).

## Judging views (both reported; neither edited)

- **§7.1 lag-only (primary).** A rung is lag-sustained iff its achieved rate is within 5% of the
  offered rate, it has zero error windows, and **zero schedule-lag-ceiling rejected windows**
  (`lag_ceiling_rejected_windows == 0`). The service ceiling is reported separately as the
  efficiency measure and does not veto the lag-only reading.
- **Both ceilings (the harness's built-in `sustained` / T1 verdict).** A rung is `sustained` only
  if BOTH the service ceiling and the lag ceiling reject zero windows. The harness's `sustained`
  field and its T1 threshold table are computed this way. Under the adopted §7.1 lag-only primary
  they are NOT the primary metric, so the lag-only numbers are re-derived from the per-rung
  `lag_ceiling_rejected_windows` and `error_windows` fields, not quoted from the harness verdict.
  The harness prints `sustained=false` at every rung for every arm because the service ceiling
  rejects ~16-40 windows per rung (the per-group `fdatasync` device floor vs a fixed 10 ms), so
  its both-ceilings T1 verdict is `NOT-MEASURABLE` — the 0/0 artifact, not the lag-only answer.

## Measurement protocol (exact commands)

Build once from HEAD, copy the release binaries, verify the byte hash, and run only that copy (the
harness re-verifies no rebuild by byte-comparing `target/release/flashdb-load` against the pinned
copy before each invocation):

```
source $HOME/.cargo/env
cargo build --offline --release --bin flashdb-load --bin flashdb-scale --bin flashdb-replay
mkdir -p /tmp/wave22-eval-bin
cp target/release/flashdb-load /tmp/wave22-eval-bin/
cp target/release/flashdb-scale /tmp/wave22-eval-bin/
cp target/release/flashdb-replay /tmp/wave22-eval-bin/
sha256sum target/release/flashdb-load /tmp/wave22-eval-bin/flashdb-load
cp -a /tmp/cold51/per_region-mixed /tmp/wave22-eval/per_region-mixed   # 1,643,925,811 B, 12.25x
```

The harness `build_binaries` still rebuilds `target/release/flashdb-load` (a no-op; no Rust
changed) and runs that path, which is byte-identical to the pinned copy (sha256 re-checked at the
end). Per arm the harness makes its own `cp -a` of the base instance under `MemoryMax=128MiB`.

The T1/T2/T3 win+adversarial runs use all three arms; the T4 drift run uses learned + static-derived
only (declared is not a T4 subject — it saturates ~85 rps on the win mix, tickets 73/77 — and its
1450-rps end-of-run drain would be expensive and useless). Every measurement command is wrapped in
`flock /tmp/wave21-measure.lock -c '<full command>'`; `--drift-rate` is always explicit (never
`auto`).

Per seed, run 1 (win + adversarial; T1/T2/T3):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave22-eval/per_region-mixed \
  --workdir /tmp/wave22-eval/<seed> \
  --report /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>.md \
  --json   /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads win adversarial \
  --arms declared learned static-derived \
  --seed <seed>'
```

Per seed, run 2 (drift; T4, write-spike shape):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave22-eval/per_region-mixed \
  --workdir /tmp/wave22-eval/<seed>-drift \
  --report /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>-drift.md \
  --json   /home/z/vibe/worktrees/flashdb/wave22-eval/.scratch/78-eval-wave21/<seed>-drift.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --max-tiles 4096 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift \
  --arms learned static-derived \
  --drift-shape write --drift-factor 10 --drift-pre-segments 6 --drift-rate 1450 \
  --seed <seed>'
```

- Win/adversarial ladder `50/100/200/400` rps (`reference-rate 50`, `rate_multiples [1,2,4,8]`),
  `segment_requests 2000` per rung (rung 50 = 40 windows; rung 100 = 20; rung 200 = 10; rung 400 = 5).
- Drift: `write` weight x10, `pre=6` pre-drift segments (drift boundary at request 12,000), fixed
  1450 rps, `segment_requests 2000` (10 segments = 20,000 requests).
- Seeds run: **42, 43, 44 (3 independent) + 42 repeat (noise band)** = 4 runs per workload set.
- The eager warm-up pass runs in every arm-run (`startup_warm`); static-derived win runs adopt the
  index eagerly (`index_entries=30000`), declared never wants it, learned starts index-OFF by the
  WP-O demand gate.
- The learner runs without the declared-derived reorg budget on the drift runs (declared excluded
  there); `--learner-cooldown-windows 3` stays active.
- Box load recorded at every boundary (1-minute loadavg).

## Gates (verbatim, to run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — totals.
3. `cd harness && uv run --with pytest pytest -q`.
4. `git diff --check` — silent.
