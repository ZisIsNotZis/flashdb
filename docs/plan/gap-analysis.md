# External performance gap — attribution from the saved M1 records (WP-5/M4)

Analysis only: no benchmark was run, no engine code edited. Authority: `../objective.md` §0 (KPI ladder, gap-attribution rule) and `../dev-loop.md` §Gap attribution. Classification is exactly (a) not learned enough / (b) defect / (c) architecture.

## 1. Records and revision

| alias | record |
|---|---|
| `Rf(b)` / `Rs(b)` / `Rp(b)` | `.scratch/82-controlled-m1/runs/b<b>/rung-200/run-{flashdb-declared,sqlite,postgres}.json` |
| `Wf(b,w)` / `Ws(b,w)` / `Wp(b,w)` | same directory, `windows-{arm}.jsonl`, window index `w` (0-19) |
| `FL(b)` | `.scratch/82-controlled-m1/runs/floor/{before,after}/floor.json` |
| `P82` / `I83` / `B0` | `.scratch/82-controlled-m1/report.md` / `.scratch/83-index-probe/issues.md` / `harness/flashdb_harness/baseline_0b.py` |
| `M3` | `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out` (rev `f38ec63` + the test edit) plus `.scratch/84-index-defect-mechanism/logs/head.out` and `I83` |

* **Corpus at 200 rps:** trace `6472cf46477e037b67820a4fa5a7da9d46e1241e6d8d33f89e10f89b22b8f93f` (4,000 requests, `B0` manifest, identical in all 15 rung-200 runs; rungs 50/100 carry `859b99a1…`/`583f9b4c…`). Instance digests: flashdb `67b3d10c…`, sqlite `46c6f1e1…`, postgres `7f9a4ea8…` (restore.sha256 in every run JSON).
* **Run ids:** 45 arm-runs = 5 blocks × 3 rungs × 3 arms; rung 200 is 15 arm-runs × 20 windows = 300 windows, 20,000 requests per arm (4,000 × 5). Every window is 1,000 ms.
* **Revision:** records introduced at `daf2db9`, report text at `51c69f3`; no run JSON carries a git revision (`unmeasured`; the run `spec.json` records argv/caps/snapshot but no code hash). Mechanism claims about `B0` were verified at HEAD `170b4ad`; the records predate that and the baseline file has not changed since `daf2db9` in the paths read here.
* **Machine state:** loadavg median 1.95 (max 4.10) during the measured phase, 90 boundaries (`P82` §2), not the quieter state of the device floor.

## 2. Per-request phase ledger at 200 rps

**Every reading below is the pre-fix access path** — the M1 records were taken before the derived index was built from the merged view, so the scan phase's cost here and the `(b) defect` classification it carries are historical, not current (`§2.1` gives the post-fix work, and the fix is measured in bytes: 42,240 -> 13,466 B/request). Reusing these phase costs as current would be wrong.

Per-arm, pooled over the 100 windows at rung 200 (5 blocks × 20):

| arm | windows/req | groups | req/group | window mean group size | max group | window service p50 (median of 5 block medians, range) | window service p99 (median of 100 window p99s) |
|---|---|---|---|---|---|---|---|
| flashdb-declared | 100 / 20,000 | 19,876 | 1.0062 | 1.0066 | 8 | **3,640 us** (3,594-3,859) | 8,229 us |
| sqlite | 100 / 20,000 | 19,806 | 1.0098 | 1.0111 | 9 | 3,438 us (3,381-3,750) | 6,738 us |
| postgres | 100 / 20,000 | 19,881 | 1.0060 | 1.0071 | 6 | 2,208 us (2,170-2,392) | 3,872 us |

Sources: `Rf/Rs/Rp(1..5)` for groups/size; every `W*(b,0..19)` for service. flashdb's median of all 100 window p50s is 3,726 us and sqlite's 3,446 us; the table uses the median of the 5 per-block medians, as `P82` §4 does.

**Group structure is effectively one request per durable commit on all three arms** (1.006-1.010 req/group): the batch cap 64 never binds, so every arm pays one barrier per request (`P82` §9.9; verified in `W*(b,w)` `groups` vs `requests` every window). SQL windows carry no `regions` and no `group_elapsed_us`, so per-region read work and per-group elapsed time are flashdb-only and `unmeasured` for the baselines.

**flashdb per-region read work**, summed over the 100 windows / 20,000 requests (`Wf(b,0..19)`, `regions` fields):

| region | count | bytes_read | bytes/read | mean_ns → us | reads/req |
|---|---|---|---|---|---|
| P\|Customer | 16,010 | 67,151,790 | 4,194 | 34.7 | 0.800 |
| P\|Stock | 16,975 | 52,027,665 | 3,065 | 25.7 | 0.849 |
| P\|StockMovement (scan) | 940 | 600,601,720 | 638,938 | 6,053.6 | 0.047 |
| U\|Backorder | 13,925 | 1,475,060 | 106 | 34.2 | 0.696 |
| U\|Customer | 16,010 | 69,177,700 | 4,321 | 76.5 | 0.800 |
| U\|Order | 13,925 | 2,416,260 | 174 | 35.6 | 0.696 |
| U\|Stock | 16,975 | 73,112,125 | 4,307 | 57.5 | 0.849 |
| **total** | **94,760** | **865,962,320** | **9,138** | — | **4.738** |

Read work averages **493 us/request** (865,962,320 B / 20,000; time = Σ mean_ns×count / 20,000). Excluding the 940 scans: 93,820 reads over 19,060 non-scan requests = 4.922 reads/req at 2,828 B/read, ~219 us/req at the same per-region means. The scan is 4.7% of requests but **69.4% of read bytes**.

**Decomposition of flashdb's 3,640 us median (what the records settle):**

| component | value | settled? |
|---|---|---|
| one `fdatasync` per commit group (barrier) | device floor p50 3,105.5 us (`FL(before)`) / 3,566 us (`FL(after)`); p99 9,821 / 9,905 us | yes, but from a **different session** (floor reused, `P82` §3) |
| read work | ~219 us/req at the non-scan means; ~493 us/req averaged with scans | yes, region telemetry |
| validation + apply + WAL append | 3,640 − barrier − read ≈ **0-0.3 ms implied**, never measured | **no** |
| barrier wait vs append vs apply | no per-phase timestamp exists | **no**; missing instrument = engine per-request phase timer (dispatch→validate→WAL append→fdatasync return→apply→reply), plus a `fdatasync` count and WAL bytes/req. `lag_breach_attribution reads= read_us=` is emitted only on breaches and ticket 82 did not keep stderr (`I83`) |

The subtraction is **bounded, not exact**: the floor was taken in a quieter session (loadavg 1.22-2.76 vs 1.95 median here), so a 0.5 ms session delta is inside the uncertainty and the implied validation/apply figure can be negative. The defensible statement is: **the barrier accounts for 85-98% of the median, reads for ~6%, and no phase timer exists to split the rest.**

### 2.1 The M3 record's ledger: the movement-scan phase, before and after the fix

The deliverable asks for a per-phase ledger for the M1 **and M3** records. §2 is M1's; this is M3's. The M3 record is a *mechanism* measurement on a rebuilt instance of the same shape (2,785 movements still in the serving memtable, 188 `find StockMovement where order_no` through `execute_request`), so its ledger is work per scan against the same floor, not a timed run.

| phase | before fix | after fix, rows in the memtable | after fix, rows published to a tile | floor | classification |
|---|---|---|---|---|---|
| index entries / movement rows | 0 / 2,785 | 2,785 / 2,785 | 2,785 / 2,785 | one entry per row | — |
| merged scans entered | 188 / 188 | 0 / 188 | 0 / 188 | 0 | **(b) defect, closed** |
| tile bytes per scan | 616,428 | 0 | **4,218** | one dependent 4 KiB page probe = 4,096 B | **(b), fixed**; 1.03x its floor |
| bytes/request over the M1 mix (0.047 scans/req + 13,268 non-scan) | 42,240 | — | **13,466** | 13,460 predicted (`§7` fix #1) | **(b), fixed**; 0.04% from prediction |
| scan time per scan | 5,902,888 ns mean (`I83`, from the M1 windows; 6,053.6 us in §2) | unmeasured | **unmeasured** | 89 us per dependent 4 KiB probe | **(b) by mechanism; the time is `unmeasured`** |

Sources: `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out` (rev `f38ec63` + the test edit, loadavg 21.43 on line 1; `arm=before/after/after-tiled` lines), `.scratch/84-index-defect-mechanism/logs/head.out`, and the M1 windows for the mix (§2). The derivation of 42,240 -> 13,466 is the README's own arithmetic (0.047 x 616,428 + 13,268 = 42,240; 0.047 x 4,218 + 13,268 = 13,466) and is reproduced in `§9`.

**Read the time column as historical.** The 5.9-6.05 ms/scan figure is the *pre-fix* phase from the M1 record; the fix is now measured in bytes and counters, not in time, because a post-fix timing needs a quiet box and this one was under loadavg 13-23. The ledger therefore says the phase's *work* is closed and its *cost* is `unmeasured` — which is exactly what M3 clause (b) and M5 have to decide, with the paired block in `.scratch/89-m3b-paired-200/` as the instrument.

## 3. Theoretical floor per phase

Constants from `../engine.md` (experiment 0a-redo): dependent point read 89 us (4 KiB) / 111-117 us (16 KiB) / 174 us (64 KiB); batched independent read 4.9 us/read at QD32; sequential bandwidth 827 MiB/s; quiet-device tail 1.26× mean. The session's own fsync floor is `FL(before/after)` p50 3,105.5/3,566 us, p99 9,821/9,905 us.

| phase | floor | arithmetic | applicability |
|---|---|---|---|
| barrier / commit | 3,105-3,566 us p50 | measured on the same box, one commit/request, no engine (`FL`) | applies to a one-request group; **not universal** — postgres measures 2,208 us (`Rp`), 0.9-1.4 ms *below* this floor, so the floor is pattern-dependent |
| non-scan point probes | 89 us per dependent 4 KiB read; 4.9 us batched | 4.922 reads/req → 438 us if fully serialized, ~114 us if one dependent + batch | **cannot be applied**: measured means are 25.7-76.5 us/read, below the 89 us device constant, i.e. page-cache hits or a different granularity. The cache-hit floor (CPU + syscall) is `unmeasured` |
| movement scan as a full scan | **not a bandwidth floor**: 98.9% of the scan's pages are page-cache hits (ticket 60, `.scratch/60-scan-readahead/issues.md` §Acceptance verdict), so the 6.05 ms is inline CPU, not device time; the 638,938 B / 827 MiB/s = 736.8 us figure is a lower bound that does not apply | one scan = 638,938 B, mostly cached | the operative floor is the indexed probe below |
| movement lookup via the declared index | **89 us/scan** (one dependent 4 KiB page) or 4.72 us/page at bandwidth | 4,096 B / 827 MiB/s = 4.72 us | the index does not exist for this region (`I83`) |
| 4 KiB page transfer | 4.72 us at 827 MiB/s | 4,096 / 867,172,352 B/s | bandwidth only; excludes one dependent-latency 89 us |

## 4. Residual and classification

**Session error band (derived, not a repeated control):** the floor is reused, and no same-session repeat of the device floor exists. The band is therefore taken from the b1..b5 per-block spread: p50 half-range/median = flashdb ±3.6% (132.5 us), sqlite ±5.4%, postgres ±5.0%. Scan band from per-block scan means 5,511.5-6,494.3 us: median 6,253.9 us, half-range ±7.9% (±494 us). This is **block-to-block repeatability, not a controlled noise estimate**; it is conservative for small residuals and far too narrow for the 573 ms event.

| phase | measured | floor | residual | band | class |
|---|---|---|---|---|---|
| barrier per request | 3,640 us total median; barrier share 3,105-3,566 us | 3,105-3,566 us (`FL`) | +0 to +535 us vs the floor; **+1,432 us vs postgres's measured 2,208 us** | ±132 us | **(c) architecture** for the barrier *count*: one durable barrier per request is the work itself, and flashdb sits on the device floor. The 1.43 ms gap to postgres is **(b) candidate**, mechanism not yet named |
| read work, non-scan | ~219 us/req; 25.7-76.5 us/read | 89 us dependent / 4.9 us batched — not applicable (cache) | `unmeasured` | — | **(c)** reads are required by the request shape and run at cache speed; no defect evidence |
| movement scan | 6,053.6 us/scan (100-window mean; per-block 5,511.5-6,494.3) at 100.7 MiB/s | 89 us indexed probe (the full-scan bandwidth floor does not apply: 98.9% cache hits, ticket 60) | **+5,964 us** vs the indexed probe; 68× | ±494 us | **(b) defect**: `build_order_no_index` scans tiles only, the region is memtable-only, so the sidecar is empty and every scan re-reads 624 KiB from cache and JSON-parses it (`I83`, `engine/src/index.rs`, `engine/src/readahead.rs:75-97`) |
| one ~573 ms group | 573,458 us window max group, `Wf(2,10)`; p50 of that window 152,081 us, p95 541,188 us, service p99 569,724 us, 178 groups/201 req | 9 scan reads × 5,902.9 us = 53,126 us + ~3,030 us for the other 2,627,693 B = ~56,156 us | **+517,302 us unexplained** | event itself is outside any band | **(b) defect** by elimination: it exceeds every phase floor by >500 ms, compaction bytes are 0, and no phase timer names the path. Mechanism `unmeasured` |

**The two 200 rps failures are separate.** (i) The **median per-request cost ~3.64 ms** is one barrier per request on the device floor — a throughput/architecture issue, not a correctness defect, and it is why the median is near sqlite and above postgres. (ii) The **~573 ms single-window group** is a rare stall: 8 of 100 windows at 200 rps have a max group >100 ms (`Wf(b,0..19)` `group_elapsed_us`: block maxima 475,967 / 573,458 / 583,421 / 404,334 / 431,950 us), and only 2 of them landed such that schedule-lag p99 breached 10 ms (`Wf(2,10)`, `Wf(3,10)`). Fixing the median does not fix the stall, and vice versa.

`group_elapsed_us` is the **maximum single-group elapsed time in the window** (a normal window is 9,950 us with ~199 groups; a sum over 199 groups at 3.6 ms would be ~720 ms, so it cannot be a sum).

## 5. Why the competitors are faster, per commit

All three run one commit per ~1.006 requests; the difference is what that commit writes and syncs. Settings read back from the runs, not assumed: sqlite `journal_mode=wal`, `synchronous=2 (FULL)`, `wal_autocheckpoint=0`, `journal_size_limit=-1`, `cache_size=-8192` (`Rs`, `durability_observed`; `B0:129-147,614-670`). postgres `fsync=on`, `synchronous_commit=on`, `shared_buffers=16MB`, `work_mem=4MB`, `maintenance_work_mem=16MB`, `wal_level=minimal`, `max_wal_senders=0` (`scripts/baseline-env.sh:86-101`; `P82` §1). flashdb `--group-requests 64`, one `fdatasync(WAL)` per group (`B0` docstring; `../engine.md` §Foreground commit).

| mechanism | flashdb | sqlite | postgres |
|---|---|---|---|
| commit unit | one WAL record with all P/U/R mutations of the block, applied to memtable, ack after barrier | `BEGIN IMMEDIATE`; per request: customer SELECT, order/backorder uniqueness, guarded stock UPDATE, movement+order INSERT; one `COMMIT` | one transaction per group; same four statements; `COMMIT` |
| WAL / page writes | one append record, no foreground page write | WAL frames for every dirty page; checkpoint off the measured path (`wal_autocheckpoint=0`) | WAL commit record; heap pages stay in `shared_buffers`, checkpoint off the commit path |
| fsync count | one `fdatasync` per group (measured indirectly: 1.006 req/group, `Rf`) | one `COMMIT`→WAL sync per group (documented sqlite behaviour; settings measured) | one synchronous commit flush per group (documented; `synchronous_commit=on` measured) |
| group commit | driver forms a group only from requests already due; size 1.006 → no amortization | same pacer, 1.010 → none | same pacer, 1.006 → none |
| movement scan | full 624 KiB region scan, no usable index (`I83`; measured region telemetry) | `stock_movement_order (order_no, movement_no)` index range scan (`B0:431`; measured code path, not separately timed) | same secondary index, planner range scan (documented; schema created by `B0:431`) |
| measured commit service p50 | 3,640 us (here) | 3,438 us (here) | 2,208 us (here) |

**Reading.** flashdb − sqlite = **+202 us** (1.06×), inside/near the ±132 us p50 band and explicable by one barrier plus one extra WAL record shape. flashdb − postgres = **+1,432 us** (1.65×), outside the band. The candidate differences that can carry 1.4 ms are postgres's smaller WAL volume (`wal_level=minimal`) and its commit path; `WAL bytes per commit` and `fsync count per commit` are **not in the run record** for any arm, so "postgres writes less WAL" is **inference from documented behaviour**, not a measurement here. sqlite losing to postgres by 1.2 ms with the same pacer is evidence the gap is in the commit path, not the client. The scan defect adds only ~284 us to the *average* request, so it explains the tail and a small part of the mean, **not** postgres's median advantage.

### 5.1 The reused floor is not a lower bound, and three named mechanisms

A bare `fdatasync` with **no engine** in the path measures p50 **3,105.5 us / 3,566 us** (`FL(before/after)`), yet postgres commits in **2,208 us** on the same box with `fsync=on` and `synchronous_commit=on` read back from the server. Postgres therefore beats the floor bench, so the floor is *not* a lower bound for this pattern. Consequences:

- The statement "flashdb's barrier is at the device floor" is **not established**. The defensible reading is that flashdb's one-barrier-per-commit **count** is architecture (category c), while its **+1,432 us per barrier versus postgres** is an unexplained defect candidate (category b) with no mechanism named yet.
- Three mechanisms to test, each with a distinct instrument:
  1. **WAL segment growth.** flashdb appends one 4 KiB frame per commit to a file that grows; postgres writes into preallocated 16 MiB WAL segments, so its `fdatasync` never pays extent/metadata allocation. Test: preallocate the WAL segment (`posix_fallocate`/`set_len` on a chunk) and re-measure one 200 rps block at `mean_group_size` ~1.0. Prediction if this is the mechanism: p50 falls from 3.64 ms toward 2.2-2.5 ms.
  2. **Sync count and bytes per commit.** Neither is recorded for any arm; the instrument is WP-3's `kpi` object plus an explicit per-arm sync counter. If flashdb issues one `fdatasync` and postgres issues one flush per ~1.8 commits (its own group commit across in-flight backends), the comparison is not barrier-for-barrier and the "same durability" premise needs restating.
  3. **Floor-benchmark validity.** Re-run `floor` with flashdb's own write pattern (append then `fdatasync`, group size 1) rather than the existing bench. If it reproduces 3.1-3.6 ms, the file pattern is the cost; if it drops to ~2.2 ms, the old floor measured a different operation and the bar shifts for every arm.

Neither (1) nor (2) can be settled from the saved records; both are cheap enough to run before any policy work, and (1) is the only candidate that could erase the whole median deficit.

## 6. Format forensics

**On disk**, per logical row (10.6M stock cells; 200k customers; 2.65M skus; ~10.8M primary documents):

| arm | numerator | bytes | bytes/row | source |
|---|---|---|---|---|
| flashdb | whole instance dir, 158 files, live WAL included | 1,686,590,701 | **159.1** | `Rf(1..5)` `instance_bytes`; `P82` §7 |
| flashdb | 4 largest tiles only | 43,628,088 | 4.1 (of the 159.1) | `Rf` `instance_files` (top-4 only, `B0:1519-1527`) |
| sqlite | db 542,330,880 + shm 163,840 + wal 0 | 542,494,720 | **51.2** | `Rs` `sqlite_files`, after the post-run `wal_checkpoint(TRUNCATE)` |
| postgres | `pg_database_size()` logical, WAL excluded | 1,103,713,983 | **104.1** | `P82` §7 |
| postgres | cluster data dir, WAL included (pre-load snapshot) | 2,201,362,806 | **207.7** | `Rf/Rs/Rp` `restore.bytes`; `P82` §7 |

The three numerators are different instruments (`P82` §7): flashdb's is a directory walk, sqlite's file+sidecars, postgres's either a catalog figure or a directory copy. **flashdb's WAL bytes, manifest bytes and any index tile are `unmeasured`**: `instance_files` keeps only the 4 largest files, all tiles, and the snapshot directory's bytes are git-ignored/absent here. What the record can say: tiles dominate (158 files averaging 10.7 MB; the footprint is 3.11× the sqlite database), and the format stores P/U/R entries with 4 KiB pages and per-page key bounds/checksums, against sqlite's fixed-schema B-tree pages and postgres's heap+two indexes. Which fraction of flashdb's 159.1 B/row is duplication vs page overhead vs WAL is **not attributable from this record**; instrument = per-category directory walk (tiles / index / WAL / manifest) at restore time, already named in `dev-loop.md` §Gap attribution step 3.

**In memory:** flashdb `memory.peak` per block is 65,990,656 / 65,880,064 / 65,830,912 / 66,129,920 / 66,170,880 B = **62.9 / 62.8 / 62.8 / 63.1 / 63.1 MiB** (`Rf(1..5)`), against a 1,686,590,701 B instance = **0.0391 resident B per data B** (about 25.6:1 data:RAM). The doc's in-memory candidates are memtable `BTreeMap<Vec,Vec>` overhead, fence RAM, page cache (kernel, outside the scope) and allocator arenas (`dev-loop.md`). `I83` records `serving_memtable_entries = 19,495` after serving; if all 62.9 MiB were entries that would be 3.4 KiB/entry, implausible for small inventory rows — so most of the peak is **not** attributable to entries, but the record has no breakdown. `unmeasured`; instrument = allocator stats or a heap profile inside the 128 MiB scope, plus `/proc/<pid>/smaps_rollup` for page-cache separation.

## 7. Ranked fix list (ranking is inference; facts named)

| # | fix | phase / class it addresses | predicted KPI effect (inference) | confirming instrument | cost |
|---|---|---|---|---|---|
| 1 | Build the derived index from the **merged** memtable+tile view and fail loudly on an empty build (`I83` WP-1) | movement scan / **(b)** | read bytes/request **43,298 → 13,460 (−69%)**; scan cost 6,053.6 us → about one 89 us dependent page probe; average read work −~280 us/req; removes the 4.7% request class that carries 69.4% of read bytes. **Efficiency KPI 1 first**, and likely removes the stall's trigger | `Wf` region `P\|StockMovement`: count ≈0.047/req, bytes_read ≈4,096/scan (vs 638,938); external bytes-read/request | WP-1 code + WP-2 5 paired 200 rps blocks (already scoped) |
| 2 | **Preallocate the WAL segment** (fix a chunk with `set_len`/`posix_fallocate`, append inside it) so a one-frame commit does not pay extent/metadata work on every `fdatasync` | barrier/commit / **(b) candidate** (§5.1 mechanism 1) | if the mechanism holds, per-barrier cost falls from 3,640 us toward postgres's 2,208 us — the median deficit to postgres closes without touching the barrier count. Derived from postgres beating the no-engine floor by 0.9-1.4 ms | service p50/p99 per window; `kpi.write_bytes` and a sync counter; the floor re-run with flashdb's own append pattern | small engine change; no durability or format change |
| 3 | Amortize the commit barrier by forming larger groups (respect the 10 ms lag ceiling), or pipeline barrier completion | barrier/commit / **(c)** | at mean group 4, barrier/request 3,105-3,566 → 776-892 us; predicted service p50 **3,640 → ~1.1-1.3 ms**, below both baselines; raises throughput headroom at the next rung. Derived from the measured one-barrier-per-request fact and the floor | `mean_group_size` in every window; a new `fdatasync` counter; service p50/p99; lag p99 to prove the ceiling holds | engine/pacer change, durability semantics unchanged; risk of latency breach while waiting to fill a group |
| 4 | Add the per-group phase timer (append / barrier wait / apply), keep stderr, then fix the ~573 ms stall | 573 ms group / **(b)** (mechanism currently unnamed) | removes the only cause of 2/5 rejected windows at 200 rps → **5/5 sustained**; service p99 403,213 → bounded by the device's 9,821 us p99. Derived from 8/100 windows >100 ms and 2 rejections | per-group phase split; `lag_breach_attribution` captured per rejected window; `group_elapsed_us` histogram | instrumentation low; fix cost unknown until the timer names the phase |
| 5 | Reduce resident overhead (memtable/arena) toward the 0.0391 B/B target | in-memory / **(c)** if entries, **(b)** if arena waste | no current KPI pressure (62.9 of 128 MiB); improves the data:memory ratio only. Low expected KPI effect now | heap profile / allocator stats vs `memory.peak` | medium; defer until 1-4 land |
| 6 | Record per-category footprint (tiles/index/WAL/manifest) and the code revision in every run | format forensics / evidence | none directly; makes §6 auditable and the floor comparable across sessions | `instance_files` schema change + `git rev` field | low; harness only |

Ranking logic: KPI 1 (efficiency) before KPI 2/3. #1 is the only fix that reduces measured **bytes read per request** and it also removes the scan latency. #2 is ranked second because it is the only candidate that closes the median deficit while touching nothing semantic, and because it is cheap enough to test before the group-size change that would move durability semantics. #3 and #4 improve KPI 2/3; #4's mechanism is unknown until instrumented.

## 8. What would falsify each attribution

| attribution | falsifier / reading that flips it |
|---|---|
| median is at the barrier floor **(c)** | a same-session, same-pacer floor p50 materially below 3.1 ms, or the phase timer showing barrier wait <2 ms with apply/append >1 ms → the cost is a defect **(b)**, not the barrier count |
| postgres's 1.43 ms advantage is commit-path/WAL volume **(b) candidate** | WAL bytes/commit and fsync count showing flashdb writes **no more** than postgres and syncs once → residual is scheduling/device **(c)**, or the records' session difference; the attribution would then revert to architecture |
| movement scan is a defect **(b)** | an indexed probe still measuring ≥5 ms (then the decoder/readahead, not index coverage, is the defect), or a 638,938 B scan measuring ≤737 us (then the current 6 ms is bandwidth-bound work, **(c)**); or `bytes_read` not equalling device bytes |
| 573 ms group is a defect **(b)** | the phase timer showing a single `fdatasync` itself taking ~570 ms → device/environment **(c)**, not a code path; or nonzero compaction/WAL-rotation bytes identifying background work |
| non-scan reads are at floor **(c)** | page-cache miss counters showing most probes are device reads: measured 25.7-76.5 us would then contradict the 89 us constant, invalidating the calibration for this session rather than proving a defect |
| **(a) not learned enough** | the declared layout is not at issue here: there is no learner arm and no oracle-best comparison in these records (`unmeasured`). It would be established only if an oracle candidate set beat declared by >band **while** each phase stayed at floor; no such run exists |
| in-memory overhead is entry cost **(c)** vs waste **(b)** | a heap profile showing arenas/fragmentation rather than live `BTreeMap` nodes; 3.4 KiB/entry for 19,495 entries makes pure-entry cost unlikely but does not prove waste |

## 9. Arithmetic provenance

All derived numbers above come from reads of the listed JSON/JSONL with `python3`; the exact commands are the heredocs run for this analysis: a per-window loader (`json.loads` per line) summing `regions[*][bytes_read|count|mean_ns]` and taking medians with `statistics.median`, plus `python3 -c "print(638938/(827*2**20)*1e6, 865962320/20000, 1686590701/10600000, 65990656/1686590701)"`, and the §2.1 mix `python3 -c "print(188/4000, 265360600/20000, 188/4000*616428+265360600/20000, 188/4000*4218+265360600/20000)"` (0.047 scans/request, 13,268 B/request, 42,240 -> 13,466). No number was copied from a source that did not also appear in a cited record; `P82` values are cited as report facts where the raw field is absent (postgres logical size, floor table).
