# 47 - latency-trusted per-rung curve, one arm (adversarial / declared)

## Why this run exists

The slice-44 curve (`a1b809f`, ticket `.scratch/44-first-trustworthy-curve/issues.md`) applied its
10 ms ceiling to a **group-inflated** latency: the old pacer took a whole `--group-requests` group as
soon as its FIRST request was due, ran ahead of the offered schedule, and assigned one group-elapsed
sample to every member. Its per-request p50/p99 was therefore the GROUP's time, and its "no rung
sustained" verdict was not yet valid.

Commit `965adf4` ("45: measure per-request latency from arrivals, name each ceiling", the commit
immediately preceding this one) replaced that pacer with an arrivals model: a group forms only from
requests ALREADY ready, dispatched as soon as one is due; each request is measured **from its own due
time**; service latency and schedule lag are separate quantities with a separately named ceiling
each. This ticket is the SAME scored run under that fixed driver. It measures; it changes no code.

## Instance (a COPY; not rebuilt, source untouched and not deleted)

- Source (unchanged): `/tmp/cold44/per_region-mixed` (built 2026-10-09 by `harness/build_cold_44.py`).
- Copy used: `/tmp/cold47/per_region-mixed` - `cp -a`, byte-identical: 338 files,
  `1643933528` bytes.
- `data_bytes=1,643,933,528`  `cap_bytes=201,326,592`  `ratio=8.17x` (below the pre-registration 10x
  floor -> `ratio_below_floor=true`; every ABSOLUTE throughput verdict carries that caveat).
- run coldness: cgroup peak == cap (201,326,592 B) -> `cache_evicting=true`; device fsync p50/p99
  `7,943`/`29,737` us; engine_open `active_tiles=336`; region_mode `per_region`.

## Ceilings - which value and why

- `--lag-ceiling-us 10000` - the pre-registration's OWN number: its primary metric is
  "achieved throughput within 5% of offered, `lag p99 <= 10 ms`, no window rejected"
  (`.scratch/15-objective-v0/pre-registration.md`, "Metric (objective v1)"). Not a post-hoc pick.
- `--ceiling-us 10000` - the SERVICE-latency ceiling, set separately and to the same value. Why:
  the pre-registration names exactly ONE absolute latency number (the 10 ms lag bound); its
  sign-off-list item 4 offers a relative alternative (2x a reference-load p99) that needs a
  reference-load p99 not measured here. Applying the single pre-registered absolute bound to the
  per-request SERVICE p99 keeps the guard pre-registered rather than post-hoc, and it is the exact
  value slice 44 passed to `--ceiling-us` - now correctly bounding the PER-REQUEST service quantity
  instead of the group-inflated one. The two ceilings are separate flags, separate counters, and
  separate `reject_reason` tags, so the lag rule can be read on its own.

## Exact command lines

Copy (the run mutates its target; the source is read-only to this task):

```
time cp -a /tmp/cold44/per_region-mixed /tmp/cold47/per_region-mixed
```

Scored run (raw stdout preserved as `run.stdout.txt`):

```
cd /home/z/vibe/flashdb/harness && source $HOME/.cargo/env
uv run python -m flashdb_harness.cold_0b --release \
  --instance /tmp/cold47/per_region-mixed \
  --workdir /tmp/cold47/run47 \
  --reference-rate 50 --workloads adversarial --arms declared \
  --segment-requests 2000 --group-requests 64 --window-ms 1000 \
  --checkpoint-bytes 8388608 --rotate-bytes 8388608 \
  --ceiling-us 10000 --lag-ceiling-us 10000 --ladder-ceiling-rps 400 \
  --report /home/z/vibe/flashdb/.scratch/47-latency-trusted-curve/curve.md \
  --json /home/z/vibe/flashdb/.scratch/47-latency-trusted-curve/curve.json \
  2>&1 | tee /home/z/vibe/flashdb/.scratch/47-latency-trusted-curve/run.stdout.txt
```

Driver payload the runner emitted (inner scoring command), verbatim from `run.stdout.txt`:

```
target/release/flashdb-load /tmp/cold47/run47/adversarial-declared /tmp/cold47/run47/adversarial.jsonl \
  --rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json --peak-report \
  --group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region \
  --policy-file /tmp/cold47/run47/adversarial-declared.policy.json --compact-bytes 67108864 \
  --ceiling-us 10000 --lag-ceiling-us 10000
```

Offered trace: 8000 requests, `counts={'stock': 2408, 'customer': 2436, 'scan': 0, 'write': 3156}`,
rates `[50, 100, 200, 400]` = reference 50 x (1,2,4,8). Cadence 8 MiB (the instance's build cadence;
`cold_0b`'s 16 KiB default aborts on this instance with the tile-cap error, see `.scratch/46`).
`--ladder-ceiling-rps 400` pins the four rungs; no `--noise-repeat`.

## Per-rung table (adversarial / declared, service ceiling 10,000 us, lag ceiling 10,000 us)

Distribution columns are PER REQUEST: the driver percentiles each window's per-request samples; the
rung `p50` is the **median of the per-window p50s**, the rung `p95`/`p99` are the **max over the
rung's windows** of the per-window p95/p99 (all 64 windows were rejected, so p99 takes the max over
ALL windows, not only valid ones). `svc` = completion minus the request's own due time; `lag` =
issue minus its own due.

| offered rps | achieved rps | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | sustained (both ceilings) | windows rejected by SERVICE ceiling | windows rejected by LAG ceiling |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|
| 50  | 49.70  | 99.41% | 5,930  | 910,504 | 990,504 | 62 | 834,223 | 914,223 | no | 36 | 35 |
| 100 | 98.95  | 98.95% | 6,153  | 549,290 | 609,290 | 76 | 517,769 | 577,769 | no | 16 | 16 |
| 200 | 200.00 | 100.00%| 9,157  | 462,085 | 522,085 | 2,689 | 421,132 | 481,132 | no | 8  | 8  |
| 400 | 400.00 | 100.00%| 19,638 | 412,788 | 445,288 | 7,992 | 374,784 | 407,284 | no | 4  | 4  |

Whole run: `achieved_rps=106.0` vs offered mean 106.67, `completed=8000`, `errors=0`,
`commit_rate=1.0`, `saturated_at_rps=50`, `late_total=2535`, `early_dispatches=0`,
`service_ceiling_rejected_windows=64`, `lag_ceiling_rejected_windows=63`,
`ceiling_rejected_windows=64` (union). Reorg (whole run) `317,915,511 B` = `compact_bytes_total`,
64 merges. Rung reorg (driver `compact_bytes`): `49,391,499 / 155,206,384 / 81,298,296 / 32,019,332`.

**No rung is sustained under the lag ceiling** (35/16/8/4 windows breach it), and no rung is
sustained under both ceilings (64/64 windows breach the service ceiling). `sustained=no` on all four
rows. The only window that did NOT breach lag is rung 50's window 0 (service-only; lag p99 2,912 us),
so even a lag-only rule rejects every other window of that rung.

The failure is a LATENCY-TAIL failure, not throughput: achieved is 99-100% of offered, 0 errors,
`achieved/offered` max 1.0000. The per-request p50 is 5.9-19.6 ms (the group-inflation on the central
value is confirmed: slice 44's "p50" was 88-108 ms); the p99 is 0.44-0.99 s because the arrivals
pacer now lets requests queue behind genuine stalls (compaction/checkpoint fsync at the 8 MiB
cadence; device fsync p99 ~30 ms) instead of running ahead of the schedule.

## Instrument invariants (observed values, recomputed from `curve.json`)

- (a) attribution: sum over windows of `completed_delta` = **8000** == `load_summary completed` =
  **8000** (`requests=8000`, `errors=0`). Per-rung `completed_delta` sums = **2000/2000/2000/2000**.
  Sum of `curve.windows` = 64 == summary `windows` = 64. No unattributed completions.
- (b) no rung above offered: `rate_exceeded=false` on all four rungs; max `achieved/offered` =
  **1.0000** (rungs 200 and 400, the offered-span floor) against the 2% tolerance.
- (c) each rejection carries its reason: `invalid` windows = **64** = windows with a non-null
  `reject_reason` (0 mismatches). Flag <-> reason tag mismatches = **0**: every
  `service_ceiling_breached` window has `service_ceiling` in its reason and every
  `lag_ceiling_breached` window has `lag_ceiling`. Reason histogram:
  `{'service_ceiling': 1, 'service_ceiling+lag_ceiling': 63}`. Per-rung service/lag/union counts
  `36/16/8/4`, `35/16/8/4`, `36/16/8/4` sum to summary `64 / 63 / 64`. `error_windows=0`.
  `early_dispatches=0` (per window and total): no request issued before its due time.

Recompute the table and invariants from the preserved JSON (no re-run):

```python
import collections, json
d = json.load(open(".scratch/47-latency-trusted-curve/curve.json"))
assert d["ceiling_us"] == 10000 and d["lag_ceiling_us"] == 10000
rec = d["arms"]["adversarial/declared"]; s, w = rec["summary"], rec["windows"]
by = collections.defaultdict(list)
for x in w: by[x["rate_rps"]].append(x)
assert sum(x["completed_delta"] for x in w) == s["completed"] == 8000              # (a)
for row in rec["curve"]:
    assert row["achieved_rps"] <= row["offered_rps"] * 1.02 and not row["rate_exceeded"]  # (b)
    print(row["offered_rps"], row["achieved_rps"], row["p50_us"], row["p95_us"], row["p99_us"],
          row["lag_p50_us"], row["lag_p95_us"], row["lag_p99_us"], row["sustained"],
          row["service_ceiling_rejected_windows"], row["lag_ceiling_rejected_windows"])
assert sum(r["service_ceiling_rejected_windows"] for r in rec["curve"]) == s["service_ceiling_rejected_windows"] == 64  # (c)
assert sum(r["lag_ceiling_rejected_windows"] for r in rec["curve"]) == s["lag_ceiling_rejected_windows"] == 63
assert all((x["reject_reason"] is not None) == x["invalid"] for x in w)
```

## Raw output (preserved, committed)

- `.scratch/47-latency-trusted-curve/curve.json` - `cold_0b --json`: per-arm `windows`
  (`service_p50/p95/p99_us`, `lag_p50/p95/p99_us`, `group_elapsed_us`, `early_dispatches`,
  `reject_reason`, `service_ceiling_breached`, `lag_ceiling_breached`, `regions`), `curve`,
  `summary`.
- `.scratch/47-latency-trusted-curve/curve.md` - the generated report (states both ceilings and the
  64 service / 63 lag rejected windows).
- `.scratch/47-latency-trusted-curve/run.stdout.txt` - runner stdout: the emitted driver payload,
  the per-arm summary line, the report. (`run.log` is gitignored; this is the committed copy.)

Not preserved: the driver's own JSONL stdout (`cold_0b` consumes it in-process; the derived
per-window records are in `curve.json`).

## Gates (verbatim)

`cd harness && uv run --with pytest pytest -q`:

```
129 passed in 4.57s
```

`git diff --check`: silent (exit 0).

## Residual risk

- One arm (`declared`), one workload (`adversarial`), one seed, no noise band
  (`--noise-repeat` not run): any run-to-run difference is unquantified.
- `saturated_at_rps=50`, `late_total=2535`: the pacer's lag rule already tripped at the first rung,
  so 50 rps is itself at the latency edge; the run measures a latency-constraint failure, not a
  throughput ceiling.
- 8.17x data:cap, below the 10x pre-registration floor. Since 8.17x is WARMER (easier) than 10x, the
  latency failure here is not a regime artifact a colder instance would rescue - but it also cannot
  be quoted as a 10x-regime verdict.
- The rung p99 is a max over the rung's windows (per-window percentile), not a pooled per-request
  percentile; the per-request samples are not retained, only per-window percentiles.
- Cadence deviation (8 MiB vs `cold_0b`'s 16 KiB default) changes reorganisation volume: the
  317,915,511 B is a property of the 8 MiB regime, not of the shipped default.

## What deriving T1/T2/T4 still needs (and the 8.17x question)

- **T1** (learned beats declared >15% on `win`): needs the `win` workload
  (`--workloads win --arms declared learned static-derived oracle`, lambdas 0.1/1.0/10.0), the noise
  band (`--noise-repeat`), and the 10-seed median/IQR.
- **T2** (learned loses <=5% on adversarial-uniform): needs the `learned` arm on THIS workload
  (`--arms learned`, lambdas 0.1/1.0/10.0) to compare against this run's declared numbers;
  `static-derived`/`oracle` for the ladder guard.
- **T4** (drift recovery <=2 windows): needs `--workloads drift --arms learned declared
  static-derived` with a resolved `--drift-rate`.
- **Is 8.17x a blocker?** For T1/T2/T4 it is NOT a blocker of the comparisons themselves - all arms
  run in the same regime, so a learned-vs-declared delta is internally valid - but the pre-registered
  regime floor is 10x (`PREREG_RATIO_FLOOR=10.0`; `ratio_below_floor=true`), so no verdict may be
  quoted as a 10x-regime result until a >=10x instance is used. It IS a blocker for any ABSOLUTE
  primary-metric quote ("highest offered level sustained"): at the pre-registered regime that number
  is not measured. It does not hide the present finding - no rung sustains even at the warmer 8.17x,
  and a colder instance is strictly harder.
- Before any threshold verdict: both compared arms at all four rungs under the same two ceilings,
  the >=10x instance, and 10 seeds.
