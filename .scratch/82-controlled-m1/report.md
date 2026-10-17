# 82 — the controlled M1 verdict, re-measured after the two fairness fixes: T5/T6/T7 at 12.55x

Status 2026-10-18, `wave/m1-controlled`. Authority: the pre-registered contract
(`.scratch/15-objective-v0/pre-registration.md` §"External baselines and thresholds") and ticket 82's
protocol (`.scratch/82-controlled-m1/issues.md`). **No pre-registered threshold was changed and no
engine code was touched.** The machine-readable document beside this file is `report.json`, generated
from the committed per-run records by `scripts/m1-controlled-score.py`.

**What changed since f70d91c.** Two fairness defects that the previous report named as *inferences*
were fixed and re-measured, with no other change to the protocol: the sqlite arm now states its WAL
auto-checkpoint policy per run instead of inheriting sqlite's compiled default, and both SQL pacers
are now **open loop**, like `flashdb-load` — the durable commit no longer holds the next dispatch back
(§1.1, §1.2, and the probe in §9.2). The previous run's records are superseded; they remain in git at
`f70d91c`.

**The one-line answer.** On an AC-powered, EPP-`performance`, 3.6 GHz box, after the fixes **all three
arms hold 50 rps in 5 of 5 paired blocks and 100 rps in 5 of 5**; at 200 rps **sqlite and postgres hold
it 5 of 5 and `flashdb-declared` holds it 3 of 5**, on two single-window schedule-lag breaches behind
one ~575 ms engine stall (§9.5). **T5 PASS (deficit 1.001 against the 1.5x requirement), T7 PASS, T6
FAIL** — flashdb's sustained throughput at the highest common offered level (200 rps) is **1.00x** the
best baseline's, against the pre-registered 3x; and the pre-registered ratio is **bounded at 1.0526 for
any pair of arms at one offered level** (§5, T6), so that gate cannot be passed by measurement for any
engine. **Neither baseline is any longer rejected for lag**: sqlite's 254 ms checkpoint stall is gone
once its policy is stated, and the SQL arms' lag p99 is now 0.03-2.5 ms — *lower than flashdb's at every
rung*, which reverses the f70d91c picture. flashdb's own remaining tail is its per-request service p99
(110-403 ms, the worst of the three) and the recurring ~0.1-0.6 s group (§9.5). **After the fairness
fixes, M1 shows flashdb non-inferior at the 1x rung and does not show it superior at 3x anywhere.**

## 1. The protocol as executed

| element | what ran |
|---|---|
| dataset | the frozen 10x instance: 200,000 customers, 2.65M skus x 4 locs = 10,600,000 stock cells, seed 42, theta 1.0, `on_hand` 1,000,000, from the frozen seed trace `runs/m1-10x/rung-50/seed.jsonl` (hard-linked into every work directory; digest recorded in each `spec.json`) |
| pre-load instance | **not rebuilt** — the f70d91c snapshots were reused, as the fix list required: flashdb 1,683,012,213 B / 158 files / `67b3d10c…`; sqlite 541,876,224 B / 1 file / `46c6f1e1…`; postgres data dir 2,201,362,806 B / 1,361 files / `7f9a4ea8…` |
| restore before every run | `restore` copies the snapshot back and **re-hashes it**; a digest mismatch stops the run. **45 of 45 arm-runs restored the same digest as their arm's snapshot** (restore cost per arm-run: flashdb 4.8 s, sqlite 1.6 s, postgres 7.0-7.5 s) |
| blocks | **N = 5**, rungs 50/100/200, arms `flashdb-declared`, `sqlite`, `postgres`, **20 s of offered load** per arm-run (1,000/2,000/4,000 requests), one process at a time; 45 arm-runs, 00:02:21 → 00:24:54 |
| order inside a block | rung-major, and the arm order **rotated per (block, rung)** so no arm always ran first or last in a window (rotation in `orchestrator.log`) |
| validity rule | one rule, `cold_0b.summarize_curve`: a window is rejected when its per-request schedule-lag p99 > 10,000 us (`lag_ceiling`), when lag grew for 3 consecutive windows (`lag_saturation`), or on any request error (`errors`); a rung is **sustained** iff the achieved rate is within 5% of the offered rate and zero windows were rejected |
| pacing | **open loop on all three arms**: the pacer dispatches at the due instant and a worker executes and commits the group, acking on completion (§1.1). flashdb already had this shape in `flashdb-load` |
| memory boundary | the **engine only** in a 128 MiB `MemoryMax` scope: `flashdb-load` in its own scope; sqlite's whole process (its engine is embedded, `PRAGMA cache_size = -8192`); the postgres server in its own named scope, client outside. The harness, the restore copies and the read-back are outside every scope |
| lock | `/tmp/m1-measure.lock` held by the orchestrator for the **whole** measured phase and inherited by every arm step (`M1_LOCK_HELD=1`), so nothing could interleave between the arms of a block |
| durability, read back from each engine | flashdb `--group-requests 64` (one WAL barrier per group); sqlite `journal_mode=wal`, `synchronous=2 (FULL)`, **`wal_autocheckpoint=0` and `journal_size_limit=-1` read back per run** (§1.2); postgres `fsync=on`, `synchronous_commit=on` echoed from the server's own `SHOW` |
| group commit | **mean group size 1.000-1.017 and max 1-9 on all three arms at all 15 crossings**, one durable commit per group (recorded per run and per window); the batch cap of 64 bound nowhere, so every arm paid one durable commit per request |
| verification | the inventory oracles ran in **block 1** and PASSed for both SQL arms (12 checks each over 10.6M cells). Blocks 2-5 record `DEFERRED` — the restored bytes are identical in every block. **flashdb's read-back is DEFERRED in all blocks** (§9.1) |
| device floor | **reused from f70d91c, not re-taken** (the two fixes change the arms, not the device): an idle pacer and an fsync-per-request pacer at 200 rps, no engine, at the same machine state, before and after that measured phase (§3) |
| concurrent work | `machine-state` refuses to continue when a build (`cargo`/`rustc`), a seed job (`flashdb-scale`/`seed-inventory`) or a second harness is running: **0 such records in 90 boundaries**. That detector cannot see other processes, and this measured phase ran while other work streams were active on the box — disclosed in full in §2, because the load average is not the same state as f70d91c's |

### 1.1 The asynchronous SQL pacer (fix 1 of 2)

The f70d91c report's §9.3 named this as a real methodological difference and did not fix it: flashdb's
driver submits its durable barrier to a background worker and keeps dispatching (open loop), while both
SQL pacers executed a batch synchronously and committed before the next dispatch (closed loop). The
schedule-lag rule therefore charged a synchronously-committing arm for its own commit time and never
charged flashdb for its barrier.

`baseline_0b.run_paced_loop(open_loop=True)` is now the SQL arms' shape: the pacer thread waits for
each request's due instant, stamps it and appends it to a ready queue; one worker thread owns the
connection, takes the requests already ready (at most `batch`, never holding a group open to fill),
executes and commits them as **one durable group**, and stamps completion. The lock covers
bookkeeping only, never `execute_batch`, so a running commit cannot delay dispatch. The device floor
keeps `open_loop=False`: it measures the device's own commit cost under a client that waits.

Nothing else about durability changed: one durable commit per group, the same `batch = 64`, the same
`PRAGMA synchronous=FULL` / `synchronous_commit=on` read-back, and the **mean 1.000-1.017 / max 1-9
group sizes** prove the group cap behaved identically on every arm. The run record states which shape
ran (`pacing`), and the pacer's two shapes are bound by
`test_open_loop_pacer_does_not_charge_dispatch_for_the_commit` and
`test_open_loop_pacer_commits_one_group_of_the_requests_already_ready`.

### 1.2 sqlite's WAL checkpoint policy (fix 2 of 2)

The f70d91c report's §9.2 named the 254 ms stall as a **checkpoint inference, not a probe**, and the
`wal_autocheckpoint` default was an unstated engine parameter. Both are now closed:

* **The probe (§9.2) confirms the hypothesis**: at the same rung, same restored snapshot, same pacer,
  `wal_autocheckpoint=1000` (sqlite's compiled default) gives a **272 ms service p99**, and
  `wal_autocheckpoint=0` gives **20.5 ms** at the same machine state.
* **The measured path states the policy**: `PRAGMA wal_autocheckpoint=0` and `journal_size_limit=-1`,
  read BACK from sqlite and recorded per run in `durability_observed` and `sqlite_wal` (the read-back
  failure is a hard stop: `require_measured_durability`).
* **This is the pre-registration's own requirement applied to the right parameter.** B1 already
  demands `sqlite` with "WAL enabled, `synchronous` setting stated explicitly"; the run states
  `synchronous` and `journal_mode` by read-back, and the parameter that turned out to decide this
  arm's latency was the auto-checkpoint, so it is stated the same way.
* **The deferred work is done and recorded, not dropped**: after the measured phase and before the
  oracle the arm runs `PRAGMA wal_checkpoint(TRUNCATE)`, and `sqlite_wal` records the WAL bytes it
  deferred (17.7 MB at 50 rps, 72.5 MB at 200 rps), the checkpoint's own result, and the footprint
  after it. What moved is *when* the write-back happens (off the durable-commit path), not whether it
  happens; a SQL arm's durability is unchanged, and the cost of the choice is the WAL and the memory it
  retains inside the 128 MiB cap (sqlite's serving peak rises to 98.7 MiB at 200 rps, §6).

## 2. Machine state, recorded at every arm boundary (90 boundaries)

| field | value over the measured phase (00:02:21 → 00:24:54) |
|---|---|
| AC online | `1` at every boundary |
| power profile (`powerprofilesctl get`) | `performance` at every boundary |
| EPP (`energy_performance_preference`, all 8 CPUs) | `performance` at every boundary |
| `scaling_cur_freq` | per-record median of the 8 cores **2,899,986-3,601,312 kHz** (median 3,600,010 kHz); per-core readings 0.4-3.6 GHz (an idle core drops even under EPP, which is why the record keeps all eight) |
| `scaling_max_freq` | 3,600,000 kHz |
| **governor name** | **`powersave`** — recorded and *not* trusted: on this intel_pstate platform the frequency is driven by EPP, so the governor's name contradicts the 3.6 GHz the cores actually ran at |
| load average (1 min, 8 cores) | **min 0.68, median 1.95, max 4.10** over 90 boundaries (ticket-80 run B's measured phase sat at 2.7-6.1; **f70d91c's measured phase was 0.32 / 0.80 / 2.02**) |
| busiest processes seen | `python` (the arm), `MainThread`/`pi` (the agent runtime), `gnome-shell`, `ptyxis`, and **foreign work**: `35-vs2-bin`, `chrome-headless` (×2), `fwupdmgr`, `localsearch-ext`, `ps` |
| `concurrent_work` records | **0** — but the detector's patterns (`cargo`, `rustc`, `flashdb-scale`, `seed-inventory`, `baseline_0b`) cannot see the foreign processes above, so this is *not* a claim that the box was idle (§9.6) |
| `nproc` | 8 |

**Read this beside the results.** This measured phase ran at **2.4x f70d91c's median load** (1.95 vs
0.80) because other work streams were active on the same box: `35-vs2-bin` at 64-73% CPU and two
`chrome-headless` processes at 26-55% appear in the boundaries around 00:10:01-00:12:36 (block 2's
rung-50 and rung-200 crossings). The cores still ran at 3.6 GHz, the pairing is within-window, and no
arm produced a request error — but the two `lag_ceiling` rejections flashdb took at 200 rps are one
~575 ms engine stall each (§9.5), and the same stall appears in the *quieter* f70d91c run, so the load
is not what causes them.

## 3. The device floor (no engine in the path) — reused, not re-taken

The floor belongs to the machine, and the two fixes change the arms; re-taking it would have made the
re-measured arms incomparable with the reused snapshots' own reading for no gain. It is therefore the
f70d91c measurement, unchanged:

| floor | rate | idle schedule-lag p50 / p99 | fsync service p50 / p99 (one durable commit per request) | fsync lag p99 | rejected windows |
|---|---|---|---|---|---|
| `before` (20:56:52, loadavg 2.76) | 200 rps | 10 / **197 us** | 3,105.5 / **9,821 us** | 162,595 us | idle 0/20, fsync **3/20** (`lag_ceiling`) |
| `after` (23:29:33, loadavg 1.22) | 200 rps | 37 / **207 us** | 3,566 / **9,905 us** | 4,096 us | idle 0/20, fsync **0/20** |

* **The machine's own scheduler floor is ~0.2 ms** (lag p99 197-207 us): 2% of the pre-registered
  10 ms ceiling.
* **The device's own durable-commit floor is ~9.8 ms p99** for a one-commit-per-request pacer with no
  engine, p50 3.1 ms. At 200 offered requests per second with one fsync each, the *device* is already
  at 98% of the 10 ms budget.
* The floor benches ran with the **closed-loop** pacer (they measure the device's own commit cost), and
  both are reported; the floor-adjusted reading uses the **element-wise minimum** of the two, so no arm
  is flattered by the calmer floor.
* The floor's `before` phase was taken while the instance snapshots were still being written back (it
  was rejected on 3 of its own 20 windows); the `after` phase, at the old measured phase's load, was
  rejected on none. §9.4 keeps that disclosure.

**The floor-adjusted reading** (the arm's median lag p99 as a multiple of the 197 us idle floor, and its
service p99 as a multiple of the 9,821 us fsync floor):

| arm | rung | lag p99 (median) | x idle floor | service p99 (median) | x fsync floor | blocks clean at `10 ms + floor` |
|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 1,038 us | 5.27x | 109,766 us | 11.2x | 5/5 |
| sqlite | 50 | 577 us | 2.93x | 21,768 us | 2.2x | 5/5 |
| postgres | 50 | 385 us | 1.95x | 14,034 us | 1.4x | 5/5 |
| flashdb-declared | 100 | 857 us | 4.35x | 106,414 us | 10.8x | 5/5 |
| sqlite | 100 | 251 us | 1.27x | 65,930 us | 6.7x | 5/5 |
| postgres | 100 | 264 us | 1.34x | 25,680 us | 2.6x | 5/5 |
| flashdb-declared | 200 | 8,389 us | 42.6x | 403,213 us | 41.1x | 3/5 |
| sqlite | 200 | 151 us | 0.77x | 59,013 us | 6.0x | 5/5 |
| postgres | 200 | 906 us | 4.60x | 40,135 us | 4.1x | 5/5 |

The absolute 10 ms ceiling stays the primary rule (§4-§6). The floor-adjusted column is what keeps a
reading honest about *whose* latency it is: after the fixes, **no baseline's lag is anywhere near the
device's own floor multiple** — sqlite sits at 0.8-2.9x the machine's scheduling floor and postgres at
1.3-4.6x, while flashdb sits at 4.4-42.6x.

## 4. The decisive table — median and IQR across the 5 paired blocks

Per (arm, rung): achieved rps, schedule-lag p99, service p99, rejected windows, errors, engine peak.
Median (IQR) over blocks; the per-block values are in `report.json` under `aggregate[].per_block` and
the raw windows in `runs/b<block>/rung-<rung>/windows-<arm>.jsonl`.

| arm | rung | achieved rps | sustained blocks | lag p99 us | service p99 us | rejected windows (total) | errors | mean / max group | engine peak MiB of 128 | blocks at `10 ms + floor` |
|---|---|---|---|---|---|---|---|---|---|---|
| **flashdb-declared** | **50** | 49.95 (0.0) | **5/5** | 1,038 (172) | 109,766 (10,710) | 0 — | 0 | 1.000 / 1 | 61.4 | 5/5 |
| sqlite | 50 | 50.00 (0.0) | **5/5** | 577 (347) | 21,768 (24,307) | 0 — | 0 | 1.001 / 2 | 43.9 | 5/5 |
| postgres | 50 | 50.00 (0.0) | **5/5** | 385 (876) | 14,034 (17,873) | 0 — | 0 | 1.000 / 2 | 33.9 | 5/5 |
| **flashdb-declared** | **100** | 99.95 (0.0) | **5/5** | 857 (950) | 106,414 (108,771) | 0 — | 0 | 1.000 / 1 | 61.6 | 5/5 |
| sqlite | 100 | 100.00 (0.0) | **5/5** | 251 (77) | 65,930 (20,692) | 0 — | 0 | 1.008 / 4 | 62.4 | 5/5 |
| postgres | 100 | 100.00 (0.0) | **5/5** | 264 (33) | 25,680 (10,649) | 0 — | 0 | 1.001 / 3 | 34.2 | 5/5 |
| **flashdb-declared** | **200** | 200.00 (0.0) | **3/5** | 8,389 (23,158) | 403,213 (142,583) | 2 (`lag_ceiling` 2) | 0 | 1.007 / 8 | 62.9 | 3/5 |
| sqlite | 200 | 200.00 (0.0) | **5/5** | 151 (85) | 59,013 (10,995) | 0 — | 0 | 1.008 / 9 | 98.7 | 5/5 |
| postgres | 200 | 200.00 (0.0) | **5/5** | 906 (694) | 40,135 (24,248) | 0 — | 0 | 1.006 / 6 | 43.9 | 5/5 |

Every arm achieved the offered rate to within 0.1% in every block. **No arm produced a request error,
and the SQL oracles passed on all 12 checks in block 1.** Every rejection in this run is a single-window
`lag_ceiling` breach on flashdb at 200 rps; the `lag_saturation` clause fired nowhere. sqlite and
postgres were rejected in **0 of 15 crossings each** (f70d91c: 28 and 10).

Per-block lag p99 (us), the raw spread the medians compress:

| arm | rung | block 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 1,038 | 3,894 | 1,055 | 846 | 883 |
| flashdb-declared | 100 | 857 | 371 | 2,288 | 1,401 | 451 |
| flashdb-declared | 200 | 7,272 | **30,430** | **33,747** | 3,088 | 8,389 |
| sqlite | 50 | 375 | 577 | 2,544 | 862 | 515 |
| sqlite | 100 | 251 | 308 | 722 | 231 | 192 |
| sqlite | 200 | 79 | 33 | 170 | 164 | 151 |
| postgres | 50 | 1,289 | 1,142 | 385 | 205 | 266 |
| postgres | 100 | 264 | 290 | 775 | 171 | 257 |
| postgres | 200 | 968 | 274 | 162 | 906 | 1,731 |

The two bold cells are flashdb's only rejections, and they share one mechanism with f70d91c's single
rejection: the window that carries a ~575 ms group (§9.5). Everything else sits at 30 us-8 ms.

## 5. T5, T6, T7 — conditions, arithmetic, verdicts

### T5 — low-load non-inferiority at the 1x rung (50 rps): **PASS**

* **Condition (pre-registered verbatim):** "At the 1x rung flashdb must be within **1.5x** of the
  slower of sqlite/postgres; **2x** is the fail line; a **10x** gap is a design failure."
* **Prong A (the pre-registered metric, achieved throughput at 50 rps):** per block,
  `slower_baseline_achieved / flashdb_achieved` = **1.001** in all 5 blocks (50.00 / 49.95); the median
  is **1.001**, against a requirement of 1.5. **PASS.**
* **Prong B (the ticket-80 cross-check):** did flashdb itself sustain the rung? **5 of 5 blocks**, zero
  rejected windows, achieved 49.95 of 50 (99.9%). **PASS.** All three arms sustained it 5/5.
* **Reading limit (stated, not hidden):** both arms are paced at ONE offered rate, so a declared arm
  that holds the rung can only score a deficit up to `1/SUSTAINED_TOLERANCE` = **1.0526**. Prong A is
  therefore a non-inferiority statement at a paced rung — it cannot show a throughput advantage — and
  **prong B carries the substance of this PASS**: flashdb held the 1x rung under the unified rule in
  every block, which is exactly what f70d91c's and ticket 80's runs failed to show.
* **Not a latency claim.** At 50 rps flashdb's schedule lag p99 (1,038 us) and service p99 (109.8 ms)
  are the **worst of the three arms** (sqlite 577 us / 21.8 ms; postgres 385 us / 14.0 ms). T5's
  pre-registered metric is achieved throughput, so this is not a T5 failure — but no reading of this
  run supports calling flashdb faster than either baseline at 50 rps.

### T6 — high-load superiority: **FAIL**

* **Condition (pre-registered verbatim):** "At the highest common offered level flashdb must reach at
  least **3x** the best baseline's **sustained** throughput under the same latency rule."
* **The highest common offered level is 200 rps** — a level is *common* when every arm was **offered**
  it, which is the pre-registration's wording. (f70d91c's scorer instead required every arm to *sustain*
  it in a majority of blocks; that is a rule the pre-registration does not state, and it is what
  produced the previous report's empty denominator. It is corrected here.)
* **Sustained blocks at 200 rps:** flashdb **3 of 5**, sqlite **5 of 5**, postgres **5 of 5**.
* **Arithmetic:** flashdb's sustained throughput at 200 rps = **200.00 rps** (median over the 3 blocks
  it sustained); the best baseline's = **200.00 rps** (sqlite, 5/5). Ratio **1.00x** against the
  required 3x. **FAIL.**
* **The gate cannot be passed by measurement, for any engine.** `summarize_curve` bounds every arm's
  achieved rate by the offered rate, and an arm that sustains a level is within 5% of it, so two arms
  that both sustain a common offered level differ by at most 5% and the ratio is confined to
  **[0.95, 1.0526]**. The pre-registered 3x is unreachable at a common offered level by construction.
  This FAIL therefore means **"the gate is not met / not demonstrated"**, not "flashdb was measured to
  be slow" — and it is equally not evidence *for* flashdb.
* **The secondary level reading now favours the baselines.** flashdb's highest sustained level (in ≥4
  of 5 blocks) is **100 rps**; sqlite's and postgres's are **200 rps** — a level ratio of **0.5x**. The
  f70d91c-era reading "flashdb 200 vs no baseline level, i.e. ≥4x in flashdb's favour" was an artifact
  of the closed-loop SQL pacer and sqlite's unstated checkpoint; with a fair client shape it inverts.
  (This also corrects the review's P1-2(b): the ≥4x level reading does not survive the fix.)
* **Smallest fix (named, not applied — it changes the pre-registration):** add rungs **below** the
  50 rps floor (10 and 25 rps) so each baseline's sustained level exists, then read the ratio of the
  highest sustained *levels* rather than of achieved throughput at a common offered level. Cost: 2
  rungs x 3 arms x ~40 s. The other named limit is the footprint regime (§7): only flashdb ran inside
  the pre-registered ≥10x floor.

### T7 — initial-policy quality: **PASS**

* **Condition:** "The declared arm alone must already pass T5."
* The declared arm is the arm measured above: deficit **1.001** (requirement 1.5) with the rung
  sustained **5/5**. **PASS**, inherited from T5 with no separate reading needed, and carrying T5's
  reading limit (non-inferiority at a paced rung).
* **Disclosure:** `flashdb-learned` was not run. M1 wires no learner controller, so a second flashdb
  arm would measure the same declared wiring twice; T7 exists to keep the *declared* arm from being
  absurd, and the declared arm is the one measured here.

## 6. Per-metric verdicts, with the defect and the smallest fix

Rules: achieved rate within 5% of offered; schedule-lag p99 <= 10,000 us (pre-registered); zero
rejected windows; zero errors; engine peak <= 128 MiB. The service p99 carries **no pre-registered
ceiling** — the pre-registration's bound is on schedule lag — so its secondary rule is the same 10 ms
bound adjusted by the device's own fsync p99 (`service p99 - 9,821 us <= 10,000 us`).

| metric | arm | rung | median (IQR) | rule | verdict | defect → smallest fix |
|---|---|---|---|---|---|---|
| achieved rps | all three | 50/100/200 | 49.95 / 99.95 / 200.00 | within 5% of offered | **PASS** (9/9 cells) | — |
| lag p99 | sqlite / postgres | 50 / 100 / 200 | 577 / 251 / 151 and 385 / 264 / 906 us | <= 10,000 us | **PASS** | — (after stating `wal_autocheckpoint` and opening the pacer, both baselines are at 0.8-4.6x the machine's own 197 us floor) |
| lag p99 | flashdb-declared | 50 / 100 / 200 | 1,038 / 857 / 8,389 us | <= 10,000 us | **PASS** (medians), 2 rejected windows at 200 | its dispatch is late only behind the ~575 ms group of §9.5; the 200 rps median carries that stall in 2 of 5 blocks. Smallest fix: diagnose the group (a per-window `group_elapsed_us` histogram exists; the engine's flush/compaction path is the named candidate) and re-run with more blocks — a 2-in-5 event needs N >= 8 to place its own rate |
| service p99 | flashdb-declared | 50 / 100 / 200 | 109,766 / 106,414 / 403,213 us | 10 ms above the 9,821 us device fsync floor | **FAIL** | 5.5-20x above the rule and the worst of the three arms: the per-request completion tail of its barrier/flush path. Smallest fix: attribute the > 100 ms completions to the group that caused them (the run record's `group_elapsed_us` already localises them to one window) before tuning anything |
| service p99 | sqlite / postgres | 50 / 100 / 200 | 21,768 / 65,930 / 59,013 and 14,034 / 25,680 / 40,135 us | as above | **FAIL** except postgres@50 (14,034 us) | 1.1-6.7x the device fsync floor: queueing behind their own commit when the batch does not form, not a stall. Disclosed, not used by any verdict |
| rejected windows | flashdb-declared | 50 / 100 / 200 | 0 / 0 / 2 | zero | **PASS** at 50 and 100, **FAIL** at 200 | two single-window `lag_ceiling` breaches, both behind a ~575 ms group (§9.5). Smallest fix: more blocks (above) — and the stall's own diagnosis |
| rejected windows | sqlite / postgres | 50 / 100 / 200 | 0 / 0 / 0 | zero | **PASS** | — |
| errors | all three | all rungs | **0 in all 45 arm-runs** | zero | **PASS** | — |
| engine peak | flashdb-declared | 50 / 100 / 200 | 61.4 / 61.6 / 62.9 MiB of 128 | <= 128 MiB | **PASS** | — (it serves a 1.57 GiB instance from *half* its budget: the cap did not bind) |
| engine peak | sqlite | 50 / 100 / 200 | 43.9 / 62.4 / **98.7** MiB of 128 | <= 128 MiB | **PASS** | its serving peak rises with the deferred checkpoint (the WAL grew to 72.5 MB at 200 rps): 77% of the cap. It is inside the cap, and it is the disclosed price of taking the checkpoint off the commit path |
| engine peak | postgres | 50 / 100 / 200 | 33.9 / 34.2 / 43.9 MiB of 128 | <= 128 MiB | **PASS** | — (sampled `memory.current` maxima, hence lower bounds) |
| inventory oracle | sqlite / postgres | block 1 | PASS, 12 checks each | PASS | **PASS** | blocks 2-5 DEFERRED (identical restored bytes); see §9.1 for flashdb |
| inventory oracle | flashdb-declared | all blocks | DEFERRED | PASS | **FAIL (uncompleted criterion)** | the read-back was not run in this phase. Smallest fix: `baseline_0b verify` on the live instance (10.7M probes, 16-100 min by ticket 80's measurement) |

## 7. Physical footprint — reported as measured, each numerator with its instrument

| arm | numerator (instrument) | bytes | x 128 MiB cap | meets the pre-registered `>= 10x` floor |
|---|---|---|---|---|
| flashdb-declared | the instance directory the arm served, live WAL included | 1,683,903,919 | **12.546x** | **yes** |
| sqlite | database file + WAL + shm sidecars, read after the post-run checkpoint | 542,052,352 | 4.039x | no |
| postgres | `pg_database_size()` — **logical** size, the WAL excluded | 1,103,713,983 | 8.223x | no |
| postgres | the cluster data directory the restore verified, **WAL included** (`restore.bytes`) | 2,201,362,806 | **16.401x** | **yes** |

**The three numerators are three different instruments**, so the bytes are comparable as regime checks
and not as a like-for-like size comparison: flashdb's is a directory walk, sqlite's is the database plus
its sidecars, and postgres's is a catalog's logical size that the WAL is outside of. That is why
postgres appears twice: by its logical size it is at 8.22x (below the floor), and by the data directory
it actually occupies it is at 16.40x (above it). f70d91c printed only the logical figure and concluded
that "only flashdb is actually inside the pre-registered regime — the baselines sit below it"; **that
sentence was wrong for postgres, and it is withdrawn here.**

What stands: **sqlite ran at 4.04x**, below the pre-registered ≥10x floor, under either numerator — the
same 10.6M logical rows in a smaller encoding. So the one methodological limit that survives every fix
is that the three arms serve one logical dataset at three physical sizes, and only flashdb and
postgres's data directory clear the floor. That makes sqlite's sustainment of every rung a reading at a
**less demanding** regime than the pre-registration demands, and it is the honest reason a "sqlite
sustains 200 rps too" statement does not by itself settle the regime question. **Smallest fix:** size
each baseline's instance to ≥10x of its own 128 MiB cap (sqlite needs ~2.5x the rows; postgres's data
directory already clears it) and re-run the three arms — a change of dataset size, not of threshold.

## 8. What the numbers support, and what they do not

**Supported.**

1. **The instrument and the protocol work.** 45 arm-runs, 45 verified restores of the same seeded
   bytes, 90 machine-state boundaries, one lock held throughout, one validity rule for every arm, one
   durable commit per request on every arm (mean group 1.000-1.017), zero request errors and all SQL
   oracle checks passing. Both fairness defects f70d91c diagnosed are fixed and their effect is
   measured, not argued.
2. **flashdb holds the 1x rung at 12.546x**: 49.95 of 50 rps in **5 of 5 paired blocks**, lag p99
   0.85-3.9 ms, zero rejected windows, engine peak 61.4 MiB of its 128 MiB budget. T5 and T7 PASS on
   their pre-registered arithmetic (deficit 1.001).
3. **flashdb holds 100 rps in 5/5 blocks as well** (lag p99 0.37-2.3 ms).
4. **With a fair client shape and a stated sqlite checkpoint policy, both baselines sustain every rung
   of the ladder** (sqlite and postgres: 5/5 at 50, 100 and 200 rps, zero rejected windows in 15
   crossings each), at a footprint below flashdb's and, for sqlite, below the pre-registered regime.
5. **The machine's own floor is 0.2 ms of schedule lag** and the device's own one-fsync-per-request
   floor is 9.8 ms p99: at 200 rps the 10 ms ceiling is at the device's edge.

**Not supported.**

1. **Any claim of 3x superiority over a baseline.** T6 FAIL: at the highest common offered level
   flashdb's sustained throughput is 1.00x the best baseline's, and the pre-registered ratio cannot
   exceed 1.0526 for any pair of arms at one scored level.
2. **Any claim that flashdb is faster than the baselines at any rung of this ladder.** After the fixes,
   its schedule-lag p99 is the **worst** of the three at every rung, its service p99 is the worst at
   every rung, and it is the only arm that fails to sustain a rung. What the run supports is
   non-inferiority at the declared 1x rung in achieved throughput (T5), and that flashdb serves a
   12.5x instance from half of its memory budget.
3. **Any claim that flashdb meets a per-request latency SLO.** Its schedule lag is inside the
   pre-registered bound at 50 and 100 rps, but its *service* p99 is 110-403 ms — 11-41x the device's
   fsync tail — because its barrier lands asynchronously. That tail is not covered by the
   pre-registered rule; it is reported.
4. **Any claim about the learned arm**, drift, reorg or compaction cost: they were not run.
5. **Any claim that generalises beyond this box and state.** One seed, one dataset, N=5 blocks, one
   machine (i5-8350U, 4C/8T, NVMe), other work streams resident (§2, §9.6). The IQRs and the per-block
   table are the bound this run supports.
6. **Any claim from the aborted first attempt, or from the superseded f70d91c measurement.** The
   former's records are excluded by timestamp (§9.7); the latter is superseded but kept in git.

## 9. Disclosures — the limits, each with its smallest fix

1. **flashdb's inventory read-back did not run in the measured phase.** Every flashdb block records
   `oracle: DEFERRED` (`--skip-verify`): the read-back is verification, it costs 16-100 min for 10.7M
   probes, and running it 15 times would have bought nothing (the restored bytes are identical in every
   block). What the records do carry is `errors = 0` in all 15 flashdb arm-runs. **Smallest fix:** `uv
   run python -m flashdb_harness.baseline_0b verify --workdir runs/b5/rung-200 --arm flashdb-declared`
   on the live instance left by the last run, merged into that run record.
2. **The sqlite stall was a checkpoint, and that is now a probe, not an inference.**
   `scripts/m1-checkpoint-probe.sh` ran the same restored snapshot at 50 rps for 20 s twice, changing
   only `PRAGMA wal_autocheckpoint`: **1000 pages → service p99 272,442 us** (and a 13-request group
   forming behind it); **0 pages → service p99 20,503 us**, lag p99 1.2 vs 2.4 ms, no rejected windows
   either way, WAL 4.13 MB vs 17.74 MB. The stall follows the setting, not the rung, and its magnitude
   matches the 254 ms that f70d91c saw at every crossing. Records:
   `runs/probe-checkpoint/{autocheckpoint-1000,autocheckpoint-0}/` + `comparison.json`. The probe ran at
   loadavg 3.1-3.5 with a foreign `world.test` at ~100-127% CPU, at 2.7-3.1 GHz — the same state for
   both variants, which is what the A/B needs; its absolute numbers are therefore a *heavier* state than
   the measured phase's.
3. **The pacer asymmetry is fixed** (§1.1), not just disclosed: the SQL arms now dispatch on the
   schedule and let a worker commit, exactly as `flashdb-load` does, with the same one-commit-per-group
   durability and the same batch cap. Its effect is the whole of this report's difference from f70d91c:
   the SQL arms' lag p99 fell from ~254 ms / 13-22 ms to 0.03-2.5 ms, and the baselines now sustain
   every rung.
4. **The floors differ by load state** (loadavg 2.76 before, 1.22 after) and the `before` floor's fsync
   bench was rejected on 3 of its own 20 windows while the snapshots were still being written back. Both
   are committed; the adjusted reading uses the element-wise minimum. The `after` floor was taken twice
   (`run_floor` overwrote `floor/after/floor.json`); the committed record is the post-measurement one.
5. **flashdb has its own recurring multi-hundred-millisecond group, and it is not a host stall.**
   The largest `group_elapsed_us` of each flashdb arm-run is 14-620 ms, in **9 of 15** arm-runs of
   f70d91c and **12 of 15** of this run, at a window between 7 and 18 (not one fixed window, not one
   block). At 200 rps it
   occasionally leaves a deep enough queue that the window's schedule-lag p99 passes 10 ms: 618 ms →
   breach (f70d91c block 5), 573 ms and 583 ms → breach (this run's blocks 2 and 3), while 404-476 ms
   did not breach. **f70d91c called its single instance "one host stall"; that label was wrong** — the
   same feature is present, at the same magnitudes, in runs that did not breach. Its cause is not
   diagnosed here; the engine's flush/compaction path is the named candidate, and the smallest fix is to
   attribute a group that exceeds the ceiling to the engine's own per-window counters (the driver's
   `--metrics-json` windows already carry `group_elapsed_us` and the region counters) before tuning.
   The two 200 rps rejections are this feature, not the box's other work: the breach window's lag p99
   (30.4/33.7 ms) is the same magnitude as the quieter f70d91c block-5 breach (34.1 ms).
6. **The box was not as quiet as f70d91c's phase, and the concurrent-work detector cannot prove it
   was.** loadavg was min 0.68 / median 1.95 / max 4.10 (f70d91c: 0.32 / 0.80 / 2.02), and the
   boundaries list foreign processes — `35-vs2-bin` at 64-73% CPU and two `chrome-headless` at 26-55%
   — around 00:10-00:12:36. `CONCURRENT_WORK_PATTERNS` does not include them, so `concurrent_work`
   stayed empty; the detector is a build/seed/second-harness check, **not** a claim of an idle box, and
   this report does not make that claim. The cores ran at 3.6 GHz throughout, the pairing is
   within-window, and the arms' ordering was rotated; **smallest fix:** add the process names of the
   other work streams (`*-bin`, `chrome-headless`, the agent runner) to `CONCURRENT_WORK_PATTERNS`, so a
   future phase stops instead of recording them.
7. **The aborted first attempt, and what it cost.** The first launch (20:46, superseded) ran the setup
   phase correctly — the floor `before` and all three snapshots are its output, reused rather than
   rebuilt — and died in block 1 at the postgres arm with a `TypeError`; the fixes (the signature, a
   bounded readiness poll, a bounded teardown, one exported `M1_PG_ROOT`, a `data_directory` check) are
   in the committed runner and bound by
   `test_the_arm_step_passes_every_keyword_its_arm_builders_accept`. Its boundary records remain in the
   work directories and are excluded from every summary here by timestamp (111 of 201 boundaries before
   the re-run's `--since`).
8. **One seed, one dataset, N=5.** No 10-seed matrix was run, so neither T5 nor T7 is a seed-to-seed
   claim; the per-block table is what supports the IQRs. The pre-registration's "10 independent
   access-distribution seeds per arm; report median and IQR" aggregation is therefore **not** met by
   this run: it is one seed x 5 paired blocks, and the 1x rung (50 rps) is the reference rate the
   pre-registration still lists as unsigned-off.
9. **Group commit.** Mean group size was 1.000-1.017 on every arm (max 1-9), so every arm ran one
   durable commit per request in practice; the batch cap of 64 never bound the rate. sqlite's max group
   reached 9 at 200 rps and flashdb's 8, which is the batch forming behind a slow group, not a relaxed
   durability setting.
10. **f70d91c's T6 arithmetic used an unregistered rule.** Its scorer defined "common" as *sustained by
    every arm in ≥4 of 5 blocks*, which is not the pre-registration's "highest common **offered**
    level"; that rule produced the empty denominator and the "extend the ladder above" fix. Both are
    corrected in `scripts/m1-controlled-score.py`: the check is the registered one, and the named
    smallest fix is now rungs **below** the floor (§5, T6).

## 10. Reproduce

```bash
# setup (the f70d91c snapshots and floor are reused as committed; nothing is rebuilt):
#   runs/snapshot/{flashdb-declared,sqlite,postgres}.json + the snapshot bytes (git-ignored)
#   runs/floor/{before,after}/floor.json
# the checkpoint probe (2 x 20 s, one restored snapshot, only wal_autocheckpoint differs):
scripts/m1-checkpoint-probe.sh
# the 5 paired blocks, re-measured with the fixes, reusing the floor and the snapshots:
scripts/m1-controlled-run.sh --phase blocks --since "$(cat .scratch/82-controlled-m1/runs/since.txt | cut -d= -f2)"
# the score (identical to the orchestrator's own step):
python3 scripts/m1-controlled-score.py --root .scratch/82-controlled-m1/runs \
    --out .scratch/82-controlled-m1/runs/report.json \
    --markdown .scratch/82-controlled-m1/runs/table.md --since "$(cat .scratch/82-controlled-m1/runs/since.txt | cut -d= -f2)"
```

Every arm-run holds `flock /tmp/m1-measure.lock` (the orchestrator holds it for the whole measured
phase and hands it down), records its own argv, caps, snapshot digest, restore digest and pacing in
`runs/b<block>/rung-<rung>/spec.json` and `run-<arm>.json`, and records the machine state at both of
its boundaries in `machine-state.jsonl`. The evidence committed beside this report:
`runs/run-spec.json`, `runs/since.txt`, `runs/orchestrator.log`, `runs/floor/{before,after}/`,
`runs/snapshot/*.json`, `runs/probe-checkpoint/`, `runs/b<block>/rung-<rung>/{manifest,spec,run-<arm>,windows-<arm>,machine-state,loadavg}`,
`runs/report.json`, `runs/table.md`. `runs/.gitignore` keeps the regenerable state (the frozen seed
trace, the instances, the snapshots' bytes, the postgres cluster) out of git.

## 11. What this supersedes in f70d91c

1. **"sqlite is rejected in every block at every rung (lag p99 ~254 ms)"** — was sqlite's unstated
   `wal_autocheckpoint=1000` default, confirmed by probe (§9.2). With the policy stated: 0 rejected
   windows in 15 crossings, lag p99 0.03-2.5 ms.
2. **"postgres in 8 of 15 blocks"** — with the pacer opened: **0 of 15**. (The count in f70d91c's own
   headline was also wrong: the committed records show **10 of 15**, not 8.)
3. **"T6 has no denominator"** — the denominator exists under the registered reading; the ratio is
   1.00x and the pre-registered 3x is unreachable for any pair of arms at one offered level (§5, T6).
4. **"flashdb's only rejection is one host stall"** — it is one instance of a recurring flashdb group of
   14-620 ms present in most arm-runs of both measurements (§9.5).
5. **"the baselines sit below the ≥10x regime floor, which is generous to them, not to flashdb"** — true
   for sqlite (4.04x), **false for postgres** by its data directory (16.40x, §7).
6. **"flashdb's schedule lag is 5-125x below the ceiling and the best of the three"** — true only against
   a closed-loop SQL client. Open loop, the baselines' lag p99 is lower than flashdb's at every rung.
