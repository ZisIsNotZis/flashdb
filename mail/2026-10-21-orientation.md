kind:    report
from:    orient-flashdb (package launched by root)
to:      root
spend:   idle-compute
id:      none — first contract, answered in the run final message

First orientation report, filed here by root. Body is the run report verbatim.

**REPORT** — node whose roadmap this changes: **root, `/home/z/vibe`** (it files this and decides what flashdb's next dispatch is). Evidence pointer: the files named inline below, all inside `/home/z/vibe/flashdb` at `HEAD 8f334d0`, tree clean, nothing staged. Decision needed: one contract clarification and two releases the project cannot grant itself (listed under (c)).

---

## (a) What the project is

`flashdb` is a single-node, log-structured document store built to test one claim: that a per-region physical layout **learned** from the workload beats a **declared** one. It is a Rust workspace (`engine/`, `bench/nvme-bench`) plus a Python harness (`harness/`), and its own README states the position plainly: the internal learned-vs-declared result is positive (median +47.6% achieved throughput, pilot grade, 3 seeds + 1 repeat), the **external** comparison against sqlite and postgres on the same box was **lost** at 200 rps (3/5 sustained blocks vs 5/5 and 5/5, ratio 0.5x), and the loss is attributed to a defect in flashdb's own access path rather than to the thesis (`README.md:68-75`, record `.scratch/82-controlled-m1/report.md` §5 at `51c69f3`). The defect: `build_order_no_index` read tiles only, so on that append-heavy instance the derived `order_no` index held 0 entries while the merged scan returned 2,785 rows, and every one of the 4.7% scan requests paid a 5.6 MB full scan (`.scratch/83-index-probe/issues.md` §The defect, `docs/plan/open-items.md:11`). That defect is fixed in code across WP-1→WP-10 (`8640a80`, `f1f9912`, `cf6bf9a`, `79f5a5f`, `a01cb15`; bound by `engine/tests/order_no_index.rs`), and **whether the fix closes the external gap is unverified** because the paired measurement has not been re-run (`README.md:75`). The project lives by a milestone chain M1→M6 in `docs/roadmap.md:62-71` with status owned by `docs/roadmap.md` and `docs/plan/workplan.md` (the two `.scratch/*.md` status files carry supersession banners and no longer own status).

## (b) The milestone it proposes next

**M3 — access-path parity, completed by running its clause (b): the paired 200 rps declared-vs-indexed comparison, 5 blocks with arm rotation, on a quiet box (WP-2).**

Evidence that this is the next gate and not something else:

- `docs/roadmap.md:68` — M3's acceptance (b) is "at 200 rps the indexed arm sustains at least as many paired blocks as declared, or every remaining breach is attributed to a named phase with evidence"; the row itself is marked "code closed; (b)/(c) await a quiet box". `docs/roadmap.md:85-88` names M3 the **current** milestone.
- `docs/plan/workplan.md:58-62` — M3 status: (a) closed in code at 339 engine / 210 harness tests; (b) and (c) **open, needs the box**.
- `docs/plan/workplan.md:21` — WP-2 is the package that delivers (b): 5 paired blocks, snapshot restore verified, stderr kept, `lag_breach_attribution` per rejected window, verdict "indexed sustains >= declared" or the named phase still responsible. No WP-2 run exists at the current code.
- The only recorded paired runs are **invalid** and pre-date the fix: `.scratch/83-index-probe/runs/p{1,2,3}-{declared,indexed}.*` (12 files, 2026-10-19 06:57) show `lag_ceiling_rejected_windows` 14/1, 7/11, 15/17 — inconsistent, and the index was empty in both arms, so `.scratch/83-index-probe/issues.md:62-68` draws no conclusion from them.
- Why not M5: M5 depends on M3 and M4 (`docs/roadmap.md:70`) and additionally cannot launch its postgres leg (`docs/plan/workplan.md:66`, `docs/plan/open-items.md:101`). Why not M4: its forensic report is **delivered** (`docs/plan/gap-analysis.md`, 23.5 KB) and its acceptance permits an unclassifiable phase to be recorded as unmeasured — which is exactly what its one open reading (the ~573 ms group, `docs/plan/gap-analysis.md:80,82,136`) does.
- Leverage: this single run is the only measurement that changes the roadmap. Gap-analysis ranks the merged-view index as fix #1 on the KPI ladder — predicted read bytes/request 43,298 → 13,460 — and names WP-2 as its confirming instrument (`docs/plan/gap-analysis.md:131`). A win restores the external claim's premise; a loss converts it into an attributed gap with the phase named.

Recommended dispatch, in order: WP-2 first (cheapest, decides whether the premise survives), then WP-6 (WAL preallocation, gap-analysis fix #2) in the same quiet window, then WP-4's read-only review of the integrated M3 packages.

## (c) Questions the project cannot answer

1. **M3's clause (c) is defined two different ways — a contract contradiction.** `docs/roadmap.md:68` defines (c) as "`cargo test --workspace` green with the pinned mirror flags and a policy-off run unchanged" (locally decidable, no box needed). `docs/plan/workplan.md:62` records (c) as "the efficiency KPI comparison against sqlite/postgres needs the same quiet window, and the postgres leg additionally needs the server reinstalled" (i.e. M5 work). Tried: read both definitions and grepped the tree — the phrase "policy-off" appears only in the roadmap line, and no policy-off run record exists, so (c) as the roadmap defines it has never been evidenced either. What reopens it: a decision naming which (c) decides M3; until then M3 cannot be declared reached by a reader who checks the roadmap's own text.
2. **Quiet box — a resource the project cannot grant itself.** Tried: WP-2 is queued and never ran; ticket 83's first probe was invalidated by the author's other project (loadavg 1.9 → 9.9, `.scratch/83-index-probe/issues.md:65`); measured now, `uptime` returns **7.13 / 5.52 / 5.13** on 12 cores (2026-10-21T03:30Z), i.e. the blocker is live, not historical. `docs/plan/workplan.md:71-72` records the rule: WP-2 never runs under foreign load and queues with an explicit resume trigger. What reopens it: sustained loadavg < 2 before each arm run.
3. **Postgres is absent — a cost the project cannot release.** Tried: `which psql pg_ctl` → nothing; `/usr/lib/postgresql` → absent; only `libpq5` is installed, though ticket 82 measured postgres 18.6 in the lost comparison. What reopens it: an authorised install (system package or unprivileged user-space build), with the version and settings read back from the running server, and no substitute engine standing in for it (`docs/plan/workplan.md:66-70`, `docs/plan/open-items.md:101`).
4. **The ~573 ms group has no named mechanism.** Tried: the M1 records' phase ledger accounts for ~56 ms of the 573 ms (`docs/plan/gap-analysis.md:80`), 8 of 100 windows at 200 rps exceed 100 ms, compaction bytes are 0, and the class is *(b) defect* only by elimination — no instrument in the saved records splits append / barrier wait / apply. What reopens it: a per-group phase timer plus one 200 rps block with stderr kept (`docs/plan/gap-analysis.md:136,149`), which is gap-analysis fix #4.

## State of the record — what the next session would otherwise not know

Verified by reading, not by running the suite (read-only contract; the test count is a claim I checked by counting `#[test]` attributes, 339, which matches):

- `docs/plan/workplan.md:45` still lists **WP-5 as "queued"** while `docs/roadmap.md:69` marks M4 "report delivered" and `docs/plan/gap-analysis.md` is in the tree — the M4 table is stale.
- `docs/plan/open-items.md:11` (row T-83) still says "**327 tests**" and "the performance claim stays unmeasured" in the acceptance column, though the tree has 339 and the last two WP-9/WP-10 packages landed after that text. The HEAD commit `8f334d0` was an SSOT repair of five facts and missed this one.
- `docs/plan/workplan.md:21` makes WP-2 depend on **WP-1**, which the same table (line 20) marks "superseded by WP-1b" and was then carried through WP-8→WP-10; the real dependency is WP-10 (which landed at 335 tests in the WP-9 row) and the tests at 339 after `a01cb15`. Also note the WP-8 and WP-9 table rows have their scope/acceptance columns shifted (the row at line 24 carries WP-9's scope under the WP-8 id) — a reader reconstructing the file will misattribute work.
- A **zero-byte file named `8.8-9.7` is tracked at the repo root** (`git ls-files`, added in `59861dd`): an apparent shell-redirect artefact that the "no trash" rule says is never committed and is never deleted to tidy either — it needs a declared owner or a stated deletion.
- `.scratch/83-index-probe/runs/` holds 12 probe output files that name neither their invalidity nor the revision on their face; the invalidity lives only in the prose of `.scratch/83-index-probe/issues.md:62`. A later session reading the artefacts first would treat them as evidence.
- `.cgroup` at the root is ignored by `.gitignore`, so it is declared, not trash.

No files were written, no code changed, no design settled — this package's deliverable is this report.