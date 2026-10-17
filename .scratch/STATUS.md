# Status

Current single source of truth for project state. Facts only: every number names its conditions.
History and process live in the tickets, not here.

The remaining work is sequenced as milestones in [`docs/roadmap.md`](../docs/roadmap.md).
M1 is delivered but its gate is CANNOT-VERIFY (T6 at >=10x unmeasured). M2's analysis slice is
delivered and merged, but its gate is UNMET (2/4): the >=10x and learned-vs-declared headroom
measurements need three engine capabilities that do not exist (multi-row `$block.field` binding, a
validator wider than `cust -> take`, aggregate-to-write). The author decision is M-B engine work vs
an explicit waiver of the >=10x item. No later milestone is authorised until that is decided.

## 1. Bound revision and instance

- **Bound revision (final integrated master, WP-79):** `482e3a1bd2a0c12dff4e0f3f758cd8a29556fed2`.
  The T1-T4 verdicts below were produced on this measurement revision.
- **Instance:** the >=10x cold instance `per_region-mixed` — **1,643,925,811 B / 128 MiB cap =
  12.25x** data:memory (30,000 seeded StockMovement docs). The decisive runs used fresh `cp -a`
  copies of it.
- **Harness floor behaviour (current):** `flashdb_harness/cold_0b.py` computes
  `ratio = data_bytes / cap_bytes` and sets `ratio_below_floor = ratio < 10`. On the 12.25x
  instance `ratio_below_floor` is `false`, so the rendered report prints no floor WARNING. The
  JSON `not_concluded` list still unconditionally contains the string "data:cap is 12.25x, below
  the 10x pre-registration floor" regardless of the ratio — a stale cosmetic harness caveat, not
  yet fixed. It does not affect the ratio, the floor check, or any verdict.

## 2. Final measured verdicts (pilot-grade)

Sample: 3 independent seeds (42/43/44) + one seed-42 repeat = 4 arm-runs per arm. **NOT the
pre-registered 10-seed matrix.** All on the 12.25x instance, one flashdb process at a time under
`MemoryMax=128MiB`, `flock /tmp/wave21-measure.lock`-serialized, shared 12-core box (loadavg
~2.9-5.9).

### Primary metric (adopted §7.1 lag-only)

A rung is lag-sustained iff achieved rate within 5% of offered, zero error windows, and zero
schedule-lag-ceiling rejected windows (`lag_ceiling_rejected_windows == 0`). The service ceiling is
reported separately and does NOT veto the lag-only reading.

- **Rung 50 — NOT lag-sustained for any arm** (lag-rejected windows out of 40): declared 39-40/40;
  learned@1.0 3-6/40 (median 3.5); static-derived 2-3/40 (median 2.5; one seed, 44, reached 0/40).
- **Rung 100 — lag-sustained:** static-derived 0/20 in 4/4 seeds; learned@1.0 0/20 in 3/4 (seed 44
  = 2/20); declared 16-18/20.
- **Rung 200 — lag-sustained:** static-derived 0/10 in 4/4 seeds; learned@1.0 0/10 in 2/4 (42/43).
  declared never sustains any rung.

### T1 (learned vs declared, win workload) — SUPPORTED

Aggregate achieved throughput: learned 106-107 rps vs declared 71-75 rps → **+41.3% to +50.7%,
median +47.6%**, outside the measured **~4.0%** throughput noise band.

### T2 (adversarial-uniform reorganisation) — SUPPORTED

Reorg ratio (learned/declared, `summary.compact_bytes_total`): **median 0.933x, max 0.995x
<= 1.5x**; **0 index builds on the 0-scan region** (`P|StockMovement.index_output_bytes = 0`,
final policy `index_order_no=False`) on all 4 runs.

### T3 (win reorganisation) — SUPPORTED

Reorg ratio: **median 0.767x, max 0.941x <= 3x**; **no single-region blowup** — win/learned
`U|Stock` is 5.13-5.23 MB / 1 merge each.

### T4 (write-spike drift) — EVALUABLE, measured NOT SUPPORTED

A real **~8-9%** capacity drop exists (static-derived ~9.4% median, learned ~7.6% median), but
**neither arm recovers** and **learned ≈ static post-drift** (median gap +2.0%, inside the noise
band). The write-spike floor is policy-independent, not a learner gap.

### Both ceilings (harness `sustained` / T1 verdict)

`sustained = false` on **all 96 win/adversarial rung-arm rows**: the service ceiling rejects
~17-40 windows per rung because the shared device's own latency floor sits above the fixed 10 ms
ceiling. The service ceiling is device-bound. §7.1 lag-only is the author-adopted primary reading;
the primary-vs-service-ceiling reconciliation is an open author item (§5).

## 3. Physical-layout mechanisms now in the tree

- **Learner-selectable `order_no` secondary access path** (index axis): the learner offers
  `index_order_no` only to `P|StockMovement`, demand-gated and one-shot sticky — the axis flips to
  index-on the first window that region serves a scan (`count > 0`) and stays on for the rest of
  the run; a region that never serves a scan never offers it. Built off the request path as an
  adopted `order_no.idx` tile.
- **Tile cap raised/configured:** `manifest::MAX_TILES = 4096`, `Engine::set_max_tiles`, and the
  `flashdb-load --max-tiles` flag; the scored runner passes `--max-tiles` through and refuses an
  incompatible cadence up front.
- **Write-spike drift shape:** the harness `--drift-shape` (`scan` | `write`) with `--drift-factor`
  drives the T4 write-spike shape.
- **Compaction-build periodic sync cadence:** `COMPACT_TILE_SYNC_EVERY_BYTES = 2 MiB`
  (`TileWriter::set_sync_every`) bounds the build's durable flush instead of one `finish`
  `sync_data` burst.

## 4. Honest limit (device-bound, not a learner gap)

The rung-50 residual is the compaction build's device I/O plus the pacer's WAL `fdatasync` tail.
The shared device's own latency floor sits above the 10 ms ceiling: the pacer's WAL `fdatasync`
tails to 77-99 ms under the build's 93-147 MB input-tile stream reads, and to 99 ms (one 166 ms
tail at seed 42 window 43) with NO build concurrent. The build's own durable flush is bounded to
11-32 ms by the cadence; what remains is the device floor, not a learner gap.

## 5. Open author items (not resolved here)

- (a) **§7.1 primary-vs-service-ceiling reconciliation.**
- (b) **The 10-seed / 15% / 5% / 3x thresholds as written vs the shared-box reality.**
- (c) **T5-T7 baseline harness:** postgres is NOT installed, and installing system dependencies
  needs user authority.

## 6. Evidence quality and accounting

- **Gates (current):** crates.io is unreachable from this box and the local cache is the aliyun
  mirror, so the bare `--offline` form fails at resolution (`no matching package named serde`). The
  command that works — the one the repo runners use, pinned by flags so it touches no host config —
  is `cargo --config 'source.crates-io.replace-with="aliyun"' --config
  'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' build --workspace
  --offline` → **0 warnings**; the same invocation with `test --workspace --offline` → **308 passed /
  0 failed / 0 ignored** in 27 test binaries; `cd harness && uv run --with pytest pytest -q` → **168
  passed**; `git diff --check` silent.
- **Cold measurement discipline:** every run uses a fresh `cp -a` instance copy, one flashdb
  process at a time under `systemd-run --user --scope -p MemoryMax=...`, `flock`-serialized.
  `cgroup_peak` = the 128 MiB cap on every arm-run.
- **Accounting convention:** the reorg metric is `summary.compact_bytes_total` (ticket 72
  convention; includes the final drained merge). Per-region per-window sums undercount by that
  final merge.
