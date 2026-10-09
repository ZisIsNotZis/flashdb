# Pre-registration (draft for the author's sign-off; runnable as written)

Status: drafted 2026-10-01 under the author's decisions in `objective-v1.md` (learn data and
access patterns; score sustained throughput / efficiency under high load) and their instruction to
proceed autonomously. It states what would **falsify** the thesis, with numbers, before 0b is run.
The author may change any number; changing one after seeing results invalidates the run.

## Hypothesis

At a data:memory ratio of at least 10:1, a per-region layout policy chosen by observing the
workload's data and access patterns sustains a higher offered load than a fixed declared policy, at
equal durability, without losing on a workload that has no locality to exploit.

## Metric (objective v1)

Primary: the **highest offered-load level the engine sustains** over the run (achieved throughput
within 5% of offered, `lag p99 <= 10 ms`, no window rejected), with the offered-load levels stepped
as 1x, 2x, 4x, 8x of a pre-registered reference rate. Secondary and reported: achieved throughput at
each level, the p99 service latency distribution, and the efficiency metrics (write amplification,
bytes read per op per pattern, RSS per data byte, CPU per op, recovery time).

Deterministic metrics are the tie-break for policy comparisons (this box is shared; wall-clock
varies run to run - recorded in `.scratch/12`).

Validity rule (from objective v1): a window is INVALID if its p99 exceeds the pre-registered
ceiling, if the schedule lag grows across three consecutive windows, or if any request is rejected.
Throughput from invalid windows is not counted, so the metric cannot be won by abandoning latency.

## Arms (all at equal durability: same group-commit batch-size distribution)

| arm | policy |
|---|---|
| `flashdb-declared` | the shipped default: one global policy, 4 KiB pages, 10 bloom bits, fixed merge budget |
| `flashdb-static-derived` | the offline best per-region policy chosen from the PRE-DRIFT trace once, built once, no adaptation |
| `flashdb-learned` | the learner: per-region policy adapted online from observed access patterns |
| relaxed oracle | per-region best over the published candidate set, computed offline with hindsight over the same run (reported as relative regret) |

## Workloads (from `docs/dev-loop.md`, scoped to what this layout space can move)

1. **win-capable**: the seeded inventory flow with Zipfian popularity, where per-region access
   shapes genuinely differ (`movements_scan` scans vs `stock/customer` probes; write-hot `P:Stock`
   next to append-only `R`/`U` families).
2. **adversarial-uniform**: uniform-random point lookups with no locality. The learner must
   recognise there is nothing to win and **not churn** - scored as a tie or a loss, never as a win.
3. **drift**: the win workload with a deterministic injected change (a shape's weight multiplied, or
   one key's popularity spiked) partway through; scored as **time to recover** (windows to return
   within 10% of the pre-drift throughput at the same offered load) with the reorganisation payback
   window stated.

## Instance size, seeds, aggregation

- Data >= **10x** the memory budget. At today's engineering scale: a cgroup cap of 512 MiB with
  >= 5 GiB of data (>= 10x); the full 128 GiB / 8 GiB regime stays a later run.
- **10** independent access-distribution seeds per arm; report **median and IQR**, and a win is
  accepted only if the median shows it and the IQR does not contradict it.
- Drift is injected deterministically (not sampled), so it is identical across arms.

## Falsification thresholds (pre-registered)

- The thesis is **rejected** if `flashdb-learned` does not beat `flashdb-declared` by **> 15%** in
  sustained load level (or in achieved throughput at the highest common sustained level) on the win
  workload, as a median over 10 seeds.
- The thesis is **rejected** if `flashdb-learned` loses to `flashdb-declared` by **> 5%** on the
  adversarial-uniform workload (a learner that churns on noise is disqualifying).
- The thesis is **rejected** if the learner needs more than **3x** the reorganisation write volume of
  the declared arm to reach its result (buying throughput with endurance is not the claim).
- A drift claim is supported only if the learner recovers to within 10% of the pre-drift throughput
  within **2 windows** while `flashdb-static-derived` does not (or recovers measurably slower).

## Model-calibration gate

Verdicts may be quoted from the simulator only if its per-region prediction error is **<= 25% at the
90th percentile of regions**, against real engine runs. The current cost model **cannot be evaluated
at all** until a cold-cache measurement separates the device terms from the CPU terms (recorded in
`.scratch/14-layout-harness/gap-analysis.md`); until then every simulated verdict is reported as
model-uncalibrated, and only real-engine arms may decide the thresholds above.

## What is NOT claimed

No absolute-throughput claim against other engines (Postgres is a passive scoreboard with its tuning
protocol fixed in advance; no tuning race). No bounded-memory claim beyond what is measured. No
claim about key-family co-location (L2) or cross-entity clustering, which this layout space cannot
express. No claim that the learner beats a per-tile prescient oracle - that space is combinatorial
and the relaxed oracle over the published candidate set is the stated reference.

## Sign-off list for the author

1. The 15% / 5% / 3x thresholds and the 10-seed count.
2. The instance size (10x cache; 512 MiB cap + 5 GiB data as the engineering scale).
3. The reference rate used to define the load steps.
4. Whether the latency ceiling is the p99 SLO from `provisional-objective.md` sec. 2 (2x the
   reference-load p99) or a fixed absolute number.
5. That a rejected verdict on this layout space is publishable as a negative result (the design docs
   require publishing the null, and this space's read-side knobs already measured only ~3%).

## Proposed additions (2026-10-06, awaiting sign-off; nothing has been run against them)

**Proposal B1 — external baselines as arms.** `sqlite` (WAL enabled, `synchronous` setting stated explicitly, one writer), `postgres` (a frozen disclosed config, no tuning race), `lmdb` and `rocksdb` (the honest comparisons named in the design log). Equal durability as closely as each product allows, and the difference disclosed. One request-level harness drives every arm, because flashdb has no SQL and its payload is opaque. Every arm runs in the same memory-constrained regime, and the same latency rule is applied to the baselines when computing their sustained level.

**Proposal T5 — low-load non-inferiority.** At the 1x rung flashdb must be within **1.5x** of the slower of sqlite/postgres; **2x** is the fail line; a **10x** gap is a design failure.

**Proposal T6 — high-load superiority.** At the highest common offered level flashdb must reach at least **3x** the best baseline's sustained throughput under the same latency rule.

**Proposal T7 — initial-policy quality.** The declared arm alone must already pass T5, which turns "the initial policy must not be absurd" into a number on a design deliverable.

The current pre-registration compares arms only against each other (T1–T4: learned vs declared / static-derived / oracle); this external claim is therefore untested.
