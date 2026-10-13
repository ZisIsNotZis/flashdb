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

| arm (adversarial) | windows | drift restarts | gated switches | policy changes | reorg bytes | lag-rej whole-run |
|---|---:|---:|---:|---:|---:|---:|
| learned seed42 | 76 | **45** | **66** | 111 | 110.7 MB | 24/40 |
| learned seed43 | 75 | **47** | **53** | 100 | 91.1 MB | 17/40 |
| declared seed42 | 76 | — | — | 0 | 23.3 MB | 0/40 (rung 50) |

`U|Stock` alone changed selection **20 times** in 76 windows, ping-ponging among
`merge_min_tiles ∈ {2, 8, 200}` and `page_size ∈ {4096, 65536}`; every other region oscillated
13-21 times. The learner never settles.

### Root cause (measured, not inferred)

The slice-39 churn controls (per-region cooldown = 3 windows; per-window reorg budget) only gate the
**switches** between re-explorations. The churn is driven upstream by the **drift detector**, which
re-opens exploration 45-47 times per run because its signal is wrong in two ways:

1. **Load-dependent (the dominant cause).** The drift deviation is computed on the *total*
   per-window cost `count * mean_ns + λ·(bytes_written + compaction_bytes)`. The harness runs a
   rate ladder (offered rate steps 50 → 100 → 200 → 400 at windows 40 / 60 / 70), so `count` per
   region roughly doubles at each step and the total cost jumps > 50% for 2 consecutive windows —
   the drift detector fires a **mass restart** in most regions at every rung boundary. Measured:
   the rate step at window 40 fired 11 simultaneous restarts (seed42); the steps at 60 and 70 fired
   the next two mass-restart waves. The rate ladder is a *measurement* construct (finding the
   sustained ceiling), not a workload shape change; the learner must not treat load scaling as
   drift.
2. **Noise-sensitive.** The remaining restarts are transient 1-2-window latency/compaction spikes on
   the shared box (`mean_ns` spiking to 1-5 ms for a window or two, e.g. windows 22-23, 48-49,
   51-52, 63-64), which exceed the `drift_margin = 0.5` relative threshold for the default
   `drift_windows = 2` consecutive windows. These spikes are themselves amplified by the churn (a
   merge-threshold switch triggers the next compaction), so the loop feeds itself.

Each restart resets the region's evidence to 3 samples and re-runs the gate; the gate's noisy
3-sample means then oscillate the merge threshold, which is what writes the reorganisation bytes.
The reward's write term is only **1.05-1.24%** of the service term (`reorg_to_service_ratio`
recorded per run), so it cannot counter-price the churn — but it is not the cause; the restarts are.

The slice-39 `reorg_budget_bytes_per_window` (declared rate × 1.0 ≈ 307-334 KB/window) is also not
the failure: `budget_blocks` is 0-7 per region (mostly 0), because an unexplored/under-sampled
challenger projects 0 added bytes and the budget only blocks a switch to an *observed*-expensive
candidate. The budget is a band-aid over a detector that re-opens exploration for the wrong reason.

## Design (chosen) and rejected alternatives

**Chosen fix — make the drift detector load-invariant and noise-robust (general statistical
discipline, not a special case):**

1. **Per-request drift signal.** Key the drift deviation on **per-request cost**
   `region_cost_ns / count` instead of total window cost, and set the region's drift reference as
   the active arm's per-request mean (`sum_cost / sum_count`). A rate step scales `count` but not
   per-request cost, so load scaling no longer looks like drift; a real access-shape change (the
   drift workload's scan share ×10, which moves `mean_ns` 10-50x and stays) still fires. This is
   the same quantity the reward already prices, normalised to the unit the drift claim is about
   (shape, not volume).
2. **Noise robustness.** Require the deviation to persist for more consecutive windows (raise the
   default `drift_windows`), so the 1-2-window shared-box latency/compaction spikes do not fire a
   restart while a sustained shape change still does. The exact value is chosen from the replay
   below (transient spikes are 1-2 windows, occasionally 3-4 at saturation; real drift is
   persistent).

Both are parameter/state changes inside `LayoutLearner`; no engine code, no workload special-case,
and the gate's existing hysteresis (`margin` + `stability_window`) is untouched.

### Rejected alternatives (recorded, with reason)

- **Price churn harder in the reward (raise λ).** λ=1 already prices a byte at device cost; the
  write term is 1.2% of service, so no λ moves an argmin against timing noise (STATUS §7.5). Raising
  λ would also change every verdict's stated unit and does not stop the restarts, which are
  triggered by the drift detector, not the reward. Rejected as the wrong lever.
- **Make unexplored challengers project a conservative reorg cost.** This tightens the slice-39
  budget so the bounded burn-in is also blocked until a challenger's reorg cost is known. It does
  not address the 45-47 restarts (which re-open the burn-in) and would slow genuine adaptation.
  Rejected: the budget is not the cause.
- **Lengthen/scale the per-region cooldown.** The cooldown already gates restarts, but the restarts
  are 3+ windows apart, so a longer cooldown only delays the same oscillation; it does not remove
  the false drift signal. Rejected as a symptom-suppressant.
- **Freeze after the reorg budget is exhaustively spent.** The budget is per-window, not cumulative;
  making it cumulative would be a new global freeze knob that stops adaptation on win/drift too.
  Rejected: freezes globally, which the acceptance forbids.
- **Minimum-improvement gate only.** Tightening the gate's `margin`/`stability_window` reduces
  *gated switches* but not the 45-47 *drift restarts* (which re-explore and switch during burn-in
  regardless). Rejected as incomplete on its own.
- **Disable drift detection on adversarial / globally.** Rejected: the acceptance explicitly
  requires the win/drift workloads to keep adapting, and a workload-conditional switch would be a
  hardcoded special case.

## Change

`harness/flashdb_harness/layout_learner.py` only (plus tests). Details after implementation.

## Tests

Extend `harness/tests/test_layout_learner.py` (and `test_cold_0b.py` if it exercises the learner) to
pin the pre-change churn behaviour: a per-request-normalised drift signal does not fire on a pure
rate step, does not fire on a 1-2-window noise spike, and still fires on a sustained shape change.
Tests must fail against the pre-change behaviour where meaningful.

## Before/after

To be filled from the engine measurement (adversarial + win, 2 seeds + repeat).

## Verbatim gates

To be filled (cargo build / cargo test / pytest / git diff --check).

## Acceptance verdict

To be filled.

## Residual items

To be filled.
