# 29 — the workloads the thesis is actually about are not generated yet

Opened 2026-10-01, from the accumulated headroom measurements. This is the uncomfortable finding
that should shape 0b, stated before running it rather than after.

## The measured headroom table

| axis | kind | measured effect on our workloads |
|---|---|---|
| read-path indexing (08a-08c) | engineering | probes 65 ms -> 15 us; scan 324 -> 605 MiB/s cold |
| write-path single materialization (10c) | engineering | publish peak 5.6-6.4xW -> 2.4-3.0xW |
| fence RAM sparse index (11) | engineering | 0.07x tile bytes -> 0.20 MiB/GiB |
| compaction shape + cap (12/19) | engineering | write_amp 5.87 -> 3.92 (cap 12) -> 2.13 (cap 1024) |
| group commit (22/23) | engineering | 150 -> 6,378 rps |
| per-region **bloom bits** | learned | **dead here**: absent probes are range-rejected before the bloom is consulted (0 block bytes) |
| per-region **page size** | learned | **flat**: the scanning region is JSON-decode bound |
| per-region **compaction trigger** | learned | realistic split byte-identical to eager (write-only regions hold ONE tile each); full deferral saves 12.63 MB of compaction output but doubles P|Stock bytes_read |
| family partitioning (24) | learned | harmful at cap 12, neutral at cap 1024 |
| cross-entity co-location (**L2**) | learned | NOT IMPLEMENTED (needs dual copies or a handle map; design defers it to v1) |

Every engineering axis paid off by a large factor. Every *learnable* axis measured neutral,
harmful, or a pure trade-off. That is a warning, not yet a verdict.

## Why it may be an artefact of the generator, and must be checked first

Our generator produces four shapes: `order_flow` (22% writes, guarded reads), `stock_lookup`,
`customer_lookup`, `movements_scan`. The workloads the design's own table names as the motivating
cases - **star-schema fan-out, multi-hop traceability, as-of join against a time-versioned dimension,
anti-join (lost sales)** - are NOT generated. Those are exactly the shapes whose cost is dominated
by *where related rows live*, i.e. the only thing L2 can move. Measuring a per-region compaction
policy against a workload whose regions are all roughly independent is close to a null by
construction.

So before concluding anything about the thesis:

1. Generate the missing shapes (multi-hop trace, as-of join, anti-join, plus the flat aggregate) per
   `docs/dev-loop.md`, deterministically and at a stated scale.
2. Measure their **access-pattern statistics**: fan-out per hop, multi-hop depth, the fraction of a
   request's cost that is dependent random reads across entities, and the co-location potential
   (how often the rows a request touches were written together / share a natural grouping key).
3. Decide from that whether a layout space with real headroom exists at all for these workloads - and
   if the answer is "only cross-entity co-location", then L2 is the axis 0b must be built on, not the
   per-region parameters we have been tuning.

## What this does NOT change

The engineering results stand and are the engine's actual value: the capacity curve, the memory
discipline, the durability protocol, group commit, and the fact that the light-weight axes are
measurably exhausted. A pre-registered null on "learned physical parameters" is publishable - the
design docs require publishing the null - but it must not be reported as a null on the thesis until
the motivating workloads have been generated and measured.

## Correction (2026-10-01): the axis comparisons so far were taken cache-WARM, which invalidates them

Before generating new workloads, a methodological error in the existing measurements has to be
stated, because it changes what "the axis is flat" means.

Every per-region comparison we have run (page size in slice 25, compaction trigger in slice 27, the
bloom snapshot in slice 26) was executed on a warm box with a ~5 GB dataset and 128 GB of RAM, i.e.
**the entire dataset was in page cache**. In that regime the layout can only move CPU cost, never
I/O: a dependent read costs ~8 us (measured probe p50) instead of the ~96.3 us device constant, and
a scan is decode-bound rather than block-bound. So those comparisons measured the *warm* regime and
cannot answer whether a layout reduces dependent random reads. The pre-registration's "instance size
>= 10x cache" requirement exists precisely to prevent this, and it was not honoured by the axis
runs even though the engine-level scale runs did honour it (ticket 10d: 5.16 GB of data under a 1 GB
`MemoryMax`, which completed with RSS ~215 MB).

Consequences:

1. "Page size is flat", "bloom bits are dead", and "the compaction trigger is neutral" are statements
   about the WARM regime only. Under a cold cache the page-size term is 96.3 us (4 KiB) vs 117.4
   (16 KiB) vs 185.8 (64 KiB) per dependent read, and a scan needs proportionally fewer dependent
   reads with larger pages - the axis can plausibly flip, and the bloom's in-range-absent case costs
   a full device read when it fires.
2. The axis comparisons must be re-run **under a memory cap that makes the working set >= 10x the
   cache**, with the cache actually evicted (a cgroup cap counts page cache, so `MemoryMax` is the
   tool we already use), and with the deterministic metrics (bytes, block counts, dependent-read
   counts) as the primary evidence because wall-clock on this box is noisy.
3. The device fsync/noise caveats recorded in ticket 12 still apply: every cold run must report the
   in-session device fsync p50 alongside its results.

This does not yet say which axes have headroom; it says we do not know, and that the earlier flat
results cannot be used as evidence either way.

## RESULT (2026-10-02): in the COLD regime the learnable axes have real headroom - the warm conclusions are reversed

The cold comparison finally ran (`harness/flashdb_harness/cold_axis_compare.py`, cap 192 MiB,
**data 1,645,273,432 B = 8.17x** the cap, 10.3 M rows, 151-324 tiles, rotate cadence 8 MiB so the
open-time suffix replay (5.15x its bytes) fits; in-session device 4 KiB O_DIRECT read p50 99.9 us
and fsync p50 ~5.8-6.2 ms; every arm `commit_rate 1.0` with the stock guard firing 2,000 times, so
the load is real).

**(a) global 4 KiB vs per-region mixed pages, at an equal offered 300 rps:**

| config | achieved rps | win p50 | win p99 | tiles | tile bytes |
|---|---|---|---|---|---|
| global-4k | 87.0 | 40,355 us | 80,629 us | 151 | 1,610,277,355 |
| per_region-mixed | **134.0 (+54%)** | **20,880 us** | **51,290 us** | 324 | 1,638,230,842 (+1.7%) |

The mechanism is in the per-region telemetry, not inferred: partitioning makes each probe consult
only its own region's tiles, so cold read amplification collapses - `P|Stock` bytes_read
7,789,516 -> 2,519,567 (3.1x less), `P|Customer` 1,279,445 -> 504,470 (2.5x less), with p50/p99
falling accordingly (447.7/4,399.3 us -> 206.8/3,567.3 us for P|Stock).

**This directly contradicts the warm "page size is flat" conclusion (slice 25) and confirms the
methodological correction in this ticket**: with the working set in page cache the axis could only
move CPU; cold it moves 54% of throughput.

**(b) per-region compaction trigger, same cold configuration:**

| config | merges | compaction bytes | rewrite factor | achieved rps | win p99 | active tiles |
|---|---|---|---|---|---|---|
| eager | 14 | 3,164,956,564 | 1.932 | 135.0 | 107,540 us | 10 |
| defer | 6 | **10,446,643** | **0.006** | 132.0 | 208,515 us | 306 |

Deferral removes 3.15 GB of compaction output (a 300x difference, deterministic) at statistically
equal throughput, at the cost of doubling p99 and holding 306 live tiles. That is exactly the
trade-off the learner is supposed to price, and it is now measured cold rather than assumed.

**So the thesis's first testable premise survives**: there IS a per-region layout decision whose
measured effect is a large fraction of throughput and write volume, and the actuator for it exists
(a per-region policy: page size/bloom plus `merge_min_tiles`, injected as JSON and echoed in the
telemetry).

Caveats, stated rather than buried: the ratio is 8.17x, below the pre-registration's 10x floor
(the runner prints a WARNING and the report says so); the read mix is a deliberately constructed
uniform probe/scan mix over a large working set rather than the design's Zipfian MIX, because the
Zipfian head fits in a small cache and would not force cold reads; and this box is shared, so the
timing figures are indicative while the byte counts and the 54%/300x gaps are not explainable by
the noise we have measured (10-20%).
