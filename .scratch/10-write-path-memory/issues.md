# 10 — write-path memory discipline (blocks the data:RAM claim)

Opened 2026-09-30 from the second scaled run (2M docs × 1 KiB, `MemoryMax=1G`,
systemd scope, page cache counted by the cgroup).

## Result of that run

The 08a-08c read-path work removed the old cliff completely:

| | before 08a-08c | after |
|---|---|---|
| seed progress | 200 k docs then stalled ~9 min | 800 k docs in **29 s** (~27.6 k docs/s) |
| outcome | never finished | **SIGKILL (exit 137)** at `rss_peak_kb=600132` |

**CORRECTED 2026-09-30 by slice 10a's measurements (see `measurements.md`):** the
publish+rotate path is **5.6-6.4× the window W** (independent of total data, confirmed at
200 MB and 466 MB), not the 8-10× guessed here; and the 600 MB plateau is the
**compaction** path, not publish — `write_compacted_tile` peaks at **3.28× total tile
bytes** and its RSS **stays resident afterwards** (the arena is not returned). The bench
reproduces the failure exactly: 400 k docs → `rss_peak_kb=1055136`.

RSS trajectory (seed progress lines): 200 k docs → 134 MB, 400 k → 573 MB,
600 k → 596 MB, 800 k → 551 MB (peak 600 MB). It **plateaus**: the working set is
not proportional to data, it is proportional to the WAL/checkpoint window (64 MB in
this run) times a large constant. With ~8-10× that window the process alone reaches
600 MB; the cgroup counts page cache too, so a 1 GB cap kills it. Data:RAM at that
point was ~250 MB data : 600 MB RAM — the opposite of the ≥20:1 objective.

## Findings to fix (measured, code-traced)

1. **The publish/verify path keeps several full materializations of the WAL window.**
   `write_verified_tile` (engine.rs:346) does `wal::replay` (all suffix payloads as
   `Vec<Vec<u8>>`), builds an `expected` `Memtable` from every record
   (`apply_op` per op), then `newest_projection(&self.mt, …)` builds *another*
   `Memtable` copy of the same content, and `Tile::write` materializes each block. At
   least three live copies of one window; per-op `Vec` keys/values in a `BTreeMap` add
   several times the payload bytes.
2. **`Memtable` overhead is unquantified.** `BTreeMap<Vec<u8>, Vec<u8>>` with
   per-entry allocations has a bytes-in-RAM per byte-of-data factor that nobody has
   measured. The 20:1 claim needs that number stated (and lowered).
3. **Rotate/checkpoint thresholds are the memory knob.** Since the working set is ∝
   window, `rotate_wal(max_wal_bytes)` and `maybe_checkpoint(max_wal_bytes)` implicitly
   set peak RAM; today they are ad-hoc call-site values (64 MB in the bench), and
   nothing enforces or documents a memory budget.
4. **Reopen verification is also O(data) in RAM.** `open_impl` builds `covered` and
   `mt` memtables while replaying the suffix and projects per tile
   (`newest_projection`) — bounded by the suffix for the WAL side, but
   `write_compacted_tile` (segment mode) additionally materializes *every tile entry*
   into one `expected` memtable before reducing it, i.e. RAM ∝ total tile bytes at
   compaction time. That is the same defect class as (1) and must be fixed in the same
   pass (stream the k-way tile merge instead of materializing it).

## Measured breakdown (10a, `measurements.md`)

| artifact | bytes |
|---|---|
| live memtable, one window | 1.14×W |
| `newest_projection(live)` | 1.14×W |
| `expected` memtable over replayed records | 1.14×W |
| `wal::replay` records | 1.04×WAL payload |
| publish peak (all of the above live) | 5.6-6.4×W |
| compaction peak (`write_compacted_tile`) | 3.28× total tile bytes, resident after |
| Memtable factor, 1 KiB docs | 1.17× payload (only 0.177 B overhead/byte) |
| Memtable factor, 64 B docs | 3.25× payload |

So the measured ranking is: **stream the compacted tile (2.28× tile bytes)** >
**single materialization in `write_verified_tile` (~3.3×W)** > memtable layout (17% at
1 KiB docs, up to 65% for tiny docs). Also measured: under a 1 G cgroup scope the
cgroup's `memory.peak` exceeded the process delta by ~128 MiB for 106 MB of files
written, i.e. the cap was partly page cache — the cgroup view and the process view must
be reported separately.

## Direction

- Single materialization rule: the serving memtable is the only full copy of a window;
  tile write and verification stream against it plus a bounded, key-grouped view of the
  WAL suffix (the suffix is already bounded by the rotate threshold).
- Verification should not rebuild the projection from the WAL in RAM: stream the tile
  being written (block by block) against the same source, and keep the WAL digest as the
  integrity anchor.
- Quantify and reduce `Memtable` bytes per payload byte (measured: 1.17× at 1 KiB docs,
  3.25× at 64 B docs; arena/sorted-Vec is the candidate, lowest leverage at 1 KiB).
- Make released memory actually return: compaction's peak stays resident, so peak RSS
  today is also the steady-state cost after any compaction.
- Make the window size an explicit, documented memory budget, then re-run the scaled
  experiment and report data:RAM and peak RSS vs window.

## Acceptance for closing

A scaled run at ≥ 10:1 data:RAM completes under a cgroup cap with peak process RSS
reported, and the reported RSS is consistent with the stated per-window budget (with the
multiplier documented, not asserted).

## Slice log

- **10a DONE** (`34d17a9`, worker `c79ec36d`): measurements above. Corrected this ticket's own
  numbers (publish+rotate = 5.6-6.4×W, not 8-10×; the plateau is compaction at 3.28× tile
  bytes and stays resident). Levers ranked by measured bytes.
- **10b = streaming `TileWriter`** (prerequisite for streaming compaction): first attempt
  (worker `0ac81316`) produced **zero tool calls in 25 minutes** — the events log is a chain
  of `auto_retry_start`, i.e. a provider/model failure, not a task failure; nothing to
  recover. Re-dispatched as `f5bc2afe` with `github-copilot/gpt-5.4` and a narrower scope
  (byte-identity proof; RSS measurement dropped). Acceptance: files produced through the
  streaming API must be byte-identical to `Tile::write` output.
- **10b DONE** (`c185575`, parent-executed): `TileWriter` streams blocks (buffers only the
  in-progress 4 KiB block), `push` enforces strictly increasing keys, `finish` writes
  directory/trailer/superblock-last and self-checks; `Tile::write` is now a thin wrapper so
  there is one encoder (format bytes unchanged). Byte-identity proven by test for a
  3000-entry multi-block tile plus empty/exact-fill/spill/oversized/out-of-order cases.
  Rust 122 (+4). Parent executed it because three worker dispatches produced zero tool calls
  (two provider failures: `litellm/volcengine` retry loop, then `github-copilot` 429).
- **10c (next)**: switch `write_verified_tile` and `write_compacted_tile` to the
  streaming writer with a bounded merge, i.e. no full materialization: target compaction
  peak ≤ ~1.5× tile bytes and publish peak ≤ ~2×W, then re-measure with `write_memory`'s
  protocol and re-run the scaled experiment.

## Measurement protocol (fixed after the first scaled run)

A `MemoryMax` cap counts page cache, so a run can be killed while the process RSS is well
below the cap. `scripts/scale-run.sh` now reports both views per phase:
`rss_peak_kb=` (process, from the binary's own progress lines) and
`cgroup_peak_bytes=` (the scope's own `memory.peak`, read from inside the scope after the
binary exits). Neither number alone is evidence; always report the pair. Smoke check:
4000 docs under `MemoryMax=256M` gives rss_peak 5572 KiB vs cgroup peak 5,857,280 B.

## Gate rule added (2026-09-30)

Two failure classes have now escaped a gate that only asserts on `test result:` lines:
a `-q` grep that hid a compile break (recorded in ticket 07), and an **inert fix** hidden by
a rustc warning. While landing 10c-2, `cargo build` reported
`warning: unreachable pattern` at `engine.rs:582` - the Err arm of the rotation rebinding
match was dead code because the Ok case was bound with an irrefutable `ok` pattern, so the
P1-2 fix from `19026c3` ("set `wal_detached` on rebinding failure") had never executed. No
test covers that path (it needs an I/O failure inside `rotate_wal`), so only the compiler
could have caught it.

Gate for every slice from now on: (1) `cargo test --workspace --offline` with the
`test result:` lines reported verbatim, **and** (2) `cargo build --workspace --offline`
reporting **zero warnings** (or an explicit statement of any that remain and why they are
acceptable), plus (3) `git diff --check`.

## 10d — scaled re-run in progress (2026-09-30)

Parameters: 10,000,000 docs x 1 KiB (~10.4 GB of data), 5000 docs/block,
rotate/checkpoint window 64 MiB, `MemoryMax=1G` systemd scope, phases seed →
verify → scanbench(2 passes). Purpose: produce the **data:RAM ratio** with the memory
discipline now in place, reported as a pair (process `rss_peak` vs the scope's
`memory.peak`, since the cap counts page cache).

Early progress (first minute): 500 k docs, `rss_kb=205,040`, retained WAL bounded at
48-60 MiB. Compare with the pre-fix run, which stalled at 200 k docs with RSS climbing
to 600 MB and was then SIGKILLed.

Expected on completion: RSS roughly `2.3 x W` (~150-210 MB) plus the sparse directory
index (0.20 MiB per GiB of tiles, ticket 11), i.e. data:RAM well above 20:1 at this
scale rather than the ~0.4:1 the old implementation produced.

Reporting rules for the result: state data bytes on disk, process peak RSS, cgroup peak,
the ratio of each, the window W, and the caveat that the ratio is a function of W (the
working set is window-bounded, not data-bounded) rather than a constant of the design.
