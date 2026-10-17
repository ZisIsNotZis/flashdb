# M1 — external baseline scoreboard (T5/T6/T7): measurement report

Status 2026-10-17, `wave/m1-baselines`. Authority: the pre-registered contract
(`.scratch/15-objective-v0/pre-registration.md` §"External baselines and thresholds") and the
milestone's acceptance (`docs/roadmap.md` M1). Nothing pre-registered was changed; no engine code was
touched. The method changes this report depends on — the per-engine memory boundary, seeding as
setup, verification as a separate step — are recorded in
`.scratch/80-external-baselines/issues.md` §"Measurement record".

**Two runs, and only one of them is at the pre-registered regime.** Both were driven through the
locked runner, and both are in `report.json`.

| run | instance (docs) | flashdb data | regime | rungs | what it is |
|---|---|---|---|---|---|
| A. harness check | 30k customers, 30k skus x 4 loc | 30.8 MB | **0.23x** of the 128 MiB cap | 50/100/200/400 rps, 20 s each | the full ladder, every arm, every rung — PROVISIONAL, not the verdict |
| B. regime run | 200k customers, 2.65M skus x 4 loc | 1,683,903,919 B | **12.546x** of the cap | 50 rps (the pre-registered 1x rung), 20 s | T5/T7 evaluated here; T6 has no ladder at this regime |

The pre-registered floor is data ≥ 10x the memory budget (1.28 GiB at the 128 MiB cap). Run B meets it
for flashdb (12.546x) and is the one that answers T5/T7; run A does not meet it and is retained only
as the harness check that its ladder works. Both runs were measured on this shared 8-core box and both
carry the machine's load average at every boundary (`loadavg.jsonl` beside each run's records); §3.1
and §6 use that record rather than a promise of quiet.

## 1. The rule, applied identically to every arm

A window is rejected when any of three flags fires, and the flags are the same for every arm:

* `lag_ceiling` — the window's per-request schedule-lag p99 exceeds **10,000 us**
  (`--lag-ceiling-us`, the pre-registration's 10 ms bound);
* `lag_saturation` — lag grew for **3 consecutive windows**, where a window joins the streak when its
  p99 breached *or* when any of its requests lagged past the ceiling; a window with neither resets the
  streak. This is `flashdb-load`'s own clause (`late_growth_windows`, `saturated =
  late_growth_windows >= 3`, `reject_reason` in `engine/src/bin/flashdb-load.rs`), and it matters
  because one late request in a 201-request window barely moves the p99: a window grows while its p99
  still reads under the ceiling. It fired on this data (run A rung 200, flashdb-declared: 4
  `lag_ceiling` + 2 `lag_saturation` windows);
* `errors` — any request in the window errored.

The flashdb arms' windows come from the driver; the SQL arms' windows are emitted by the harness in
the driver's own JSONL dialect, and the harness's reducer now applies all three flags
(`WindowSamples.advance_growth` / `snapshot` / `_reject_reason` in `baseline_0b.py`). Before this fix
the SQL reducer could only reject on `lag_ceiling` and `errors`, so the SQL arms were scored by a
weaker instrument than flashdb's — the one acceptance claim this milestone must not get wrong.
`tests/test_baseline_0b.py::test_lag_growth_is_rejected_exactly_where_flashdb_load_rejects_it` binds
the two: the same per-window series (201 requests, one late, p99 under the ceiling, three windows in a
row) is scored by the reducer and by a transcription of the driver's rule, and the verdicts must agree
window by window, including the reset on a clean window.

A rung is **sustained** when the achieved rate is within 5% of the offered rate
(`cold_0b.SUSTAINED_TOLERANCE`) and zero windows were rejected; `summarize_curve` adds its
`rate_exceeded` guard. Service latency is reported beside the rule, not used by it.

One process at a time for every rung: `flock /tmp/m1-measure.lock`, held for the whole rung by
`scripts/m1-baseline-run.sh`, which also records its own argv, both caps and the lock path in the
rung's `spec.json`, and the machine's load average at every `prepare` / arm / report boundary in
`loadavg.jsonl`. Nothing about the run's conditions rests on this report's word.

## 2. Table A — the ladder (harness check, PROVISIONAL, data 0.23x of the cap)

Instance: 120,000 stock cells (30k skus x 4 loc) + 30,000 customers, seed 42, zipf theta 1.0, one
frozen trace per rung (rung-50 `9a7b06a4…`, rung-100 `dcbf758d…`, rung-200 `30778694…`, rung-400
`14e966c1…`, identical for every arm), 20 windows of 1 s per rung, B=64 on every arm. Columns: engine
cap; the **serving** peak (measured phase only — see §5); the arm-scope lifetime peak (includes the
bulk load) where it differs; the construction peak (flashdb's separate seed scope); instance bytes;
oracle.

| arm | offered rps | achieved rps | achieved/offered | lag p99 us | service p99 us | windows | rejected windows (reason) | sustained | mean group | cap | serving peak B | arm-scope peak B | seed peak B | data B | oracle |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 49.95 | 99.9% | 4,949 | 11,572 | 20 | 0 | yes | 1.00 | 128 MiB | 11,198,464 | — | 91,148,288 | 24,541,978 | PASS |
| flashdb-learned | 50 | 49.95 | 99.9% | 1,790 | 11,086 | 20 | 0 | yes | 1.00 | 128 MiB | 11,522,048 | — | 91,377,664 | 24,541,978 | PASS |
| sqlite | 50 | 50.00 | 100.0% | 5,960 | 20,772 | 20 | 0 | yes | 1.00 | 128 MiB | 42,434,560 | 42,434,560 | — | 11,800,152 | PASS |
| postgres | 50 | 50.00 | 100.0% | 2,574 | 16,609 | 20 | 0 | yes | 1.00 | 128 MiB | 80,973,824 | 81,682,432 | — | 21,255,871 | PASS |
| flashdb-declared | 100 | 99.95 | 100.0% | 1,540 | 13,225 | 20 | 0 | yes | 1.00 | 128 MiB | 12,345,344 | — | 91,086,848 | 25,403,943 | PASS |
| flashdb-learned | 100 | 99.95 | 100.0% | 3,261 | 13,097 | 20 | 0 | yes | 1.00 | 128 MiB | 11,436,032 | — | 91,205,632 | 25,403,943 | PASS |
| sqlite | 100 | 100.00 | 100.0% | 2,376 | 14,201 | 20 | 0 | yes | 1.00 | 128 MiB | 43,167,744 | 43,167,744 | — | 11,902,576 | PASS |
| postgres | 100 | 100.00 | 100.0% | 12,374 | 15,452 | 20 | 1 (lag_ceiling) | **no** | 1.00 | 128 MiB | 70,017,024 | 77,529,088 | — | 21,509,823 | PASS |
| flashdb-declared | 200 | 200.00 | 100.0% | 19,047 | 18,302 | 20 | 6 (lag_ceiling 4, lag_saturation 2) | **no** | 1.02 | 128 MiB | 16,097,280 | — | 91,299,840 | 27,224,067 | PASS |
| flashdb-learned | 200 | 200.00 | 100.0% | 7,623 | 17,962 | 20 | 0 | yes | 1.01 | 128 MiB | 15,826,944 | — | 91,279,360 | 27,224,067 | PASS |
| sqlite | 200 | 200.00 | 100.0% | 6,564 | 16,944 | 20 | 0 | yes | 1.00 | 128 MiB | 43,204,608 | 43,204,608 | — | 12,136,048 | PASS |
| postgres | 200 | 200.00 | 100.0% | 8,472 | 17,989 | 20 | 0 | yes | 1.06 | 128 MiB | 77,500,416 | 77,590,528 | — | 21,919,423 | PASS |
| flashdb-declared | 400 | 399.20 | 99.8% | 51,201 | 59,982 | 20 | 14 (lag_ceiling) | **no** | 1.36 | 128 MiB | 28,803,072 | — | 91,111,424 | 30,782,166 | PASS |
| flashdb-learned | 400 | 398.97 | 99.7% | 96,929 | 38,893 | 20 | 14 (lag_ceiling) | **no** | 1.37 | 128 MiB | 28,573,696 | — | 91,660,288 | 30,782,166 | PASS |
| sqlite | 400 | 400.00 | 100.0% | 15,849 | 9,688 | 20 | 1 (lag_ceiling) | **no** | 1.04 | 128 MiB | 43,794,432 | 43,798,528 | — | 12,598,968 | PASS |
| postgres | 400 | 399.23 | 99.8% | 78,681 | 157,039 | 21 | 21 (lag_ceiling) | **no** | 11.19 | 128 MiB | 80,388,096 | 80,388,096 | — | 22,730,431 | PASS |

Machine load (1-minute, 8 cores) at the rung boundaries: 2.80 → 2.19 (rung 50), 2.19 → 3.74 (rung
100), 3.74 → 4.08 (rung 200), 4.08 → 3.68 (rung 400).

What run A says, and does not: with the whole dataset resident inside the cap, every arm reaches the
offered rate until its own commit path breaks the 10 ms lag rule. sqlite sustains the highest offered
level of the four (200 rps, 0 rejections, then 1 rejection at 400); postgres sustains 200 (0) after
being rejected once at 100; flashdb-learned sustains 200 (0 rejections) and flashdb-declared is
rejected at 200 (6 windows). In this warm regime flashdb is **not** faster than sqlite; that is a
legitimate observation about a fully cached dataset and says nothing about the pre-registered regime.

**Equal-durability batch condition at the top rung — it does not hold there.** At 50-100 rps every arm
formed groups of 1.00 request, so each request was one durable commit everywhere. At 400 rps the mean
group size was **flashdb-declared 1.36, flashdb-learned 1.37, sqlite 1.04, postgres 11.19**: postgres
amortised one fsync over ~11 requests where the others committed one per request. The pre-registration
asks for equal durability "as closely as each product allows", with the difference disclosed; at 400
rps postgres's group-commit amortisation is a real advantage and it exists because postgres fell
behind (21 rejected windows, 21st window partial) and its pacer then found ready requests to batch.
It does not inflate any pass: postgres's achieved rate at 400 (399.23) was the *lowest* of the four
and it was rejected on every window.

## 3. Table B — the regime run (1x rung, flashdb at 12.546x of the cap)

Instance: 10,600,000 stock cells (2.65M skus x 4 loc) + 200,000 customers, seed 42, zipf theta 1.0,
one frozen trace of 1,000 requests at 50 rps offered (`859b99a1…`, the same sha256 for every arm), 20
windows, B=64, engine cap 128 MiB, construction cap 512 MiB.

| arm | offered rps | achieved rps | lag p99 us | service p99 us | windows | rejected (reason) | sustained | mean group | serving peak B | arm-scope peak B | seed peak B | data B (x cap) | oracle |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 49.95 | 40,901 | 29,230 | 20 | 12 (lag_ceiling) | **no** | 1.01 | 134,217,728 | — | 536,870,912 | 1,683,903,919 (12.546x) | PASS |
| sqlite | 50 | 50.00 | 1,969,377 | 2,006,844 | 20 | 1 (lag_ceiling) | **no** | 1.11 | 134,217,728 | 134,217,728 | — | 546,123,232 (4.069x) | PASS |
| postgres | 50 | 50.00 | 41,827 | 27,705 | 20 | 3 (lag_ceiling) | **no** | 1.02 | 134,217,728 | 134,221,824 | — | 1,103,779,519 (8.224x) | PASS |

All three arms held 50 rps with **zero errors** and all three inventory oracles **passed** (§4). None
of them sustained the rung under the rule: flashdb on 12 of 20 windows, sqlite on 1, postgres on 3.

### 3.1 The contention confound in run B (stated, not smoothed)

The runner recorded the machine at every boundary: prepare start 2.98, prepare end 6.14, flashdb arm
start 6.14, flashdb arm end (after the read-back) 3.39, sqlite arm start 3.39 / end 3.81, postgres arm
start 3.81 / end 2.69 (1-minute load average, 8 cores; `runs/m1-10x/rung-50/loadavg.jsonl`). The
flashdb arm's 20 s measured phase fell inside the loaded stretch: the 1-minute reading was 6.14 at the
arm's start and the box was running other tenants' CPU-heavy processes throughout. The two SQL arms
were measured later, at 2.7-3.8.

That matters because the rule is a *scheduler-latency* rule: 12 of 20 windows breached a 10 ms
schedule-lag ceiling while the driver still achieved 49.95 rps with zero errors, and sqlite — an
embedded engine in a 128 MiB scope — shows a single window whose lag p99 reached **1.97 s**, which is
a host stall, not a commit path.

For comparison, the earlier run of the same rung (records at git `8fcf807`,
`.scratch/80-external-baselines/runs/m1-10x/rung-50/`; it is the run this report replaces for
provenance reasons, not for its numbers) measured flashdb-declared at 49.95 rps with **1** rejected
window (lag p99 12,470 us) and both baselines **clean** (sqlite lag p99 6,120 us, postgres 2,199 us,
0 rejected windows) — but it recorded no load average and `spec.json` carried no argv, which is
exactly the provenance defect this fix cycle closed. **The two runs disagree on the sustained outcome,
and neither is the whole story**: the earlier one cannot show its conditions, this one is contended.

**Therefore: T5's throughput reading reproduces in both runs (deficit 1.001); its sustained reading
does not, and this report does not claim it.** A quiet-box repeat of this rung is the next measurement
(§9, §7). Nothing in Table B is used to claim that flashdb sustains the regime.

## 4. The inventory oracle, per arm, on the measured run

| arm | run A (0.23x), per rung | run B (12.546x) |
|---|---|---|
| flashdb-declared | PASS, 12 checks (lag p99 re-read of every cell, customer and order number) | **PASS, 12 checks**: 10,600,000 cells read back (`stock_cell_present`), per-cell `on_hand == initial + sum(delta)` (10,600,000), `on_hand >= 0` (10,600,000), 200,000 customers, 694 order numbers with at most one movement each, movement count == 694 committed takes, 0 request errors |
| flashdb-learned | PASS, 12 checks | not run at 10x (same declared wiring as the arm above; the read-back is per instance) |
| sqlite | PASS, 12 checks | PASS, 12 checks (per-cell arithmetic over 10.6M cells, no duplicate order number, movement/order/backorder counts against the committed counters, `sum(delta) == -sum(qty)`, `sum(on_hand) == cells*initial - taken qty`) |
| postgres | PASS, 12 checks | PASS, 12 checks (same list, read from the server's own catalog) |

The read-back is the one acceptance criterion the earlier regime run missed, and it is now **closed**:
the DEFERRED gap is gone. Cost, measured: the probe file (10.8M probes) is written and replayed in
37 min 57 s of wall clock (12:49:34 → 13:27:31, from the arm boundaries in `loadavg.jsonl`). The
record's `oracle_s` reads `0.0` because the timer wrapped the *call* of a generator instead of its
consumption — a defect in the timing field, fixed in `_verify_flashdb` for later runs; the elapsed
time above is the honest figure and is derivable from that record.

The oracle fails loudly at the first violation (exit 2, the offending cell or order named). Both
oracles now have negative controls in pytest: `oracle_sql` against a corrupted `on_hand` and a
duplicate order number, and `oracle_flashdb` against a synthetic replayed iterable (a corrupted cell
count, a negative cell, an absent cell, a movement whose `order_no` does not match, a movement booked
against a cell that is never read back, an absent customer, a nonzero error counter and a take count
that does not match) — the flashdb oracle previously had no test at all.

## 5. What each arm's memory number is, and is not

The measured budget is the **storage engine's**, and the number reported as the serving peak is
measured **over the measured phase only** — the bulk load is not charged to it:

| arm | what the 128 MiB cap covers | engine memory control | serving peak measured how |
|---|---|---|---|
| flashdb-declared / -learned | `flashdb-load` only, in its own systemd scope; the harness, pacer and read-back are outside it | the cgroup cap | exact `memory.peak` of that per-invocation scope, read from inside it — the scope exists only for the measured invocation |
| sqlite | the whole arm process, because sqlite's engine is embedded in it | `PRAGMA cache_size = -8192` (8 MiB), recorded per run, plus the process cap | sampled `memory.current` of the arm's own scope at 20 Hz across the measured phase (395 samples in run B), so the bulk load that precedes it is excluded |
| postgres | the server process tree, in its own named scope; the client is outside it | the cgroup cap plus `shared_buffers=16MB`, `fsync=on`, `synchronous_commit=on`, echoed from the server's own `SHOW` | sampled `memory.current` of the *server* scope across the measured phase; the runner also records the scope's lifetime peak (which includes the bulk load) separately |

The disclosed limits on all three: this kernel ignores a write to `memory.peak` (the write is accepted
and has no effect — verified), so the peak could not be reset at the measured phase's start; flashdb's
number is exact because its scope is per-invocation, and the SQL arms' numbers are sampled maxima,
hence lower bounds. flashdb's number is the high-water mark of a cgroup's `memory.current`, which
includes page cache charged to that scope; instance pages already resident (charged to the
construction scope that wrote them) are not re-charged to it, so the value is also a lower bound on
the memory the serving phase actually touched — which is why it reads 11-29 MB for a 24-31 MB dataset
in run A and at the 128 MiB cap for the 1.68 GB dataset in run B. Neither is a lifetime maximum presented as serving pressure: the arm-scope
lifetime peaks (sqlite 134,217,728 B, postgres 134,221,824 B) and flashdb's construction peak
(536,870,912 B at the 512 MiB construction cap) are recorded in their own fields and labelled.

Durability, observed rather than asserted: sqlite's `journal_mode`/`synchronous` are read back with
`PRAGMA` after the bulk load and before the measured phase, and the run fails loudly if they are not
`wal`/`FULL` — run B records `{"journal_mode": "wal", "synchronous": 2, "synchronous_name": "FULL"}`
(the seeding path runs journal-less, so this is not cosmetic); postgres records the server's own
`SHOW` (`fsync=on`, `synchronous_commit=on`, `shared_buffers=16MB`, `server_version 18.6`); flashdb
runs `--group-requests 64`, one WAL barrier per group. At 50 rps every arm formed groups of 1.0-1.1
requests (run B), so the batch ceiling did not bind there; at 400 rps in run A it did for every arm
and most for postgres (§2).

One further difference between the arms, disclosed because it is real: the SQL pacers execute a batch
synchronously, while `flashdb-load` submits its barrier to a background worker and can overlap. That
is a harness difference, not an engine measurement, and it is *visible* in run A only where a batch
actually forms (postgres's mean 11.19 / max 34 requests per group at 400 rps, sqlite's max 10). It is
consistent with sqlite's 400 rps rejection but is not established as its cause: postgres was the arm
that batched ~11 per commit, and it was the slowest.

## 6. T5, T6, T7 — conditions, arithmetic, verdicts

### T5 — low-load non-inferiority

**Condition:** at the 1x rung, flashdb must be within **1.5x** of the slower of sqlite/postgres; **2x**
is the fail line; 10x is a design failure. Metric: `slower_baseline_rps / flashdb_rps`.

* **(a) Achieved throughput, pre-registered regime (run B, 12.546x):** flashdb-declared 49.95 rps;
  slower baseline sqlite 50.00 (postgres also 50.00) → deficit **1.001**, inside the 1.5x requirement.
  Identical in the earlier (unrecorded-load) run.
* **(b) Sustained rule, same run:** flashdb-declared was rejected on 12 of 20 windows, sqlite on 1,
  postgres on 3, so **no arm has a sustained level at this rung in this run**; the earlier run had
  both baselines sustained and flashdb rejected once. The comparison is therefore **not
  like-for-like**, and under the stricter reading (throughput from rejected windows is not counted)
  T5 is not established by either run.
* **Verdict: (a) PASS (1.001 within 1.5x), (b) NOT ROBUSTLY DETERMINED** — a quiet-box repeat decides
  it; this report does not claim T5 as a passed gate.
* Harness-check regime (run A, PROVISIONAL): slower baseline sqlite 50.00, flashdb-declared 49.95 →
  deficit **1.001**, and every arm sustained that rung cleanly, so the reading is like-for-like.

### T6 — high-load superiority

**Condition:** at the highest common offered level, flashdb must reach at least **3x** the best
baseline's **sustained** throughput under the same latency rule.

* **Pre-registered regime: UNVERIFIED.** T6 needs the ladder at 10x. A 10x ladder is 8 flashdb
  arm-runs, each ~7 min of seeding (416 s measured here) plus a read-back that took 38 min at 10.6M
  cells, plus the SQL arms — roughly 8 h of measured cost, beyond this work package's 4 h cap.
  Stating a T6 number from run A would state a warm-cache result as the external claim.
* **Harness-check regime (PROVISIONAL), the pre-registered comparison at the top rung:** the highest
  common offered level is 400 rps, and **no baseline sustained it** (sqlite 1 rejection, postgres 21),
  so the ratio has no denominator at that level. T6 at 400 rps is **not evaluable**, not "2.00x" and
  not a FAIL against a number taken from another rung.
* **Harness-check regime, sustained-vs-sustained reading of the same data** (this crosses offered
  levels and is labelled as its own reading): the declared arm's highest sustained level is 100 rps
  (achieved 99.95); the best baseline's is sqlite at 200 rps (achieved 200.00) → **0.50x**. The
  learned arm sustained 200 rps at 200.00 → **1.00x**. Against the pre-registered 3x both are **FAIL**,
  by 2x and by 3x respectively.
* **Verdict: FAIL at the harness-check regime on the sustained reading; NOT EVALUABLE at the
  pre-registered regime (no 10x ladder).** The earlier report's "1.999x / 2.00x" compared flashdb's
  achieved rate at a rung it did not sustain with sqlite's sustained rate from a different rung; that
  was an overstated multiple and is withdrawn.

### T7 — initial-policy quality

**Condition:** the declared arm alone must already pass T5.

* At the pre-registered regime: deficit 1.001, so **PASS on (a)** exactly as T5 does, and **NOT
  ROBUSTLY DETERMINED on (b)** for the same reason (declared arm unsustained at 12 of 20 windows).
* At the harness-check regime: deficit 1.001 with every arm sustained → PASS.
* Disclosure: `flashdb-learned` runs the same declared-default wiring in M1 (no learner controller is
  wired), so its T5 number and T7's declared number are the same measurement twice, not two
  independent arms.

## 7. What is unverified, and why

1. **T6 at the pre-registered regime.** No ladder at 10x; the measured cost (~8 h) does not fit the
   4 h wall-clock cap, and the reason is a measurement, not an estimate.
2. **The sustained-rule outcome at the regime rung.** The committed run B was measured at loadavg
   6.1-8.3 and no arm sustained 50 rps; the earlier run of the same seed and data (no load average
   recorded) had the baselines clean. A quiet-box repeat — same command, same seed — is the next
   measurement, and until it exists T5/T7 are reported as (a) PASS, (b) not robustly determined.
3. **The pre-registered 10-seed median/IQR aggregation.** Both runs are one seed, one rung, one repeat.
4. **Run A is a second measurement of the same traces**, not the first one repaired: its SQL windows
   were re-measured on 2026-10-17 at loadavg 2.2-4.1 (the first ladder ran at 1.5-2.5), so its numbers
   differ from the earlier report's in several cells. The traces are byte-identical (same sha256 per
   rung).
5. **The growth clause's effect on the previously committed SQL windows cannot be re-derived.** Those
   windows were emitted before the reducer applied the clause and carry no per-window late-request
   count, so they were re-measured rather than re-scored. In the re-measured windows the clause
   changed no SQL verdict: every SQL rejection is a `lag_ceiling` one, and the growth clause fired only
   on the flashdb driver's own windows (run A rung 200).
6. **The 12.546x footprint is re-derivable only by re-running the build** (416 s): the instance files
   are git-ignored. The run record now carries the total and the four largest files (10.84-10.96 MB
   tiles); the full tiling is not enumerated.
7. **The regime run exercised only the take branch** (all 694 order flows took stock; 0 backorders),
   so at 12.546x the backorder checks are vacuous. That branch is exercised end to end by the
   `runs/m1-build-else` run (200 requests, `on_hand=2`: 51 takes + 92 backorders, all three oracles
   PASS), which is a correctness run at 0.02x, not at the regime.
8. **The shared box.** Run A saw load 1.5-4.1 on 8 cores and run B 2.7-8.3; a quiet box would move the
   rejection counts, not the mechanism.

## 8. What the numbers support, honestly

The instrument works: one harness drives four arms over one frozen trace at matched durability, each
engine in its own 128 MiB scope, under one latency rule — now genuinely one rule, bound by a test to
`flashdb-load`'s own verdicts — and every inventory oracle passes everywhere it was run, including
flashdb's read-back over 10.6M cells at 12.546x, which the earlier run deferred.

At the pre-registered regime, all three arms held 50 rps with zero errors, and T5's throughput deficit
is 1.001 against a 1.5x requirement. But the run was contended: no arm sustained the rung, flashdb on
12 of 20 windows, sqlite on 1 (a 1.97 s host stall) and postgres on 3, while the earlier run of the
same seed and data had both baselines clean. So the honest reading is narrower than "T5 passes at
12.5x": **flashdb's throughput is not behind the baselines at this rung in either run, and whether it
holds the rung as cleanly as they do is undecided** until a quiet-box repeat exists. T7 inherits both
halves of that.

In the warm 0.23x instance the ordering reverses and flashdb is not the fastest arm above 100 rps:
sqlite sustains 200 cleanly and 400 with one rejection, flashdb-learned sustains 200, flashdb-declared
is rejected at 200. T6 there has no denominator at 400 rps at all, and its sustained-vs-sustained
reading is 0.50x (declared) and 1.00x (learned) against the 3x line — FAIL, and further from it than
the earlier report's withdrawn 2.0x implied.

T6 at the pre-registered regime — the actual external-superiority claim — remains unevaluated.
Nothing here supports a claim that flashdb beats postgres or sqlite at scale, a claim about the
learned arm (in M1 it is the declared wiring), or a claim about drift and reorg cost.

## 9. Reproduce

The two commands that produced the committed records, as `spec.json` records them (`runner.argv`;
the `--arms` value's shell-escaped commas are unescaped here for readability):

```bash
# Run B — the regime rung at 12.546x, through the locked runner, with the flashdb read-back
scripts/m1-baseline-run.sh --workdir .scratch/80-external-baselines/runs/m1-10x/rung-50 \
    --requests 1000 --rate 50 --customers 200000 --skus 2650000 --locs 4 \
    --seed-cap-mib 512 --arms flashdb-declared,sqlite,postgres --skip-build

# Run A — the ladder (harness check): 4 rungs, 4 arms, 20 s each, one locked run per rung
scripts/m1-ladder-run.sh --customers 30000 --skus 30000 --locs 4 --seconds 20 --skip-build
```

`--skip-build` because `target/release/{flashdb-load,flashdb-scale,flashdb-replay}` were current
(engine sources untouched since the M1 harness commit); without it the runner builds them from the
aliyun crates mirror first. Each run holds `flock /tmp/m1-measure.lock` for its whole length, records
its argv, caps and lock in `spec.json`, and writes `loadavg.jsonl` at every boundary. To settle the
sustained reading (§3.1, §7.2), repeat run B's command unchanged in a quiet window — the seed and the
trace are deterministic (sha256 `859b99a1…`), so the numbers are comparable.

```bash
# score the two runs and merge them into the machine-readable report
uv run python scripts/m1-ladder-score.py --root .scratch/80-external-baselines/runs/m1-measure
uv run python scripts/m1-ladder-score.py --root .scratch/80-external-baselines/runs/m1-10x
uv run python scripts/m1-ladder-score.py \
    --merge .scratch/80-external-baselines/runs/m1-measure/report.json \
            .scratch/80-external-baselines/runs/m1-10x/report.json \
    --out .scratch/80-external-baselines/report.json
```

`report.json` is the single machine-readable document: `runs[0]` is the ladder, `runs[1]` the regime
run, `verdicts[]` one row per threshold per run with its condition and arithmetic (T6 carries both its
readings), and `unverified[]` / `summary` are §7-§8 in the same form. Raw evidence committed beside
this report: `runs/<run>/rung-<rps>/{manifest,spec,loadavg,run-<arm>,windows-<arm>}.json(l)`;
`runs/.gitignore` keeps the regenerable state (trace, seed trace, engine instances, the sqlite file,
driver/probe logs) out of git.
