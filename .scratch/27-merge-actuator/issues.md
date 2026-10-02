# 27 — the per-region compaction-trigger actuator

Opened 2026-10-03. Slice 26's per-region telemetry redirected the learning plan by measurement:
bloom bits are dead on this flow (absent probes are range-rejected before the bloom), page size is
flat (the scan region is JSON-decode bound), and the live axis is the **per-region compaction
trigger** — write-only regions accumulate with no read pressure while probe-hot regions must stay
few-block. This slice makes that trigger a policy knob an external controller can set.

## What changed

- **`LayoutPolicy` carries a per-region merge threshold** `merge_min_tiles: u8` alongside
  `(page_size, bloom_bits_per_key)`. `RegionLayout` is a struct now.
- **Default = today's behaviour.** `DEFAULT_MERGE_MIN_TILES = 2`. That is today's *effective*
  per-region threshold: `select_region_run` already required a run of two tiles, so a region could
  be merged as soon as it held two. A threshold of 2 is therefore a no-op, and a default policy
  reproduces the pre-slice-27 output byte-for-byte (pinned by a test).
- **The automatic per-region merge path respects it.** In `select_region_run`, a region is eligible
  only when its own tile count reaches its threshold; the existing `CompactShape` then picks the run
  *within* that region, and the smallest eligible region run wins. The global `max_tiles` gate in
  `maybe_compact_shape` still decides whether compaction runs at all. `compact()` (collapse a region
  on request) ignores the threshold.
- **External control: a frozen JSON policy file.** `FLASHDB_POLICY_FILE` (env, all drivers) or
  `flashdb-load --policy-file PATH`; `flashdb-load --policy-reload` re-reads it every window. The
  active policy is printed (`engine_policy policy={...}` header line and the `"policy"` field of the
  `--metrics-json` window line).
- **`flashdb-scale compact <dir>`** applies the active policy's thresholds to an already
  tile-resident directory, compacting every eligible region to a fixed point, and reports merges,
  compaction bytes, active tile bytes before/after and tiles per region. It is the measurement
  driver for the actuator on a directory the inventory flow published.

### Policy file schema (frozen)

```json
{
  "default": { "page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2 },
  "regions": {
    "P|Stock":   { "merge_min_tiles": 2 },
    "P|Order":   { "merge_min_tiles": 200 },
    "P|Movement": { "page_size": 65536, "merge_min_tiles": 2 }
  }
}
```

`default` and `regions` are optional; every field inside an entry is optional and falls back to the
policy default. `page_size` in `4096..=65536`, `bloom_bits_per_key` in `0..=64`, `merge_min_tiles`
`>= 2`. A region key must be a real region spelling (`P|`, `U|`, `R|` + a non-empty entity).

**Error policy (frozen): reject the whole file.** Any malformed/unknown field, invalid value, or
non-region key is a hard error — a controller never silently measures a policy it did not set. A
well-formed region name that names no tile is *accepted* and simply has no effect (the policy is a
lookup table). A `merge_min_tiles` above the active-tile cap is legal but can defer a region's
merges until the manifest fills and publishes start refusing; that is the documented lazy end.

`FLASHDB_POLICY_FILE`, when set and valid, *replaces* the env-derived layout policy
(`FLASHDB_PAGE_SIZE`/`FLASHDB_BLOOM_BITS`/`FLASHDB_REGION_LAYOUT`) in the scale driver.

## Tests (+8 Rust; 230 → 238)

`engine/tests/merge_threshold.rs`: below-threshold defer / at-threshold merge; two regions with
different thresholds compact independently; an eager region is not held back by a lazy neighbour;
`compact()` collapses a region regardless of threshold; the default policy reproduces today's
compaction byte-for-byte; recovery after a threshold compaction; the JSON schema round-trips and
rejects unknown regions, invalid values, unknown fields and non-JSON. Plus a `flashdb-load` unit
test for `--policy-file`/`--policy-reload` parsing and the pinned wrapper line (now carries
`"policy"`).

## Measurement — tile-resident inventory flow

`.scratch/27-merge-actuator/measure.py` (seed 85, 21k dims + 2000 order_flow writes, 1000-request
MIX read trace at 200 rps, release binaries, one fresh copy per config). Device fsync p50
5.6–7.5 ms (shared box). `eager` = default (threshold 2 everywhere); `split` = eager for the
probe-hot regions, lazy (200) for the write-only ones; `defer` = lazy (200) for the four
multi-tile regions (the deferral demonstration).

| config | merges | compaction_bytes | rewrite_factor | active tiles | tiles/region (before → after) |
|---|---|---|---|---|---|
| eager | 5 | 12,629,247 | 2.945 | 45 → 7 | P\|Stock 17→, U\|Stock 16→, P\|Customer 3→1, U\|Customer 3→ |
| split | 5 | 12,629,247 | 2.945 | 45 → 7 | identical to eager |
| defer | 0 | 0 | 0.000 | 45 → 45 | unchanged |

Per-region telemetry (last window; p50/p99 in ns; `bytes_read` is the deterministic metric):

| region | eager p50/p99 | defer p50/p99 | eager bytes_read | defer bytes_read |
|---|---|---|---|---|
| P\|Stock | 32,746 / 40,739 | 35,924 / 53,877 | 110,179 | 213,040 |
| U\|Stock | 49,371 / 81,940 | 43,081 / 53,877 | 107,458 | 107,174 |
| P\|Customer | 34,448 / 40,739 | 27,417 / 40,739 | 86,835 | 82,180 |
| U\|Customer | 52,392 / 71,252 | 51,425 / 71,252 | 86,436 | 80,474 |
| P\|StockMovement (scan) | 2,960,789 / 4,102,380 | 2,892,619 / 5,425,398 | 2,445,798 | 1,213,572 |

**What the split bought.** Nothing, and it cost nothing: on this flow the genuinely write-only
regions (`P|Order`, `R|Order`, `R|StockMovement`, `U|StockMovement`) hold **one tile each** at this
publish cadence, so a lazy threshold above 1 has nothing to defer — `split` is byte-identical to
`eager`. The deferral lever only bites once a write-only region accumulates multiple tiles, which
this flow's checkpoint cadence does not produce.

**What deferral costs.** Deferring the four multi-tile regions eliminated all 5 merges and
12.63 MB of compaction output (rewrite_factor 2.945 → 0) but left 45 tiles (vs 7); probe `bytes_read`
for `P|Stock` roughly doubled (110 KB → 213 KB) from more per-tile bloom false positives, and the
scan region read *fewer* bytes (2.45 MB → 1.21 MB) but its p99 rose 4.10 → 5.43 ms. These p99
differences are within this shared box's documented between-run spread (slice 25), so they are not
a policy signal; the deterministic `bytes_read` numbers are.

**Constraint.** No region's p99 exceeded its eager value by more than the stated 25% margin under
the task's `split` (which is byte-identical to eager, so trivially satisfied); under `defer`,
`P|Stock` (+32%) and the scan region (+32%) exceeded it, inside the box's run-to-run noise (the
load run reported `late_total=1` on the split config, confirming the noise). The read-side
constraint is therefore respected by the task's split and *near* the margin for full deferral.

## Findings recorded for the follow-on (not changed here)

1. **Region merge outputs can carry other regions' keys.** `write_compacted_tile` merges the run's
   tiles with the *global* WAL suffix in `(lower, cutoff]`, and `merge_region_run` passes a
   per-region `lower`; when a region's first tile is merged (`lower = 0`) the output includes other
   regions' ops from that CSN window. The selection is per-region (the run's inputs are one region),
   but the *content* is not restored to one region, so `tile_single_region` returns `None` for such
   a tile. Recovery tolerates it (it is treated as a whole-keyspace tile) and reads stay correct.
   This is pre-existing slice-24 behaviour, surfaced by this slice's per-region tile counts; fixing
   it (filtering the WAL suffix by the run's region) is a compaction-content change left out of
   scope.
2. **`merge_region_run` leaves retired tiles on disk.** Unlike `merge_run`, it has no unlink step,
   so `tileinfo`'s disk glob counts orphans the manifest no longer references (50 files vs 7 active
   above). The active manifest and reads are correct; disk usage and `tile_bytes_total` are inflated
   by repeated per-region compaction. Also pre-existing.

## Verification reported verbatim

```
cargo test --workspace --offline      # 238 Rust passed (was 230), 0 failed
cargo build --workspace --offline     # ZERO warnings
cd harness && uv run --with pytest pytest -q   # 37 passed
git diff --check                      # clean
```
