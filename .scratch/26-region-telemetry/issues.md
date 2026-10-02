# 26 — per-region telemetry (the signal a per-region layout learner needs)

Opened 2026-10-02. Slice 24 gave every region its own tile and a per-region
`(page_size, bloom_bits)` policy; slice 25 closed the page-size axis as flat and
left **bloom bits** and **compaction parameters** as the axes with measured
gradients. This slice is the observational prerequisite for a learner to act on
those axes: per-region counters, keyed by the region prefix bytes
(`keys::region_prefix`, `MARKER | entity | 0`), plus a machine-readable dump.

Nothing here is persisted; nothing on the correctness path consults it;
`Engine::set_metrics_enabled(false)` is a true no-op. `manifest.rs`, `wal.rs`
were untouched; `tile.rs` gained one observability-only counter (no format, no
read/write semantics, no durability change).

## Counters (per region, per window)

Keyed by the region prefix bytes, alongside the existing per-pattern latency
histograms:

| counter | meaning |
|---|---|
| `probes` | point lookups served for this region (`get`, `unique_lookup`, `reverse_lookup`) |
| `probe_misses` | probes that returned no row (the bloom-would-pay signal) |
| `bytes_read` | data-block bytes read from tile cursors while serving this region |
| `bytes_written` | exact encoded WAL op bytes for this region's keys |
| `commit_blocks` | committed WAL records touching this region (once per block) |
| `commit_ops` | committed ops in this region |
| `count`/`mean_ns`/`p50_ns`/`p99_ns` | the same log-bucket latency estimator, per region |

**Region cap / folding rule.** At most `MAX_REGIONS = 256` distinct region
prefixes are interned; past the cap a new region folds into one `other` row
(`region = "other"`). Names render via `keys::region_name` (`P|Stock`); row
order is first-interned, `other` last; an idle region is omitted, not
zero-filled. Interning allocates only on first sight (a reused scratch buffer
builds `MARKER|entity|0` on the commit path). Window rollup moves every region
counter to `previous` and clears the current window while keeping indices stable.

`bytes_read` is attributed to the region **served** (the request prefix), so it
is correct under both Global and PerRegion tile layouts; a bloom miss that reads
a block shows `bytes_read > 0`, a bloom reject shows 0. Counted in
`TileCursor::load_block` as the data-block length actually read; directory
blocks, superblock and trailer are excluded.

## JSON schema (frozen)

`WindowSnapshot::to_json` (engine/src/metrics.rs), consumed by
`flashdb-load --metrics-json`, one object per window line:

```json
{"window":3,"rate_rps":1000.0,"requests":217,"completed":2173,"errors":0,
 "p50_us":6915,"p99_us":31973,
 "current":{"patterns":[{"pattern":"P:Stock","count":191,"mean_ns":190000.0,"p50_ns":..,"p99_ns":..}],
            "regions":[{"region":"P|Stock","probes":191,"probe_misses":0,
                        "bytes_read":796070,"bytes_written":11109,
                        "commit_blocks":161,"commit_ops":161,
                        "count":191,"mean_ns":..,"p50_ns":..,"p99_ns":..}]},
 "previous":{...}|null}
```

Top-level keys: `window`, `rate_rps`, `requests` (this window),
`completed`/`errors` (cumulative), `p50_us`/`p99_us` (this window), `current`,
`previous`. A window snapshot is `{"patterns":[{pattern,count,p50_ns,p99_ns,mean_ns}],
"regions":[{region,probes,probe_misses,bytes_read,bytes_written,commit_blocks,
commit_ops,count,p50_ns,p99_ns,mean_ns}]}`. Object keys are serde_json's sorted
map, so a deterministic window dumps byte-identically (pinned by a test).

## Measurement — tile-resident inventory flow

`harness/flashdb_harness/inventory_workload.py` (seed 85 blocks / 85k docs,
3000 flow requests, commit_rate 1.0000) then `flashdb-scale checkpoint` with
`FLASHDB_REGION_MODE=per_region` → **10 region tiles, serving_memtable_entries=0**
(reads are provably tile-resident). The pacer was then run against that
directory; because a replayed flow's block ids are already committed, a
same-trace rerun is idempotent (`bytes_written=0`). To measure write attribution
with the scan region also tile-resident, a fresh 5000-request trace was run
against the same tile-rich directory (new order numbers → new commits).

`flashdb-load <dir> flow3.jsonl --rate-rps 1000 --window-ms 1500 --metrics-json`,
window 8:

```
region              probes  misses  miss%  bytes_read  bytes_wr  blk   ops  lat_n   mean_ms    p99_ms
-----------------------------------------------------------------------------------------------------
P|StockMovement          0       0   0.0%     1790898     21467  161   161      6    30.453    38.389
U|Stock                191       0   0.0%      806910         0    0     0    191     0.169     0.249
P|Stock                191       0   0.0%      796070     11109  161   161    191     0.190     0.251
P|Customer             181       0   0.0%      760840         0    0     0    181     0.177     0.246
U|Customer             181       0   0.0%      750956         0    0     0    181     0.186     0.331
U|Order                161     161 100.0%           0      6923  161   161    161     0.013     0.023
U|Backorder            161     161 100.0%           0         0    0     0    161     0.008     0.023
U|StockMovement          0       0   0.0%           0      8694  161   161      0     0.000     0.000
R|StockMovement          0       0   0.0%           0      6923  161   161      0     0.000     0.000
P|Order                  0       0   0.0%           0     17844  161   161      0     0.000     0.000
R|Order                  0       0   0.0%           0      5635  161   161      0     0.000     0.000
```

## Reading (the input for the learner)

- **Bloom bits: no region would benefit from *more* bits on this flow.** Every
  absent probe costs **0 block bytes**. `U|Order`'s 100% misses are new order
  numbers that sort *above* the tile's last key, so the tile key-range filter
  rejects them before any read (not even the bloom is consulted); `U|Backorder`
  has no tile at all. The four probe-hot regions all *hit*, so the bloom never
  fires. The flow does not exercise the bloom; a bloom-bit gradient needs absent
  probes *inside* an existing tile's key range (e.g. random/deleted lookups).
- **Page size / scan:** `P|StockMovement` is scan-bound (6 scans read 1.79 MB,
  ~300 KB each ≈ the 4 KiB-page tile, mean 30 ms) — the region-local `scan` cost
  dwarfs every point probe. It is the only region where a page-size change could
  reach the read path, and slice 25 already showed it is JSON-decode-bound here.
- **Compaction trigger:** the write-only regions accumulate with no read
  pressure — `P|Order` 17.8 KB/window, and `P|StockMovement`/`R|StockMovement`/
  `U|StockMovement` from the flow's movements. Their `commit_blocks`/`bytes_written`
  are the per-region trigger input; deferring compaction there is free, while the
  scan region wants its tile kept few-block and compact.

## Tests (+8 Rust; 222 → 230)

`engine/tests/metrics.rs`: probe/miss/bytes/commit attribution on a tile-resident
synthetic workload with an in-range miss (`bytes_read > 0`); region cap folding;
disabled mode records nothing; window rollup moves counters and omits idle
regions; the JSON dump is byte-deterministic and its field names are pinned;
identical engine runs produce the same region dump.
`engine/src/bin/flashdb-load.rs`: `--metrics-json` parsing and the frozen wrapper
line.

## Verification

```
cargo build --workspace --offline          # ZERO warnings
cargo test --workspace --offline           # 230 Rust passed (was 222), 0 failed
cd harness && uv run --with pytest pytest -q   # 37 passed
git diff --check                            # clean
```
(`cargo test` additionally surfaces 2 pre-existing `unused_mut` warnings in the
untouched `engine/tests/manifest_engine.rs`; `cargo build` is clean.)
