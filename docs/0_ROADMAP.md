# EXPLICIT

## Decisions (2026-10-19, delegated author authority)

The author delegated every open decision before leaving. Recorded here so no later session reopens
them without new evidence:

1. **The external bar replaces T6.** "Win" is the `2_GOAL.md` §0 ladder: efficiency no worse with
   one strict gain, then sustained level at least the best baseline's, then tail latency reported. The
   3x ratio is withdrawn as structurally unpassable. `.scratch/STATUS.md` §5(b) is thereby closed;
   the learner's 15% / 5% / 10-seed thresholds stand for T1-T4 exactly as pre-registered.
2. **§7.1 is closed.** Schedule-lag p99 <= 10 ms is the throughput-feasibility rule; the service
   ceiling is reported, not used as a veto, and counts against the ladder only when a baseline holds
   the same level without breaching.
3. **M2 structural proposal accepted.** No general L2 co-location in v0. The narrow `star_fanout`
   case stays a conditional future item, reopened only if a real star-schema workload makes it worth
   the engine contract change. The three missing engine capabilities are therefore **not** planned.
4. **The baseline setup is un-handicapped.** sqlite keeps its default WAL autocheckpoint; postgres is
   tuned as a competent operator would tune it inside the same memory budget; both are driven
   open-loop. Any arm that fails this is invalid and is re-run, not reported.

## Non-goals for v0 (explicit)

- Multi-node operation, online SQL, the full 128 GiB / 8 GiB regime, and tuning any baseline into a
  race.
- General L2 co-location (decision 3); a claim about it before it is measured on a real run.
- Treating the simulator's verdict as evidence before the <=25% p90 per-region calibration gate.
- Any performance claim from an arm that fails the fair-setup rules in `2_GOAL.md` §0.

# CONFIRMED

## Decisions (2026-10-21, the node that now owns this roadmap)

Taken by the layer above and relayed as a contract answer; recorded here because they change what this
roadmap's own text means.

5. **M3's clause (c) is the roadmap's definition, not the workplan's paraphrase.** (c) is
   `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged, which is
   locally decidable. The efficiency KPI comparison against sqlite and postgres is **M5's** work, and
   `docs/0_ROADMAP.md` §M3 status carried a stale restatement of it. Recorded as assumed by the
   layer above; reversing it would move M3 from "awaiting a quiet box" to "needs postgres too", which
   this roadmap's own text does not support.
6. **The claim splits: mechanism now, throughput in a window.** "Is the defect fixed" is work done
   per request — counted from the engine's own counters, invariant under foreign load, and measured at
   ticket 84. "Does it now beat sqlite and postgres at 200 rps" is a rate and needs a quiet box; the
   window is granted on a trigger in decision 7, which replaces this decision's "deferred" with
   `unmeasured`. No throughput number is claimed until a block runs, and the comparison may not be
   taken early under load — the twelve invalid `.scratch/83-index-probe/runs/` probe files are the
   measured cost of doing that.

7. **The quiet window is granted on a trigger, and (b) may not be deferred a third time.** The layer
   above granted one exclusive window in root's contract (`e5fb0e1:inbox/2026-10-21-next-contract.md`,
   deleted after landing): the trigger is `uptime` reading loadavg under 2 for one minute with no other
   agent writing in this tree, and the reading the block started at is recorded in the log. The pair
   lives in `.scratch/89-m3b-paired-200/`. What the grant changes: clause (b) is `unmeasured` until a
   block runs — not "deferred", which names a state where nobody is expected to measure it — and its
   instrument (the paired block, the regeneration chain, the trigger) is written where the clause is.
   Everything else in the tree runs under load and does not wait for the window.

8. **The postgres leg runs from a container, and every result names the version it measured.**
   Root's answer of 2026-10-23 (`mail/2026-10-23-answer-postgres-present.md`) reads this box as
   carrying **postgres 16.15 (Debian 16.15-1.pgdg13+2)** in container `jolly_lamarr` on 5432 and no
   18.x, and forbids installing a system package. The M5 arm therefore comes from a docker image
   (`docker pull postgres:18` for the major version ticket 82 measured; a second container on 5433
   with the M1 record's settings block, read back from the running server), and
   `.scratch/90-m5-postgres-env/postgres-server.sh` is the one command that starts and checks it.
   No document inherits the 18.6 conclusion for a server of another version, and an environment
   fact is written with the moment it was read and the method that read it.

# INFERRED

The plan is milestone-based, not feature-based: each milestone is an ordered user gate that delivers
a vertical slice with a decidable done-criterion. The product bar and the KPI ladder are owned by
`2_GOAL.md` §0; the measurement method and the gap-attribution protocol by `9_IMPLEMENTATION.md`; the
pre-registered falsification contract (`.scratch/15-objective-v0/pre-registration.md`) is the fixed
acceptance contract for the *learner* claim (T1-T4) only. No pre-registered learner threshold is
changed here.

## Goal and done-criterion

- Goal: prove or reject that a per-region layout policy chosen by observing data and access patterns
  sustains a higher offered load than a fixed declared policy, without losing on a no-locality
  workload, without buying the win with unbounded reorganisation and with drift recovery — **and**
  that the engine as a whole wins the external comparison, in the ladder's order (`2_GOAL.md` §0).
- Done: the external comparison resolves to **win** under `2_GOAL.md` §0 on one reasonably
  specified machine, the learner verdicts T1-T4 are evaluated on a >=10x instance with the
  pre-registered aggregation over 10 seeds, both are independently reviewed, and the result is
  published. A rejected thesis, or an attributed architecture gap with a published negative result,
  is a valid outcome.

## Where we are (measured)

- **External (M1, ticket 82):** T5 PASS (deficit 1.001), T7 PASS, **T6 measured and withdrawn** as
  structurally unpassable. At 200 rps flashdb-declared sustained 3/5 blocks while sqlite and postgres
  (**18.6, Ubuntu 18.6-3build2**) each sustained 5/5 (highest-level ratio 0.5x); the postgres version
  is the one that arm read from its running server, and a claim from another version names it. The
  external-superiority claim is **not supported at this revision**, and the cause is now diagnosed
  (ticket 83): the derived `order_no` index is built from tiles only and was empty on this workload,
  so every scan request paid a 5.6 MB full scan.
  That defect is fixed and the fix is **measured, not argued** (ticket 84): on a rebuilt instance of
  this record's shape the index holds 2,785 entries for 2,785 rows against 0 before the fix, 0 of 188
  scan requests enter the merged scan, and read bytes per request fall 42,240 → 13,466 against the
  predicted 43,298 → 13,460. **The 200 rps comparison itself is `unmeasured`** (decision 7): the window
  is granted on a trigger (loadavg under 2 for one minute, no other agent writing in this tree) and no
  paired block has run, so no throughput number is claimed anywhere; the arms and the chain that
  regenerates what they need are `.scratch/89-m3b-paired-200/`.
- **Learner (pilot-grade, 3 seeds + 1 repeat, 12.25x):** T1 supported (+41.3% to +50.7%, median
  +47.6%, vs a ~4.0% noise band); T2 supported (reorg median 0.933x, 0 index builds on the 0-scan
  region); T3 supported (median 0.767x, max 0.941x); T4 evaluable, **not supported** (a real ~8-9%
  write-spike capacity drop, neither arm recovers). Not the pre-registered 10-seed matrix.
- **Device-bound residual:** rung 50 is not lag-sustained for any arm; the shared device's own fsync
  tail (77-166 ms) sits above the 10 ms lag ceiling with no build concurrent. Attribution to the
  device is recorded, not inferred.
- **Footprint:** postgres 16.40x by data directory (inside the regime), sqlite 4.04x (below the 10x
  floor, so its 5/5 is a reading at a smaller physical size). Both figures are against **postgres
  18.6 (Ubuntu 18.6-3build2)**, ticket 82; a run against another version re-takes them.
- **M2 (join-heavy descriptors):** delivered and merged; its gate is UNMET (2/4) because the >=10x
  and learned-vs-declared join measurements need three engine capabilities that do not exist
  (multi-row `$block.field` binding, a validator wider than `cust -> take`, aggregate-to-write).

## Milestones

| # | Milestone (user gate) | Deliverable / vertical slice | Acceptance (decidable) | Depends |
|---|---|---|---|---|
| M1 | External baseline scoreboard (T5-T7) | One request-level harness driving flashdb (declared + learned), sqlite and postgres on the same frozen trace at matched durability in the same memory-capped regime | **DONE (ticket 82)**: T5 PASS, T7 PASS, T6 measured and withdrawn; two fairness defects removed and measured | none |
| M2 | Motivating join-heavy workload and layout-space decision | The four join-heavy shapes in the generator, frozen and hashed; a headroom measurement at >=10x; a go/no-go on L2 co-location | **DELIVERED, gate UNMET (2/4)**, proposal accepted — no general L2 (decision 3) | none |
| M3 | Access-path parity (**reached** on (a) and (c); (b) unmeasured, window granted on a trigger — decision 7) | The derived `order_no` index and the startup scan warm built from the merged memtable+tile view; a quiet-box paired 200 rps comparison of declared vs indexed | (a) an index built over a memtable-only region has one entry per row and `coverage_csn` = max covered csn; an empty build while `scan_primary` yields rows is a hard error naming both counts, bound by a test; the same for `startup_warm scan_entries` — **met**: the entries half is measured at scale (ticket 84: 2,785 entries for 2,785 rows, 0 of 188 scans entering the merged scan, `engine/tests/index_defect_mechanism.rs`), the `coverage_csn` half is bound by `engine/tests/order_no_index.rs:171` (a memtable-only region, handles committed out of order, covered csn 3 = the maximum over the rows, which "the last row's csn" would fail) and by `:221` (the maximum over *live* versions, not the tombstone's), and the startup-warm half by `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable` (`engine/src/readahead.rs:583`: 2 scan entries for a region with no tile at all) — all three green in the pinned run at `f38ec63` (`.scratch/88-m3-notes/logs/workspace-head.out`); (b) at 200 rps the indexed arm sustains at least as many paired blocks as declared, or every remaining breach is attributed to a named phase with evidence — **unmeasured** (decision 7): no decidable pair has run, so the clause is neither met nor falsified; the window is granted on a trigger and the instrument is `.scratch/89-m3b-paired-200/` (two arms on the same frozen trace, the chain that regenerates what they need, a loadavg gate that refuses an arm above it, and a digest-verified restore per arm); the gate polled 191 times over 16 minutes on 2026-10-21 and **not one reading was under 2.0** (min 4.92, max 20.59; `logs/paired-200-gate.out`, summary `logs/window-observation.out`), and on 2026-10-27 it held for one minute (0.87, opened 15:30:03) so the first pair ran inside it (0.88 / 1.63), after which the box carried 2.15-6.75 and the remaining nine blocks were refused by the AGENTS.md per-arm rule and marked `# INVALID - do not cite:` in `runs/invalid-2026-10-27/`; the script now re-checks the gate before every arm and moves a lost run's blocks out of the scorer's glob. No throughput number is claimed anywhere, and the invalid blocks' declared 11-13 vs indexed 2-5 lag-rejected windows per 20 is recorded as a hint, not evidence; (c) `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged — **met**: exit 0 with **340 passed / 0 failed**, rerun after the answers comparison was widened — `.scratch/88-m3-notes/logs/workspace-head.out` at `f38ec63` (loadavg 13.20 at start) and `logs/workspace-after-rowbytes.out` at `1870e77` are both 340/0; the earlier log at `ba248dd` is `.scratch/85-m3-clause-c/workspace-pinned.out`; every log carries its own loadavg on line 1 — 13.20 / 8.24 / 23.35 at start for the three, `head -1 .scratch/88-m3-notes/logs/*.out .scratch/85-m3-clause-c/workspace-pinned.out` — and no reading here is timing-derived. The no-index arm returns the same 188 answers byte for byte — status and serialized row, not a row count — as the indexed arm (`before_answers == after_answers` in `engine/tests/index_defect_mechanism.rs`; `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out`). The policy-off reading is one in-process arm through `execute_request`, not a run through the driver's policy gate | M1 |
| M4 | Gap attribution and the theoretical floor (**DONE** — accepted as met by the layer above, 2026-10-21; `docs/6_ARCHITECTURE.md`) | The forensic report required by `2_GOAL.md` §0 and `9_IMPLEMENTATION.md` §Gap attribution: per-phase ledger for the M1 and M3 records, each phase compared with its theoretical floor, each gap classified (a) not learned enough / (b) defect / (c) architecture, plus the byte-level format forensics (on-disk and in-memory bytes per row) | every losing phase has a classification with its evidence and revision; the ranked fix list names each fix's predicted effect on the KPI ladder and its instrument; unclassifiable phases are recorded as unmeasured | M3 |
| M5 | Fair external re-run under the KPI ladder | The three-arm comparison re-run on a quiet box with the efficiency instruments wired: write amplification, bytes read/request, CPU µs/request, resident bytes/data byte, recovery seconds, next to the sustained level and the tail | the comparison resolves to win / not-worse / loss per `2_GOAL.md` §0 with every number conditioned; any remaining loss carries its M4 classification | M3, M4 |
| M6 | Pre-registered 10-seed verdict and publication | T1-T4 re-run at 10 independent seeds on the >=10x instance; the integrated, independently reviewed evidence package; README and docs reflect the present; the verdict published | 10 seeds, median and IQR, thresholds exactly as pre-registered; every acceptance criterion either passes or is disclosed unverified; an outsider can read the result from the artefacts | M5 |

Milestones are ordered by dependency, not by appeal. M3 -> M4 -> M5 is one chain: the access path
must be correct before a gap is attributable, and the gap must be attributed before anything is
tuned or re-measured.

## Current milestone

**M3 — access-path parity** (ticket 83): reached on (a) and (c), **unmeasured** on (b) — the quiet
window is granted on a trigger (decision 7) and `.scratch/89-m3b-paired-200/` is its instrument; the
trigger has opened twice and the gate has refused both runs (2026-10-21: never under 2.0 in 191 readings;
2026-10-27: one quiet minute, first pair inside it, then 2.15-6.75 and the rest refused and marked), so
both readings are accepted by the layer above (2026-10-21), and WP-4's review of
the evidence (PASS with notes) landed its three text notes at `31c85c6` with the runs that back them in
`.scratch/88-m3-notes/`. `docs/0_ROADMAP.md` §M3 status carries the clause-by-clause state and the
reopening condition. **M4** is met at `docs/6_ARCHITECTURE.md` — one reading, the ~573 ms group, is
recorded **unmeasured** with its instrument named, and that instrument needs the same quiet box. So
**the next milestone is M5**, and it is held, not idle: it needs a quiet window (person), and the
postgres leg's server is no longer the blocker it was — this box carries 16.15 in container
`jolly_lamarr` (read 2026-10-23, method in `.scratch/90-m5-postgres-env/issues.md`), the M5 server
starts and checks on 5433 with the M1 record's settings, and `scripts/m1-baseline-run.sh` now runs
the **postgres arm through that checked container**: the version is read back from the running server
and carried in the record (`16.15 (Debian 16.15-1.pgdg13+2)`, arm log
`.scratch/90-m5-postgres-env/logs/arm-16.15-chain-203101.out`), a setting or a required version that
differs stops the arm, and the data directory is restored under a stopped container. That run was
under loadavg 3.77, so it is a chain check and no rate is claimed; one further block, taken inside
the quiet trigger at 20:34 (1-minute loadavg 0.92), measured the arm at **200 rps: 199.87 achieved,
21 windows, 0 rejected, service p99 41,652 µs, serving peak 39.5 MiB**, with the version 16.15
stated — not the three-arm comparison and not comparable to ticket 82's 200k/2.65M rung. An 18.x
arm follows only if the image pull lands (`pull-postgres18.sh`), and then it names the 18.x it reads. Nothing in this tree can
produce the quiet window. What
the project does while it is held is the box-free work on M4's own ranked fix list, because that can be
done under load and it re-ranks what M5's re-run has to measure: the record-side re-scoring of the M1
blocks under the decided lag rule is **done** (WP-12: 0 of 45 rows move, so the headline is
rule-invariant), the barrier-path reading is **in flight** as WP-6's code half (a WAL-preallocation
switch, off by default, built and unit-tested; its timing verdict waits for the same quiet box), and what
is left after those is fix #4's per-group phase timer, which is the instrument the ~573 ms group has no
substitute for. Work packages, owners, acceptance and budgets are in
`docs/0_ROADMAP.md`; the open-item inventory with leverage ranking is `docs/9_IMPLEMENTATION.md`.

Owner: the main agent. Status is a field of each work package, rewritten in place. The product bar is
`2_GOAL.md` §0; the method and gap-attribution protocol are `9_IMPLEMENTATION.md`; milestone order is
`0_ROADMAP.md`; the item inventory with leverage ranking is `9_IMPLEMENTATION.md`.

## M3 — access-path parity (current)

Budget: 8 writer dispatches, 2 review cycles per package, 3 no-progress attempts per package, fresh
context for every child. One writer per worktree or file area; the main agent integrates.

| id | status | owner | scope (exact files) | acceptance and proof | depends |
|---|---|---|---|---|---|
| WP-1 | **integrated `8640a80`; review found D1-D8; superseded by WP-1b on the blocked items** | worker (fresh) | `engine/src/index.rs`, `engine/src/engine.rs` (merged primary scan with csn), `engine/src/readahead.rs` (startup warm source), `engine/src/bin/flashdb-load.rs` (eager build call site) + their tests | the index is built from the **merged** memtable+tile view: a region whose rows are all in the memtable yields one entry per row and `coverage_csn` = max covered csn, bound by a test; an empty build while the merged scan yields rows is a hard `io::Error` naming both counts; `startup_warm scan_entries` is non-zero for a memtable-only region; `cargo --config 'source.crates-io.replace-with="aliyun"' --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' test --workspace --offline` green; policy-off path unchanged | none |
| WP-2 | **unmeasured, instrument armed and hardened** — the window is granted on a trigger (`0_ROADMAP.md` decision 7); 2026-10-27 the trigger held for one minute, the first pair ran inside it, the box then carried 2.15-6.75 and the remaining nine blocks were refused and marked invalid; the arms and the regeneration chain are `.scratch/89-m3b-paired-200/`, which now re-checks the gate before every arm and moves a lost run's blocks out of the scorer's glob | writer (fresh) | measurement only: `scripts/` + `.scratch/83-index-probe/` (no engine edits) | quiet box (loadavg < 2 sustained before each arm run), 5 paired 200 rps blocks with arm rotation, snapshot restore verified, stderr kept; reports paired per-block sustainment declared vs indexed plus `lag_breach_attribution` for every rejected window; verdict "indexed sustains >= declared" or the named phase still responsible | WP-10 |
| WP-3 | **integrated `4916fb7`, harness 210 tests green** | worker (fresh) | `harness/flashdb_harness/baseline_0b.py` (+ its tests) | the run record carries the KPI-ladder instruments: bytes written/ingested, bytes read/request, CPU µs/request, resident bytes/data byte, recovery seconds — each with the command that read it; recomputing the M1 records yields a table whose throughput and latency columns reproduce ticket 82's report within its stated error band | none |
| WP-6 | **held on its measurement half, code half in flight** — dispatched 2026-10-22 to a fresh worker in a worktree (`wp6-wal-prealloc-code-half`): the preallocation switch, default off, with extent-level and recovery tests; its verdict may not be claimed until the paired block runs, so it lands as *built and unit-tested, unmeasured* | worker (fresh) | `engine/src/wal.rs`, the `Wal` construction call site in `engine/src/engine.rs`, the flag in `engine/src/bin/flashdb-load.rs` + one 200 rps paired block in `.scratch/83-index-probe/` | preallocating the WAL segment is tested as the mechanism behind the +1,432 us barrier gap vs postgres (`docs/6_ARCHITECTURE.md` §5.1 mechanism 1, ranked fix #2): the block reports service p50/p99 with and without preallocation, and if the gap does not close the sync-count-per-commit measurement is taken instead; verdict names the mechanism or records it unconfirmed. **Box-free half**: the switch off by default leaves `file_len == valid_len` and every pre-existing WAL test unchanged; on, the file is really allocated (`metadata().blocks()*512 >= chunk*3/4`, not a sparse hole), fills in chunks, and `open_or_recover` replays exactly the appended payloads and truncates the zero tail | WP-3, quiet box |
| WP-1b | **integrated `f1f9912`, 316 green; re-review PASS with notes** | worker (fresh) | same five files as WP-1 | the loud error is reachable in production: an empty build is refused when a signal derived from a *different* structure (`active_tile_regions` / `serving_memtable_entries`) says the region holds rows, naming visited rows, entries and the signal, bound by a test whose scan yields nothing while the engine holds rows; the tautological `debug_assert_eq!` is gone; the boundary-time scan is timed on the window record as `index_scan_us` and a failed scan is one-shot; metrics rotate after the eager build and warm so window 0's region telemetry is clean; the memory docstring matches the caller's copy; a tombstone + re-versioned handle test binds one-entry-per-live-row and `.max()` csn; counts move from 311 by exactly the new tests | WP-1, review |
| WP-8 | **integrated `cf6bf9a`, 327 green; review OK-with-notes** (carried the four WP-1c items) | worker (fresh) |
| WP-9 | **integrated `79f5a5f`, 335 green; review OK with notes** | worker (fresh) | base+delta incremental index instead of a whole-index rewrite, the read path probing delta then base, an amortised merge, and the WP-8 review's P2s (end-of-run drain, startup refusal fallback, honest byte accounting, single-flight slot in the type, true docstrings, a bound that survives region growth). Acceptance includes a scripted test that total index bytes written stay <= ~2x the rows' own bytes, replacing any benchmark claim. **Measured**: 4,200 rows / 579,600 B of documents -> 292,133 B of index (0.504x row bytes), >= 13.6x less than the per-boundary whole rebuild | WP-8 | `engine/src/bin/flashdb-load.rs`, `engine/src/index.rs`, `engine/src/readahead.rs` | after a memtable publish the adopted index is rebuilt off-path (or marked stale and rebuilt at the next boundary), so a region that was empty at build time is not served by the full scan for the whole run with no log line; the tile half of the scan runs on the builder thread, not the driver, with the memtable rows snapshotted inline under a stated byte bound; acceptance: a run whose region grows after the build ends with a covering index, and no inline scan exceeds the bound | WP-1b |
| WP-10 | **integrated `a01cb15`, 339 green** | worker (fresh) | the WP-9 review notes: the adopt-refusal latch race (a policy toggle while a delta is in flight latches a transient error permanently), a negative test for the mismatched base coverage, an absolute delta entry cap (`INDEX_DELTA_MAX_ENTRIES = 200_000`) beside the ratio so the RAM bound does not grow with the base, charging the tile half of a delta build against the inline bound, and recording that a repeated `order_no` collapses in the delta (hint quality, not correctness) | WP-9 |
| WP-7 | **integrated `e5a3020`** | worker (fresh) | `README.md`, `docs/8_DETAIL.md`, `docs/9_IMPLEMENTATION.md` | every current-state sentence resolves to a code path or a record path; no false absence and no false capability; every printed command ran; README states the negative external result, the licence and where to report a problem; `docs/9_IMPLEMENTATION.md` becomes the current reconciliation record (closes PLAN-readme-stale, PLAN-engine-doc-stale, PLAN-evidence-reconciliation) | none |
| WP-11 | **integrated** — the deferral wording and the (c) definition, with (c)'s evidence; pointer: `git log -1 -- README.md` and `.scratch/85-m3-clause-c/workspace-pinned.out` | the node that owns this roadmap (not dispatched: the fixed cost of a contract is large against the work) | `README.md`, `0_ROADMAP.md`, this file, `evidence.md`, `AGENTS.md` | the 200 rps comparison reads **deferred, not pending** wherever the external claim is stated (README, the M3 row, §M3 status, `evidence.md`), and (c) is evidenced by a pinned-flag `cargo test --workspace` run whose log is committed with its loadavg; the deferral wording is superseded by `0_ROADMAP.md` decision 7, which reads **unmeasured** with its instrument named | none |
| WP-4 | **PASS with notes** — review of M3's evidence (`.scratch/84-index-defect-mechanism/`, `engine/tests/index_defect_mechanism.rs`) at `1f31c44`; report `.scratch/86-index-mechanism-review/issues.md`. All three figures re-derived exactly; the pre-fix log fails at the entries assertion, not an unrelated one. Its three text notes are landed in `0_ROADMAP.md` (M3 row) and in §M3 status below: the answers comparison was `status:rows` and is now row bytes too (`.scratch/88-m3-notes/`), (a)'s `coverage_csn` and startup-warm halves are cited to the tests that carry them, and the policy-off arm is restated as one in-process arm, not a run through the driver's policy gate | reviewer (fresh) | read-only review of WP-1..WP-3 at the integrated revision | package verdicts with defect evidence (location, criterion, reproduction); no commit | WP-1..WP-3 |

**WP-1b re-review (PASS with notes, folded into WP-8).** P1: the memtable arm of the witness counts the whole memtable, so a restored target with a non-empty memtable and no live `StockMovement` row aborted the eager build at startup (`?` on the build) rather than falling back — the "over-report is harmless" acceptance I recorded was wrong. Fixed in WP-8 (`cf6bf9a`): the memtable arm counts the region's live rows from the memtable prefix. P2a: the Global-mode limit is misdescribed (`active_tile_regions()` reports the first key's region, so the witness can be wrong in either direction). P2b: a refused build re-scans at every boundary because the latch is only set on snapshot failure. P2c: no negative control for a legitimately empty region.

**WP-1b stated limit (recorded at integration, `f1f9912`).** The `RegionRows` witness derives "the region holds rows" from a tile that names the region or from a non-empty serving memtable. Under `RegionMode::Global` a tile does not name one region, so tile-resident rows are invisible to the witness and it under-reports; the merged scan keeps reads correct, and the guard simply cannot fire there. The memtable arm can over-report ("some rows are live"), which is safe for its purpose (it can only turn a would-be-silent empty build into an error) but would refuse a build on a region with no rows while another region sits in the memtable — accepted, because that state is served by the full scan either way and the refusal names both counts.

**WP-1 known risk (recorded at integration, `8640a80`).** The async build now snapshots the merged rows into a `Vec` on the driver thread at a window boundary (`movement_rows`), because the memtable is not shareable with the builder thread. For a memtable-only region that is bounded RAM work, but for a tiled 624 KiB region it streams from device on the pacer — the same stall class M3 exists to remove. Follow-up if the review confirms the exposure: keep the tile scan on the worker and snapshot only the memtable rows inline (the hybrid), with a bound on the inline bytes.

Stop conditions: a package that cannot meet acceptance reports incomplete with the exact blocker
rather than widening scope; WP-2 never runs while the box carries foreign load and queues with an
explicit resume trigger instead of reporting a contended result as a verdict.

## M5 — fair external re-run (server leg advanced; the run waits on the quiet window)

| id | status | owner | scope (exact files) | acceptance and proof | depends |
|---|---|---|---|---|---|
| WP-13 | **the postgres arm runs through the checked container at 16.15; the refusal is in the chain** — `postgres-server.sh check` read back `PostgreSQL 16.15 (Debian 16.15-1.pgdg13+2)` and the M1 record's settings block (shared_buffers 16MB, max_connections 32, work_mem 4MB, maintenance_work_mem 16MB, fsync on, synchronous_commit on, wal_level minimal, max_wal_senders 0) from the running container at 128 MiB; `smoke-migration.sh` built the harness's postgres instance (database `m1-controlled`, 200 customers, 2,000 stocks, 8,150,039 B) and read the counts back from the server. `scripts/m1-baseline-run.sh`'s postgres branch now starts/stops that container, refuses a setting or a `M5_PG_REQUIRE` version that differs, refuses a cap other than `--cap-mib`, removes the container before a restore writes the data directory, and writes `server_version` read back from the server into `postgres-server.jsonl`; the arm ran end to end through the chain at 16.15 (`.scratch/90-m5-postgres-env/logs/arm-16.15-chain-203101.out`, loadavg 3.77 at start — a chain check, not a rate), and one block inside the quiet trigger (1-minute loadavg 1.84 → 0.92 → 0.95, 20:32:35-20:34:57) measured it at **200 rps: achieved 199.87, 21 windows, 0 rejected, service p99 41,652 µs, lag p99 4 µs, serving peak 39.5 MiB, oracle PASS** (`.scratch/90-m5-postgres-env/logs/arm-16.15-200rps-quiet-203431.out`); not the three-arm comparison, and not comparable to ticket 82's 200k-customer / 2.65M-SKU rung. `psycopg[binary]==3.3.6` resolves offline (re-read 2026-10-23T20:30). Open: the 18.x arm (pull in flight) and the three-arm comparison's own window | worker (fresh) | `.scratch/90-m5-postgres-env/*`, `scripts/m1-baseline-run.sh`, `scripts/m1-ladder-run.sh`, `scripts/m1-controlled-run.sh` (postgres arm only), `scripts/baseline-env.sh` (header fact) | one committed command starts the postgres arm's server and refuses when a setting or a required version differs; the M1/ladder chains launch the postgres arm against it with a digest-verified restore of `/var/tmp/m5-pg/data`; the run record's `postgres_settings` carries `server_version` read from the running server; the `psycopg[binary]==3.3.6` pin resolves offline or is replaced by a version read from `import psycopg`; `scripts/baseline-env.sh` no longer claims Docker Hub is unreachable as a present fact | none |

## M4 — gap attribution (after M3)

| id | status | owner | scope | acceptance |
|---|---|---|---|---|
| WP-5 | **delivered** — `6_ARCHITECTURE.md`, 23.5 KB; M4's acceptance is met as written, with the ~573 ms group recorded unmeasured and its instrument named | scout/analyst (fresh) | `.scratch/82-controlled-m1/runs/b1..b5`, the M3 records, `docs/8_DETAIL.md` device constants; writes `docs/6_ARCHITECTURE.md` | every losing phase classified (a) not learned enough / (b) defect / (c) architecture with its evidence and revision; the byte-level format forensics (on-disk and in-memory bytes per row vs the format's declared minimum); a ranked fix list, each fix naming its predicted effect on the KPI ladder and its instrument |

| WP-12 | **integrated** — `.scratch/87-m1-decided-rule/` (report, `rows.json`, `rows.md`, `analysis.py`, `run.sh`, `reproduce/`); the finding, and its one sentence, is that the 3 of 5 is rule-invariant: **0 of 45** rows and **0 of 47** arm-runs differ, because no saved M1 arm-run was ever scored with a service ceiling (`"service_ceiling_us": null` in 45 of 45 window files), and `scripts/m1-controlled-score.py` re-emits `table.md` byte-identically (diff 0 lines, sha256 `cda02ab0…ccad7`). Landed: the README's rule-invariance bullet, `9_IMPLEMENTATION.md` rows `T-84` and leverage item 2 | worker (fresh) | the saved M1 records `.scratch/82-controlled-m1/runs/**`, `harness/flashdb_harness/baseline_0b.py` (`summarize_curve`), `scripts/m1-controlled-score.py`; no engine change, no new run | every rung/arm row re-read from the record with `sustained`, `sustained_primary` and `service_ceiling_reported_windows`; the count of rows whose verdict differs and which ceiling decided each; and one sentence stating whether the README's "3 of 5 blocks at 200 rps" is rule-invariant under the decided lag-only rule or must be restated — plus `scripts/m1-controlled-score.py` reproducing `table.md` byte-identically (T-84's own claim) |
**Index maintenance design (decided 2026-10-19, WP-9).** Three candidates were priced before building; the chosen one is **B: RAM delta over a disk base**. The rejected ones, with the number that rejects them:
- *A single cumulative delta sidecar* (the literal reading of "base + delta"): each rebuild must rewrite every entry accumulated since the last base merge, so cost within a cycle is quadratic in the interval. At the ticket-83 region that is ~6x ingest, at 30k rows ~53x — worse than the whole-index rebuild it was meant to replace, and worsening with base size. Rejected.
- *A append-only delta generations*: each generation is written once (good), but the tier reaches `base/interval` files (~216 transient files at 30k rows) and a crash leaves orphans, which is the `PLAN-orphan-cleanup` failure class again. Rejected on complexity, not on write amp.
- **B, chosen:** the delta is a RAM map (order_no -> handle plus handle tombstones) built on the builder thread and written nowhere; the read path probes the RAM delta then the base tile; when `delta_entries >= max(1024, base_entries)` an off-path merge writes a new base and drops the delta. Every entry is written once into the delta (no device bytes) and once into the merged base, so total index bytes <= ~2x entry bytes, independent of base size. RAM cost is the delta, bounded by the threshold (~1.9 MB at 30k rows, 0.13% of the 1.57 GiB instance). The delta is not durable, which is irrelevant: the index is a hint, rebuilt at every open and never on the correctness path.

**Rebuild cadence — superseded by the WP-8 review (2026-10-19).** The estimate below priced device time only. The review priced it on the KPI ladder and the answer changes: rebuilding the whole index every window writes ~0.19 MB (ticket-83 region) to ~2.1 MB (30 k rows) per rebuild, which is **1.4-3.6x ingest on the WP-2 snapshot and 15-40x on the 30 k-row instance** against a T3 baseline write amp of 3.92 — up to ~10x the entire existing amplification, charged to `index_output_bytes`, which is also the learner's reorg reward. A cadence floor alone cannot fix it: the floor needed to bound write amp (~10^4 csn at 200 rps) makes the index tens of seconds stale, and a scan that targets recent order numbers then misses, restoring the 6 ms full scan. **The fix is incremental maintenance (base + delta), not a longer interval**; WP-9 owns it. No 200 rps arm runs until it lands.

**Original estimate (kept as the rejected reading).** The demand arm fires at every boundary where the state moved and the movement scan consulted the index once, so under continuous scan demand a run can rebuild once per window (~2.1 MB of index tile + a directory fsync each, charged to `index_output_bytes`). Measured scale: the win workload writes ~139 movements/s at 200 rps and issues ~9.4 scans/s, so one rebuild per window costs ~6 ms of device time against up to ~56 ms of full-scan time it avoids — favourable while scans are being missed, which is exactly when the demand arm fires. **No cadence floor is added before WP-2**: the run record now carries `index_output_bytes` and the decision lines, so WP-2 decides the floor from measurement rather than from this estimate. If WP-2 shows the index rewriting more than a few percent of the instance per run or the rebuild competing with the barrier, the floor lands as `max(demand lag, N csn)` in `rebuild_trigger` and is re-measured.

**WP-9 review numbers (2026-10-19).** The index write amp is now 0.504x row bytes, but the review corrected the RAM accounting: at a 30,000-entry base the merge instant peaks at ~16.5 MB (serving delta ~3.3 MB + merge snapshot clone ~3.3 MB + merge entries ~8.6 MB + ~1.3 MB), and the delta alone is ~110 B/entry, so at a 1M-entry base it reaches ~110 MB — inside a 128 MiB cap that is fatal, and it is why WP-10 adds an absolute `INDEX_DELTA_MAX_ENTRIES` cap beside the ratio. The review also found one real bug (a transient adopt failure after a policy toggle latches permanently) and one hint-quality note (a duplicated `order_no` collapses to one delta entry, so the pre-merge delta handle wins and the post-merge lowest handle wins; every answer stays correct through the document cross-check).

## M3 status

- (a) **Met, and measured as work per request.** The index is built from the merged view, the empty/partial build is a loud error against a witness the scan does not walk, the adopted index is maintained incrementally (RAM delta over a durable base) with a logged decision line per rebuild, and index write amp is 0.504x row bytes in the scripted test. Ticket 84 adds the scale measurement on a rebuilt append-heavy instance (156 tiles, 2,785 movements, 188 scan requests): 2,785 index entries for 2,785 rows against 0 pre-fix, 0 of 188 scan requests entering the merged scan, 188 consultations, 188 cross-checked point probes, tile bytes per scan request 616,428 → 0 (memtable-resident) or → 4,218 (tile-resident), i.e. 42,240 → 13,466 B/request against the predicted 43,298 → 13,460. Bound by `engine/tests/index_defect_mechanism.rs`, which fails on pre-fix `f584ecc` with `left: 0, right: 2785`. **The clause's other two halves are bound by their own tests, not by ticket 84** (review WP-4 note 2): `coverage_csn` = max covered csn by `engine/tests/order_no_index.rs:171` (memtable-only region, handles out of order, covered csn 3 = the max over the rows, not the last row's) and `:221` (the max over live versions, not the tombstone's); `flashdb-load`'s `a_stale_adopted_index_is_rebuilt_and_coverage_advances` (`engine/src/bin/flashdb-load.rs:3065`) binds the narrower fact that a rebuild advances coverage, not the max-over-rows reading, and `startup_warm scan_entries` for a memtable-only region by `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable` (`engine/src/readahead.rs:583`). Both green at `f38ec63` in `.scratch/88-m3-notes/logs/workspace-head.out`.
- (b) **Unmeasured** (`0_ROADMAP.md` decision 7). The paired 200 rps declared-vs-indexed comparison (WP-2) has not produced a decidable run, so the clause is neither met nor falsified; its instrument is `.scratch/89-m3b-paired-200/` — two arms on the same frozen trace and rebuilt instance, the chain that regenerates both from committed specs, a gate that refuses an arm whose start loadavg is not under the granted trigger, and a digest-verified restore per arm. The gate has decided against a run twice: on 2026-10-21 it polled 191 times over 16 minutes and no reading was under 2.0 (min 4.92, max 20.59); on 2026-10-27 it held for one minute (0.82 → 0.87, opened 15:30:03) so the first pair ran inside it (0.88 / 1.63), after which the box carried 2.15-6.75 and the remaining nine blocks were refused and marked `# INVALID - do not cite:` in `runs/invalid-2026-10-27/` — `AGENTS.md` requires refusing an arm above the threshold and the script had gated only once, so it now re-checks before every arm and moves a lost run's blocks out of the scorer's glob (`refuse_run`, both behaviours exercised in `logs/paired-200.out`). Nothing is claimed about throughput, including the invalid blocks' direction (declared 11-13 lag-rejected windows per 20 vs indexed 2-5), which the ticket records as a hint, not evidence. The twelve invalid `.scratch/83-index-probe/runs/` files are what an early pair under load cost, and they stay marked.
- (c) **Met.** This is the roadmap's definition (`0_ROADMAP.md` decision 5) — `cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged — and it is locally decidable: the pinned-flag `--offline` workspace suite returned exit 0 with **340 passed, 0 failed** at `f38ec63` (`.scratch/88-m3-notes/logs/workspace-head.out`, loadavg 13.20 at start) and again at `1870e77` after the answers comparison was widened (`logs/workspace-after-rowbytes.out`, loadavg 23.35 at start; `logs/index-defect-mechanism-rowbytes.out` started at 21.43; every log states its own reading on line 1, `head -1 .scratch/88-m3-notes/logs/*.out`); the same result at `ba248dd` is `.scratch/85-m3-clause-c/workspace-pinned.out`, and the no-index arm of the ticket-84 test returns the **same 188 answers byte for byte** — status plus the serialized row, not a row count — as the indexed arm (`before_answers == after_answers`, `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out`; the §4 note of the WP-4 review is why the comparison was widened from `status:rows`). Two limits on that reading, stated rather than implied: the policy-off arm is one in-process arm through `execute_request`, not a run through the driver's policy gate, and `movement_scan_falls_back_to_full_scan_when_index_absent` is what binds the fallback path itself. The efficiency KPI comparison against sqlite and postgres is **M5's** work; the earlier line here that put it under M3 was a stale restatement and is superseded.

## Known blockers

- **Postgres: the arm runs at 16.15, and a result never inherits another version's sentence.**
  Root's answer of 2026-10-23 reads this box as carrying **16.15 (Debian 16.15-1.pgdg13+2)** in
  container `jolly_lamarr` on 5432 and no 18.x. The M5 server is a container this tree starts and
  checks (`.scratch/90-m5-postgres-env/postgres-server.sh up|check|env`), and
  `scripts/m1-baseline-run.sh`'s postgres branch now drives it: `down` before each step, `up` with a
  pull only when the image is absent, the M1 record's settings and an optional `M5_PG_REQUIRE` read
  back with a non-zero refusal on any difference, the container cap compared with `--cap-mib`, the
  container removed before a restore writes `$M1_PG_ROOT/data`, and `server_version` written into
  `postgres-server.jsonl` from the running server. The arm ran end to end at **16.15**
  (`logs/arm-16.15-chain-203101.out`, oracle PASS, 128 MiB, serving peak 36.8 MiB) under loadavg 3.77,
  so it is a chain check and **no rate is claimed there**. One block inside the quiet trigger measured
the arm at 200 rps (above); what still blocks M5: (i) the 18.x arm — the
  image pull is a repeatable background attempt (`pull-postgres18.sh`, no layer completed at 20:32),
  and if it lands the arm is re-run and names the 18.x it reads, never 18.6 by inheritance; (ii) the
  quiet window, which no work in this tree can produce. `scripts/m1-ladder-run.sh` and
  `m1-controlled-run.sh` build their own clusters through `baseline-env.sh` in their own setup and
  still need the same wiring before they drive postgres end to end.
- **The quiet window for M3 clause (b) is granted and armed, not denied.** Root granted one exclusive
  window (root's contract, `e5fb0e1:inbox/2026-10-21-next-contract.md`, landed as `0_ROADMAP.md`
  decision 7): the trigger is `uptime` reading loadavg under 2 for one minute with no other agent
  writing in this tree, and each arm records the reading it started at. The trace and instance are regenerable from committed specs;
  the chain, the gate and the arms are `.scratch/89-m3b-paired-200/` (`issues.md`,
  `paired-200.sh`). While the trigger is unmet the clause reads **unmeasured** with that instrument
  named, and no throughput number is claimed.

## Not planned (bounded backlog)

Multi-node, online SQL, general L2 co-location, the three M2 engine capabilities
(`$block.field` multi-row binding, wider validator, aggregate-to-write), and tuning a baseline into a
race. Reopening any of these needs a measured reason recorded here first.

## Completion evidence

Each work package closes with: the revision (commit hash), the command and its observed result, the
artefact path, and the reviewer verdict. The main area stays on `main`; a worktree's branch is pushed
at every safe point and merged once its review passes.

## How the doc base is filed

One home per meaning, and the level is a property of the file rather than of a section inside it:
`1_PROBLEM.md` why · `2_GOAL.md` the bar · `5_PRODUCT.md` the learner · `6_ARCHITECTURE.md` invariants,
components and gap attribution · `7_INTERFACE.md` the contracts · `8_DETAIL.md` the engine inside ·
`9_IMPLEMENTATION.md` method, evidence and open items · `0_ROADMAP.md` this file · `0_STAGING.md`
unfiled · `glossary.md` the term index, which carries no level of its own.

Inside each file the H1 zoning is the source tier, lower index first: `EXPLICIT` (the author said it),
`CONFIRMED` (agreed without rephrasing), `INFERRED` (this node concluded it, by measurement or by
reasoning, with the evidence quoted where it is used).

Migrated 2026-10-27 out of twelve files that mixed levels. What the move did, and what it refused:

- **Split.** `design.md` fed three levels — its thesis to 1, its goals and non-goals to 2, its component
  map and invariants to 6, its open questions to 5. `plan/` dissolved: the workplan into this file, the
  evidence record and the open-item inventory into 9, the gap attribution into 6.
- **Merged.** The workplan became part of this file, because a roadmap by definition holds the worksets
  and workpackages, and the split had already drifted once: decision 5 records a stale restatement that
  lived only in the workplan.
- **Deleted**, each line with its reason in `.scratch/91-docs-migration/docs-dropped.txt`: per-file line
  budgets, the source titles (the filename now carries level and name, the H1 carries the tier), and two
  sections that restated a higher tier within their own file — design's prior-art summary, and the
  workplan's recap of decisions 1-4.
- **Refused.** A full editorial rewrite, and sentence-level zoning of every section: the move is
  section-granular and byte-preserving (`.scratch/91-docs-migration/migrate.py`), so no measured number
  or revision pin was retyped, and the mixed sections are zoned by hand as they are next touched.
- **Enforced.** `scripts/check-docs.py` fails on a non-level filename, an out-of-order or missing tier,
  a duplicated heading, a dangling `docs/` reference, or a line of the previous doc base that left
  without an entry in the dropped list.

A source-tier judgement is not permanent: a claim that moves from `INFERRED` to `CONFIRMED` moves when
the author agrees to it, and the line changes tier rather than being duplicated.
