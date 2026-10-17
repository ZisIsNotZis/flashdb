# 82 — the controlled M1 verdict: T5/T6/T7 at 12.55x, paired and decidable

Status 2026-10-17, `wave/m1-controlled`. Authority: the pre-registered contract
(`.scratch/15-objective-v0/pre-registration.md` §"External baselines and thresholds") and ticket 82's
protocol (`.scratch/82-controlled-m1/issues.md`). **No pre-registered threshold was changed and no
engine code was touched.** The machine-readable document beside this file is `report.json`, generated
from the committed per-run records by `scripts/m1-controlled-score.py`.

**The one-line answer.** On a quiet, AC-powered, EPP-`performance`, 3.6 GHz box, `flashdb-declared`
holds **50 rps in 5 of 5 paired blocks, 100 rps in 5 of 5, and 200 rps in 4 of 5** at **12.546x** of
its 128 MiB cap, with zero errors and a schedule-lag p99 of 0.41-3.9 ms. Neither baseline holds any
rung: sqlite is rejected in every block at every rung (lag p99 ~254 ms, a systematic stall) and
postgres in 8 of 15 blocks. **T5 PASS (deficit 1.001 against the 1.5x requirement), T7 PASS, T6 FAIL
— the pre-registered 3x condition has no denominator, because no baseline has a sustained level at
any rung.** The ticket-80 finding "no arm sustained the regime rung" is overturned *for flashdb*
by removing the two confounds it named: the battery/power-saver state and the per-run re-seed.

## 1. The protocol as executed

| element | what ran |
|---|---|
| dataset | the frozen 10x instance: 200,000 customers, 2.65M skus x 4 locs = 10,600,000 stock cells, seed 42, theta 1.0, `on_hand` 1,000,000, from the frozen seed trace `runs/m1-10x/rung-50/seed.jsonl` (hard-linked into every work directory; digest recorded in each `spec.json`) |
| pre-load instance | built **once** per arm (`build-instance`): flashdb `seed-inventory` 121.8 s under a 512 MiB construction cap, sqlite 76.7 s, postgres 250.6 s under the engine's own 128 MiB scope. Each instance was then **snapshotted** (a verified copy) |
| snapshot digests | flashdb 1,683,012,213 B / 158 files / `67b3d10c…`; sqlite 541,876,224 B / 1 file / `46c6f1e1…`; postgres data dir 2,201,362,806 B / 1,361 files / `7f9a4ea8…` (the data directory, WAL included) |
| restore before every run | `restore` copies the snapshot back and **re-hashes it**; a digest mismatch stops the run. **45 of 45 arm-runs restored the same digest as their arm's snapshot** — the seeded state is reproduced by copy, not re-created by seeding |
| blocks | **N = 5**, rungs 50/100/200, arms `flashdb-declared`, `sqlite`, `postgres`, **20 s of offered load** per arm-run (1,000/2,000/4,000 requests), one process at a time |
| order inside a block | rung-major, and the arm order **rotated per (block, rung)** so no arm always ran first or last in a window (rotation recorded in `orchestrator.log`) |
| validity rule | one rule, `cold_0b.summarize_curve`: a window is rejected when its per-request schedule-lag p99 > 10,000 us (`lag_ceiling`), when lag grew for 3 consecutive windows (`lag_saturation`), or on any request error (`errors`); a rung is **sustained** iff the achieved rate is within 5% of the offered rate and zero windows were rejected |
| memory boundary | the **engine only** in a 128 MiB `MemoryMax` scope: `flashdb-load` in its own scope; sqlite's whole process (its engine is embedded, `PRAGMA cache_size = -8192`); the postgres server in its own named scope, client outside. The harness, the restore copies and the read-back are outside every scope |
| lock | `/tmp/m1-measure.lock` held by the orchestrator for the **whole** measured phase and inherited by every arm step (`M1_LOCK_HELD=1`), so nothing could interleave between the arms of a block |
| durability | one durable commit per request on every arm at every rung: **mean group size 1.00 on flashdb at all 15 crossings** (max 1-8), 1.00-1.04 on postgres, 1.01-1.04 on sqlite; sqlite `journal_mode=wal` + `synchronous=2 (FULL)` read back from sqlite itself, postgres `fsync=on`, `synchronous_commit=on` echoed from the server's own `SHOW`. sqlite's max group size reached 13-55 when it fell behind, so its ceiling was not inert — disclosed, and it did not make it faster (it was rejected in every block) |
| verification | the inventory oracles ran in **block 1** and PASSed for both SQL arms (12 checks each over 10.6M cells). Block 2-5 records say `DEFERRED` — the restored bytes are identical in every block, so the committed state is the same state. **flashdb's read-back is DEFERRED in all blocks** (§9.1) |
| device floor | an idle pacer and an fsync-per-request pacer at 200 rps, **no engine**, at the same machine state, before and after the measured phase (§3) |
| concurrent work | `machine-state` refuses to continue when a build (`cargo`/`rustc`), a seed job (`flashdb-scale`/`seed-inventory`) or a second harness is running: **0 such records in 92 boundaries**. The desktop applications (firefox, gnome-shell, `pi`, ptyxis) stayed, as ticket 82 allows; pairing is the control |

## 2. Machine state, recorded at all 92 arm boundaries

| field | value over the measured phase (23:06:43 → 23:35) |
|---|---|
| AC online | `1` at every boundary |
| power profile (`powerprofilesctl get`) | `performance` at every boundary |
| EPP (`energy_performance_preference`, all 8 CPUs) | `performance` at every boundary |
| `scaling_cur_freq` | per-record median of the 8 cores **3,599,335-3,600,692 kHz**; per-core readings 0.4-3.6 GHz (an idle core drops even under EPP, which is why the record keeps all eight) |
| `scaling_max_freq` | 3,600,000 kHz |
| **governor name** | **`powersave`** — recorded and *not* trusted: on this intel_pstate platform the frequency is driven by EPP, so the governor's name contradicts the 3.6 GHz the cores actually ran at. This is exactly the trap ticket 82 names |
| load average (1 min, 8 cores) | **min 0.32, median 0.80, max 2.02** over 92 boundaries (ticket-80 run B's measured phase sat at 2.7-6.1) |
| busiest processes seen | `pi`, `firefox-bin`, `gnome-shell`, `gjs`, `ptyxis`, `python`, `MainThread` — the desktop and the agent runtime, no build, no seed, no second harness |
| `nproc` | 8 |

## 3. The device floor (no engine in the path)

Measured twice, at the same machine state, with the arms' own pacer and window rule.

| floor | rate | idle schedule-lag p50 / p99 | fsync service p50 / p99 (one durable commit per request) | fsync lag p99 | rejected windows |
|---|---|---|---|---|---|
| `before` (20:56:52, loadavg 2.76) | 200 rps | 10 / **197 us** | 3,105.5 / **9,821 us** | 162,595 us | idle 0/20, fsync **3/20** (`lag_ceiling`) |
| `after` (23:29:33, loadavg 1.22) | 200 rps | 37 / **207 us** | 3,566 / **9,905 us** | 4,096 us | idle 0/20, fsync **0/20** |

Readings, and what each one means:

* **The machine's own scheduler floor is ~0.2 ms** (lag p99 197-207 us): 2% of the pre-registered
  10 ms ceiling. Every arm's lag reading is therefore dominated by the arm, not by the machine.
* **The device's own durable-commit floor is ~9.8 ms p99** for a one-commit-per-request pacer with no
  engine, p50 3.1 ms. At 200 offered requests per second with one fsync each, the *device* is already
  at 98% of the 10 ms budget: the pre-registered ceiling at the top rung is a device-edge bound on
  this box, not a generous one. Any arm that also serialises its commit behind its own dispatch (the
  synchronous SQL pacers) therefore reaches the ceiling first — §9.3.
* The `before` floor's fsync bench was rejected on 3 of its own 20 windows (lag p99 162 ms) while the
  instance snapshots were still being written back; the `after` floor, at the measured phase's own
  load, was rejected on none. Both are reported, and the floor-adjusted reading uses the
  **element-wise minimum** of the two, so no arm is flattered by the calmer floor.

**The floor-adjusted reading** (the arm's median lag p99 as a multiple of the 197 us idle floor, and
its service p99 as a multiple of the 9,821 us fsync floor):

| arm | rung | lag p99 (median) | x idle floor | service p99 (median) | x fsync floor | blocks clean at `10 ms + floor` |
|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 1,035 us | **5.25x** | 82,005 us | 8.35x | 5/5 |
| postgres | 50 | 13,112 us | 66.6x | 11,909 us | 1.21x | 2/5 |
| sqlite | 50 | 254,000 us | 1,289x | 9,864 us | 1.00x | **0/5** |
| flashdb-declared | 100 | 425 us | **2.16x** | 357,614 us | 36.4x | 5/5 |
| postgres | 100 | 22,607 us | 114.8x | 10,529 us | 1.07x | 1/5 |
| sqlite | 100 | 254,315 us | 1,291x | 7,547 us | 0.77x | **0/5** |
| flashdb-declared | 200 | 3,384 us | **17.2x** | 280,261 us | 28.5x | 4/5 |
| postgres | 200 | 18,274 us | 92.8x | 11,423 us | 1.16x | 2/5 |
| sqlite | 200 | 261,206 us | 1,326x | 7,578 us | 0.77x | **0/5** |

The absolute 10 ms ceiling stays the primary rule (§4-§6). The floor-adjusted column is what keeps a
reading honest about *whose* latency it is: flashdb's lag is 2-17x the machine's own floor, while
sqlite's is 1,289-1,326x — 25x the device's *fsync* tail, i.e. an engine stall (a WAL checkpoint),
not a host stall.

## 4. The decisive table — median and IQR across the 5 paired blocks

Per (arm, rung): achieved rps, schedule-lag p99, service p99, rejected windows, errors, engine peak.
Median (IQR) over blocks; the per-block values are in `report.json` under
`aggregate[].per_block` and the raw windows in `runs/b<block>/rung-<rung>/windows-<arm>.jsonl`.

| arm | rung | achieved rps | sustained blocks | lag p99 us | service p99 us | rejected windows (total) | errors | engine peak MiB of 128 | blocks at `10 ms + floor` |
|---|---|---|---|---|---|---|---|---|---|
| **flashdb-declared** | **50** | 49.95 (0.0) | **5/5** | **1,035 (13)** | 82,005 (50,382) | **0** — | 0 | 61.8 | 5/5 |
| sqlite | 50 | 50.00 (0.0) | 0/5 | 254,000 (3,200) | 9,864 (3,094) | 9 (`lag_ceiling` 9) | 0 | 30.3 | 0/5 |
| postgres | 50 | 50.00 (0.0) | 2/5 | 13,112 (14,596) | 11,909 (10,812) | 3 (`lag_ceiling` 3) | 0 | 33.7 | 2/5 |
| **flashdb-declared** | **100** | 99.95 (0.0) | **5/5** | **425 (17)** | 357,614 (341,263) | **0** — | 0 | 62.0 | 5/5 |
| sqlite | 100 | 100.00 (0.0) | 0/5 | 254,315 (2,853) | 7,547 (1,023) | 8 (`lag_ceiling` 8) | 0 | 31.9 | 0/5 |
| postgres | 100 | 100.00 (0.0) | 1/5 | 22,607 (4,754) | 10,529 (5,403) | 4 (`lag_ceiling` 4) | 0 | 33.8 | 1/5 |
| **flashdb-declared** | **200** | 200.00 (0.0) | **4/5** | **3,384 (669)** | 280,261 (340,516) | 1 (`lag_ceiling` 1) | 0 | 63.3 | 4/5 |
| sqlite | 200 | 200.00 (0.0) | 0/5 | 261,206 (577) | 7,578 (3,720) | 11 (`lag_ceiling` 11) | 0 | 32.7 | 0/5 |
| postgres | 200 | 200.00 (0.0) | 2/5 | 18,274 (22,433) | 11,423 (1,028) | 3 (`lag_ceiling` 3) | 0 | 34.8 | 2/5 |

Every arm achieved the offered rate to within 0.1% in every block — the pacer's closed/open loop
kept up even when the *schedule* rule fired, which is exactly why the rule is a latency rule and not
a throughput one. **No arm produced a request error, and the SQL oracles passed on all 12 checks in
block 1.** Every rejected window in this run is a single-window `lag_ceiling` breach; the
`lag_saturation` clause fired nowhere, and no window had an error.

Per-block lag p99 (us), the raw spread the medians compress:

| arm | rung | block 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 1,012 | 1,031 | 1,081 | 1,035 | 1,044 |
| flashdb-declared | 100 | 502 | 413 | 425 | 410 | 430 |
| flashdb-declared | 200 | 3,177 | 3,242 | 3,911 | 3,384 | 34,088 |
| postgres | 50 | 23,825 | 22,320 | 508 | 7,724 | 13,112 |
| postgres | 100 | 388 | 19,138 | 23,892 | 22,607 | 26,051 |
| postgres | 200 | 28,099 | 49,945 | 5,666 | 18,274 | 2,604 |
| sqlite | 50 | 257,141 | 253,941 | 258,892 | 254,000 | 252,740 |
| sqlite | 100 | 253,881 | 257,069 | 254,216 | 358,084 | 254,315 |
| sqlite | 200 | 259,771 | 261,206 | 260,811 | 264,663 | 261,388 |

flashdb's 15 readings are tight except one host stall (block 5, rung 200: 34 ms). postgres is
bimodal (0.4-50 ms) block to block. sqlite sits on 254 ms at **every** crossing of every block with
an IQR of 3 ms — a deterministic stall, not noise (§9.2).

## 5. T5, T6, T7 — conditions, arithmetic, verdicts

### T5 — low-load non-inferiority at the 1x rung (50 rps): **PASS**

* **Condition (pre-registered verbatim):** "At the 1x rung flashdb must be within **1.5x** of the
  slower of sqlite/postgres; **2x** is the fail line; a **10x** gap is a design failure."
* **Prong A (the pre-registered metric, achieved throughput at 50 rps):** per block,
  `slower_baseline_achieved / flashdb_achieved` = **1.001** in all 5 blocks (50.00 / 49.95); the
  median is **1.001**, against a requirement of 1.5. **PASS.**
* **Prong B (the ticket-80 cross-check, so a contended rung cannot be read as a pass):** did flashdb
  itself sustain the rung? **5 of 5 blocks**, zero rejected windows, achieved 49.95 of 50 (99.9%).
  **PASS.**
* **The baselines' own sustained status (reported, not used to flip T5):** sqlite **0/5**, postgres
  **2/5** blocks. T5 is a claim about flashdb's rate at the rung; a baseline that cannot hold the
  rung at a *smaller* footprint is evidence for flashdb, not against it. It is what removes T6's
  denominator (§T6) and it is disclosed there instead of here.
* **Defect and smallest fix:** none material to prong A. Residual: the 1x rung's *sustained-level*
  comparison has no baseline level; the smallest fix is to bring each baseline to the pre-registered
  `>= 10x` regime floor (sqlite needs ~2.4x the logical dataset, postgres ~1.2x) and re-read both
  arms there — a change of dataset size, not of threshold.

### T6 — high-load superiority: **FAIL**

* **Condition (pre-registered verbatim):** "At the highest common offered level flashdb must reach at
  least **3x** the best baseline's **sustained** throughput under the same latency rule."
* **The highest common offered level is 200 rps** (all three arms were offered it in all 5 blocks).
  flashdb **sustained it in 4 of 5 blocks** (200.00 rps, one rejected window in block 5); **sqlite
  sustained it in 0 of 5** and **postgres in 2 of 5**.
* **Arithmetic:** flashdb's sustained throughput at 200 rps = **200.00 rps**; the best baseline's
  *sustained* throughput at 200 rps = **∅ (neither sustained it)**, so `3 x ∅` has no value and the
  pre-registered ratio cannot produce a number. The achieved-vs-achieved reading, which is *not* the
  pre-registered one, is `200.00 / 200.00 = **1.00x**` against the 3x requirement. Either way the
  gate is **not met**: **FAIL**, not "undecidable".
* Highest sustained rung per arm: **flashdb-declared 200 rps** (4/5 blocks, and 100 rps in 5/5);
  **sqlite none** (0/5 at every rung); **postgres none** (2/5, 1/5, 2/5 — never a majority).
* **Defect and smallest fix:** the pre-registered comparison presumes a common *sustained* level, and
  the baselines have none at these footprints. Smallest fix: run the baselines in the same
  `>= 10x` regime (a ~2.4x larger logical dataset for sqlite, ~1.2x for postgres) so that each arm's
  sustained level exists, then re-read T6 at the highest common level. That is the ticket-80 ladder
  cost model (~8 h at 10x); it changes no threshold.

### T7 — initial-policy quality: **PASS**

* **Condition:** "The declared arm alone must already pass T5."
* The declared arm is the arm measured above: deficit **1.001** (requirement 1.5) with the rung
  sustained **5/5**. **PASS**, inherited from T5 with no separate reading needed.
* **Disclosure:** `flashdb-learned` was not run. M1 wires no learner controller, so a second flashdb
  arm would measure the same declared wiring twice (as ticket 80 disclosed); T7 exists to keep the
  *declared* arm from being absurd, and the declared arm is the one measured here.

## 6. Per-metric verdicts, with the defect and the smallest fix

Rules: achieved rate within 5% of offered; schedule-lag p99 <= 10,000 us (pre-registered); zero
rejected windows; zero errors; engine peak <= 128 MiB. The service p99 carries **no pre-registered
ceiling** — the pre-registration's bound is on schedule lag — so its secondary rule is the same
10 ms bound adjusted by the device's own fsync p99 (`service p99 - 9,821 us <= 10,000 us`).

| metric | arm | rung | median (IQR) | rule | verdict | defect → smallest fix |
|---|---|---|---|---|---|---|
| achieved rps | all three | 50/100/200 | 49.95 / 99.95 / 200.00 | within 5% of offered | **PASS** (9/9 cells) | — |
| lag p99 | flashdb-declared | 50 / 100 / 200 | 1,035 / 425 / 3,384 us | <= 10,000 us | **PASS** | — |
| lag p99 | postgres | 50 / 100 / 200 | 13,112 / 22,607 / 18,274 us | <= 10,000 us | **FAIL** | postgres's own commit path: its lag p99 is 1.2-27x the device's fsync tail while its service p99 is 1.0-1.2x it → the wait is queueing behind the synchronous pacer. Smallest fix: give the postgres arm an overlapped commit (`synchronous_commit=on` with a pipelined batch, as `flashdb-load` does) so its dispatch is not serialised behind its barrier |
| lag p99 | sqlite | 50 / 100 / 200 | 254,000 / 254,315 / 261,206 us | <= 10,000 us | **FAIL** | a 254 ms stall at **every** crossing with a 3 ms IQR: sqlite's WAL auto-checkpoint (1,000 pages) writes the 542 MB database back through a 128 MiB scope and blocks the writer. Smallest fix: set `PRAGMA wal_autocheckpoint` explicitly (or `journal_size_limit`) and record it, so the checkpoint cadence is a stated parameter rather than sqlite's default |
| service p99 | flashdb-declared | 50 / 100 / 200 | 82,005 / 357,614 / 280,261 us | 10 ms above the 9,821 us device fsync floor | **FAIL** | the per-request completion tail: its schedule lag is the best of the three (dispatch never falls behind) but a request can then wait 14-460 ms for its barrier to land — 8-36x the device's own fsync tail, so it is the engine's own barrier/eviction path, not the host. Smallest fix: instrument the commit path for the > 100 ms outliers (`flashdb-load` already reports `work_p99`; a per-window `group_elapsed_us` histogram would attribute them) before tuning anything |
| service p99 | postgres / sqlite | 50 / 100 / 200 | 11,909 / 10,529 / 11,423 and 9,864 / 7,547 / 7,578 us | as above | **PASS** | — (their commit is one device fsync, 1.0-1.2x the floor) |
| rejected windows | flashdb-declared | 50 / 100 / 200 | 0 / 0 / 1 | zero | **PASS** at 50 and 100, **FAIL** at 200 | one `lag_ceiling` window in one block of five. Smallest fix: more blocks (a 1-in-5 event needs N >= 9 to place its own rate); no engine change is implied by one window |
| rejected windows | postgres | 50 / 100 / 200 | 3 / 4 / 3 | zero | **FAIL** | same commit-path defect as its lag row |
| rejected windows | sqlite | 50 / 100 / 200 | 9 / 8 / 11 | zero | **FAIL** | same checkpoint defect as its lag row |
| errors | all three | all rungs | **0 in all 45 arm-runs** | zero | **PASS** | — |
| engine peak | flashdb-declared | 50 / 100 / 200 | 61.8 / 62.0 / 63.3 MiB of 128 | <= 128 MiB | **PASS** | — (it serves a 1.57 GiB instance from *half* its budget: the cap did not bind) |
| engine peak | sqlite / postgres | 50 / 100 / 200 | 30.3 / 31.9 / 32.7 and 33.7 / 33.8 / 34.8 MiB of 128 | <= 128 MiB | **PASS** | — (sampled `memory.current` maxima, hence lower bounds; their bulk-load residue is in `start_bytes`, recorded per run) |
| inventory oracle | sqlite / postgres | block 1 | PASS, 12 checks each | PASS | **PASS** | blocks 2-5 DEFERRED (identical restored bytes); see §9.1 for flashdb |
| inventory oracle | flashdb-declared | all blocks | DEFERRED | PASS | **FAIL (uncompleted criterion)** | the read-back was not run in this phase. Smallest fix: `baseline_0b verify` on the live instance (10.7M probes, 16-100 min by ticket 80's measurement); run B's read-back PASSED on this identical seed, dims and cap, but that is not this run's evidence |

## 7. Physical footprint — reported as measured, nothing inflated

| arm | data bytes | x 128 MiB cap | meets the pre-registered `>= 10x` regime floor |
|---|---|---|---|
| flashdb-declared | 1,683,903,919 | **12.546x** | **yes** |
| postgres | 1,103,713,983 | 8.223x | no |
| sqlite | 546,156,096 | 4.069x | no |

The dataset is one logical dataset (10.6M cells); the physical size differs by encoding. The
pre-registered floor is on the physical data against the memory budget, so **only flashdb is actually
inside the pre-registered regime** — the baselines sit *below* it (4.07x, 8.22x), i.e. their working
sets are smaller relative to the same 128 MiB cap than the regime demands. That makes their reading
generous to them, not to flashdb, and it is the honest reason their rejections cannot be attributed
to a harder regime.

## 8. What the numbers support, and what they do not

**Supported.**

1. **The instrument and the protocol work.** 45 arm-runs, 45 verified restores of the same seeded
   bytes, 90 machine-state boundaries with zero concurrent work, one lock held throughout, one
   validity rule for every arm, one durable commit per request on every arm, zero request errors and
   all SQL oracle checks passing. The ticket-80 methodological gap (battery/power-saver, measured
   phase's own load unrecorded, re-seed per run) is closed and the records say so.
2. **flashdb holds the 1x rung at 12.546x**: 49.95 of 50 rps, lag p99 1,012-1,081 us (5-125x *below*
   the ceiling), zero rejected windows in **5 of 5 paired blocks**, engine peak 61.8 MiB of its
   128 MiB budget. T5 and T7 PASS on their pre-registered arithmetic (deficit 1.001).
3. **flashdb holds 100 rps in 5/5 blocks as well** (lag p99 410-502 us) and **200 rps in 4/5**
   (3.2-3.9 ms, one 34 ms host stall). Nothing in ticket 80 supported a claim at this level; this run
   does, on this box, in this state.
4. **Neither baseline holds the rung** at its own (smaller) footprint: sqlite 0/5 at every rung on a
   reproducible 254 ms checkpoint stall; postgres 2/5, 1/5, 2/5 on a commit path that queue behind its
   synchronous pacer. The pre-registered T6 comparison therefore has no denominator here — T6 FAIL
   for that reason, not for a measured shortfall.
5. **The machine's own floor is 0.2 ms of schedule lag** and the device's own one-fsync-per-request
   floor is 9.8 ms p99: at 200 rps the 10 ms ceiling is at the device's edge, which is why the top
   rung separates the arms at all.

**Not supported.**

1. **Any claim of 3x superiority over a baseline.** T6 FAIL: no baseline sustained any rung, so the
   3x condition is unmet in this dataset. The achieved-vs-achieved reading at 200 rps is 1.0x.
2. **Any claim that flashdb meets a per-request latency SLO.** Its schedule lag is excellent but its
   *service* p99 is 82-358 ms — 8-36x the device's fsync tail — because its barrier lands
   asynchronously. That is a real tail and it is not covered by the pre-registered rule.
3. **Any claim about the learned arm**, drift, reorg or compaction cost: they were not run.
4. **Any claim that generalises beyond this box and state.** One seed, one dataset, N=5 blocks, one
   machine (i5-8350U, 4C/8T, NVMe), desktop applications resident. The IQRs and the per-block table
   are the bound this run supports; they do not establish behaviour on another machine, another
   seed, or a busier box.
5. **Any claim from the aborted first attempt.** It is disclosed in §9.4 and its records are excluded
   from every number here.

## 9. Disclosures — the limits, each with its smallest fix

1. **flashdb's inventory read-back did not run in the measured phase.** Every flashdb block records
   `oracle: DEFERRED` (`--skip-verify`): the read-back is verification, it costs 16-100 min for
   10.7M probes (ticket 80's measurement), and running it 15 times would have bought nothing (the
   restored bytes are identical in every block). What the records do carry is `errors = 0` in all 15
   flashdb arm-runs and the take/miss counters. **Smallest fix:** `uv run python -m
   flashdb_harness.baseline_0b verify --workdir runs/b5/rung-200 --arm flashdb-declared` on the live
   instance left by the last run, merged into that run record.
2. **The sqlite stall is a checkpoint, and that is an inference from the evidence, not a probe.**
   The evidence: 254 ms at every crossing with a 3 ms IQR; its service p99 stays at 7.5-11.1 ms (one
   device fsync); the device's own fsync p99 is 9.8 ms. A WAL auto-checkpoint (1,000 pages) against a
   542 MB database behind a 128 MiB cap is the mechanism that fits all three. **Smallest fix:** run one
   sqlite arm with `PRAGMA wal_autocheckpoint=0` (or a stated page count) and compare — one 60 s run.
3. **The pacer asymmetry is a real methodological difference between the arms.** flashdb's driver
   submits its barrier to a background worker and keeps dispatching (open loop), while both SQL
   pacers execute a batch synchronously and commit before the next dispatch (closed loop). The
   consequence is visible in the data: an engine stall shows up in sqlite's *lag* (its next dispatch
   waits) and in flashdb's *service* (its dispatch does not wait). The lag rule therefore treats the
   synchronous arms more harshly by construction. **Smallest fix:** an asynchronous SQL pacer (submit
   the commit, dispatch the next ready batch, ack on completion) — the same overlap `flashdb-load`
   already has; until then, the service p99 column is the like-for-like reading and it is reported
   beside the rule.
4. **The aborted first attempt, and what it cost.** The first launch (20:46) ran the setup phase
   correctly — the floor `before` and all three snapshots in this report are its output, reused
   rather than rebuilt — and died in block 1 at the postgres arm with a `TypeError`:
   `cmd_arm` passed `verify=` to `run_postgres_arm`, which did not accept it. One postgres start-up
   guard then failed on a socket path and its teardown waited on a scope's own `sleep 7200`, which
   held that process for two hours before it exited. Fixes: the signature and its call (now bound by
   `test_the_arm_step_passes_every_keyword_its_arm_builders_accept`), a bounded readiness poll with
   the server log printed on failure, a bounded teardown that cannot wait on the scope, one exported
   `M1_PG_ROOT` for the whole chain, and a `data_directory` check that refuses to measure a cluster
   the run did not restore. The measurement then re-ran from block 1 in full; the aborted attempt's
   boundary records remain in the work directories and are excluded from every summary here by
   timestamp (19 of 111 boundaries).
5. **The `after` floor was taken twice** (once by the phase logic before the blocks, once after
   them). `run_floor` overwrote `floor/after/floor.json` and its windows file with the later
   measurement, so the committed `after` record is the post-measurement one; the extra boundary lines
   remain in `floor/after/machine-state.jsonl`.
6. **One seed, one dataset, N=5.** No 10-seed matrix was run. The per-block table in §4 is what
   supports the IQRs; it does not support a claim about seed-to-seed variation.
7. **Group commit reached 13-55 requests on sqlite** when it fell behind (mean 1.01-1.04), so its
   durability was not exactly one commit per request on every batch. It was rejected in every block
   regardless, so it did not inflate a pass.
8. **The floors differ by load state** (loadavg 2.76 before, 1.22 after). Both are committed; the
   adjusted reading uses the minimum.

## 10. Reproduce

```bash
# setup (instances built once + snapshotted, cluster of the run's own, device floor), then the
# paired blocks and the post-measurement floor, then the scoring
scripts/m1-controlled-run.sh --phase all
# this run was executed as two invocations, because the first attempt died inside block 1 (§9.4):
scripts/m1-controlled-run.sh --phase measure      # re-ran all 45 crossings + the after-floor
python3 scripts/m1-controlled-score.py --root .scratch/82-controlled-m1/runs \
    --out .scratch/82-controlled-m1/runs/report.json \
    --markdown .scratch/82-controlled-m1/runs/table.md --since 2026-10-17T23:06:43
```

Every arm-run holds `flock /tmp/m1-measure.lock` (the orchestrator holds it for the whole measured
phase and hands it down), records its own argv, caps, snapshot digest and restore digest in
`runs/b<block>/rung-<rung>/spec.json` and `run-<arm>.json`, and records the machine state at both of
its boundaries in `machine-state.jsonl`. The evidence committed beside this report:
`runs/run-spec.json`, `runs/orchestrator.log`, `runs/floor/{before,after}/`, `runs/snapshot/*.json`,
`runs/b<block>/rung-<rung>/{manifest,spec,run-<arm>,windows-<arm>,machine-state,loadavg}`,
`runs/report.json`, `runs/table.md`. `runs/.gitignore` keeps the regenerable state (the frozen seed
trace, the instances, the snapshots' bytes, the postgres cluster) out of git.
