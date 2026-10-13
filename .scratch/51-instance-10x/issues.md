# 51 — cold instance at 12.25x data:regime-cap (meets the pre-registration 10x floor)

Measure-only lane: one new build script, no existing source touched. Built 2026-10-13T18:22:37+08:00.

## Goal

Every published curve (44/47) ran the slice-44 instance at **8.17x** (1.64 GB / 192 MiB), below the
pre-registration's own floor, "Data >= **10x** the memory budget"
(`.scratch/15-objective-v0/pre-registration.md:52`), so each verdict had to be qualified. Rebuild
the same instance and judge it against a **128 MiB** regime cap.

## Pair and ratio

- `data_bytes = 1,643,925,811` (~1.53 GiB) — dimensions unchanged from `build_cold_44.py`.
- regime cap `= 128 MiB = 134,217,728 B` (the cap the scored run should use, `cold_0b --cap-mib 128`).
- **ratio = 1,643,925,811 / 134,217,728 = 12.25x** (floor 10x; margin **+22.5%**).
- Trade rule honored: the dataset is **unchanged**; the floor is met by lowering the cap, per the
  lane's "prefer a smaller cap over a smaller dataset". A 128 MiB cap was also tried as the *build*
  cap and fails (below), so the build uses a separate transient cap.

## Build parameters — `harness/build_cold_51.py` (new)

```
cd harness && source $HOME/.cargo/env
uv run python build_cold_51.py --release --workdir /tmp/cold51 --build-cap-mib 192 --regime-cap-mib 128
```

- seed=42 customers=2,000,000 skus=2,100,000 locs=4 on_hand=1,000,000 block_docs=2000
- write-requests=30000 write-rate=100000 group-requests=64
- checkpoint-bytes=rotate-bytes=**8,388,608** (8 MiB; slice 46: the runner's shipped 16 KiB default
  exhausts the 1024-tile cap and aborts)
- region_mode=per_region, layout `P|StockMovement=65536:10` (`MIXED_REGION_LAYOUT`)

Two caps, measured not assumed. Construction needs a **192 MiB** transient scope: seed peaks
~117 MiB; opening the just-seeded instance (8 MiB retained WAL suffix replayed) peaks ~140 MiB; the
grouped write phase peaks ~163 MiB (`rss_peak_bytes=170,885,120`). A **128 MiB build scope was tried
first and failed** — `build failed rc=-15` right after `phase=seed-inventory ok=true` (write phase
SIGTERM/OOM-killed). The **judged regime cap is 128 MiB** because the scored *load* path is much
lighter: slice 44/47 recorded `load_hwm_bytes` 74.1/73.7 MB, and the smoke below peaked at
81,068 KB open / 65,956 KB RSS.

## Seed-phase output (verbatim)

```
phase=seed-inventory ok=true blocks=5200 docs=10400000 customers=2000000 stocks=8400000 csn=5200 publishes=146 rotations=145 tiles=294 serving_memtable_entries=0 region_mode=per_region layout_page_size=4096 layout_bloom_bits=10 dir=/tmp/cold51/per_region-mixed elapsed_s=242.588
build_scope peak_bytes=201326592 rss_peak_bytes=170885120 rc=0
```

## openinfo

```
phase=openinfo ok=true dir=/tmp/cold51/per_region-mixed active_tiles=336 serving_memtable_entries=0 tile_bytes_total=1638589638 rss_kb=65860 vm_hwm_kb=80692 vsize_kb=83024
```

## WAL ingest expected for the 8000-request scored trace

Smoke measured 74,534 B of WAL for 80 order_flow writes = **931.7 B/write**. The scored adversarial
trace has **3156 writes** (slice 44/47: `counts={'stock': 2408, 'customer': 2436, 'scan': 0, 'write':
3156}`), so expected ingest ≈ 3156 x 931.7 ≈ **2.94 MB (2.80 MiB)** — far under the 8 MiB cadence, so
**0 checkpoints/rotations** and no new per-region publish during the scored run; `active_tiles`
stays 336, nowhere near the 1024-tile cap.

## Smoke (one offered level, one arm)

```
cp -a /tmp/cold51/per_region-mixed /tmp/cold51/smoke       # 800-request mix 50/20/20/10
systemd-run --user --scope -q -p MemoryMax=134217728 -p MemorySwapMax=0 bash -c '
  target/release/flashdb-load /tmp/cold51/smoke /tmp/cold51/smoke.jsonl --rate-rps 50 \
    --window-ms 1000 --metrics-json --peak-report --group-requests 64 \
    --checkpoint-bytes 8388608 --rotate-bytes 8388608 --region-mode per_region \
    --ceiling-us 10000 --lag-ceiling-us 10000'
```

Result: **rc=0, completed=800/800, errors=0**, `else_used=80` (the forced backorder branch fired for
every write), `cgroup_peak_bytes=134217728` == cap -> `cache_evicting`, `load_peak` open=81,068 KB /
end=65,956 KB, device_fsync_p50=7,203 us. The instance is usable at 128 MiB. (6/16 windows breached
the 10 ms lag ceiling — the box is shared and loaded; irrelevant to usability.)

## Residual risk

- Build wall ~6-7 min (seed 242.6 s dominates), on a loaded shared box; the deterministic traces
  (`seed.jsonl` 1.34 GB) remain in `/tmp/cold51`.
- Smoke used no per-window compaction. The future scored curve passes `--compact-bytes 64 MiB`, so
  compaction-time memory at 128 MiB is **not** directly verified; observed anonymous peak <= ~81 MiB
  leaves ~47 MiB headroom — watch the first scored run's `cgroup_peak`.
- `data_bytes` differs from slice 44 by 7,717 B across 338 files (WAL/segment sizing noise); the
  seed counts and openinfo match slice 44 exactly, so this is not a content change.
- 128 MiB is the smallest regime cap tested; lower was not attempted because construction cannot run
  below ~170 MiB.
