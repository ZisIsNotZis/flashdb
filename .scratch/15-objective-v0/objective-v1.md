# Objective v1 — decided by the author 2026-10-01

The author resolved the two questions that gated 0b, in their own words: the engine should
**learn data and access patterns**, and the score is **final throughput / efficiency — whether it
can hold up under high throughput and high load**. Everything else is delegated to the
orchestrator under the project's existing philosophy (I2: a bad layout may be slow, never wrong;
declared controls; deterministic replay; evidence before claims).

This SUPERSEDES the estimand choice in `provisional-2_GOAL.md`, which was written while the
author was away. It keeps that document's measurement discipline (deterministic metrics primary,
seeds reported, calibration error stated) and replaces the latency-penalty formulation.

## 1. Primary metric: sustained throughput against offered load

Measure a **capacity curve**, not a single number:

- a fixed offered-load profile (request mix, Zipfian key popularity, declared durability) stepped
  through levels, e.g. 1x, 2x, 4x, 8x of a reference rate;
- at each level report **achieved throughput** (completed operations per second, and bytes per
  second), the **queue/latency** it implies, and the run's efficiency;
- the headline scalar is the **highest load level at which the engine sustains the offered load
  without queue growth**, together with the efficiency measured there.

Why this is the right primary metric for "can it hold up": a store that is overwhelmed does not
merely slow down, it **stops completing** the offered work - achieved throughput falls below
offered load and the backlog grows. Throughput is therefore a *sufficient* statistic for being
overwhelmed, and it is the one the author named.

Why a latency ceiling is still mandatory as a CONSTRAINT (not a score): without it, throughput
could be "won" by abandoning tail latency entirely (unbounded queueing looks like high throughput
until the backlog is counted). Rule: a window in which the p99 exceeds a pre-registered ceiling,
or in which the backlog grows across consecutive windows, is marked invalid and its throughput is
not counted. The ceiling is a declared, pre-registered number per pattern, anchored the same way
as before (2x the reference-load p99), so it moves with the machine but not with the candidate.

## 2. Secondary metrics (efficiency, reported alongside)

| metric | why |
|---|---|
| write amplification (bytes written / bytes ingested) | the measured lever: cap 12 -> 96 cuts it ~59% (ticket 19) |
| bytes read per operation, per pattern | separates a layout that helps probes from one that helps scans |
| RSS per byte of data (data:RAM) | currently ~22-24:1; a layout must not buy speed with memory |
| CPU time per operation | distinguishes device-bound from CPU-bound behaviour (a real effect we measured: 4 KiB vs 64 KiB pages) |
| recovery time to serve | ticket 13's open O(data) issue |

## 3. What "learn data and access patterns" means concretely

The physical decisions that (a) preserve correctness and (b) have measured headroom are, in
descending order of measured leverage:

1. **Merge/compaction policy and trigger, per prefix-region** - the 59% lever; the caller must
   not fix it globally (today the driver picks one budget for everything). This is the axis the
   learner controls, informed by each region's observed read/write ratio and access shape.
2. **Page size and bloom bits per tile** - ~3% on the read mix, but measured to matter a lot in
   specific regimes (64 KiB pages: scan +41%, point probe -4.6x), so the learner should exploit it
   where a region is scan-heavy or probe-heavy, not globally.
3. **Family partitioning** - CLOSED by measurement at the current cap (write amp 4.09 -> 7.75);
   reconsider only after ticket 19 raises the cap, and even then the simulated ceiling is +8.7%,
   below the pre-registered 15% threshold.

The learned object is therefore a **per-region policy** chosen from a published, small candidate
set (so a relaxed oracle over it is tractable), evaluated by the capacity curve above.

## 4. Consequences for the harness

- The driver must be able to **offer load** (a rate, not just a batch of requests) and to measure
  achieved throughput per window, with Zipfian popularity and **injected drift**; today it replays
  a fixed trace as fast as possible.
- The two controls stay as designed: `flashdb-declared` (one fixed policy) and
  `flashdb-static-derived` (offline-chosen policy from a frozen trace, no adaptation).
- Deterministic metrics remain the tie-break for policy comparisons, because this box is shared
  and wall-clock varies run to run (recorded in `.scratch/12`).
- The pre-registered falsification contract now names the capacity metric: at >=10x cache-size
  data, the learner must sustain at least one load level higher than `flashdb-declared` (or beat
  it by >=15% in achieved throughput at the same offered load) on the win workload, and must not
  lose on the adversarial-uniform workload. Threshold and seed count remain to be fixed before
  the run, and the estimate must state its calibration error.

## Update 2026-10-01: which actuator the measurements actually support

Slice 26's per-region telemetry was built precisely to answer "what would the learner act on", and
it redirected the plan by measuring instead of assuming:

| axis | status after measurement |
|---|---|
| per-region bloom bits | **dead on this workload.** Every absent probe is rejected by the range filter before the bloom is consulted (new monotonic order numbers sort above the tile's last key; `U|Backorder` has no tile), so absent probes cost 0 block bytes. The measured bloom gradient (151.84 -> 1.59 us) needs absent probes INSIDE a tile's range, which this flow does not produce. |
| per-region page size | **flat** (slice 25): the scanning region is JSON-decode bound, so block size never reaches the critical path. |
| **per-region compaction trigger** | **live, with a measured gradient.** Write-only regions (`P|Order`, movement families) accumulate with zero read pressure while probe-hot regions (`P/U|Stock`, `P/U|Customer`) must stay few-block. Write amplification is the efficiency metric (4.09 -> 1.66 measured from cap headroom; up to 59% in the tier simulation) and the per-region p99 is the constraint that prevents "defer everything" from winning. |

So the learned object is a per-region merge/compaction policy (how eagerly a region's tiers are
merged), applied per emitted tile, with the capacity curve as the primary metric and write
amplification reported alongside. Blooms and page size stay in `LayoutPolicy` as available knobs
whose gradient is real but absent on this workload - the honest statement is that they would matter
for a workload with in-range absent probes or block-bound scans, and the harness can now prove which
case a workload is.
