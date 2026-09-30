# Slice 10a — where write-path memory goes (MEASUREMENT ONLY)

Isolated worktree at `be33973`. One new file: `engine/examples/write_memory.rs`.
No engine behavior changed. **No bench-only inspection hooks were added** — the
example uses only public API (`Engine::{create,next_handle,commit_block,maybe_checkpoint,rotate_wal,compact,scan_all,serving_memtable_entries,csn}`,
`Memtable::{new,apply,len,scan_iter}`, `keys::{primary_key,key_csn}`, `wal::replay`).
Two private helpers were replicated in the example, both stated below.

Every number below is printed by the binary itself (`key=value`); ratios are
computed in-process. Peak is `VmHWM` after `echo 5 > /proc/self/clear_refs`
(resets the kernel high-water mark to the current RSS) or the max of per-block
`VmRSS` samples; `malloc_trim(0)` returns freed arena memory before a baseline.
One process per invocation (VmHWM is monotonic).

Build/run: `cargo build --release --offline --example write_memory` then
`./target/release/examples/write_memory <mode> ...`. Every command finished well
under 60 s (slowest: 31.8 s for window 64×7 cycles, 30.9 s for the 400k-doc bench
seed; memtable V=64 10.0 s; compaction 64×4 18.6 s; breakdown 1.5 s).

Workload is `PutDoc`-only unless stated, blocks of 1000 docs, one entity (`E`),
doc bodies `\x78` repeated to exactly V bytes. This isolates memtable +
verification state from the engine's O(n) per-op unique probe.

## 1. Memtable bytes per payload byte

64 MiB of doc payload per V (`docs = 64 MiB / V`), no tiles, no rotation.
`factor = peak-RSS-delta / doc payload bytes`; `factor_wal` uses WAL file bytes.

| V (B) | docs | blocks | payload B | WAL B | entries | base KiB | VmHWM KiB | Δ KiB | factor | factor_wal |
|------:|-----:|-------:|----------:|------:|--------:|---------:|----------:|------:|-------:|-----------:|
| 64 | 1,048,576 | 1049 | 67,108,864 | 86,017,788 | 1,048,576 | 2,272 | 215,252 | 212,980 | **3.2498** | 2.5354 |
| 256 | 262,144 | 263 | 67,108,864 | 71,836,025 | 262,144 | 2,272 | 105,280 | 103,008 | **1.5718** | 1.4683 |
| 1024 | 65,536 | 66 | 67,108,864 | 68,290,614 | 65,536 | 2,332 | 79,220 | 76,888 | **1.1732** | 1.1529 |
| 4096 | 16,384 | 17 | 67,108,864 | 67,404,310 | 16,384 | 2,268 | 76,744 | 74,476 | **1.1364** | 1.1314 |

Raw lines:

```
mode=memtable value_bytes=64 docs=1048576 blocks=1049 payload_bytes=67108864 wal_bytes=86017788 memtable_entries=1048576 ram_base_kb=2272 ram_hwm_kb=215252 ram_delta_kb=212980 factor=3.2498 factor_wal=2.5354
mode=memtable value_bytes=256 docs=262144 blocks=263 payload_bytes=67108864 wal_bytes=71836025 memtable_entries=262144 ram_base_kb=2272 ram_hwm_kb=105280 ram_delta_kb=103008 factor=1.5718 factor_wal=1.4683
mode=memtable value_bytes=1024 docs=65536 blocks=66 payload_bytes=67108864 wal_bytes=68290614 memtable_entries=65536 ram_base_kb=2332 ram_hwm_kb=79220 ram_delta_kb=76888 factor=1.1732 factor_wal=1.1529
mode=memtable value_bytes=4096 docs=16384 blocks=17 payload_bytes=67108864 wal_bytes=67404310 memtable_entries=16384 ram_base_kb=2268 ram_hwm_kb=76744 ram_delta_kb=74476 factor=1.1364 factor_wal=1.1314
```

**Finding 2 quantified.** The factor falls from 3.25× at 64 B docs to 1.14× at
4096 B docs: `BTreeMap<Vec<u8>,Vec<u8>>` has roughly a fixed ~140-180 B per entry
(24 B key allocation + node pointer/slot + malloc rounding), plus one value
buffer per entry. At the scaled run's 1 KiB docs the single-copy factor is
**1.17×** (0.177 B of RAM per payload byte). The 4096 row (1.136×) shows the
same fixed per-entry cost fully amortized; it is a touch above a pure
`value + fixed` model because one in-flight block encodes 4 MiB (1000×4096) of
ephemeral payload/ops/frame at the sampled peak. So for 1 KiB docs the remaining
per-copy overhead is only ~17%; the bytes are dominated by the *number of
copies*, below.

## 2. Working set vs window size

`maybe_checkpoint(W)` + `rotate_wal(W)` after every block, exactly as the bench
does; 1 KiB docs; one process per row. `delta = VmHWM − base`, all deltas in KiB;
`delta_over_window` is bytes/bytes.

| W MiB | cycles | blocks | payload B | tile B | Δ KiB | peak/W | peak/payload | peak/disk |
|------:|-------:|-------:|----------:|-------:|------:|-------:|-------------:|----------:|
| 8  | 3 | 27  | 27,648,000  | 29,002,155  | 52,628  | 6.424 | 1.9492 | 1.8577 |
| 16 | 3 | 51  | 52,224,000  | 54,770,913  | 97,080  | 5.925 | 1.9035 | 1.8147 |
| 32 | 3 | 99  | 101,376,000 | 106,308,225 | 187,780 | 5.731 | 1.8968 | 1.8086 |
| 64 | 3 | 195 | 199,680,000 | 209,383,053 | 369,636 | 5.640 | 1.8956 | 1.8077 |
| 64 | 7 | 455 | 465,920,000 | 488,560,457 | 377,504 | 5.760 | 0.8297 | 0.7912 |

Raw lines (cgroup line elided):

```
mode=window window_mb=8 cycles=3 blocks=27 payload_bytes=27648000 tile_bytes=29002155 wal_bytes=0 disk_bytes=29010347 ram_base_kb=2268 ram_peak_sampled_kb=54896 ram_hwm_kb=54896 ram_delta_kb=52628 delta_over_window=6.424 delta_over_payload=1.9492 delta_over_disk=1.8577 elapsed_s=2.051
mode=window window_mb=16 cycles=3 blocks=51 payload_bytes=52224000 tile_bytes=54770913 wal_bytes=0 disk_bytes=54779105 ram_base_kb=2332 ram_peak_sampled_kb=90704 ram_hwm_kb=99412 ram_delta_kb=97080 delta_over_window=5.925 delta_over_payload=1.9035 delta_over_disk=1.8147 elapsed_s=2.981
mode=window window_mb=32 cycles=3 blocks=99 payload_bytes=101376000 tile_bytes=106308225 wal_bytes=0 disk_bytes=106316417 ram_base_kb=2268 ram_peak_sampled_kb=187036 ram_hwm_kb=190048 ram_delta_kb=187780 delta_over_window=5.731 delta_over_payload=1.8968 delta_over_disk=1.8086 elapsed_s=4.761
mode=window window_mb=64 cycles=3 blocks=195 payload_bytes=199680000 tile_bytes=209383053 wal_bytes=0 disk_bytes=209391245 ram_base_kb=2276 ram_peak_sampled_kb=371912 ram_hwm_kb=371912 ram_delta_kb=369636 delta_over_window=5.640 delta_over_payload=1.8956 delta_over_disk=1.8077 elapsed_s=9.184
mode=window window_mb=64 cycles=7 blocks=455 payload_bytes=465920000 tile_bytes=488560457 wal_bytes=0 disk_bytes=488568649 ram_base_kb=2268 ram_peak_sampled_kb=379772 ram_hwm_kb=379772 ram_delta_kb=377504 delta_over_window=5.760 delta_over_payload=0.8297 delta_over_disk=0.7912 elapsed_s=31.772
```

**Hypothesis: peak ≈ 8-10× W and roughly independent of total data.**
- *Independence from total data: CONFIRMED.* Same W=64: 3 cycles (200 MB data)
  gives peak/W = 5.640; 7 cycles (466 MB data) gives 5.760. The peak tracks W,
  not the data (`peak/payload` halves as data doubles).
- *8-10×: REFUTED for the publish+rotate path alone.* Measured 5.64-6.42× W
  (peak/W is even slightly *smaller* at larger W, the opposite of growing). The
  publish path holds 4 live memtable-sized copies (see §3), giving ≈4.4×W plus
  allocator/block ephemera.

The full bench driver reproduces a much larger peak because it also compacts:

```
$ ./target/release/flashdb-scale seed <tmp> 400000 1000 1024
phase=seed ok=true docs=400000 csn=400 wal_bytes=30381344 rss_kb=761628 rss_peak_kb=1055136 blocks=400 elapsed_s=30.794
```

`rss_peak_kb = 1,055,136 KiB = 1030 MiB ≈ 16× W` for W=64 at only 400 k docs;
`rss_kb` plateaus at 761,628 KiB = 744 MiB. So the scaled run's ~600 MB plateau
is *not* the publish path — it is the compaction path (§4).

## 3. Where the peak comes from — W = 32 MiB

One window committed (32,768 docs, payload 33,554,432 B). Order: (d) live
memtable, (c) projection copy, (a) `wal::replay` records, (b) `expected`
memtable built on top of the held records. `malloc_trim` between phases.

| step | entries | bytes | rss_before KiB | rss_after KiB | Δ KiB | Δ/payload |
|------|--------:|------:|---------------:|--------------:|------:|----------:|
| (d) live memtable | 32,768 | — | 2,272 | 39,752 | 37,480 | 1.144 |
| (c) newest_projection(live mt) | 32,768 | — | 39,752 | 77,096 | 37,344 | 1.140 |
| (a) `wal::replay` records | 33 recs | records_bytes 34,145,038 | 39,820 | 73,232 | 33,412 | 1.044 of records |
| (b) expected memtable (over records) | 32,768 | key_value 34,177,024 | 73,232 | 110,444 | 37,212 | 1.136 |
| peak, all live | | | 39,752 | VmHWM 110,448 | 70,696 over live | 2.16 over live |
| freed all (`malloc_trim`) | | | | 39,828 | | |

Raw lines:

```
mode=breakdown step=live_memtable w_mb=32 entries=32768 rss_before_kb=2272 rss_after_kb=39752 delta_kb=37480
mode=breakdown step=newest_projection entries=32768 rss_before_kb=39752 rss_after_kb=77096 delta_kb=37344
mode=breakdown step=wal_replay records=33 records_bytes=34145038 wal_valid_len=34145302 rss_before_kb=39820 rss_after_kb=73232 delta_kb=33412
mode=breakdown step=expected_memtable entries=32768 key_value_bytes=34177024 rss_before_kb=73232 rss_after_kb=110444 delta_kb=37212
mode=breakdown peak step=all_live rss_hwm_kb=110448 rss_base_kb=39752 peak_delta_kb=70696
mode=breakdown step=freed_all rss_end_kb=39828 rss_live_kb=39752
```

**Diagnosis.** Every full-window artifact costs ≈1.14×W: the live memtable
(37,480 KiB), a `newest_projection` copy (37,344 KiB), and an `expected`
memtable (37,212 KiB). `wal::replay`'s `Vec<Vec<u8>>` costs ≈1.0× of the WAL
payload bytes (33,412 KiB for 34,145,038 B). `write_verified_tile` holds three
of the four at once — live mt + records + `expected` — and then one projection
of each (`Tile::write` consumes `&newest_projection(&self.mt,…)` and, after that
temporary drops, `verify_projection(&newest_projection(&expected,…))`). That is
≈4.4×W, which is exactly the publish-path peak measured in §2 (5.6-6.4×W with
ephemera). The bytes are **copies, not the memtable layout** — the memtable
factor itself is 1.14.

Replication note (private helper unreachable): `newest_projection` is private in
`engine.rs`. The example reproduces it in `reduce_step` over
`Engine::scan_all`, which with no published tiles yields precisely the live
memtable entries in the same key order that `Memtable::entries()` feeds the
original: keep the first (CSN-descending) version per logical key (full key
minus its trailing 8-byte `~csn`) within `(lower, cutoff]`. The (b) memtable is
built by applying `keys::primary_key(entity, handle, csn)` + the 1 KiB value for
each committed doc; the workload is `PutDoc`-only, so this is byte-identical to
`decode_payload` + `apply_op` of those records (no private codec duplicated).

## 4. Compaction peak vs tile bytes

Build N tiles of W via checkpoint+rotate, then drop to baseline and reset
`VmHWM` immediately before `compact()`.

| W MiB | tiles | tile B before | tile B after | base KiB | RSS post KiB | VmHWM KiB | peak Δ KiB | peak/tile B |
|------:|------:|--------------:|-------------:|---------:|-------------:|----------:|-----------:|------------:|
| 32 | 4 | 141,744,300 | 141,731,925 | 6,156 | 454,992 | 460,464 | **454,308** | **3.282** |
| 64 | 4 | 279,177,404 | 279,164,961 | 9,972 | 893,348 | 905,496 | **895,524** | **3.285** |

Raw lines:

```
mode=compaction window_mb=32 tiles=4 tile_bytes_before=141744300 tile_bytes_after=141731925 wal_bytes=0 ram_base_kb=6156 ram_post_kb=454992 ram_hwm_kb=460464 peak_delta_kb=454308 peak_over_tile_bytes=3.282 elapsed_s=2.067
mode=compaction window_mb=64 tiles=4 tile_bytes_before=279177404 tile_bytes_after=279164961 wal_bytes=0 ram_base_kb=9972 ram_post_kb=893348 ram_hwm_kb=905496 peak_delta_kb=895524 peak_over_tile_bytes=3.285 elapsed_s=4.580
```

**Finding 4 quantified.** `write_compacted_tile` materializes **every tile entry**
into one `expected` memtable, then builds `newest_projection` of it — peak ≈
**3.28× total tile bytes**, and the peak is ~2.3-3.3× the two-copy estimate
because block decode buffers and `BTreeMap` node churn add to the arena. Critical
detail: `ram_post_kb` ≈ the peak (454,992 vs 454,308; 893,348 vs 895,524). glibc
does **not** return the arena afterward, so the process **stays resident at the
compaction peak** — this is the observed RSS plateau, not a transient.

Reconciling the bench: the seeded run compacts 4 tiles of ~64 MB = 256 MiB;
3.285 × 256 MiB ≈ 841 MiB + prior arena + live state ≈ the measured 1030 MiB.
So the 08-era "600 MB plateau at 64 MB window" is the compaction peak (finding 4),
with the publish-path copies (finding 1) as the second contributor.

## 5. Ranked shortlist (by measured bytes saved)

1. **Stream the compacted tile (finding 4) — biggest, and the cause of the
   plateau.** Measured peak = **3.28× total tile bytes** (895,524 KiB for
   279,177,404 B of tiles at 64×4; 454,308 KiB for 141,744,300 B at 32×4), and
   RSS **stays** there (`ram_post_kb` 893,348 / 454,992). Removing the
   `expected` all-tile materialization and its `newest_projection` copy saves
   ≈2.28× tile bytes (≈600 MiB at 4×64 MB) and turns compaction O(window) instead
   of O(total tile bytes).
2. **Single materialization in `write_verified_tile` (finding 1).** Publish peak =
   **5.64-6.42× W**; each redundant memtable copy is **1.14× W** (measured
   37,344 KiB at W=32, 37,212 KiB for `expected`). Dropping `expected` + the
   projection copy saves ≈2.3× W (≈74 MiB at W=32); streaming the suffix instead
   of holding `wal::replay`'s records removes another ~1.0× W (33,412 KiB at
   W=32). Combined ~3.3× W (≈210 MiB at W=64), bringing publish near the 1.14×
   single-copy floor.
3. **Memtable layout (finding 2) — per-copy, smaller for 1 KiB docs.** 1.1732×
   at 1 KiB docs (only ~0.177 B overhead per payload byte) vs **3.2498×** at 64 B
   docs. Replacing `BTreeMap<Vec,Vec>` with an arena/sorted-Vec saves the ~17%
   per-copy overhead at 1 KiB (~9 MiB per copy at W=64) and up to ~65% for small
   docs. Lowest leverage for the 1 KiB scaled run; highest if small docs matter.

## 6. Method caveats

- Numbers are **process RSS** (`VmRSS`/`VmHWM` from `/proc/self/status`). WAL and
  tile files are written with `write(2)`, not `mmap`, so their page cache is not
  in process RSS.
- **The cgroup does count page cache, and is readable here.** Normal runs see the
  whole terminal scope at
  `/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-ghostty-surface-transient-6531.scope`
  (`memory.current` 2,760,388,608 B; `memory.peak` 4,498,001,920 B; `memory.max`
  `max`) — shared with the terminal, so not attributable. Under a dedicated
  `systemd-run --user --scope -p MemoryMax=1G` the example's own scope was
  `…/run-r880f7e4add544912a454a1a2a1e71ccc.scope` with `memory.max=1073741824`;
  for the W=32 window run whose process VmHWM delta was 187,616 KiB (192 MiB),
  the scope reported `memory.peak = 335,118,336 B = 320 MiB` — ~128 MiB more,
  i.e. page cache for the 106 MB of WAL/tile files the run wrote. The scaled
  run's 1 GB cap was therefore partly page cache, not only process RSS.
- Ratios read `mode=cgroup` and per-block `VmRSS` samples; the `Δ` and `factor`
  fields are the binary's own output, not arithmetic on other runs. Deterministic
  workload (constant doc bodies), single-writer engine.
- Compaction peak is `VmHWM − base` with the HWM reset immediately before
  `compact()`; the ~2.3-3.3× spread above the two-copy estimate is allocator
  arena growth, which is also why RSS stays at the peak afterward.

## 7. Verification (run on this revision)

```
$ cargo test --workspace --offline
test result: ok. 66 passed; 0 failed; ...   (flashdb-engine unit)
test result: ok. 4 passed; 0 failed; ...    (engine/tests/*)
test result: ok. 6 passed; 0 failed; ...
test result: ok. 13 passed; 0 failed; ...
test result: ok. 7 passed; 0 failed; ...
test result: ok. 5 passed; 0 failed; ...
test result: ok. 3 passed; 0 failed; ...
test result: ok. 1 passed; 0 failed; ...
test result: ok. 2 passed; 0 failed; ...
test result: ok. 11 passed; 0 failed; ...
(test result: ok. 0 passed ×4 for empty targets)
=> 118 Rust tests, 0 failed

$ git diff --check
(no output)
```
