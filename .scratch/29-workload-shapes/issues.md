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
