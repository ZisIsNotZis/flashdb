# 86 - Review of ticket 84: does the M3 (a)/(c) claim follow from its own evidence?

Contract: read-only audit of `.scratch/84-index-defect-mechanism/` and
`engine/tests/index_defect_mechanism.rs`. No workload run, no re-measurement, no edit of the
record under audit, the engine, or the docs.

Revision judged: HEAD `1f31c44`. The artefact's own revisions: `logs/head.out` was produced at
`23c4af1`; the file it ran (`engine/tests/index_defect_mechanism.rs`) is **unchanged since
`23c4af1`** (`git log --oneline -- engine/tests/index_defect_mechanism.rs` → one commit) and
`git diff --stat 23c4af1..HEAD -- engine/` is empty, so the log still describes the code under
judgement and no re-run was needed. `.scratch/84-.../probe-at-HEAD.rs` is byte-identical to the
test file (`diff` empty).

**Verdict: PASS with notes.** The mechanism claim — the merged-view index holds one entry per
memtable-resident row, the merged scan is not entered, and the tile-byte cost per scan collapses
from ~616 KB to ~4.2 KB — re-derives exactly from `logs/head.out` and is bound by exact
assertions. Two statements in the prose and in the roadmap row are **stronger than their
evidence**: R1 (the "identical answers" / "byte-identical answers" claim rests on an assertion
that compares status and row *count*, never row content) and R2/R3 (M3 (a)'s `coverage_csn` and
startup-warm halves are not in this artefact at all and `issues.md` does not say so). Neither is a
measurement error; both are fixable by wording plus a one-line assertion, without re-measuring.

What I read beyond the contract's read list, and why: `docs/6_ARCHITECTURE.md` §2/§3/§9 (to
source the `13,268` and `0.047` terms the per-request rows are built from — acceptance item 1
cannot be discharged from `head.out` alone), `docs/0_ROADMAP.md` §Milestones + decision 5 (the clause
wording the verdict is about), and one 7-line function, `engine/src/engine.rs:3105-3111`
(`lookup_movement_by_order_no`), to decide whether the `order_no_index_lookups` counter can witness
"the index was consulted". All read-only; nothing was modified.

---

## 1. Re-derivation of the three figures from the logs alone

**All three follow. No number in the table is wrong.** Worked from `logs/head.out` +
`logs/pre-fix.out` + `6_ARCHITECTURE.md` §2 (the record's own decomposition):

### (i) Index entries vs movement rows

| figure in `issues.md` | line in the log | follows? |
|---|---|---|
| `0 / 2,785` (pre-fix) | `arm=build index_entries=0 movements=2785` (`pre-fix.out`) | yes |
| `2,785 / 2,785` (after, memtable) | `arm=build index_entries=2785 movements=2785` (`head.out`) | yes |
| `2,785 / 2,785` (after, tiled) | **no line** — see R7 | inferred, not logged |

### (ii) Scan requests that entered the merged scan, per arm

`arm=before ... scans_entered=188`, `arm=after ... scans_entered=0`, `arm=after-tiled ...
scans_entered=0`, each `requests=188`. The record's `188 / 188`, `0 / 188`, `0 / 188` follows
verbatim. The same lines carry `movement_row_probes=0 / 188 / 188`, which the record reports as
"movement point probes (get cross-check)" — that mapping is corroborated internally: the arm that
never reaches `Engine::get` (before) reads 0, the two arms that do read exactly one per request.

### (iii) Tile bytes per scan request

- before: `115,888,464 / 188 = 616,428.0` **exactly** (the log prints `616428`; it is also the
  log's own integer division).
- after-tiled: `793,037 / 188 = 4,218.28…`; the log prints `4218` (integer division), remainder
  `53 B`. The record's `4,218` is the log's figure, not a rounding of its own.
- after (memtable-resident): `bytes_read=0` → `0`.

### (iv) The per-request pair `42,240 → 13,466` (and the predicted `43,298 → 13,460`)

The arithmetic the record states is `0.047 × scan_bytes + 13,268`, and every term is the
record's own, not a reconstruction: `6_ARCHITECTURE.md` §2 gives the scan row
`count=940, bytes_read=600,601,720, bytes/read=638,938` and the non-scan rows summing to
`265,360,600`; `940 × 638,938 + 265,360,600 = 865,962,320`, which `/20,000 = 43,298.116` — the
predicted before figure to the byte. `0.047 = 188/4,000 = 940/20,000`, `13,268.03 =
265,360,600/20,000`. Then:

| | value | record says | ok |
|---|---|---|---|
| before/request | 13,268.03 + 0.047×616,428 = **42,240.15** | 42,240 | yes |
| after/request, tiled | 13,268.03 + 0.047×4,218.28 = **13,466.29** | 13,466 | yes |
| after/request, memtable | **13,268.03** | 13,268 | yes |
| predicted before/request | **43,298.12** | 43,298 | yes |
| predicted after/request | **13,460.54** | 13,460 | yes (truncated; nearest is 13,461 — R12) |
| reduction, predicted | **29,837.57** | 29,838 | yes (rounded) |
| reduction, measured | **28,773.86** | 28,774 | yes |
| measured reduction / predicted | **96.43 %** | 96.4 % | yes |
| before/request vs predicted | **−2.44 %** | "2.4% low" | yes |
| before B/scan vs record's | **−3.52 %** | "3.5% low" | yes |
| after-tiled B/scan vs predicted | **+0.045 %** | "+0.05%" | yes |
| per-tile block, 616,428/156 | **3,951.46** | "average 3,951 B" | yes, as an *average* (R6) |

**Only defect in this section is a defect of attribution, not of arithmetic: R6.** Nothing the
record needs for the claim depends on it.

## 2. Does the test fail if the fix is reverted, and which assertion pins it?

**Yes, and the assertion that fires is the right one.** `logs/pre-fix.out`:

```
arm=build index_entries=0 movements=2785
...
panicked at engine/tests/index_defect_mechanism.rs:260:5:
assertion `left == right` failed: one index entry per movement row, from the merged view
  left: 0
 right: 2785
```

The message and the `0 / 2785` pair are the entries assertion — `engine/tests/index_defect_mechanism.rs:264`
in the file under audit. It is not an unrelated assertion, and it is not a setup assertion
(`append_heavy_instance`'s two `assert!`s at lines 167 and 169 had already passed, and
`pre-fix.out` shows they passed: the run reached the build and the after arm).

The regression is over-determined rather than fragile: on the pre-fix counters the later
assertions would also fire — line 266 (`after.scans_entered == 0`, measured 188) and line 267
(`lookups == 188`, see R4) — but line 264 is evaluated first and is the one that fired.

Scope of that evidence, precisely (R9): `run.sh` copies `pre-fix-probe.rs` **over** the test in a
`f584ecc` worktree, so the log shows *pre-fix code failing the claim*, with the tiles-only 3-arg
build call substituted. It is not literally "this file against reverted code". Under a revert of
the build source that keeps the 6-arg signature, the file would fail at the `.expect("the
merged-view build must not refuse…")` on line 253 if the reverted build refuses, or at line 264 if
it returns 0 entries. Either way it fails; the log pins the second shape.

### Vacuous or self-fulfilling assertions

None is self-fulfilling, but three carry less than their labels say:

- **R4 — `lookups == SCANS` (lines 267, 288) does not witness index consultation.**
  `Engine::order_no_index_lookups` is incremented at the *top* of
  `lookup_movement_by_order_no`, before the `match &self.order_no_index`
  (`engine/src/engine.rs:3105-3110`). It therefore counts *lookup attempts*, and would also read
  `188` with no index adopted — indeed the `arm=before` block already made 188 such calls, they
  are just invisible because `lookups_before` is read after that arm. The assertion's message
  ("the index was consulted on every scan request") and `issues.md`'s "the index was consulted on
  every one" overstate the counter. The index *answering* is pinned by line 266
  (`scans_entered == 0`), line 275 (`after.bytes_read == 0`) and the `probes=188` readout.
- **R5 — the headline byte figures are printed, not pinned.** `before_scan_budget >=
  BLOCK*tiles/2` (line 271-274) is 319,488 against a measured 616,428: the full scan could halve
  in cost and the test would still pass. `tiled_scan_budget <= 2*BLOCK` (line 293) is 8,192
  against a defect cost of 638,976, so *that* one does discriminate. The pair `616,428 → 4,218`
  that the T-83 row and the per-request rows quote is therefore bounded by a factor of two, not
  asserted; a legitimate change in scan cost would move the cited numbers without failing.
- **Not vacuous, and worth saying so:** `ok == 188` with `empty == 0` (lines 268-269) does pin
  non-empty answers, because `execute_request` returns status `"empty"` exactly when the row set
  is empty (`engine/src/request.rs:360-363`) — it is not the case that both arms could return
  zero rows and pass. And `before.scans_entered == 188` (line 265) is the one assertion that
  would catch a change to the *no-index* path, i.e. it is the assertion that carries clause (c)'s
  "unchanged" in this file.
- `assert_eq!(entries, MOVEMENTS)` compares the build against the test's own constant; the
  instance has 2,785 rows *by construction* of the write loop, not by an independent engine count
  (unlike ticket 83's "`scan_primary` returned 2785 rows"). A build returning a different count
  still fails, so the assertion is real; just note that "one entry per row" rests on the loop,
  with the `serving_memtable_entries == 2 × MOVEMENTS` assertion (line 167) supporting it.

## 3. Does `arm=before` evidence clause (c)'s "a policy-off run unchanged"?

**Two answers, and they differ.**

**Is it genuinely the no-index path through `request::execute_request`? Yes.** At the `arm=before`
call (line 240) no index has been adopted — adoption happens later, at line 255. On that state
`execute_request`'s `ReadKind::Movements` branch calls
`engine.lookup_movement_by_order_no` (`engine/src/request.rs:319`), which returns `Ok(None)` when
no index is adopted (`engine/src/engine.rs:3107-3110`), and control falls through to
`engine.scan_primary` (line 334). `execute_request` contains **no other switch** on
index policy: adoption is the only "off" state the request path can observe. Corroborated by the
pre-fix log, which is the strongest single fact in the record: an index **adopted with 0 entries**
produces counters identical to no index at all (`arm=after index_adopted=1`, yet
`scans_entered=188 bytes_read=115888464 bytes_per_scan=616428 movement_row_probes=0`), so
"absent" and "present but empty" are the same read path.

**Is it a "policy-off run"? No, not in the sense the roadmap's phrase carries.** It is one arm of
an in-process test calling `execute_request` 188 times. It never goes through the driver's policy
gate (`flashdb-load.rs::order_no_index_wanted`), its csn trigger, its rebuild, or the startup
build; no generator run, no rate, no policy object. So the artefact supports "the read path is
unchanged when no index is adopted", not "a policy-off *run* is unchanged". `issues.md` does not
distinguish the two readings (R10).

**Does the file assert row-level equality of answers? No — and this is R1, the one finding that
touches the milestone row.** `arm()` records
`format!("{}:{}", out["q"].status, row_array.len())` (line 201), so `before_answers` /
`after_answers` are strings of **status and row count**. Line 270 (`before_answers ==
after_answers`) and line 289 (`tiled_answers == before_answers`) compare those strings. No field
of any answer is ever compared. What is evidenced is "188 × `ok`, one row each, both arms" —
which is a real and useful statement — and **not** "the same rows", as `issues.md` §The path taken
puts it, and **not** "byte-identical answers", as `docs/0_ROADMAP.md`'s M3 row puts it ("the
no-index arm returns byte-identical answers (`before_answers == after_answers` in the same
test)"). The variable names match the roadmap's citation; the content does not. Structurally the
bytes almost certainly *are* identical — both arms build the row with `document_row` on the same
document bytes (`request.rs:321` index path, `:333` scan path) — but the artefact does not measure
it, so the claim as written is unverified. Fix without re-measuring: collect
`out["q"]["row"].clone()` in `arm()` and `assert_eq!` the two vectors, or reword both documents to
"same status and same row count (1) on all 188 requests".

## 4. What the record does not measure, and whether `issues.md` says so

Honestly disclosed (checked against the text, not assumed):

| not measured | disclosed? |
|---|---|
| any rate: throughput, achieved rps, sustained blocks, latency percentiles | yes, opening paragraph and §What is NOT measured |
| the seeded 1.6 GB M1 instance (snapshot git-ignored, absent; instance rebuilt to shape) | yes, including that the non-scan read work is the record's, named as such in every row |
| device bytes vs the engine's own `bytes_read` | yes |
| debug-profile latencies not reported | yes, §Residual risks |
| the rebuilt instance's **3.5 % low block fill** | yes, twice (the table row and §Residual risks) — see below |

**The 3.5 % block fill: disclosed, and it changes no stated conclusion.** It moves only the
*before* per-request figure (−2.4 %), and therefore the reduction to 96.4 % of predicted instead
of ~100 %; the record states both. The entries/rows, scans-entered, probes, answers and the
classification ("the scan is not entered; the cross-check `get` answers") are untouched by it. One
honest asymmetry the record does not point out: the after-tiled figure is **+3.0 % above**
prediction — the opposite sign — so it is not explained by the fill, and the per-request "hit"
(+0.05 %) is partly a cancellation of the two residuals. The conclusion survives; the derivation
of the "hit" is flattered.

**Not disclosed, and each matters to the clause the artefact is cited for:**

- **R2 — `coverage_csn`.** M3 (a) is "one entry per row **and** `coverage_csn` = max covered csn".
  Neither `head.out` nor the test carries a coverage figure: `grep -n coverage
  engine/tests/index_defect_mechanism.rs` → no match; the same grep over `logs/*.out` matches only
  other tests' names in the workspace log. `issues.md` never mentions coverage. So the record
  evidences the entries half of (a) and nothing about the coverage half — and does not say so.
  (`workspace-tests.out` does show tests named `a_stale_adopted_index_is_rebuilt_and_coverage_advances`
  and `readahead::tests::warm_startup_walks_the_scan_prefix_and_the_adopted_index`, so the half is
  plausibly bound elsewhere; that is outside this review and I did not verify it.)
- **R3 — startup warm `scan_entries`.** M3 (a)'s third conjunct ("the same for `startup_warm
  scan_entries`") has no figure anywhere in this record and no assertion in this test. Also
  undisclosed.
- **R10 — the driver path.** The test adopts the index by hand (`e.adopt_order_no_index(build.index)`,
  line 255). What a *run* does — `order_no_index_wanted(policy)`, the csn trigger, rebuild on
  publish, the startup build — is not exercised, so the record shows the merged-view build is
  correct, not that a run adopts it.
- **R11 — the per-request rows are cross-run recombinations.** `42,240 → 13,466 B per request`
  joins this run's measured scan term to the M1 record's `13,268` non-scan term and its `0.047`
  mix. The record's opening sentence ("every reported number is a count, a byte total, or a ratio
  of two counts from the same run") does not describe those two rows, though the table does
  disclose the mixing inline. Reword the opening to exempt them.
- The µs half of `6_ARCHITECTURE.md` §7 fix #1 ("scan cost 6,053.6 us → about one 89 us dependent
  page probe; average read work −~280 us/req") is not measured here at all; §What is NOT measured
  names "latency percentiles", which a reader can take as covering it, but the per-region *mean*
  the prediction also moved is never named.

Smaller, disclosed-enough-or-harmless items:

- **R6 — inference stated as decomposition.** "4,096 B intended, 122 B of block padding" and
  "candidate blocks average 3,951 B" are averages: `122 = 4,218.28 − 4,096`, `3,951 =
  616,428/156`. The log does not decompose either figure, and `793,037 = 188 × 4,218 + 53` shows
  the per-scan cost is not a constant, so it cannot literally be one padded block per scan in
  every request. The supported statement is "4,218 B average per scan, +3.0 % over the predicted
  4,096". No conclusion depends on the attribution.
- **R7 — the tiled arm's entry count is not logged.** `head.out` prints `index_entries` only at
  the build, before the publish; the third column's `2,785 / 2,785` is inferred (the index is not
  rebuilt). Reprint it after the publish, or mark the cell as inferred.
- **R8 — "byte-identical" probes is loose.** `pre-fix.out`'s build line omits `tiles=`, its
  after-arm line prints `index_adopted=1` where `head.out` prints `index_lookups=188`, and the
  panic lands on line 260 of the copy vs line 264 of the file under audit — so the two are not
  byte-identical, and the sentence should read "identical except the build call and two
  print/assert sites". The pin itself is unaffected.
- **R12 — rounding.** `13,460` truncates `13,460.54` (nearest is `13,461`); `29,838` rounds
  `29,837.57` where the other figures truncate. All differences reconcile; at most 1 B.
- **R13 — "non-emptiness assertion".** `issues.md` and `run.sh` call the entries assertion the
  "non-emptiness assertion". What fires is `entries == MOVEMENTS` (index entries), not the
  answers' non-emptiness. The answers' non-emptiness does hold separately (`empty == 0`, and
  "empty" ⇔ zero rows, `request.rs:360-363`); the label is just attached to the wrong object.
- Minor shape deviation: `after-tiled` puts all 2,785 movements in **one** tile
  (`tile-movements.tile`), where the recorded shape would have them distributed. Immaterial to
  "one dependent probe per scan", and not disclosed.

## 5. Verdict

**PASS with notes** on "M3's clauses (a) and (c) are met on this evidence", read as: the mechanism
the fix claims is real, measured at the M1 instance's scale, and reproducible from the logs.

- **Clause (a), index-build half: met and correctly evidenced.** 2,785 entries for 2,785
  memtable-resident rows, the pre-fix build at 0, and the pre-fix log failing exactly here.
- **Clause (a), `coverage_csn` and `startup_warm scan_entries` halves: not evidenced by this
  artefact and not disclosed as unmeasured (R2, R3).** If those halves are met, the milestone row
  must cite the tests that carry them; on this artefact alone (a) would be **FAIL**, and the
  verdict would flip on a citation, not a measurement.
- **Clause (c), "`cargo test --workspace` green": outside this artefact** — `logs/workspace-tests.out`
  is 340 passed / 0 failed at HEAD (`grep -c "test result"` = 28, every one `0 failed`), but the
  *pinned-flag* run the clause names lives in `.scratch/85-m3-clause-c/`, which I did not open.
- **Clause (c), "a policy-off run unchanged": evidenced only in the weaker form** — same status,
  same row count, same scan counters in the no-index arm, not byte-identical answers (R1), and not
  a *run* (R10).

Nothing measured is wrong and nothing needs re-measuring. What must change is one assertion (or
two sentences of wording) for R1, and the milestone row's citation for R2/R3; R4-R13 are notes for
the next session that touches the file.

## Evidence pointers

- `.scratch/84-index-defect-mechanism/logs/head.out` (revision `23c4af1`), `logs/pre-fix.out`,
  `logs/workspace-tests.out`
- `engine/tests/index_defect_mechanism.rs` lines 167, 169, 201, 209-210, 240, 253, 255, 264-275,
  282-295
- `engine/src/request.rs:319` (lookup), `:320` (cross-check `get`), `:334` (scan), `:360-363`
  (`empty` ⇔ zero rows)
- `engine/src/engine.rs:3105-3110` (the lookups counter increments before the index check)
- `docs/6_ARCHITECTURE.md` §2 (the `940 × 638,938` scan row and the `265,360,600` non-scan
  total), §7 fix #1, §9 (`865,962,320/20000`)
- `docs/9_IMPLEMENTATION.md` row `T-83`; `docs/0_ROADMAP.md` §Milestones row M3
- `git log --oneline -- engine/tests/index_defect_mechanism.rs` (one commit, `23c4af1`);
  `git diff --stat 23c4af1..HEAD -- engine/` (empty)

## Commands run

```bash
# arithmetic, from the log values only
python3 -c 'b=115888464; t=793037; n=188; '
#   616428.0 / 4218.28 / remainder 53 / 3951.46 per tile / 42240.15 / 13466.29 / 43298.12 /
#   13460.54 / 28773.86 / 29837.57 / 96.43% / 2.44% / 3.52%
diff -u .scratch/84-index-defect-mechanism/probe-at-HEAD.rs engine/tests/index_defect_mechanism.rs  # empty
git diff --stat 23c4af1..HEAD -- engine/                                                               # empty
grep -n coverage engine/tests/index_defect_mechanism.rs                                               # 0 hits
grep -c "test result" .scratch/84-index-defect-mechanism/logs/workspace-tests.out                     # 28, all 0 failed
# tail sum of "test result: ok. N passed" over that file                                              # 340
```

No `cargo test` was run: the test file and the engine are unchanged since the log's revision, and
no number needed for this judgement is missing from the logs. The 200 rps paired comparison was
not run (`AGENTS.md` §Do not).
