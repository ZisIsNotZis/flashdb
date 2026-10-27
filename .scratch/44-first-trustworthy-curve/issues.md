# 44 - first trustworthy per-rung capacity curve, one arm (adversarial / declared)

## Instance (NOT rebuilt this task)

`/tmp/cold44/per_region-mixed` (built 2026-10-09 by `harness/build_cold_44.py`; see its header comment)

- `data_bytes=1,643,933,528`  `cap_bytes=201,326,592`  `ratio=8.17x`
  - below the pre-registration 10x floor, so every throughput verdict here carries that caveat
- seed-inventory: `docs=10,400,000 customers=2,000,000 stocks=8,400,000 publishes=146 rotations=145 tiles=294`
- openinfo: `active_tiles=336 tile_bytes_total=1,638,589,633 rss_kb=65,868 vm_hwm_kb=80,832`
- `region_mode=per_region`
- run coldness: cgroup peak == cap (201,326,592 B) -> `cache_evicting=true`; device fsync p50/p99 `8,027`/`16,991` us; engine_open `active_tiles=336`

## Latency ceiling

`--ceiling-us 10000` (10 ms). Justification: the pre-registration's own primary metric is defined as
achieved throughput within 5% of offered with `lag p99 <= 10 ms` and no window rejected
(`.scratch/15-objective-v0/pre-registration.md`, "Metric (objective v1)"), so 10 ms is the
pre-registration's own number, not a post-hoc pick; it is NOT circular - the driver applies that
stated validity bound per window and the rung cannot be scored sustained if any window breaches it.

Sign-off-list alternative (same file, "Sign-off list" item 4): the p99 SLO from
`provisional-objective.md` sec. 2 is 2x the **reference-load** p99, which would need a
reference-load p99 measured first; not measured here. We chose the pre-registration's own absolute
bound instead.

## Exact scoring command lines

Scored run (the table below comes from this one), 2026-10-09T21:07:12+08:00, wall 87 s:

```
cd harness && source $HOME/.cargo/env
uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold44/per_region-mixed \
  --workdir /tmp/cold44/run44b \
  --reference-rate 50 --workloads adversarial --arms declared \
  --segment-requests 2000 --group-requests 64 --window-ms 1000 \
  --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --ceiling-us 10000 --ladder-ceiling-rps 400 \
  --report ../.scratch/44-first-trustworthy-curve/curve.md \
  --json ../.scratch/44-first-trustworthy-curve/curve.json
```

Driver payload the runner emitted (the inner scoring command), verbatim from `run.stdout.txt`:

```
target/release/flashdb-load /tmp/cold44/run44b/adversarial-declared /tmp/cold44/run44b/adversarial.jsonl \
  --rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json --peak-report \
  --group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region \
  --policy-file /tmp/cold44/run44b/adversarial-declared.policy.json --compact-bytes 67108864 --ceiling-us 10000
```

Offered trace: 8000 requests, `counts={'stock': 2408, 'customer': 2436, 'scan': 0, 'write': 3156}`,
rates `[50, 100, 200, 400]` = reference rate 50 x (1,2,4,8).

**One deviation from the CLI defaults, forced by an abort.** Reused verbatim with cold_0b's default
`--checkpoint-bytes/--rotate-bytes 16384`, the declared arm aborts `rc=1` six windows into the 200 rps
rung with `flashdb-load: active tile list is full for this publish: a partitioned publish emits one
tile per region and the remaining capacity is smaller` (manifest `MAX_TILES=1024`): 16 KiB
checkpoints flood the manifest with one tile per region per checkpoint. The scored run uses the
cadence the instance was built in, **8 MiB** (`build_cold_44.py` default; slices 29/31: "rotate
cadence 8 MiB so the retained suffix stays bounded"). The aborted attempt is preserved whole under
`attempt1-default-cadence-abort/` (rc=1, no `load_summary`, rungs 50/100/200 only). The earlier cold
runs' exact cadence is not recorded anywhere in the repo; cold_0b's shipped default is what I could
recover, and it does not survive on this instance.

`--ladder-ceiling-rps 400` pins the four pre-registered rungs so no ladder extension could start
(none would: no rung was sustained). No `--noise-repeat`.

## Per-rung table (adversarial / declared, ceiling 10 ms)

| offered rps | achieved rps | achieved % | p50 us | p99 us (max over all windows) | sustained | invalid / ceiling-rejected windows | reorg bytes |
|---:|---:|---:|---:|---:|:--:|---:|---:|
| 50  | 49.91  | 99.81% | 108,282 | 164,823 | no | 32 / 32 | 49,391,499 |
| 100 | 99.69  | 99.69% | 87,874  | 147,022 | no | 15 / 15 | 145,044,097 |
| 200 | 185.18 | 92.59% | 89,330  | 126,494 | no | 8 / 8   | 81,298,296 |
| 400 | 400.00 | 100.00% | 100,498 | 304,597 | no | 4 / 4   | 23,656,768 |

Whole run: `achieved_rps=106.0` vs offered mean 106.67, `completed=8000`, `errors=0`,
`commit_rate=1.0`, `saturated_at_rps=100`, `late_total=3616`, `ceilings` above. Reorg (whole run)
`299,390,660 B` = summary `compact_bytes_total` = summed per-region `compaction_bytes`, 63 merges.

- `p99 us` is the honest rung figure: max window p99 over **all** windows of the rung. The generated
  report's `p99 us` column takes the max over **valid** windows only and reads `0` for 50 rps because
  the only valid windows there are four empty boundary windows (`requests=0`, `p99_us=0`); that `0`
  is not a measured rung p99.
- `reorg bytes` is the per-rung driver `compact_bytes`. The report prints the whole-run total on
  every row.
- Rung 200 is also below the 95% sustained band on throughput alone (wall 10.8 s > its 10 s offered
  span); rung 400's achieved equals offered exactly because the offered span is a denominator floor
  and its wall time (4.45 s) was shorter - it is a bound, not headroom.

## Instrument invariants (observed on the real output)

- (a) attribution: sum over windows of `completed_delta` = **8000** == `load_summary completed` =
  **8000** (`requests=8000`, `errors=0`). Per-rung `completed_delta` sums are 2000/2000/2000/2000.
  No unattributed completions.
- (b) no rung above offered: `rate_exceeded=false` on all four rungs; max `achieved/offered` =
  **1.0000** (rung 400, exactly the offered-span floor) against the 2% tolerance.
- (c) rejections: `ceiling_rejected_windows` per rung = **32 / 15 / 8 / 4 = 59** (measured non-zero),
  `invalid_windows` = **59**, total windows = **63**, `error_windows` = 0. All 59 windows with
  `p99_us > 10,000` carry `ceiling_breached=true` (59 with the flag = 59 over the ceiling). The
  pacer's lag rule also tripped (`saturated_at_rps=100`) but added no invalid window beyond the
  ceiling.

Recompute the table and the invariants from the preserved JSON (no re-run):

```python
import collections, json
d = json.load(open(".scratch/44-first-trustworthy-curve/curve.json"))
assert d["ceiling_us"] == 10000
rec = d["arms"]["adversarial/declared"]; s, w = rec["summary"], rec["windows"]
by = collections.defaultdict(list)
for x in w: by[x["rate_rps"]].append(x)
assert sum(x["completed_delta"] for x in w) == s["completed"] == 8000          # (a)
for row in rec["curve"]:
    ws = by[row["offered_rps"]]
    assert row["achieved_rps"] <= row["offered_rps"] * 1.02 and not row["rate_exceeded"]  # (b)
    print(row["offered_rps"], row["achieved_rps"], max(x["p99_us"] for x in ws),
          row["invalid_windows"], row["ceiling_rejected_windows"], row["compact_bytes"])
assert sum(r["ceiling_rejected_windows"] for r in rec["curve"]) == s["ceiling_rejected_windows"] == 59  # (c)
```

## Raw output (preserved, committed)

- `.scratch/44-first-trustworthy-curve/curve.json` - `cold_0b --json`. Carries per-arm `windows`
  (per-window `requests`, `completed_delta`, `p50_us`, `p99_us`, `invalid`, `ceiling_breached`,
  `regions`) plus `curve` and `summary`. The table and all three invariants recompute from it.
- `.scratch/44-first-trustworthy-curve/curve.md` - the generated report (states the ceiling and the
  59 rejected windows).
- `.scratch/44-first-trustworthy-curve/run.stdout.txt` - runner stdout: the emitted driver payload,
  the per-arm summary line, the report. (The live file is `run.log`, kept locally; `*.log` is
  gitignored, so the committed copy is the `.txt`.)
- `.scratch/44-first-trustworthy-curve/attempt1-default-cadence-abort/` - the 16 KiB-cadence attempt
  (`curve.json`, `curve.md`, `run.stdout.txt`, `adversarial-declared.stderr`).

Not preserved: the driver's own JSONL stdout. `cold_0b` consumes it in-process; the per-window
records it derives are in `curve.json` (it does not re-emit the raw lines to disk).

## Residual risk

- The table is a valid measurement but a stark one: with the 10 ms ceiling the declared arm sustains
  **no** offered level in 50-400 rps (59 of 63 windows rejected; rung p99 126-305 ms). This is a
  latency-constraint failure, not a throughput failure (achieved 93-100% of offered, 0 errors).
- 8.17x data:cap, below the 10x pre-registration floor.
- One seed, one arm, one workload; no noise band (`--noise-repeat` not run), so any run-to-run
  difference is unquantified.
- The cadence deviation (8 MiB vs the 16 KiB CLI default) changes the reorganisation volume; the
  299 MB is a property of the 8 MiB regime, not of the shipped default.
- Window counts differ from published runs (slice 42's rung-boundary splitting; 36 windows for the
  40 s rung); the achieved estimator uses summed wall time, not window count, so the table is
  unaffected.

## What T1/T2/T4 still need

- **T1** (learned beats declared >15% on `win`): the `win` workload is not in this run at all. Needs
  `--workloads win --arms declared learned static-derived oracle` (lambdas 0.1/1.0/10.0), plus the
  noise band (`--noise-repeat`) and the 10-seed median/IQR.
- **T2** (learned loses <=5% on adversarial-uniform): needs the `learned` arm on this same
  adversarial workload (`--arms learned`, lambdas 0.1/1.0/10.0) to compare against this run's
  declared numbers; `static-derived`/`oracle` are also needed for the ladder guard.
- **T3** (learned reorg <=3x declared): needs adversarial/learned@lambda reorg against this run's
  declared **299,390,660 B** (and the `win` pair, which is the pre-registered workload).
- **T4** (drift recovery <=2 windows): needs `--workloads drift --arms learned declared static-derived`
  with a resolved `--drift-rate` (slice 39 `auto` pre-flight).
- Before any threshold verdict: the primary metric ("highest offered level sustained") needs both
  compared arms at all four rungs under the same ceiling, and the whole matrix needs the >=10x
  data:cap instance and 10 seeds.

## What I could not do

- Recover the earlier cold runs' exact cadence: not recorded in `.scratch/36|37|38|39` or in
  `cold_0b`'s CLI; cold_0b's default 16 KiB aborts on this instance, so the instance build cadence
  (8 MiB) was used and the abort is preserved as evidence.
- Preserve the driver's raw JSONL stdout (see "Raw output").
- Any arm other than `declared`, any workload other than `adversarial`, any noise band, any second
  seed.
