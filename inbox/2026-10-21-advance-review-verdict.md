## Base I used

`f38ec63` — found from the project's own records, not from a hunch: the advance's first commit `1abd7ee` names it as parent, `f38ec63` is the previous session's final commit ("mail: M4 met as written…", 2026-10-21 12:45), its mailbox message `mail/2026-10-21-m4-met.md` is the contract this advance answers (present at `f38ec63`, deleted by `31c85c6` inside the advance), and `.scratch/88-m3-notes/issues.md` (new in the advance) cites `f38ec63` as the revision its runs were taken at. So the advance is `f38ec63..39c537f`, 8 commits: `1abd7ee` (test), `31c85c6`, `1748b65`, `2281116`, `1870e77`, `2af702f`, `27e7ba0`, `39c537f` (docs/mail/logs). `git diff --stat f38ec63..39c537f` shows only `engine/tests/index_defect_mechanism.rs` (+14/−2) as code; everything else is docs, mailbox and logs.

## Verdict per milestone

**M3 — not met as its own acceptance writes it; two of three clauses met at 39c537f.**

- (a) met, verified: `engine/tests/order_no_index.rs:171` binds *memtable-only region → one entry per row and `coverage_csn` = 3 with rows committed out of order* (so "last row's csn" would fail); `:221` binds the tombstone/re-version case (2 entries, coverage 2, max over live versions); `:256` binds the loud refusal naming produced entries, visited rows and the signal, and writes no sidecar; `engine/src/readahead.rs:583` binds `warm_startup scan_entries == 2` for a region with no tile. All four green in my run at `39c537f`. The scale half reproduces: `index_entries=2785` for 2,785 rows, `scans_entered_after=0` of 188, tiled 4,218 B/scan.
- (c) met, verified: pinned-flag offline workspace suite at `39c537f` → exit 0, **340 passed / 0 failed / 0 ignored across 28 test binaries**; the answers comparison now carries status + serialized row with a non-vacuity assertion (`engine/tests/index_defect_mechanism.rs:198-215, 271-283`), and `movement_scan_falls_back_to_full_scan_when_index_absent` binds the policy-off path. The disclosed limit (one in-process arm, not the driver's policy gate) is written where the claim is.
- (b) **deferred, unrun**: no paired 200 rps declared-vs-indexed block exists anywhere in the tree, so "the indexed arm sustains at least as many paired blocks as declared" is neither met nor falsified. The advance says exactly that ("reached on (a) and (c); (b) deferred") and the deferral has an authorising contract (`mail/2026-10-21-m4-met.md`, read from `git show f38ec63:`). M3 as a gate is therefore **not closed**.

**M4 — met under the reading the layer above recorded; the central ledger reproduces from the committed records.**

- `docs/plan/gap-analysis.md` is committed (last touched `f584ecc`) and does what the acceptance column asks: §4 classifies every phase (barrier count **(c)** + the +1,432 us vs postgres as **(b) candidate**; non-scan reads **(c)**; movement scan **(b) defect**; the ~573 ms group **(b) by elimination, mechanism `unmeasured`**), §7 lists six fixes each with predicted KPI effect and confirming instrument, §8 gives a falsifier per attribution, and every unmeasured reading carries its instrument.
- I re-derived §2 from the committed window files myself (`python3` over `.scratch/82-controlled-m1/runs/b*/rung-200/windows-flashdb-declared.jsonl`): 94,760 reads, 865,962,320 B, `P|StockMovement` 940 @ 638,938 B/scan, **43,298 B/request**, non-scan 13,268 B/req — matching the document, and matching the README's `42,240 → 13,466` arithmetic (0.047 × 616,428 + 13,268 = 42,240; 0.047 × 4,218 + 13,268 ≈ 13,466).
- The M4 "DONE" is an *assent*, and its source is recoverable: `mail/2026-10-21-m4-met.md` (contract, root) rules explicitly that an unclassifiable phase recorded as unmeasured with its instrument named satisfies the acceptance.

## Rerunnable from the committed tree alone — yes for the four evidence claims, with my reruns below

1. Workspace suite `340/0`: rerun at `39c537f`, exit 0, 340/0.
2. Mechanism + row-bytes answers: rerun at `39c537f`, 1 passed, same counters as the committed log (2,785 entries, 0/188 scans, 616,428 → 0 / 4,218 B per scan).
3. Negative control (`gate probe`): `git archive f584ecc | tar -x -C /tmp/rev-prefix`, the committed `pre-fix-probe.rs` copied to `engine/tests/index_defect_mechanism.rs`, run in `/tmp` → **fails** at the entries assertion with `left: 0, right: 2785`. The test is not vacuous and nothing about it needs the uncommitted worktrees.
4. WP-12's recomputation: `scripts/m1-controlled-score.py` over the committed runs → `table.reproduced.md` sha256 `cda02ab0833f60559b32ffffcc98841012f41299a758f016379d9952ad2ccad7`, **byte-identical** to `table.md`, diff 0 lines — so the README's "recompute with `.scratch/87-m1-decided-rule/run.sh`" holds from the tree (I ran its commands with outputs redirected to `/tmp` so I wrote nothing in the project).

Pre-existing caveat, cured rather than satisfied by the logs: `.scratch/88-m3-notes/logs/index-defect-mechanism-rowbytes.out` binds to `f38ec63 + worktree edit`, i.e. a non-commit state, and ticket 84's `head.out` binds to `HEAD 23c4af1`. My rerun at `39c537f` is what makes the claim true at a named revision.

## Blocking — things that would make a pass unsafe even though the checks pass

1. **M3(b) is unrun.** Passing "M3 met" would be unsafe: no rate was measured, and work-per-request counts are not a rate. The specific danger is a downstream reader treating "2,785 entries, 0 of 188 scans" as recovery of 200 rps — the project's own `dev-loop.md`/decision 6 forbid it, and the 12 invalid probe files are the measured cost of doing that.
2. **M4 is "met", not "attributed".** The phase where the rung is actually lost (~573 ms group, two lag breaches 30,430 / 33,747 us) is classified `(b)` by elimination with **no named mechanism**, and its classification rests on what the evidence is *not* (compaction bytes 0, no phase timer). Passing M4 as if the gap were attributed is unsafe downstream: an M5 re-run without fix #4's per-group phase timer reproduces 3 of 5 and names no phase. The roadmap and `open-items.md` leverage item 2 do say this; only a reader who stops at "M4 DONE" is exposed.
3. **M4's ledger is a pre-fix reading.** Its records predate the access-path fix, so its scan-phase `(b)` classification and 6,053.6 us/scan are historical; the fix is now measured (42,240 → 13,466 B/req). Reusing those phase costs as current would be wrong.
4. **The "not quiet" box.** loadavg 13–23 during every cited run. Irrelevant to functional green (no timing assertion decides these claims), fatal to any timing reading taken from these logs — none is claimed. Any future pass must keep that separation.
5. **Disclosed limit on (c):** the policy-off reading is one in-process arm through `execute_request`, not a run through the driver's policy gate. It is stated, so it is not a surprise; it is also not the same artefact the roadmap's wording ("a policy-off run unchanged") could be read to demand.

## Defects found (none blocking; I repaired nothing)

- **`mail/README.md:3`** — says "Four header lines" while the block it introduces lists five (`kind`, `from`, `to`, `spend`, `id`), and the commit that added the file (`1748b65`) says "five header lines". Criterion: a document may not be wrong; one fact, one home. Repro: `sed -n '1,12p' mail/README.md`.
- **`mail/2026-10-22-m3-evidence-closed-m4-met.md`** (mail body, "Nothing in this advance produces a throughput or latency number, and the box was not quiet (loadavg 13.20 on the first line of every log)") — 13.20 is only `workspace-head.out`; the other two are 21.43 and 23.35. Criterion: primary evidence — a claim about every log, traceable to the logs. Repro: `head -1 .scratch/88-m3-notes/logs/*.out`.
- **`docs/roadmap.md` M3 row, clause (a)** cites `engine/src/bin/flashdb-load.rs:3065` for "`coverage_csn` = max covered csn". That test asserts coverage equals the csn the build snapshotted and advances on rebuild; the assertion that actually binds *max over rows, not the last row's csn* is `engine/tests/order_no_index.rs:171` (and `:221`). The cited test is true but weaker than the clause; the stronger test is uncited in the roadmap (it is cited only implicitly through WP-4's notes). Criterion: evidence at source.
- **M4 deliverable half**: the row's deliverable says "a per-phase ledger for the M1 **and M3** records"; `gap-analysis.md` §2 ledgers M1 only (the M3/ticket-84 mechanism is cited, not led). The Acceptance column does not require it, so this does not move the verdict — recorded as an observation, not a defect against acceptance.

## Not exercised

- The `harness/` Python suite (WP-3's "210 tests green") — not named by any command this advance's evidence relies on.
- Anything wall-clock: no timing claim was checked, and none should be from these logs.
- WP-6's code half: named in-flight/unreviewed in the mail; no branch of it is in this tree (`git worktree list` shows only the main tree), so nothing of it was reviewed here.