# M1 — external baseline scoreboard (T5/T6/T7): measurement report

Status 2026-10-17, `wave/m1-baselines`. Authority: the pre-registered contract
(`.scratch/15-objective-v0/pre-registration.md` §"External baselines and thresholds") and the
milestone's acceptance (`docs/roadmap.md` M1). Nothing pre-registered was changed; no engine code was
touched. The method changes this report depends on — the per-engine memory boundary, seeding as
setup, verification as a separate step — are recorded in
`.scratch/80-external-baselines/issues.md` §"Measurement record".

**Two runs, and only one of them is at the pre-registered regime.**

| run | instance (docs) | flashdb data | regime | rungs | what it is |
|---|---|---|---|---|---|
| A. harness check | 30k customers, 30k skus x 4 loc | 18.6-23.8 MB | **0.15-0.19x** of the 128 MiB cap | 50/100/200/400 rps, 20 s each | the full ladder, every arm, every rung — PROVISIONAL, not the verdict |
| B. regime run | 200k customers, 2.65M skus x 4 loc | 1.68 GB | **12.5x** of the cap | 50 rps (the pre-registered 1x rung), 20 s | T5/T7 evaluated here; T6 has no ladder at this regime |

Every number carries its conditions. **The pre-registered floor is data >= 10x the memory budget
(1.28 GiB at the 128 MiB cap); run B meets it for flashdb (12.5x) and is the one that answers T5/T7.
Run A does not meet it and is retained only as the harness check that its ladder works.**

## 1. The rule, applied identically to every arm

A window is rejected when its per-request **schedule-lag p99 exceeds 10,000 us**
(`--lag-ceiling-us`, the pre-registration's primary ceiling) or any request errored. A rung is
**sustained** when the achieved rate is within 5% of the offered rate and zero windows were rejected
(`cold_0b.SUSTAINED_TOLERANCE` / `summarize_curve`). The same function scores every arm's windows, so
no arm has its own rule. Service latency is reported beside the rule, not used by it.

One process at a time for every run: `flock /tmp/m1-measure.lock`, held for a whole rung. Run A's
load average was recorded before and after every rung: 1.47-2.54 (1-minute) on 8 cores, in
`runs/m1-measure/loadavg.jsonl`; the 400 rps rung — the one where every arm is rejected — began with a
1.47 reading, and its own closing reading is missing (the runner was edited while that rung was in
flight, §7.5). A 5.16 reading appeared between the 100 and 200 rps rungs, when the other tenants were
busiest. **Run B's boundaries were not load-average-recorded**: it was
driven by direct arm invocations rather than by the ladder runner, and its 1-minute load average is
therefore unknown (the surrounding half hour read 2-3 on 8 cores). That is a measurement gap, stated
rather than papered over; it matters because the scheduler — not the engine — decides a 12.5 ms lag
p99 at 50 rps.

## 2. Table A — the ladder (harness check, PROVISIONAL, data 0.15-0.19x of the cap)

Instance: 120,000 stock cells (30k skus x 4 loc) + 30,000 customers, seed 42, zipf theta 1.0, one
frozen trace per rung (`runs/m1-measure/rung-<rps>/manifest.json`), 20 windows per rung, batch B=64
on every arm (mean group size recorded; it is 1.00 at 50-200 rps and 1.03-10.78 at 400).

| arm | offered rps | achieved rps | achieved/offered | lag p99 us | service p99 us | rejected windows (reason) | sustained | engine cgroup peak |
|---|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 49.95 | 99.9% | 1,350 | 12,523 | 0 | yes | 73 MiB |
| flashdb-learned | 50 | 49.95 | 99.9% | 2,545 | 16,926 | 0 | yes | 73 MiB |
| sqlite | 50 | 50.00 | 100.0% | 601 | 17,752 | 0 | yes | 39 MiB |
| postgres | 50 | 50.00 | 100.0% | 3,504 | 12,473 | 0 | yes | 78 MiB |
| flashdb-declared | 100 | 99.95 | 100.0% | 2,505 | 14,271 | 0 | yes | 74 MiB |
| flashdb-learned | 100 | 99.95 | 100.0% | 713 | 12,763 | 0 | yes | 74 MiB |
| sqlite | 100 | 100.00 | 100.0% | 1,654 | 15,070 | 0 | yes | 39 MiB |
| postgres | 100 | 100.00 | 100.0% | 24,426 | 9,996 | 1 (lag_ceiling) | **no** | 73 MiB |
| flashdb-declared | 200 | 200.00 | 100.0% | 12,689 | 14,102 | 3 (lag_ceiling 1, lag_saturation 2) | **no** | 75 MiB |
| flashdb-learned | 200 | 200.00 | 100.0% | 12,745 | 19,959 | 2 (lag_ceiling) | **no** | 75 MiB |
| sqlite | 200 | 200.00 | 100.0% | 3,406 | 12,780 | 0 | yes | 40 MiB |
| postgres | 200 | 199.99 | 100.0% | 8,653 | 19,953 | 0 | yes | 74 MiB |
| flashdb-declared | 400 | 399.75 | 99.9% | 40,307 | 52,005 | 11 (lag_ceiling) | **no** | 81 MiB |
| flashdb-learned | 400 | 399.75 | 99.9% | 40,177 | 73,252 | 11 (lag_ceiling 10, lag_saturation 1) | **no** | 80 MiB |
| sqlite | 400 | 400.00 | 100.0% | 14,919 | 10,358 | 1 (lag_ceiling) | **no** | 41 MiB |
| postgres | 400 | 391.16 | 97.8% | 442,749 | 601,221 | 21 (lag_ceiling) | **no** | 76 MiB |

What run A says (and does not): with the whole dataset resident in the 128 MiB budget, every arm
reaches the offered rate until its own commit path breaks the 10 ms lag rule. sqlite sustains the
highest offered level of the four (200 rps with 0 rejections), postgres sustains 200 after being
rejected once at 100, and both flashdb arms are rejected at 200 while still achieving the offered
rate — in this warm regime flashdb is **not** faster than sqlite. That is a legitimate observation
about a fully cached dataset and says nothing about the pre-registered regime, which is why it is not
reported as a T5/T6 answer.

## 3. Table B — the regime run (1x rung, flashdb at 12.5x of the cap)

Instance: 10,600,000 stock cells (2.65M skus x 4 loc) + 200,000 customers, seed 42, zipf theta 1.0,
one frozen trace of 1,000 requests at 50 rps offered (`runs/m1-10x/rung-50/`), 20 windows, B=64.
flashdb's instance is **1,683,903,919 B = 12.55x** the 128 MiB engine cap; the same logical rows
occupy 0.6-1.2x in the SQL arms, whose row encoding is more compact (see §5).

| arm | offered rps | achieved rps | lag p99 us | service p99 us | rejected windows (reason) | sustained | engine cgroup peak | data bytes (x cap) |
|---|---|---|---|---|---|---|---|---|
| flashdb-declared | 50 | 49.95 | 12,470 | 24,900 | 1 (lag_ceiling, window 8) | **no** | 134,217,728 B (the full 128 MiB cap) | 1,683,903,919 (12.55x) |
| postgres | 50 | 50.00 | 2,199 | 22,004 | 0 | **yes** | 134,221,824 B (the cap) | 1,103,779,519 (8.22x) |
| sqlite | 50 | 50.00 | 6,120 | 25,909 | 0 | **yes** | 134,217,728 B (the cap) | 546,160,216 (4.07x) |

flashdb-declared at the pre-registered regime: it **achieves** the offered 49.95 rps with zero
errors, and is rejected on exactly one of twenty windows (window 8: lag p99 12.5 ms against the
10 ms ceiling, service p99 32.3 ms). It is therefore *not* sustained under the pre-registered rule —
a marginal, one-window failure, not a collapse — and its engine cgroup sits at the cap for the whole
run. Both baselines sustain the same rung on the same rows, each in its own capped cgroup: sqlite
50.00 rps with lag p99 6.1 ms and no rejected window, postgres 50.00 rps with lag p99 2.2 ms and no
rejected window, and both oracles pass. So on this one seed and this one rung, **both external
baselines hold 50 rps at the pre-registered regime and flashdb-declared does not**: the achieved rate
is identical, the tail is not. All three engine cgroups sit at the 128 MiB cap.

The three arms' files differ in compactness (156, 51 and 102 B/row for flashdb, sqlite and postgres on
the same 10.8M rows), so one logical dataset occupies 12.55x, 4.07x and 8.22x of the cap. flashdb's
footprint alone meets the pre-registered 10x floor; postgres (8.22x) and sqlite (4.07x) are below it
because their encodings are smaller, and no threshold was changed to make that read differently.

## 4. The inventory oracle, per arm, on the measured run

| arm | run A (0.15-0.19x) | run B (12.5x) |
|---|---|---|
| flashdb-declared | PASS (11 checks) | **not run** — see §7, the read-back costs 16 min-1.6 h at this size; the arm's run record says `oracle: DEFERRED` |
| flashdb-learned | PASS (11 checks) | not run (same wiring as declared in M1) |
| sqlite | PASS (12 checks) | PASS (12 checks) |
| postgres | PASS (12 checks) | PASS (12 checks) |

The oracle is the same code in both runs and fails loudly at the first violation (exit 2, the
offending cell or order named). Its checks in run A: cell count, per-cell `on_hand == initial +
sum(delta)` and `on_hand >= 0`, no duplicate order number, movement/order/backorder counts against the
committed take/backorder/miss counters, every movement's sku/delta against its order line, `sum(delta)
== -sum(qty)`, and (flashdb) `movements_scan` returning at most one movement per order with the
matching order number plus zero request errors.

## 5. What each arm's memory boundary was (disclosed, not hidden)

| arm | what the 128 MiB cap covers | engine-side memory control |
|---|---|---|
| flashdb-declared / flashdb-learned | `flashdb-load` only, in its own cgroup; the harness, pacer and read-back run outside it | the cgroup cap; the instance is BUILT under a separate transient construction scope (192 MiB in run A, 512 MiB in run B — seeding peaks with the dimension set) |
| sqlite | the whole arm process, because sqlite's engine is EMBEDDED in it | `PRAGMA cache_size = -8192` (8 MiB), recorded per run |
| postgres | the server process tree only, in its own named scope; the client is outside | the cgroup cap plus `shared_buffers=16MB`, `fsync=on`, `synchronous_commit=on` (echoed from the server's own `SHOW` in every run record) |

Durability, identical in both runs: one durable commit per batch on every arm — flashdb
`--group-requests 64` (one WAL barrier per group), sqlite `journal_mode=WAL` + `synchronous=FULL`,
postgres `fsync=on` + `synchronous_commit=on` (its data directory is on ext4, so its fsync is real;
the build record's earlier run had it on tmpfs, where fsync is a no-op). At 50 rps the batch ceiling
never binds: the measured mean group size is 1.00, i.e. one durable commit per request on all three
engines. The SQL pacers execute a batch synchronously; `flashdb-load` submits its barrier to a
background worker and can overlap. That difference is disclosed and is the main reason the SQL arms
fall behind at 400 rps in run A.

## 6. T5, T6, T7 — conditions, arithmetic, verdicts

### T5 — low-load non-inferiority

**Condition (pre-registered):** at the 1x rung, flashdb must be within **1.5x** of the slower of
sqlite/postgres; **2x** is the fail line; 10x is a design failure. Metric used: the deficit
`slower_baseline_rps / flashdb_rps` (1.0 = equal, >1 = flashdb slower).

- At the **pre-registered regime (run B, 12.5x)**: flashdb-declared 49.95 rps; the slower baseline is
  sqlite at 50.00 rps (postgres also 50.00) → deficit **1.001** → **within the 1.5x requirement**, and
  more than 10x inside the 2x fail line. Reading made explicit: T5 names a *throughput gap*, and a
  0.1% gap satisfies it. Under the stricter reading of the validity rule (throughput from rejected
  windows is not counted) flashdb has no *sustained* level at this rung while postgres does — its
  single rejected window is the whole difference, and it is disclosed above rather than averaged away.
- At the harness-check regime (run A, 0.15-0.19x, **PROVISIONAL**): slower baseline = sqlite at 50.00 rps
  (postgres 50.00), flashdb-declared 49.95 rps → deficit **1.001** → within 1.5x. Note that at 50
  rps the offered rate is the ceiling for every arm, so the metric cannot distinguish headroom; it is
  a non-inferiority check by construction, and the discriminating quantity is the highest sustained
  level (T6).

### T6 — high-load superiority

**Condition (pre-registered):** at the highest common offered level, flashdb must reach at least
**3x** the best baseline's sustained throughput under the same latency rule.

- At the pre-registered regime: **UNVERIFIED.** T6 needs the ladder at 10x, and a 10x ladder could
  not be run inside this work package's wall clock: each flashdb arm-run at 10x costs ~6 min of
  seeding plus the read-back, and four rungs x two flashdb arms do not fit beside the SQL arms.
  Stating a T6 number from run A would state a warm-cache result as the external claim.
- At the harness-check regime (PROVISIONAL): highest common offered level = 400 rps; best baseline's
  highest sustained level = sqlite at 200 rps; flashdb-declared 399.75 rps at 400 → **2.00x**, and
  neither flashdb arm sustained 400 (11 rejected windows). Verdict at that regime: **FAIL** (2.00x
  < 3x), and because flashdb does not sustain 200 rps while sqlite does, the number is not the whole
  story.

### T7 — initial-policy quality

**Condition (pre-registered):** the declared arm alone must already pass T5.

- At the pre-registered regime (run B): declared arm deficit **1.001** → **passes T5** at 12.5x
  (with the same one-rejected-window caveat as T5).
- At the harness-check regime (PROVISIONAL): declared arm deficit 1.001 → **passes** T5.
- Disclosure: `flashdb-learned` runs the same declared-default wiring in M1 (no learner controller is
  wired), so its T5 number and T7's declared number are the same measurement twice, not two
  independent arms.

## 7. What is unverified, and why

1. **T6 at the pre-registered regime.** The ladder at 10x needs 8 flashdb arm-runs, each ~6 min of
   seeding (10.8M docs) plus a read-back that runs at 1.7k-10.7k probes/s (16 min-1.6 h for 10.6M
   cells). Not representable in the 3 h cap; the measured cost is the reason, not a guess.
2. **The inventory oracle at the pre-registered regime, for the flashdb arms only.** Run B's flashdb
   arm is measured but its read-back was deferred: at 10.6M cells the verification is 16 min-1.6 h
   per arm. The SQL arms' oracles were run at 10x (see §4). This is the one acceptance criterion the
   regime run does not meet; it is why run B's flashdb row is not claimed as a passing measurement of
   the *full* acceptance.
3. **The single-seed, single-rung nature of run B.** One access-distribution seed, one offered level,
   one repeat — not the pre-registered 10 seeds / median-and-IQR, and not a ladder.
4. **Run B's load average at its boundaries** (see §1): not captured, so the one rejected flashdb
   window at the regime cannot be attributed to the engine or the machine from this run alone.
5. **Run A's rung-400 scoreboard** was regenerated after the fact (`rung-400/scoreboard.md`): the
   runner was edited while that rung was in flight and its final report step did not run. The run
   records themselves were unaffected; only the convenience markdown was regenerated from them.
6. **The shared box.** Load averages of 1.5-2.5 on 8 cores during run A mean some rejections are
   contaminated by other tenants; a quiet box would move the rejection thresholds, not the
   mechanism.

## 8. What the numbers support, honestly

The numbers support five claims and no more. First, the instrument works: one harness drives four arms
over one frozen trace at matched durability, each engine in its own 128 MiB cgroup, and the inventory
oracle passes for every arm on both runs except the one case disclosed above — so the equal-durability,
same-rule and correctness conditions are satisfied. Second, at the pre-registered regime (flashdb
1.68 GB = 12.55x of the cap, one seed, the 1x rung) **T5 passes by an enormous margin and T7 with it**:
flashdb-declared achieves 49.95 rps against sqlite's and postgres's 50.00, a deficit of 1.001 against a
1.5x requirement. Third — and this is what a reader should carry away — that same run shows both
baselines **sustaining** the rung cleanly (0 rejections, lag p99 6.1 ms for sqlite and 2.2 ms for
postgres) while flashdb-declared fails the sustained rule on one window of twenty (lag p99 12.5 ms).
T5's number is therefore not evidence that flashdb is *as good* at 12.5x; it is evidence that flashdb's
throughput is not behind, plus a tail-latency miss that a second seed would have to confirm or retire.
Fourth, in the warm 0.15-0.19x instance the ordering reverses and flashdb is not the fastest arm at any rung
above 100 rps: sqlite sustains 200 cleanly, both flashdb arms are rejected at 200, and T6 there is 2.0x
against the pre-registered 3x. Fifth, T6 at the pre-registered regime — the actual external-superiority
claim — **remains unevaluated**: it needs a ladder at 10x, and a 10x flashdb arm-run costs ~6 min of
seeding plus a read-back that runs at 1.7k-10.7k probes/s (16 min-1.6 h for 10.6M cells), which does not
fit in this work package's wall clock. Nothing here supports a claim that flashdb beats postgres or
sqlite at scale, a claim about the learned arm (in M1 it is the declared wiring), or a claim about
drift, reorg cost or the pre-registered 10-seed aggregation; those were not measured.

## 9. Reproduce

```
# the ladder (harness check): 4 rungs, 4 arms, 20 s each, one locked run per rung
scripts/m1-ladder-run.sh --customers 30000 --skus 30000 --locs 4 --seconds 20
uv run python scripts/m1-ladder-score.py                  # -> runs/m1-measure/report.json

# the regime run (1x rung) at >=10x: build the 10.8M-doc instance, engine in its own 128 MiB cgroup
scripts/m1-baseline-run.sh --workdir .scratch/80-external-baselines/runs/m1-10x/rung-50 \
    --requests 1000 --rate 50 --customers 200000 --skus 2650000 --locs 4 --skip-verify \
    --arms flashdb-declared,sqlite,postgres
uv run python scripts/m1-ladder-score.py --root .scratch/80-external-baselines/runs/m1-10x
```

`report.json` is the single machine-readable document: `runs[0]` is the ladder, `runs[1]` the regime
run, `verdicts[]` is one row per threshold per run with its condition and arithmetic, and
`unverified[]` / `summary` are §7-§8 of this report in the same form (they are the same content, not a
second verdict).

Raw evidence committed beside this report: `runs/m1-measure/rung-<rps>/{manifest,spec,run-<arm>,
windows-<arm>}.json(l)` and `runs/m1-10x/rung-50/…` (the trace, the instances, the sqlite file and the
driver/probe logs stay out of git per `runs/.gitignore`; everything is regenerable from the seed and
`spec.json`). `report.json` is the machine-readable form of §2-§6.
