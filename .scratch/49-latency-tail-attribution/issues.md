# 49 - attribute the latency tail (adversarial / declared, cold 8.17x instance)

MEASURE-ONLY: no engine or existing harness module changed. One new tool,
`harness/cold49_tail_attribution.py`, imports `flashdb_harness.cold_0b`, overrides only its binary
resolver so every arm runs the ONE frozen driver, and drives the slice-47 scored configuration
under flag variations. Raw artifacts under `/tmp/cold49/` (not committed).

## The question

Slice 47 (`.scratch/47-latency-trusted-curve/issues.md`) recorded service p50 5,930/6,153/9,157/
19,638 us and p99 990,504/609,290/522,085/445,288 us (rungs 50/100/200/400), all 64 windows
rejected by a 10 ms ceiling, throughput 99.4-100%, 0 errors. It asserted "compaction/checkpoint
fsync at the 8 MiB cadence" as the tail cause without separating it. This run separates:
group head-of-line, rotation/fsync, compaction, and page-cache pressure.

## Instrument (frozen)

- Instance: `/tmp/cold44/per_region-mixed` (data 1,643,933,528 B, cap 192 MiB = 201,326,592 B,
  8.17x, `region_mode=per_region`, 336 active tiles). Each config gets its own `cp -a`.
- Driver: built ONCE (`cargo build --release --offline` at `93740b6`), frozen copy
  `/tmp/cold49/flashdb-load`, `sha256=456d062866871028439c07243e7f54aebc281eb6fd4ae5cc89e61831b30ca987`;
  the harness runs only this copy (verified: every arm's payload is `/tmp/cold49/flashdb-load`).
- Trace: identical bytes across all configs (deterministic, seed 42): `adversarial.jsonl`, 3,013,195 B,
  8000 lines,  `sha256=bf0a5170...`, `{stock 2408, customer 2436, scan 0, write 3156}`, rates
  50/100/200/400 x 2000. Policy file identical across configs (`4d486dd0...`). Two ceilings applied as in slice 47: `--ceiling-us 10000 --lag-ceiling-us 10000`,
  `--ladder-ceiling-rps 400`.
- Each config run cold in `systemd-run --user --scope MemoryMax=<cap>`; cgroup peak recorded.

## Configurations (exact)

Runner (all configs; `--only`/`--root` select the config and output dir):

```
cd harness && uv run python cold49_tail_attribution.py [--only NAME] --root /tmp/cold49
```

The emitted driver payload is (frozen binary; run-47's exact scored args):

```
/tmp/cold49/flashdb-load <workdir>/adversarial-declared <workdir>/adversarial.jsonl \
  --rate-rps 50,100,200,400 --segment-requests 2000 --window-ms 1000 --metrics-json --peak-report \
  --group-requests 64 --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region \
  --policy-file <workdir>/adversarial-declared.policy.json --compact-bytes 67108864 \
  --ceiling-us 10000 --lag-ceiling-us 10000
```

| # | config | variation from the baseline payload |
|---|---|---|
| 1 | `baseline` | none (group 64, 8 MiB checkpoint/rotate, `--compact-bytes 67108864`) |
| 2 | `group1` | `--group-requests 1` (one request per group) |
| 3 | `nocompact` | compaction off. There is NO `--no-compact` flag and the driver REJECTS `--compact-bytes 0` ("--compact-bytes must be a positive integer", `flashdb-load.rs:222-224`); disabling is documented as omitting the flag ("Off by default", `flashdb-load.rs:69`). The `cold_0b` runner maps `--compact-bytes 0` to omitting the driver flag (`cold_0b.py:846`), used here. |
| 4 | `bigcadence` | `--checkpoint-bytes 1073741824 --rotate-bytes 1073741824` (1 GiB). Large enough: the instance opens with a retained WAL of 4,922,007 B and the recorded run WAL ingest is 979,080 B (5,901,087 B total, ~182x below 1 GiB), so `maybe_checkpoint`/`rotate_wal` (both gate on `wal.valid_len() > N`, `engine.rs:2022,1230`) cannot fire. Confirmed: WAL segment unchanged (`data-31376.wal` 4,922,007 -> 8,422,738 B, no rename). |
| 5 | `bigcap` | `--cap-mib 384` (MemoryMax 384 MiB; data:cap 8.17x -> 4.08x) |
| 6 | `neither` | separation experiment, not one of the five requested: `--compact-bytes 0` AND 1 GiB cadence (both suspected stall sources off) |

Note: the recorded 979,080 B is the per-region `bytes_written` sum, NOT actual WAL bytes. The
baseline run rotated its WAL (below), so its true WAL ingest is >= 3,466,601 B (the amount needed
past 4,922,007 B to cross 8 MiB); the 979,080 B figure undercounts WAL bytes by >= 3.5x.

## Per-rung tables (pass 1; slice-47 semantics)

`svc` = completion minus the request's own due time; `lag` = issue minus due. Rung `p50` = median
of the windows' per-window p50; rung `p95`/`p99` = MAX over the rung's windows of the per-window
p95/p99 (slice-47 convention). All units us unless noted.

### 1 baseline
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 49.78 | 99.55% | 6,424 | 874,795 | 954,795 | 65 | 782,658 | 862,658 | 44,163,084 |
| 100 | 97.66 | 97.66% | 10,841 | 827,064 | 897,064 | 2,240 | 681,335 | 751,335 | 140,110,225 |
| 200 | 199.25 | 99.62% | 252,110 | 827,072 | 902,072 | 104,024 | 741,329 | 816,329 | 71,136,009 |
| 400 | 400.00 | 100.00% | 332,296 | 756,881 | 821,434 | 241,872 | 695,218 | 765,218 | 23,655,742 |

### 2 group1
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 49.55 | 99.10% | 6,592 | 538,106 | 576,553 | 64 | 537,493 | 561,140 | 49,391,499 |
| 100 | 97.89 | 97.89% | 111,598 | 809,880 | 834,759 | 102,162 | 799,952 | 833,961 | 134,881,810 |
| 200 | 125.52 | 62.76% | 3,239,816 | 6,168,888 | 6,196,873 | 3,236,888 | 6,162,358 | 6,175,145 | 111,785,157 |
| 400 | 95.18 | 23.80% | 16,610,608 | 22,770,006 | 22,772,448 | 16,603,062 | 22,767,508 | 22,769,950 | 95,215,207 |

### 3 nocompact
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 50.00 | 100.00% | 2,467 | 256,124 | 279,954 | 62 | 219,957 | 259,957 | 0 |
| 100 | 100.00 | 100.00% | 3,066 | 213,093 | 243,093 | 64 | 170,883 | 210,883 | 0 |
| 200 | 199.74 | 99.87% | 7,538 | 48,997 | 58,926 | 1,669 | 21,513 | 29,921 | 0 |
| 400 | 390.83 | 97.71% | 60,555 | 261,175 | 291,484 | 15,558 | 211,179 | 246,179 | 0 |

### 4 bigcadence
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 49.82 | 99.64% | 3,584 | 292,998 | 332,998 | 63 | 226,559 | 263,171 | 55,281,100 |
| 100 | 98.17 | 98.17% | 7,513 | 590,711 | 660,711 | 219 | 544,424 | 614,424 | 149,316,783 |
| 200 | 200.00 | 100.00% | 252,582 | 1,323,054 | 1,418,054 | 165,105 | 1,272,465 | 1,367,465 | 60,973,722 |
| 400 | 400.00 | 100.00% | 82,985 | 564,228 | 624,228 | 53,238 | 476,733 | 532,079 | 40,649,148 |

### 5 bigcap
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 49.42 | 98.84% | 3,251 | 910,416 | 990,416 | 62 | 862,669 | 942,669 | 49,391,499 |
| 100 | 100.00 | 100.00% | 8,854 | 849,328 | 929,328 | 541 | 768,093 | 848,093 | 145,044,097 |
| 200 | 200.00 | 100.00% | 13,908 | 422,783 | 477,783 | 5,044 | 337,677 | 374,511 | 81,298,296 |
| 400 | 400.00 | 100.00% | 20,924 | 394,391 | 434,391 | 7,988 | 333,487 | 373,487 | 32,019,332 |

### 6 neither (separation)
| offered | achieved | achieved % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | compact B |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 50 | 50.00 | 100.00% | 1,930 | 359,170 | 419,170 | 60 | 332,351 | 392,351 | 0 |
| 100 | 100.00 | 100.00% | 4,491 | 273,963 | 311,063 | 64 | 223,966 | 263,966 | 0 |
| 200 | 200.00 | 100.00% | 15,360 | 74,168 | 90,152 | 4,510 | 34,769 | 45,754 | 0 |
| 400 | 400.00 | 100.00% | 19,948 | 477,763 | 507,763 | 6,664 | 412,596 | 452,596 | 0 |

## Deltas (pass 1, svc p95/p99 and lag p99 max-over-windows, us; negative = tail moved in)

| config | rung | svc p95 | svc p99 | lag p99 | achieved rps |
|---|---:|---:|---:|---:|---:|
| group1 | 50 | -336,689 | -378,242 | -301,518 | -0.23 |
| group1 | 100 | -17,184 | -62,305 | +82,626 | +0.23 |
| group1 | 200 | +5,341,816 | +5,294,801 | +5,358,816 | -73.73 |
| group1 | 400 | +22,013,125 | +21,951,014 | +22,004,732 | -304.82 |
| nocompact | 50 | -618,671 | -674,841 | -602,701 | +0.22 |
| nocompact | 100 | -613,971 | -653,971 | -540,452 | +2.34 |
| nocompact | 200 | -778,075 | -843,146 | -786,408 | +0.49 |
| nocompact | 400 | -495,706 | -529,950 | -519,039 | -9.17 |
| bigcadence | 50 | -581,797 | -621,797 | -599,487 | +0.04 |
| bigcadence | 100 | -236,353 | -236,353 | -136,911 | +0.50 |
| bigcadence | 200 | +495,982 | +515,982 | +551,136 | +0.75 |
| bigcadence | 400 | -192,653 | -197,206 | -233,139 | +0.00 |
| bigcap | 50 | +35,621 | +35,621 | +80,011 | -0.35 |
| bigcap | 100 | +22,264 | +32,264 | +96,758 | +2.34 |
| bigcap | 200 | -404,289 | -424,289 | -441,818 | +0.75 |
| bigcap | 400 | -362,490 | -387,043 | -391,731 | +0.00 |
| neither | 50 | -515,625 | -535,625 | -470,307 | +0.22 |
| neither | 100 | -553,101 | -586,001 | -487,369 | +2.34 |
| neither | 200 | -752,904 | -811,920 | -770,575 | +0.75 |
| neither | 400 | -279,118 | -313,671 | -312,622 | +0.00 |

The max-over-windows p99 is dominated by single-window outliers, so the robust statistic is the
median over each rung's windows of the per-window p95/p99:

| config | stat | rung50 | rung100 | rung200 | rung400 |
|---|---|---:|---:|---:|---:|
| baseline | svc p95 | 116,586 | 387,608 | 624,019 | 590,578 |
| group1 | svc p95 | 100,910 | 447,497 | 3,365,133 | 16,875,924 |
| nocompact | svc p95 | 14,061 | 13,660 | 24,432 | 141,402 |
| bigcadence | svc p95 | 61,167 | 357,023 | 785,511 | 437,735 |
| bigcap | svc p95 | 82,525 | 308,433 | 343,865 | 323,248 |
| neither | svc p95 | 11,839 | 16,963 | 39,511 | 41,102 |
| baseline | svc p99 | 163,919 | 447,608 | 684,019 | 624,328 |
| nocompact | svc p99 | 17,728 | 20,695 | 30,505 | 178,902 |
| neither | svc p99 | 15,988 | 27,542 | 52,083 | 52,682 |

## Whole-run summary (pass 1)

| config | achieved rps | completed | errors | invalid win | svc-rej | lag-rej | late_total | compact B | merges | fsync p50/p99 us | cgroup peak / cap B | cache_evicting | WAL before -> after |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---|
| baseline | 106.0 | 8000 | 0 | 61 | 61 | 61 | 4580 | 279,065,060 | 61 | 7574/16854 | 201,326,592 / 201,326,592 | True | data-31376.wal 4,922,007 -> data-38329.wal 30,126 |
| group1 | 82.0 | 8000 | 0 | 72 | 72 | 71 | 5708 | 391,273,673 | 72 | 7572/22192 | 201,326,592 / 201,326,592 | True | -> data-38326.wal 33,278 |
| nocompact | 106.0 | 8000 | 0 | 75 | 75 | 29 | 1351 | 0 | 0 | 7118/18399 | 201,326,592 / 201,326,592 | True | -> data-38327.wal 32,346 |
| bigcadence | 106.0 | 8000 | 0 | 62 | 62 | 61 | 3637 | 306,220,753 | 62 | 6070/14892 | 201,326,592 / 201,326,592 | True | data-31376.wal 8,422,738 (no rotate) |
| bigcap | 106.0 | 8000 | 0 | 63 | 63 | 62 | 2710 | 307,753,224 | 63 | 6285/14464 | 402,653,184 / 402,653,184 | True | -> data-38327.wal 32,346 |
| neither | 107.0 | 8000 | 0 | 75 | 75 | 32 | 1249 | 0 | 0 | 6807/24034 | 201,326,592 / 201,326,592 | True | data-31376.wal 8,422,738 (no rotate) |

Baseline DID rotate its WAL (`data-31376.wal` -> `data-38329.wal`), so rotation/fsync fires at the
8 MiB cadence; `bigcadence`/`neither` did not (same segment, 8.4 MB). All runs: 8000 completed,
0 errors, commit_rate 1.0, cache_evicting true.

## Repeatability (pass 2, same frozen binary; median per-window svc p95, us, pass1/pass2)

| config | rung50 | rung100 | rung200 | rung400 |
|---|---:|---:|---:|---:|
| baseline | 116,586/86,458 | 387,608/334,445 | 624,019/404,711 | 590,578/1,421,856 |
| group1 | 100,910/92,962 | 447,497/470,024 | 3,365,133/7,785,916 | 16,875,924/29,408,561 |
| nocompact | 14,061/15,072 | 13,660/23,906 | 24,432/43,420 | 141,402/81,850 |
| bigcadence | 61,167/126,197 | 357,023/425,183 | 785,511/312,986 | 437,735/332,037 |
| bigcap | 82,525/93,334 | 308,433/438,916 | 343,865/604,038 | 323,248/601,462 |

Run-to-run variance is large: up to 2.4x on median per-window p95 (baseline rung 400: 0.59 s ->
1.42 s; baseline rung 200 p50 252 ms -> 23 ms), and baseline throughput 106.0 -> 105.0. A
single-pass delta below ~2.5x is NOT significant. The `nocompact` gap survives: its worst pass-2
p95 (43,420 at rung 200) is still 9.3x below the baseline's best pass (404,711).

## Attribution

**Dominant mechanism: compaction actuation (the driver's per-window `--compact-bytes` merge).**
- Removing it (`nocompact`) collapses the tail at every rung: median per-window svc p99
  163,919/447,608/684,019/624,328 -> 17,728/20,695/30,505/178,902 (3.5x-22x) and svc p95 median
  116,586/387,608/624,019/590,578 -> 14,061/13,660/24,432/141,402 (4.2x-28x); lag p99 max drops
  862,658/751,335/816,329/765,218 -> 259,957/210,883/29,921/246,179. This survives the repeat: at
  every rung the worst `nocompact` median svc p95 is still >= 4.2x below the best baseline median
  svc p95 (e.g. 141,402 vs 590,578 at rung 400; 43,420 vs 404,711 at rung 200). It is the only
  configuration that moves the tail outside the baseline range.
- Removing WAL maintenance/rotation instead (`bigcadence`) leaves the tail in the baseline range or
  worse (rung 200 svc p99 1,418,054 vs baseline 902,072; passes overlap at 313k-786k). So
  **rotation/fsync stalls are NOT the dominant mechanism** — even though baseline rotates, deleting
  the rotation does not help.
- Removing both (`neither`) gives a tail indistinguishable from `nocompact` (svc p95 median
  11,839/16,963/39,511/41,102 vs 14,061/13,660/24,432/141,402): the residual is not WAL maintenance.
- **Group head-of-line is NOT the cause.** Group size 1 removes group HOL but the tail explodes and
  throughput collapses (106 -> 82 rps; rung 400 400 -> 95 rps; svc p95 median 3.4 s / 16.9 s at
  rungs 200/400). If the tail were group HOL, removing grouping would remove it.
- **Page-cache pressure is not the dominant cause.** Doubling the cap (`bigcap`, 8.17x -> 4.08x)
  leaves the tail interleaved with baseline across passes; still fully cache-evicting (cgroup peak
  equals the 384 MiB cap; data 1.64 GB >> cap).

**Residual, un-attributed:** with both compaction and WAL maintenance off (`neither`) the median
per-window svc p95 is still 11,839/16,963/39,511/41,102 us (rungs 50/100/200/400; max-over-windows
p99 419,170/311,063/90,152/507,763) — above the 10 ms ceiling, so no rung is sustained even with
both stall sources removed. Candidates not separated here: cold-cache read page-ins (1.64 GB data
vs a 192 MiB cgroup cap), allocator (`trim_heap`) / OS scheduler jitter.

## What remains unseparated

- The residual tail above (read-page-in vs allocator/OS) — needs a config that removes the read
  path or makes the instance fit the cap (a warm run contradicts the cold regime and is out of
  scope), or a per-window stall log correlating stalls with major faults / `trim_heap`.
- Group head-of-line *within* a group is confounded with durability batching: `group1` also turns
  one group commit into one commit per request, which is what makes it worse. A separation would
  keep group batching but issue requests so a member never waits on a sibling (not available).
- Compaction's stall is attributed only at the actuation level (`compact_window` ->
  `maybe_compact_shape(67108864, ...)` once per window). It is not split into merge CPU vs tile
  write/fsync cost, and the per-window merge volume is not controlled (it follows the policy).
- `bigcap` did not make the run warm (cgroup peak still == cap), so it bounds "less pressure", not
  "resident working set".
- Absolute numbers are not comparable to slice 47: the slice-47 baseline p50 at rungs 200/400 was
  9,157/19,638 us; here 252,110/332,296 us (session drift). All attributions above are
  within-run comparisons of the same frozen binary and identical trace.

## Gates (verbatim)

`cd harness && uv run --with pytest pytest -q`:

```
129 passed in 7.50s
```

`git diff --check`: silent (exit 0).

## Raw artifacts (not committed)

`/tmp/cold49/{baseline,group1,nocompact,bigcadence,bigcap,neither}/` (curve.json, report.md, arm
stderr, kept engine copies), `/tmp/cold49/pass2/` (repeat), `/tmp/cold49/comparison.md`,
`/tmp/cold49/flashdb-load` (frozen driver).
