# 61 - WP-J: stop the learner churning on the adversarial-uniform workload (T2)

## Contract

The WP-H pilot (`.scratch/59-eval-pilot/issues.md`) measured, on the 12.25x `c51` instance, that on
the adversarial-uniform workload `declared` sustains rung 50 at 0/40 lag-rejections on BOTH seeds,
while `learned` does not (8/40 seed42, 9/40 seed43) and writes **3.6-4.4x** declared's
reorganisation bytes (110.7 / 91.1 MB vs 23.3 / 25.0 MB) at identical whole-run throughput
(106-107 rps). The pre-registration makes this disqualifying (T2: a learner that churns on noise is
rejected). This work package makes the learner stop churning on a uniform/no-locality workload so
`learned` matches `declared` on adversarial, **without** giving up adaptation where it is warranted
(the win and drift workloads must still let the learner explore/adapt).

Acceptance:

- on adversarial-uniform, learned's reorganisation is within declared's band (<= 1.5x) AND learned
  is rung-50 lag-sustained (0/40) like declared, at >= 2 seeds;
- on win, the learner still explores/adapts (decision count > 0);
- OR the exact remaining cause with evidence. Never overstate; pilot-grade labelled as such.

## Quantified diagnosis

Instrumented by replaying the actual per-window telemetry from the WP-H artifacts through the real
`LayoutLearner` (reproducing the recorded `restarts`/`gated_switches` exactly), plus reading the
recorded `learner_decisions`.

| arm (adversarial) | windows | drift restarts | gated switches | policy changes | reorg bytes | rung-50 lag-rej |
|---|---:|---:|---:|---:|---:|---:|
| learned seed42 | 76 | **45** | **66** | 111 | 110.7 MB | 8/40 |
| learned seed43 | 75 | **47** | **53** | 100 | 91.1 MB | 9/40 |
| declared seed42 | 76 | — | — | 0 | 23.3 MB | 0/40 |

`U|Stock` alone changed selection **20 times** in 76 windows, ping-ponging among
`merge_min_tiles ∈ {2, 8, 200}` and `page_size ∈ {4096, 65536}`; every other region oscillated
13-21 times. The learner never settles.

### Root cause (measured, not inferred)

The slice-39 churn controls (per-region cooldown = 3 windows; per-window reorg budget) only gate the
**switches** between re-explorations. The churn is driven upstream by the **drift detector** and the
**gate**, both keyed on a raw, load-dependent, noise-dominated cost signal:

1. **Load-dependent (the dominant cause).** The cost was the *total* per-window cost
   `count * mean_ns + λ·(bytes_written + compaction_bytes)`. The harness runs a rate ladder (offered
   rate steps 50 → 100 → 200 → 400 at windows 40 / 60 / 70), so `count` per region roughly doubles
   at each step and the total cost jumps > 50% for 2 consecutive windows — a **mass drift restart**
   in most regions at every rung boundary (measured: 11 simultaneous restarts at window 41, seed42).
   The rate ladder is a *measurement* construct, not a workload shape change.
2. **Noise-sensitive.** The gate compared *mean* costs with a 5% margin: a one-window shared-box
   latency/compaction spike (1-50x) inflated the incumbent's mean and made a rival look >5% cheaper,
   so the gate switched; and the drift detector's 50% threshold fired on transient 1-2-window spikes.
   Each switch/restart resets evidence and re-runs the gate, oscillating the merge threshold — which
   is what writes the reorganisation bytes.

The reward's write term is only **1.05-1.24%** of the service term, so it cannot counter-price the
churn (STATUS §7.5); the slice-39 `reorg_budget_bytes_per_window` (declared rate × 1.0 ≈ 307-334 KB/
window) is not the failure either (`budget_blocks` 0-7, mostly 0), because an unexplored challenger
projects 0 added bytes and the budget only blocks a switch to an *observed*-expensive candidate.

## Design (chosen) and rejected alternatives

**Chosen fix — make the learner's cost signal load-invariant and spike-robust, and raise the two
decision thresholds above the noise floor (general statistical discipline, no workload special case):**

1. **Per-request cost.** The cost sample is `region_cost_ns / count` (absolute write cost for a
   request-free region), so a rate step — which scales `count` and the service term together — does
   not look like a cost change. This kills the rung-boundary mass restarts.
2. **Median scoring.** Each arm is scored by the **median** of its per-request samples, not the
   mean, so a one-window latency/compaction spike cannot move an argmin. This is the heavy-tail-safe
   form of the bandit and is what lets a no-locality workload settle.
3. **Switch margin 5% → 50%** (the same as the drift threshold). The measured per-window cost noise
   is ~30% CV on this shared box, so 5% let noise fabricate a switch; 50% is above the noise floor
   while a genuine gradient (the win workload's 50-500% per-region spread, or a large shape change)
   still switches.
4. **Drift threshold 50% → 200%.** The rate ladder's high offered rungs (100-400) saturate the
   device and raise per-request latency 2-9x — a *load* artifact, not an access-shape change. The
   200% floor filters the rung-100 saturation while a genuine shape change (an order of magnitude)
   still re-opens exploration.

All four are parameter/state changes inside `LayoutLearner`; no engine code, no workload conditional,
and the gate's own hysteresis (`stability_window`) is untouched. The win/drift adaptation is NOT
disabled: the burn-in still explores all 6 candidates, the gate still commits per-region argmins, and
the drift detector still re-opens on a large shape change.

### Rejected alternatives (recorded, with reason)

- **Price churn harder in the reward (raise λ).** λ=1 already prices a byte at device cost; the write
  term is 1.2% of service, so no λ moves an argmin against timing noise (STATUS §7.5). It also does
  not stop the restarts, which are triggered by the drift detector. Rejected as the wrong lever.
- **Make unexplored challengers project a conservative reorg cost.** Tightens the slice-39 budget so
  the bounded burn-in is also blocked until a challenger's reorg cost is known. It does not address
  the 45-47 restarts (which re-open the burn-in) and would slow genuine adaptation. Rejected.
- **Lengthen/scale the per-region cooldown.** The cooldown is tied to `min_samples` (3): raising it
  above 3 would stretch the *initial* burn-in past rung 50 (6 candidates × cooldown windows) and the
  learner would still be exploring during the acceptance rung. Rejected as a symptom-suppressant that
  hurts the rung-50 window.
- **Freeze after the reorg budget is exhaustively spent / commit-once.** A cumulative freeze or a
  "commit once then stop" gate would bound the churn, but it changes the gate's semantics enough to
  invalidate the existing slice-39 tests (which pin that cooldown+budget is what bounds the
  uncontrolled churn) and would need a larger re-design. Deferred as a residual, not adopted here.
- **Disable drift detection on adversarial / globally.** Rejected: the acceptance requires the
  win/drift workloads to keep adapting, and a workload conditional is a hardcoded special case.

## Change

`harness/flashdb_harness/layout_learner.py` only (plus tests):

- `_Arm` stores `cost_samples: list[float]` and exposes `median_cost` (median of samples; `inf` if
  unsampled) instead of `sum_cost`/`mean_cost`.
- `record` appends the per-request cost sample (`region_cost_ns / count`, or the absolute write cost
  when `count == 0`) and notes drift on the same per-request quantity.
- `_gate`, `_ucb_pick`, `_initial_pick`, `_reexplore_targets` use `median_cost`.
- defaults: `margin = 0.5`, `drift_margin = 2.0` (were 0.05 / 0.5); `drift_windows`, `min_samples`,
  `cooldown_windows`, `reorg_budget_bytes_per_window` unchanged.
- `_restart` clears `cost_samples` alongside `n`.

## Tests

`harness/tests/test_layout_learner.py`:

- `test_drift_is_load_invariant_a_rate_step_does_not_fire` — a count-doubling rate step (mean_ns
  unchanged) must not fire a drift restart. Fails against the pre-change total-cost drift signal.
- `test_median_cost_holds_the_incumbent_through_a_one_window_spike` — one 100x stall window must not
  make the gate switch to a noise-cheaper rival. Fails against the pre-change mean-cost gate.
- `test_targeted_drift_response_window_count_and_the_two_window_gap` — pre-drift phase extended to
  `candidates * min_samples` windows so the burn-in completes before the drift boundary (the median
  reference is a clean pre-drift baseline); the drift still re-explores and recovers.

Full suite: **134 Python tests, 291 Rust tests** green (below).

## Before / after (measured, c51 12.25x, shared box, load avg 4-5 during the runs)

Learner decision counts (final config, `λ=1.0`, `cooldown=3`, `reorg budget = declared rate × 1.0`):

| workload/arm | drift restarts | gated switches | decision-change windows |
|---|---:|---:|---:|
| adversarial learned seed42 | 8 | 4 | — |
| adversarial learned seed42 (repeat) | 5 | 3 | — |
| adversarial learned seed43 | 6 | 15 | — |
| adversarial learned (pilot, pre-fix) | 45-47 | 53-66 | 100-111 changes |

Reorganisation bytes (the T2/T3 quantity):

| workload | arm | seed42 | seed43 | seed42 repeat |
|---|---:|---:|---:|---:|
| adversarial | declared | 25.0 MB | 23.3 MB | 23.3 MB |
| adversarial | learned (pre-fix pilot) | 110.7 MB | 91.1 MB | — |
| adversarial | learned (post-fix) | 60.1 MB | 19.7 MB | 4.4 MB |
| adversarial | learned/declared ratio | **2.40x** | **0.84x** | **0.19x** |
| win | declared | 8.86 MB | 8.02 MB | 6.40 MB |
| win | learned (post-fix) | 2.16 MB | 2.84 MB | 3.44 MB |
| win | learned/declared ratio | 0.24x | 0.35x | 0.54x |

Rung-50 schedule-lag rejections (the primary T2 metric):

| workload | arm | seed42 | seed43 | seed42 repeat |
|---|---:|---:|---:|---:|
| adversarial | declared | 1/40 | 0/40 | 1/40 |
| adversarial | learned (pre-fix pilot) | 8/40 | 9/40 | — |
| adversarial | learned (post-fix) | 1/40 | 0/40 | 1/40 |

Whole-run achieved throughput is unchanged (adversarial 106-107 rps; win 73-76 rps, both arms equal).
On win the learner still adapts: **12/12 regions non-default on both seeds**, 7-13 decision-change
windows, reorg 0.24-0.54x declared. On adversarial the learner's per-region `gated_switches` +
`restarts` fell from 111/100 (pilot) to 12/8/21 (post-fix).

### Both judging views

- **Lag-only (STATUS §7.1):** adversarial learned is rung-50 lag-sustained on both seeds (0-1/40,
  matching declared's 0-1/40 on the same box); the pre-fix 8-9/40 failure is gone. Win is still not
  lag-sustained for ANY arm (its 30% movement-scan share does cold reads on the pacer — WP-I's
  readiness gap, unchanged by this package).
- **Both ceilings (pre-registration):** the service ceiling rejects ~69-76/40 windows whole-run on
  every arm (the per-group `fdatasync` durability floor, STATUS §3), so both-ceilings sustained = 0
  rps everywhere — the open §7.1 reconciliation, NOT resolved here.

## Verbatim gates (final HEAD)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.05s
```
2. `cargo test --workspace --offline` — totals: **291 passed; 0 failed; 0 ignored**
   (124 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints + 3 family_colocation
   + 11 group_commit + 6 manifest_engine + 7 merge_threshold + 14 metrics + 33 multi_tile + 7 order_flow
   + 1 partitioned_publish_cap + 5 read_requests + 3 region_compaction + 6 region_partition +
   3 replay_trace + 8 request_commit + 2 scale_bin + 3 tile_format + 3 tile_layout + 12 tile_read +
   0 flashdb-replay + 0 flashdb-scale + 0 main + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
134 passed in 3.94s
```
4. `git diff --check` — silent (exit 0).

## Acceptance verdict

**PILOT-GRADE, PARTIAL-PASS with the exact remaining cause stated.** On adversarial-uniform at
2 seeds + 1 repeat (12.25x c51, shared box):

- **Rung-50 lag (the disqualifying failure) is FIXED:** learned 0-1/40 on every run, matching
  declared's own 0-1/40 under the same load, vs the pre-fix 8-9/40.
- **Reorganisation is dramatically reduced:** learned/declared ratio **0.19x, 0.84x, 2.40x** (median
  0.84x) vs the pre-fix **3.6-4.4x**; the `<= 1.5x` band is met on 2 of 3 adversarial runs and
  exceeded on one (2.40x).
- **Adaptation is not disabled:** on win the learner still explores and commits non-default policies
  in **12/12 regions on both seeds** (decision count > 0), writing 0.24-0.54x declared's reorg.

The one 2.40x run is a specific late-run (rung 100+) episode: a drift restart re-committed a
different `merge_min_tiles` for `P|Customer` and the region oscillated 2↔200↔8, producing five
10.3 MB merges (51.5 MB of the 60 MB). The trigger is the rate ladder's device saturation (per-request
latency 2-9x at offered 100-400 rps) occasionally crossing the 200% drift threshold; the trajectory
is timing-dependent on the shared box (the pilot already documented a 14x reorg spread). This is the
**exact remaining cause**, and it is a saturation artifact of the measurement ladder, not the rung-50
noise churn the pre-registration disqualifies.

## Residual items

1. **Adversarial reorg is not yet robustly <= 1.5x on every run** (0.19-2.40x spread): the remaining
   variance is late-rung device-saturation drift. A quiet box (the full 10-seed campaign's own
   prerequisite) or a deeper re-design (commit-once per epoch, a rolling/rate-rebaselined drift
   reference, or a cumulative reorg freeze) would close it; all three were considered and deferred as
   too large a semantic change for this package.
2. **T4 drift sensitivity is now set by `drift_margin = 2.0`** (a 200% per-request cost change before
   re-exploration). T4 was already underived (ticket 58: the 2-window target is unreachable at the
   published defaults); the exact drift-workload cost-change magnitude has not been measured, so this
   threshold is a reasoned choice, not a verified one. Re-derive T4 on the drift workload before
   relying on it.
3. The **primary-vs-service-ceiling reconciliation stays an OPEN AUTHOR ITEM** (unchanged, per the
   task): the pre-registration was not edited.
4. The reorg budget is still a per-region projection; the engine's global compaction selection can
   funnel merges into an eager neighbour (slice 37/38), so the reorganisation figure is a bound under
   this control, not a proof of optimality.

## Dispatch

- Timestamp: 2026-10-14T02:41:00+08:00 (final commit).
- Actor: worker (single writer, `wave9/churn`, worktree `/home/z/vibe/worktrees/flashdb/wave9-churn`).
- Base HEAD at start: `59b01bc` (integrated master minus the wave8 record commit); final HEAD below.
