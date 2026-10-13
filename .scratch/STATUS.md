# Status and handoff

Consolidated current state, for the next session. Everything below is measured unless it says
otherwise; every number names its conditions; decisions already adopted are separated from the ones
that are still open.

## 1. The engine

Single-node, single-writer, IO-dense document store; physical layout is per-region and
policy-driven; the durability protocol is fault-injected at every boundary.

| property | measured | conditions |
|---|---|---|
| tests | **264 Rust, 129 Python, zero warnings** (both `cargo build` and `cargo test`) | `cargo test --workspace --offline`; `uv run --with pytest pytest -q` in `harness/` |
| data:RAM | ~22-24:1 steady state | 5.16 GB, 1 KiB docs, 64 MiB window, **seed phase** |
| open footprint | ~5 MB + bloom/dir, independent of data and tile count | rotated WAL; the driver is the retained suffix (open replays it at 5.15x its bytes) |
| scan | 605 MiB/s cold, 3,155 MiB/s warm at 64 KiB pages | 5 GB single pass |
| unique probe | 0.46 us append-shaped, 1.59 us in-range absent | 12 active tiles |
| write throughput | 143 -> 6,378 rps with batch commit; 184 -> 4,232 rps on the real request path | one fsync per request vs per batch; WAL bytes byte-identical |
| write amplification | 5.87 -> 3.92 (merge shape) -> 2.13 (cap 1024) | 5 M / 1 M docs x 1 KiB |

NOT claimed: bounded recovery time (open is O(suffix), not O(data)); a bounded engine scan footprint
(the request adapter streams into a bounded top-100 map — verified — but the engine's own scan path
was not separately bounded); any absolute comparison against another engine yet (see the B1/T5-T7
work below).

## 2. The 0b measurement instrument is now trustworthy (three defects fixed, all verified)

Every per-rung number published before commit `744caaa` was unusable. The causes, all now fixed and
independently verified:

1. the curve builder took a **median of per-window deltas**, so a whole group's completions landed in
   one window and `achieved` exceeded `offered` (the "128%" figure);
2. the per-window `invalid`, `lag_p99_us`, `window_us` and `ceiling_us` fields **never reached
   `--metrics-json`**, so the aggregator read `invalid` as always false and the pre-registration's
   "no rejected window" criterion was **vacuous in every published run**;
3. the **trailing partial window was never flushed** (declared 7552 vs summary 8000 — 448
   completions unattributed);
4. the ceiling was applied to a **group-aggregated** quantity: the driver took one latency sample per
   group and assigned it to all members, so the "per-request" p50 was the 64-request group's elapsed
   time. Commit `965adf4` replaced this with an arrivals model: a group forms only from requests
   already due, requests are never consumed early, each request is sampled from its own due time, and
   the report separates per-request service latency from per-request schedule lag with a **named
   ceiling for each**.

Invariants verified on real output (commit `9bae6fe`): the sum of per-rung completions equals the
summary total (8000 = 8000); no rung exceeds its offered rate beyond the 2% tolerance
(`max achieved/offered = 1.0000`); all 64 invalid windows carry a `reject_reason` (service ceiling 64,
lag ceiling 63, union 64); `early_dispatches = 0` with zero tolerance; `error_windows = 0`.

## 3. First valid evaluation of the primary metric

Commit `9bae6fe`, verified. `adversarial` workload, `declared` arm, cold instance 1,643,933,528 B
under a 192 MiB cap = **8.17x**
(below the 10x floor; see §7), one seed. Per-request, microseconds; both ceilings 10,000 us.

| offered | achieved | % | svc p50 | svc p95 | svc p99 | lag p50 | lag p95 | lag p99 | sustained | svc-rejected | lag-rejected |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|:--:|---:|---:|
| 50 | 49.70 | 99.41 | 5,930 | 910,504 | 990,504 | 62 | 834,223 | 914,223 | no | 36 | 35 |
| 100 | 98.95 | 98.95 | 6,153 | 549,290 | 609,290 | 76 | 517,769 | 577,769 | no | 16 | 16 |
| 200 | 200.00 | 100.00 | 9,157 | 462,085 | 522,085 | 2,689 | 421,132 | 481,132 | no | 8 | 8 |
| 400 | 400.00 | 100.00 | 19,638 | 412,788 | 445,288 | 7,992 | 374,784 | 407,284 | no | 4 | 4 |

Whole run: 8000/8000 completed, 0 errors, 106.0 rps against an offered mean of 106.67, reorganisation
299,390,660 B over 63 merges.

**Verdict: no rung is sustained.** The failure is a **latency tail**, not throughput: the per-request
median is ~6 ms at rest (the same order as the session's device fsync p50 of 5-6.7 ms — a hypothesis,
not a verified attribution) rising to 19.6 ms at 400 rps, while p95/p99 sit at 0.4-1.0 s at every
rung. Under the pre-registration's own rule (lag p99 <= 10 ms, no rejected window) every one of the 64
windows is invalid, so the primary metric for this arm is currently **zero sustained rungs**.

## 4. Verdict board

| threshold | status |
|---|---|
| primary metric, `adversarial`/`declared` | **0 sustained rungs** at 8.17x (§3) |
| T1 learned beats declared by >15% on the win workload | **underived** — the earlier "tie" rested on the broken aggregator and is retracted as a verdict |
| T2 no >5% loss on adversarial-uniform | **underived** — the earlier "7.7% loss" rested on the broken aggregator and is retracted as a verdict |
| T3 reorganisation writes <=3x declared | **FAIL, 4.13-4.27x** — byte sums, unaffected by the aggregator defects |
| T4 drift recovery within 2 windows | **underived** — the earlier run saturated (drift rate above its mix's capacity); the default parameters need 6 windows, not 2 |
| T5/T6/T7 external baselines | **adopted** (§7); the request-level baseline harness does not exist yet |

Still missing for T1/T2/T4: the other arms (learned, static-derived), the win and drift workloads, a
measured noise band, the 10-seed median/IQR, and a >=10x instance.

## 5. The one remaining learnable axis has measured headroom

An offline measurement over the repository's own generator (ticket 43, verified) gives dependent
random reads per request: today's placement 4.740; **family co-location** (`U|X` and `R|X` in the
same tile as the `P|X` row) **3.092** = **-34.8%**, stable across tile sizes 64-16384 (-34.8 to
-37.1%); distribution-chosen region boundaries -7.7% at 256 and -23.4% at 4096.

This is a **read-level** result. It is explicitly **not** the pre-registration's 15% threshold (that
is defined on sustained offered load and this count cannot clear or fail it), so no load gain is
claimed. Co-location is currently inexpressible because tiles are monotone key-range fenced and the
key encoding orders by family prefix; ticket 50 makes it expressible. This is the leading explanation
of the measured zero throughput gain: the request level supplies co-occurrence, but the learner's
action space (per-region merge threshold, page size, bloom bits) contains no placement action.

## 6. Defects found and their state

Each of items 1-12 was found because a number looked too good, or a specification and an
implementation disagreed. All are fixed or recorded.

1. scan rate inferred from probe timings — corrected.
2. device probe that truncated without writing (reported a 0 us device) — fixed; the baseline it
   invalidated is retracted.
3. warm/cold conflation in every axis comparison — corrected; the axes were re-run cold (page size is
   only +54% cold, and the "flat" era was a page-cache artefact).
4. pacer timer granularity masquerading as saturation — fixed (burst pacing).
5. `sustained=800` with `achieved=129` (the curve builder ignored `invalid` and lag) — fixed with a
   regression test.
6. "T3 = 0 vs 0, not measurable" was actually the knob never being driven — fixed by actuating
   compaction on the load path.
7. achieved > offered (128%) — **fixed** in `744caaa` (see §2).
8. a unique conflict in `Engine::commit_batch` rejected the whole batch, so a request's verdict
   depended on its neighbours (the `--group-requests` path was not affected) — fixed in `4a348d7`
   plus a fail-closed P0 fix (a repeated `block_id` inside one request is refused); the validation
   unit is the request (`docs/contracts.md`, ticket 41).
9. the per-window `invalid`/ceiling fields never reaching the JSON — **fixed** in `744caaa`.
10. the trailing window never flushed — **fixed** in `744caaa`.
11. the ceiling was applied to a group-aggregated latency while the pre-registration names schedule
    lag — **fixed** in `965adf4` (two named ceilings, per-request samples).
12. a per-region publish emits one tile per region, so the runner's shipped 16 KiB checkpoint/rotate
    cadence exhausts `MAX_TILES = 1024` and aborts a scored run mid-way — **pinned** by a regression
    test in `bab567c`; the fix is adopted (§7; ticket `.scratch/46-partitioned-publish-tile-cap`).

## 7. Decisions adopted

Adopted by the orchestrator under the author's standing autonomy grant (the author stated on
2026-10-09 that the project has one milestone, that decisions must be made from the original
philosophy rather than asked, and that work must not stop). Reviewable and reversible by the author.

1. **Keep the 10 ms bound; the primary metric is judged on the LAG ceiling.** The pre-registration
   defines its primary metric over schedule lag; the implementation now carries a lag ceiling
   (default 10,000 us) and a service-latency ceiling separately. The relative alternative (two times
   the reference-load p99) becomes concrete only once a reference-load p99 is measured. Moving the
   bound after seeing a failure would be moving the goalposts; the response to a latency failure is to
   attack the latency.
2. **T5/T6/T7 adopted** (they were proposals): at the 1x rung, within 1.5x of the slower of
   {sqlite, postgres}, 2x the fail line, 10x a design failure (T5); at the highest common offered
   level, at least 3x the best baseline's sustained throughput under the same latency rule (T6); the
   declared arm alone must pass T5 (T7). Baselines: sqlite (WAL, `synchronous` stated), postgres
   (frozen disclosed config, no tuning race), one request-level harness driving every arm, every arm
   in the same memory-constrained regime.
3. **The 10x floor is a requirement, not a qualification.** Verdicts from an 8.17x instance are
   provisional; final verdicts are re-derived on a >=10x instance (`.scratch/51-instance-10x`). An
   8.17x instance is warmer than a 10x one, so a failure there cannot be explained away by the lower
   ratio.
4. **The tile-cap defect is fixed on both sides** (`.scratch/46-partitioned-publish-tile-cap`): raise
   and make configurable the tile cap, make the run harness's default cadence compatible with
   per-region publishing, and make a scored run refuse an incompatible cadence loudly instead of
   aborting mid-run. The shipped default must be able to run the shipped engine in the documented
   regime.
5. **Endurance is a constraint, not a price** (`docs/objective.md`): the reorganisation write term is
   0.083-0.669% of the service-time term, so no weight can move an argmin; the objective is maximise
   sustained load subject to the latency ceiling and a write-amplitude budget.

Still open for the author (none of these blocks the plan in §9): the 15% / 5% / 3x numbers and the
10-seed requirement as written for a shared box; the 2-window drift target against the 6 windows the
current parameters need; ticket 13 (open-time corruption detection); D1-D5 (constraint surface);
whether the request grammar gets a create/seed shape.

## 8. Environment notes for re-running

- The box is shared: the same 1 M-doc workload has measured 6,137 / 15,060 / 16,264 docs/s on
  different runs. Deterministic metrics (bytes, counts, block reads) are the primary evidence;
  timings must state their concurrency.
- Device fsync p50 is 5-6.7 ms in session (p99 to 23 ms) and must accompany any write-side number.
- Cold runs need `MemoryMax` (it counts page cache), a rotate cadence large enough for the per-region
  publish to stay inside the tile cap, and a fully checkpointed instance before the load.
- **Reviewer hygiene:** do not use the `oracle` agent as a reviewer — it runs with forked context,
  inherits the supervisor's framing, and twice produced narration instead of a review (once with a
  fabricated sub-run id). Use a read-only worker-as-verifier that must paste command output and must
  write `CANNOT-VERIFY` rather than guess.
- **Delegation:** the work is executed by subagents. At this handoff the `subagent` tool was missing
  from the session's tool list, which blocks all dispatch; a session with it available can run the
  staged plan directly.

## 9. Resume plan

The plan is staged as a runnable workflow script: `.scratch/52-program/wave1.js`. Its five lanes are
independent and touch disjoint files:

1. **latency instrumentation** — per-window group count/size, per-group max elapsed, `min_lag_us`, so
   the tail can be attributed without a re-run;
2. **tail attribution** — measure-only isolation runs (group size 1, compaction off, large rotation
   cadence, doubled cap) to name the dominant tail mechanism instead of guessing;
3. **family co-location** — implement the entity-major key encoding behind a selector so `P|X`/`U|X`/
   `R|X` can share a tile, with a test asserting co-location, without measuring yet;
4. **>=10x instance** — build and smoke-test an instance that meets the pre-registration's floor;
5. **decision record** — write the §7 decisions into the pre-registration, objective and status docs.

Then, in order: fix the dominant tail mechanism; re-run the three arms on the >=10x instance with the
instrumented driver and re-derive T1/T2/T4 with a measured noise band; measure whether family
co-location converts the -34.8% read reduction into a load gain; build the request-level baseline
harness and evaluate T5/T6/T7; fix the tile cap on both sides; final polish and a consolidated report.
