# 77 — WP-W: close the T4 evaluability gap with a non-scan drift shape

## Contract

Work package W of the program. Single writer in `/home/z/vibe/worktrees/flashdb/wave21-driftshape`
(isolated worktree, branch `wave21/driftshape`, based on integrated master `8a4ea20`). Do not cd
to `/home/z/vibe/flashdb`. One flashdb process at a time under `MemoryMax=128MiB`;
`flock /tmp/wave21-measure.lock -c '<full command>'` serialises every arm-run on the shared box;
box load recorded at every boundary. Do NOT edit `.scratch/15-objective-v0/pre-registration.md`,
and do NOT change any pre-registered threshold (the 2-window T4 target, the 10% band, or any
ceiling).

T4 (pre-registered, verbatim): *A drift claim is supported only if the learner recovers to within
10% of the pre-drift throughput within 2 windows while `flashdb-static-derived` does not (or
recovers measurably slower).*

Ticket 73 proved T4 UNDERIVABLE with the only drift shape the harness exposed: the post-drift mix
is `scan` weight x10, which cuts the write share 40% -> ~11% (the fsync-bound bottleneck) and
leaves an `order_no` index already demanded by the win mix's 30% scans, so post-drift capacity >=
pre-drift capacity and no recovery event occurs. The pre-registration also names the other shapes
— "a shape's weight multiplied, or one key's popularity spiked" — so the gap is an instrument gap,
not a verdict.

## Objective

Add a configurable drift SHAPE to `harness/flashdb_harness/cold_0b.py` (at least a write-weighted
drift; the key-popularity spike is a secondary candidate), keep the existing scan-drift behaviour
selectable, re-test T4, and report the honest verdict. A capacity-reducing drift shape must exist
and be selectable; T4 is either evaluated (recovery windows for learned vs static-derived, with
the throughput series and seed count) or honestly proven still-underivable with evidence.

## Measurement contract (verbatim, inherited from the dispatch)

- `cp -a` copy of `/tmp/cold51/per_region-mixed` (12.25x) into a fresh workdir; build once; copy
  the release binaries; run only the copy; one process at a time.
- First validate evaluability: find a drift rate below pre-drift capacity for the new shape; if
  none exists, prove it.
- Then run learned + static-derived (+ declared if cheap), >= 3 seeds + a repeat.
- Every measurement command wrapped in `flock /tmp/wave21-measure.lock -c '<full command>'`;
  record which runs were lock-protected and the box load.
- Judging: SS7.1 lag-only primary; report both lag-only and both-ceilings.
- Gates, verbatim: `cargo build --workspace --offline` (0 warnings);
  `cargo test --workspace --offline` (totals); `cd harness && uv run --with pytest pytest -q`;
  `git diff --check` (silent).

## Design (checkpoint before implementation)

The drift-plan builder already generalises to any WIN_MIX key:

```
drifted = dict(WIN_MIX)
drifted[drift_shape] = drifted[drift_shape] * drift_factor
```

so a write-spike is reachable by exposing the existing `drift_shape`/`drift_factor` knobs on the
CLI instead of adding a parallel trace path. The harness currently hard-codes the defaults in the
`main()` call site (`plan_workload(wl, ...)` passes only `drift_rate`), so the only gap is two
arguments.

Change 1 (minimum, write-spike):
- `--drift-shape {scan,write}` default `scan` (preserves ticket 73's behaviour exactly).
- `--drift-factor INT` default `10` (so `scan x10` remains the default and `write x10` is
  selectable; the factor is a stated shape knob, not a threshold).
- Thread both into `plan_workload`.

For `--drift-shape write`, the post-drift mix becomes `{stock:20, customer:10, scan:30,
write:400}` (total weight 460): write share 40% -> ~87%. Mechanism expectation: each `order_flow`
write carries a cold `Customer` email find plus a cold `Stock` patch read plus the WAL puts, so
raising the write share raises the cold-read+fsync work per offered request — the opposite
direction from the scan drift. This is the capacity-reducing hypothesis to test, not an assumed
result.

Change 2 (discovered during evaluability probing, recorded before the T4 run): the drift plan
hard-codes `pre=2` pre-drift segments (4,000 requests). Ticket 73 residual (c) already flags that
this is shorter than the learned arm's demand-gated index burn-in at >= 800 rps, so learned's
pre-drift baseline is polluted at exactly the rates where a capacity-reducing drift saturates.
To re-test T4 honestly the pre-drift segment count must be configurable:
- `--drift-pre-segments INT` default `2` (preserves the ticket-73 scan-drift trace exactly);
  the post-drift stays at 4 segments, so the default total stays 6 segments as before.
- Thread it into `plan_workload`. This is a harness trace-length knob, not a threshold; the
  2-window target, the 10% band, and every ceiling are untouched.

Key-popularity spike (stretch, only if cheap): the generator already has `set_key_spike` (a
flash-sale hot-SKU knob) but `build_trace` never exposes it, and a hot-key drift is not obviously
capacity-reducing on this schema (a hot key warms up, which removes cold reads). It is therefore
NOT the primary shape; if it is added it must be a separate, explicit shape and must be measured,
not assumed.

The pre-registration and all thresholds stay untouched. The T4 judging remains on throughput
(per-window pre-drift vs post-drift achieved rps), with the learner's internal
`recovery_windows` reported separately exactly as ticket 73 did.

## Change log

- `efddc42` `77: WP-W contract + design checkpoint (write-spike drift shape)` — ticket
  contract/design checkpoint, no code.
- `e115f8c` `77: add write-spike drift shape + configurable pre-drift segments` — the
  `--drift-shape {scan,write}`, `--drift-factor`, and `--drift-pre-segments` knobs plus tests.

## Measurement protocol (exact commands)

Build once from HEAD, copy the binaries, verify the byte hash, and run only that copy:

```
source $HOME/.cargo/env
cargo build --offline --release --bin flashdb-load --bin flashdb-scale --bin flashdb-replay
mkdir -p /tmp/wave21-driftshape-bin
cp target/release/flashdb-load /tmp/wave21-driftshape-bin/
cp target/release/flashdb-scale /tmp/wave21-driftshape-bin/
cp target/release/flashdb-replay /tmp/wave21-driftshape-bin/
sha256sum target/release/flashdb-load /tmp/wave21-driftshape-bin/flashdb-load
# 47c0e5a333881b97dde44c0ce1403ec9bd0f2ddf4f1195a232d097aaf58fabbe  (byte-identical to WP-S HEAD)
cp -a /tmp/cold51/per_region-mixed /tmp/wave21-driftshape/per_region-mixed   # 1,643,925,811 B, 12.25x
```

The harness `build_binaries` still rebuilds `target/release/flashdb-load` (a no-op; no Rust
changed) and runs that path, which is byte-identical to the pinned copy (sha256 above re-checked
at the end). Per arm the harness makes its own `cp -a` of the base instance under `MemoryMax`.

Per run (one harness invocation runs the arms sequentially, one process at a time, under the
harness's `systemd-run --user --scope -p MemoryMax=134217728 -p MemorySwapMax=0`):

```
cd harness && flock /tmp/wave21-measure.lock -c 'uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/wave21-driftshape/per_region-mixed --workdir /tmp/wave21-driftshape/<tag> \
  --report /home/z/vibe/worktrees/flashdb/wave21-driftshape/.scratch/77-drift-shape/<tag>.md \
  --json   /home/z/vibe/worktrees/flashdb/wave21-driftshape/.scratch/77-drift-shape/<tag>.json \
  --cap-mib 128 --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --compact-bytes 67108864 --ceiling-us 10000 --lag-ceiling-us 10000 \
  --lambdas 1.0 --workloads drift --arms learned static-derived \
  --drift-shape write --drift-factor 10 --drift-pre-segments 6 --drift-rate <R> --seed <seed>'
```

`segment_requests 2000`; `--window-ms 1000`. Every measurement command was lock-protected with
`flock /tmp/wave21-measure.lock`; `--drift-rate` is always explicit (never `auto`). declared was
NOT run: it saturates ~85 rps on the win mix (ticket 73) and is not a T4 subject; its end-of-run
drain would be expensive. The learner ran without the declared-derived reorg budget (declared
excluded); `--learner-cooldown-windows 3` stayed active, reorg stayed 0 B.

Box load recorded at every boundary (1-minute loadavg): **4.48-8.85** across the measurement
window 14:41-15:17+08:00. All four T4 runs and all three evaluability probes were lock-protected.

## Evaluability proof — a capacity-reducing drift shape now exists and is selectable

Post-drift (write x10) vs pre-drift (win) achieved throughput, static-derived (the clean arm,
index adopted at open; no burn-in). `--drift-shape write`, default `pre=2`:

| run | offered rps | pre-drift achieved | post-drift achieved | drop? |
|---|---:|---:|---:|:--:|
| probe600 | 600 | 598.3 rps (6 w) | 601.0 rps (13 w) | no (sustains) |
| probe1000 | 1000 | 904.3 rps (4 w, partial) | 1000.5 rps (7 w) | no (sustains) |
| probe1500 | 1500 | 1460.3 rps (2 w) | 1259.7 rps (6 w) | **yes** |

Pre-drift (win) capacity is >= ~1460 rps (ticket 73 measured static 1485 rps at 1500 offered;
probe1500 re-measures 1460 rps at 1500 offered, 2 clean windows). Post-drift (write x10) capacity
is ~1260 rps: it sustains 600 and 1000, and saturates to 1259.7 rps at 1500 offered. Therefore a
rate **R=1450** satisfies `post-capacity (~1260) < R (1450) < pre-capacity (~1460-1485)`, so the
recovery event T4 needs now exists. This is the opposite of ticket 73's scan-spike (which removed
load); the write-spike adds load (write share 40% -> ~87%) and cuts capacity ~13%.

## T4 measurement (R=1450, write x10, pre=6 segments => drift boundary at request 12,000)

`pre=6` gives the learned arm 6 pre-drift segments (12,000 requests, ~8-9 windows) so its
demand-gated index burn-in (windows 0-2) completes before the drift — ticket 73 residual (c).
Four runs: seeds 42, 43, 44 + a seed-42 repeat. learned@1.0 + static-derived per run.

Per-window throughput around the boundary (rps = `completed_delta / window_s`; the straddle
window that contains the 12,000 boundary is excluded; learned's windows 0-2 are burn-in):

### seed 42

| arm | pre (w3-w8) | straddle w9 | post (w10-w15) |
|---|---:|---:|---|
| learned@1.0 | 1812.5, 1861.3, 1916.5, 1834.4, 1319.6, 1865.3 | 1538.3 | 1218.5, 1328.8, 1384.0, 1182.4, 982.1, 1233.1 |
| static-derived | 1418.3, 1430.8, 1471.1, 1428.5, 1481.3, 1069.4, 1752.3, 1488.2 (w0-w7) | 1302.0 | 1213.4, 1293.3, 1364.0, 1181.8, 942.1, 1448.1 |

### seed 43

| arm | pre (w3-w8) | straddle w9 | post (w10-w16) |
|---|---:|---:|---|
| learned@1.0 | 1925.7, 1555.4, 1623.6, 1898.2, 1462.0, 1763.4 | 1573.5 | 1273.7, 1328.3, 1189.7, 1207.4, 1277.0, 939.2, 1632.1 |
| static-derived | 1298.9, 1546.9, 1456.4, 1280.3, 1492.1, 1215.4, 1832.2, 1463.1 (w0-w7) | 1227.4 | 1314.6, 1213.3, 1048.0, 1328.7, 783.4, 1234.6 |

### seed 44

| arm | pre (w3-w8) | straddle w9 | post (w10-w16) |
|---|---:|---:|---|
| learned@1.0 | 1687.1, 1885.4, 1787.3, 1681.8, 1444.9, 1804.9 | 1593.2 | 1186.8, 1317.7, 1276.0, 1276.5, 1227.8, 895.0, 1526.7 |
| static-derived | 1403.6, 1445.5, 1434.1, 1429.7, 1485.8, 1059.6, 1806.8, 1485.4 (w0-w7) | 1275.6 | 1308.5, 1322.8, 1319.6, 1330.6, 694.5, 1279.0 |

### seed 42 repeat

| arm | pre (w3-w8) | straddle w9 | post (w10-w15) |
|---|---:|---:|---|
| learned@1.0 | 1807.4, 2001.6, 1762.8, 1257.6, 1300.7, 1686.5 | 1727.2 | 1425.2, 1383.8, 1345.2, 1380.1, 1056.2, 1273.0 |
| static-derived | 1403.9, 1461.2, 1381.3, 1528.4, 1452.0, 730.0, 1929.5, 1736.9 (w0-w7) | 1341.9 | 1349.4, 1423.3, 1265.0, 1369.1, 928.3, 1627.5 |

Steady pre-drift vs steady post-drift (pre = windows 3+ of the pre segment; post = post windows
minus the last two run-tail windows; learned's pre is a catch-up backlog sum and is NOT a clean
throughput number — its clean pre-drift is the offered 1450 rps it sustains after burn-in):

| run | arm | pre steady rps | post steady rps | post / offered |
|---|---|---:|---:|---:|
| s42 | static-derived | 1444.1 | 1263.6 | 87.1% |
| s43 | static-derived | 1455.0 | 1225.2 | 84.5% |
| s44 | static-derived | 1452.4 | 1320.3 | 91.1% |
| s42-rpt | static-derived | 1460.2 | 1352.1 | 93.2% |
| s42 | learned@1.0 | (1450 offered) | 1276.4 | 88.0% |
| s43 | learned@1.0 | (1450 offered) | 1254.6 | 86.5% |
| s44 | learned@1.0 | (1450 offered) | 1256.6 | 86.7% |
| s42-rpt | learned@1.0 | (1450 offered) | 1383.5 | 95.4% |

Static-derived pre steady median 1453.7 rps -> post steady median 1292.0 rps = **-11.1%**.
learned post steady median 1266.5 rps vs the offered 1450 = **-12.7%**. The pre->post drop is
real (outside the ~8% same-seed repeat noise below), while the learned-vs-static post gap
(1266.5 vs 1292.0 = -2.0%) is inside the noise band.

## T4 verdict (seed count + noise band)

**Seed count:** 3 independent seeds (42, 43, 44) + one seed-42 repeat = 4 arm-runs per arm
(learned@1.0 + static-derived), 8 arm-runs total at R=1450, plus 3 evaluability probes
(600/1000/1500) = 6 more arm-runs. Pilot-grade, NOT the pre-registered 10-seed matrix.

**Noise band:** the seed-42 vs seed-42-repeat post-drift steady throughput max relative diff is
8.1% (learned) and 6.8% (static); call the same-seed repeat noise band **~8%**. (WP-S's 2.48%
band was on the win ladder declared repeat; this is the same-seed repeat on the write-spike
post-drift quantity, the quantity T4 actually uses.)

**Honest T4 verdict: T4 is now EVALUABLE (a real drop exists) and is MEASURED NOT SUPPORTED — the
learner does not recover, and static-derived does not recover measurably slower.** The write-spike
capacity floor (~1260-1290 rps) is policy-independent: the learner never triggers drift
re-exploration (`recovery_windows = {}` in all 8 arm-runs) and keeps `index_order_no=True` for
P|StockMovement throughout, and its post-drift steady throughput (median 1266.5 rps) is
statistically indistinguishable from static-derived's (median 1292.0 rps, -2.0% inside the ~8%
noise). Neither arm returns to within 10% of the 1450 pre-drift throughput in a sustained way
(the 10% band [1305, 1595] straddles the noisy post floor), and there is no differential
recovery to time. The learner's policy knobs (page size / merge threshold / order_no index) do
not reduce the write-heavy mix's inherent cost, so the capacity-reducing drift does not
manufacture a learner advantage. This is a measured null, NOT the ticket-73 underivability (the
drop now exists), and NOT a PASS.

## Both judging views

- **SS7.1 lag-only (primary):** judged on throughput, the result stands — both arms drop ~11-13%
  at the drift and neither recovers; learned-vs-static post gap ~2% (noise). Lag-rejected windows
  are scattered shared-box/device noise and do not affect the throughput reading (achieved
  fractions below).
- **Both ceilings (harness / pre-registration):** `sustained=false` for every arm (the service
  ceiling rejects ~15-17 of 16-17 windows on every arm — the per-group `fdatasync` device floor
  vs a fixed 10 ms, the open SS7.1 reconciliation). The throughput T4 reading is unchanged; the
  open primary-vs-service reconciliation is not edited.

Whole-run achieved fraction (R=1450): learned@1.0 0.758-0.776 (burn-in drags the average),
static-derived 0.903-0.948. `commit_rate = 1.0` and `error_windows = 0` on every arm; reorg
0 B everywhere; coldness `cache_evicting=true` (cgroup peak = cap) on every arm.

## Residual

(a) The learned pre-drift throughput at 1450 rps is a catch-up backlog sum (windows 0-2 are its
demand-gated index burn-in, then windows 3+ drain the backlog above the offered rate), so the
10% band is judged against the OFFERED 1450 rps it sustains after burn-in, not against the
inflated per-window sum — stated, not hidden. (b) The drift-workload static-derived is still
derived over the FULL pre+post run (the harness `max_rate` filter cannot split the two segments at
one fixed rate; ticket 73 residual b), so it is not a strictly pre-drift-frozen control — this
does not change the null (learned also shows no recovery) but is stated. (c) The write-spike
post floor sits close to the 10% band boundary (1260-1290 vs band [1305,1595]), so the "within 2
windows" call is noise-straddled at 1450; the robust signal is the empty `recovery_windows` and
the learned≈static post gap, not a per-window pop above 1305. (d) 3 seeds + 1 repeat, not 10
seeds; shared box loadavg 4.48-8.85; pilot-grade. (e) The key-popularity spike (the second shape
the pre-registration names) was not added: the generator's `set_key_spike` exists but is not
wired into `build_trace`, and a hot-key drift is not obviously capacity-reducing here (it warms
a key); it remains a separate follow-up, not part of this package.

## Gates (verbatim, run after the measurement)

1. `cargo build --workspace --offline` — 0 warnings:
```
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.03s
```
2. `cargo test --workspace --offline` — totals: **304 passed; 0 failed; 0 ignored**
(129 lib + 22 flashdb-load + 9 batch_commit + 5 compaction_job + 4 constraints +
3 family_colocation + 12 group_commit + 6 manifest_engine + 8 merge_threshold + 15 metrics +
33 multi_tile + 7 order_flow + 5 order_no_index + 1 partitioned_publish_cap + 5 read_requests +
3 region_compaction + 6 region_partition + 3 replay_trace + 8 request_commit + 2 scale_bin +
3 tile_format + 3 tile_layout + 12 tile_read + 0 flashdb-replay + 0 flashdb-scale +
0 nvme_bench + 0 doc-tests).
3. `cd harness && uv run --with pytest pytest -q`:
```
warning: `VIRTUAL_ENV=/home/z/.venv` does not match the project environment path `.venv` and will be ignored; use `--active` to target the active environment instead
142 passed in 3.98s
```
4. `git diff --check` — silent (exit 0).
