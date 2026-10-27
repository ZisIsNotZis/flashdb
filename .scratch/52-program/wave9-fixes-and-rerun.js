// Phase 9: integrate wave8/eval docs; fix the two pilot-blocking defects (scan-shape
// read readiness + learner churn) in parallel lanes; integrate; re-evaluate T1/T2; verify.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const M = "/home/z/vibe/worktrees/flashdb";
const WSCAN = M + "/wave9-scanwarm";
const WCHURN = M + "/wave9-churn";
const WEVAL = M + "/wave10-eval";

const gateLines = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at 29d7658. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave8/eval` into master (carries the WP-H pilot ticket + artifacts; docs only, expect no code conflicts). If the shell reports it is already up to date or nothing to merge, reconcile and say so precisely.",
"2. Create the two lane worktrees from integrated master: `git worktree add -b wave9/scanwarm " + WSCAN + " master` and `git worktree add -b wave9/churn " + WCHURN + " master`. Verify and report both HEADs.",
"3. Run the combined gates at the new master HEAD and capture verbatim.",
"4. Append an 'Integration record: wave8/eval' section to .scratch/52-program/issues.md (real clock, commit, gates). Commit any untracked .scratch/52-program/*.js control scripts. Commit.",
"REPORT: master HEAD, merge result, both lane worktree paths/HEADs, verbatim gates."
].join("\n");

const scanTask = [
"flashdb work package I: make the win workload's SCAN reads part of the read-ahead warm set so scan latency stops becoming schedule lag. Single writer in " + WSCAN + " (isolated worktree, branch wave9/scanwarm, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read them first): .scratch/STATUS.md (SSOT) and .scratch/59-eval-pilot/issues.md. WP-G (ticket 58, engine/src/readahead.rs + `Engine::tile_read_view`) took cold UNIQUE point lookups off the pacer by warming their blocks on a background worker; rung-50 lag then went to 0/40 on the point-lookup workloads. But the WP-H pilot found that NO arm sustains rung 50 on the win workload because the win mix is ~30% `movements_scan`, and `request_read_lookups` returns `[]` for the scan shape, so the scan does cold full-entity reads synchronously on the pacer. That is a READINESS GAP (both arms fail), not a thesis verdict.",
"",
"OBJECTIVE: extend the read-ahead warm path to the scan shape(s) the win workload actually issues, so scan reads no longer block the single pacer, while preserving every invariant (layout never on the correctness path; the warm path is read-only and never feeds a decision; authoritative reads stay inline; durability untouched).",
"",
"DO: first DIAGNOSE exactly what `movements_scan` (and any other scan shape in the win mix) reads -- which keys/families/entities, how many blocks, dependent or not -- by reading harness/flashdb_harness/cold_0b.py's win mix and engine/src/request.rs + the driver. Then choose the smallest sound mechanism and record rejected alternatives: e.g. (a) extract the scan's read set in `request_read_lookups` and warm it; (b) warm bounded predicted scan ranges; (c) make the scan read itself overlapped/async. BOUND the warm work (a scan may touch many blocks; do not turn read-ahead into a second full-scan that saturates the device). If warming a scan is unsound or unbounded, say so with evidence and implement the sound alternative.",
"",
"MEASUREMENT: cp -a copies of /tmp/cold51/per_region-mixed (12.25x) and, if time, /tmp/cold44/per_region-mixed (8.17x). Build once, copy the binary, run only that copy, one process at a time. Use the harness `cold_0b.py` with `--workloads win --arms declared` for a before/after on the win workload (the point-lookup workloads must not regress). Report per-rung achieved_rps, lag-rej, svc-rej, lag p99, and the breach attribution. The box is shared/noisy -- record load and repeat a run.",
"JUDGING: report BOTH sustained-by-lag-only and sustained-by-both-ceilings; keep the primary-vs-service-ceiling reconciliation an OPEN AUTHOR ITEM (do not edit the pre-registration or docs/2_GOAL.md's thresholds).",
"ACCEPTANCE: on the win workload, scan-attributed lag breaches are removed (rung 50 lag-rej drops materially toward 0) with no regression on the point-lookup workloads and no read/write amplification blowup; OR the exact remaining cause with evidence. Never overstate.",
"TESTS: add tests that fail against the pre-change behaviour where meaningful; keep all Rust and Python tests green.",
"TICKET .scratch/60-scan-readahead/issues.md: contract, diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after table, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gateLines + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const churnTask = [
"flashdb work package J: stop the learner churning on the adversarial-uniform workload (T2). Single writer in " + WCHURN + " (isolated worktree, branch wave9/churn, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read them first): .scratch/STATUS.md (SSOT), .scratch/59-eval-pilot/issues.md, and harness/flashdb_harness/layout_learner.py. The learner is a per-region UCB1 with slice-39 churn control: a per-region `cooldown_windows` (default 3) and a per-window `reorg_budget_bytes_per_window` derived from the declared arm's measured reorganisation rate (default budget factor 1.0). The WP-H pilot measured that on the adversarial-uniform workload declared sustains rung 50 at 0/40 lag-rejections on BOTH seeds, but learned does NOT (8/40, 9/40) and writes 3.6-4.75x declared's reorganisation bytes (110.7/91.1 MB vs 23.3/25.0 MB); throughput is identical (106-107 rps), so the loss is learner churn showing up as lag. The pre-registration explicitly makes this disqualifying (T2: a learner that churns on noise is rejected).",
"",
"OBJECTIVE: make the learner not churn on a uniform/no-locality workload, so learned matches declared on adversarial (reorg within the declared band and rung-50 lag-sustained like declared), WITHOUT giving up adaptation where adaptation is warranted (the win and drift workloads must still let the learner explore/adapt -- verify you did not just freeze it globally).",
"",
"DIAGNOSE FIRST, with numbers: instrument WHY the slice-39 control fails on adversarial -- e.g. is the cooldown (3 windows) too short relative to the workload length; does the 'unexplored challenger projects 0 added bytes so the budget never blocks it' rule let perpetual exploration through; does the reward have no penalty for churn; how many policy changes actually fire per window? Read the actual `learner_decisions` from the WS-H artifacts in .scratch/59-eval-pilot/*.json and quantify. Then choose the smallest sound fix and record rejected alternatives: e.g. price churn in the reward, make unexplored candidates project a conservative reorg cost, lengthen/scale the cooldown, require a confidence/minimum-sample gate and a minimum *improvement* to switch, or freeze after the reorg budget is exhaustively spent. Do not reach the target by simply disabling exploration.",
"",
"MEASUREMENT: cp -a copies of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Use `cold_0b.py` with `--workloads adversarial --arms declared learned` and `--workloads win --arms declared learned` for before/after; at least 2 seeds each and a repeat for a noise band. Report per-rung achieved_rps, lag-rej, svc-rej, reorg bytes, and the learner decision count. The box is shared/noisy -- record load.",
"JUDGING: report BOTH sustained-by-lag-only and sustained-by-both-ceilings; keep the primary-vs-service-ceiling reconciliation an OPEN AUTHOR ITEM (do not edit the pre-registration).",
"ACCEPTANCE: on adversarial-uniform, learned's reorganisation is within declared's band (say <= 1.5x) and learned is rung-50 lag-sustained (0/40) like declared, at >= 2 seeds; and on win the learner still explores/adapts (decision count > 0). OR the exact remaining cause with evidence. Never overstate; a pilot-grade result is labelled as such.",
"TESTS: add/extend harness tests (harness/tests/test_layout_learner.py, test_cold_0b.py) that fail against the pre-change churn behaviour; keep all Python and Rust tests green.",
"TICKET .scratch/61-learner-churn/issues.md: contract, quantified diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gateLines + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const scanVerifyTask = [
"You are a fresh independent verifier for WP-I, worktree " + WSCAN + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/readahead.rs, engine/src/request.rs, engine/src/engine.rs (the `request_read_lookups` / warm path), the flashdb-load driver, engine/tests/, and ticket .scratch/60-scan-readahead/issues.md.",
"PASTE exact output for: " + gateLines + ".",
"FALSIFY: (a) is the warm path strictly read-only and off the correctness path, with authoritative reads still inline and newest-first precedence intact? Look for any stale-snapshot or race fault. (b) is the scan read set genuinely the one the win mix issues, and is the warm work BOUNDED (not an unbounded second scan)? (c) are the before/after figures and the scan-attribution reproducible from the raw /tmp artifacts (state paths)? (d) does the ticket honestly state anything not achieved and keep the open author item? (e) do the new tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const churnVerifyTask = [
"You are a fresh independent verifier for WP-J, worktree " + WCHURN + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: harness/flashdb_harness/layout_learner.py, harness/flashdb_harness/cold_0b.py, harness/tests/, and ticket .scratch/61-learner-churn/issues.md.",
"PASTE exact output for: " + gateLines + ".",
"FALSIFY: (a) was the fix achieved by disabling exploration rather than controlling churn? Check the win/drift behaviour: does the learner still adapt (decision count > 0) where adaptation is warranted? (b) are the adversarial reorg/decision figures reproducible from the raw /tmp artifacts (state paths), and is the before/after a real >= 2-seed result (not a single run)? (c) is the reward/switch rule sound (a genuine minimum-improvement or conservative-projection gate), not a hardcoded special case for the adversarial workload? (d) does the ticket honestly state anything not achieved and keep the open author item? (e) do the new tests exercise the change and fail against pre-change behaviour? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ2Task = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at the WP-H integration HEAD (it already contains the wave8/eval docs). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave9/scanwarm` then `git merge --no-ff wave9/churn` into master (disjoint layers: engine vs harness; expect no conflicts). Resolve minimally and record any conflict.",
"2. Create the re-evaluation worktree from integrated master: `git worktree add -b wave10/eval " + WEVAL + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gateLines + ".",
"4. Append an 'Integration record: wave9/scanwarm + wave9/churn' section to .scratch/52-program/issues.md (real clock, commits, gates). Commit.",
"REPORT: master HEAD, both merges, wave10 worktree path/HEAD, verbatim gates."
].join("\n");

const reevalTask = [
"flashdb work package K: RE-RUN the T1/T2 evaluation on the engine AFTER the WP-I scan-readahead fix and the WP-J learner-churn fix, to see whether the pilot's two blocking findings are resolved. Single writer in " + WEVAL + " (isolated worktree, branch wave10/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md (SSOT) and .scratch/59-eval-pilot/issues.md first. The pilot found T1 (win) UNDERIVABLE (scan reads on the pacer) and T2 (adversarial) FAIL direction (learner churn: 3.6-4.75x reorg, 8-9/40 lag-rej vs declared 0/40). WP-I and WP-J were meant to fix exactly those. This package re-measures.",
"",
"RUN: instance `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy). Arms declared + learned (+ static-derived if cheap; skip it if it doubles wall-clock and say so). Workloads win (T1) and adversarial-uniform (T2). Full ladder 50/100/200/400. At least 3 independent access seeds per (arm, workload) plus a repeat for a noise band -- MORE if wall-clock allows. One process at a time under the memory cap; record box load at every run boundary.",
"",
"REPORT, honestly:",
"1. T1 (win): does learned beat declared by >15% in sustained load level? Is the win workload lag-sustained for any arm now (was the scan gap closed)? Give numbers + noise band + seed count; label underivable if still not measurable.",
"2. T2 (adversarial): does learned stay within declared's reorg band and remain rung-50 lag-sustained like declared? Give the ratio, lag-rej, seed count.",
"3. Both judging views (lag-only per STATUS 7.1, and both-ceilings); flag the harness's own both-ceilings 'sustain' artifact if it is a 0/0 disguise.",
"4. Whether the slice-39 zero-gain problem persists (numbers), and any NEW defect the fixes introduced.",
"5. The wall-clock cost per seed and what a full 10-seed pre-registered campaign would take.",
"TICKET .scratch/62-eval-rerun/issues.md: contract, exact commands, per-arm/rung tables, both judging views, T1/T2 verdicts with seed counts and noise band, residual gaps, cost estimate. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gateLines + ". Commit; report hash and the honest verdict (a pilot-grade result is labelled as such; do NOT claim a 10-seed pre-registered verdict)."
].join("\n");

const reevalVerifyTask = [
"You are a fresh independent verifier for WP-K, worktree " + WEVAL + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: the ticket .scratch/62-eval-rerun/issues.md, the committed JSON/MD run artifacts, and (to confirm the fixes are actually present) the relevant engine read path and harness learner. Diff the worktree against the pre-WP-I/WP-J base and confirm both fixes are in the measured revision.",
"PASTE exact output for: " + gateLines + ".",
"FALSIFY: (a) is the measured revision the one that actually contains BOTH the WP-I and WP-J fixes (paste the relevant code/hashes)? (b) are the T1/T2 verdicts computed from the raw committed artifacts with the true seed count and a measured noise band, and are both judging views reported? (c) do the numbers show the pilot's two findings are resolved, or honestly still open? (d) does the ticket avoid claiming a pre-registered 10-seed verdict it did not earn? (e) is the pre-registration untouched? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Integrate wave8 docs and create wave9 lanes", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };

const lanes = await runs.all([
  { key: "scan", label: "WP-I: scan-shape read-ahead", agent: "worker", model: STRONG + ":high", cwd: WSCAN, task: scanTask },
  { key: "churn", label: "WP-J: stop learner churn on adversarial", agent: "worker", model: STRONG + ":high", cwd: WCHURN, task: churnTask }
]);

const reviews = await runs.all([
  { key: "vscan", label: "Verify WP-I", agent: "worker", model: STRONG + ":high", cwd: WSCAN, task: scanVerifyTask },
  { key: "vchurn", label: "Verify WP-J", agent: "worker", model: STRONG + ":high", cwd: WCHURN, task: churnVerifyTask }
]);

const integ2 = await runs.run("integ2", { label: "Integrate wave9 lanes and create wave10", agent: "worker", model: STRONG, cwd: REPO, task: integ2Task });
if (!integ2.ok) return { setup: { ok: setup.ok, output: setup.output }, lanes, reviews, integ2: { ok: false, output: integ2.output } };

const reeval = await runs.run("reeval", { label: "WP-K: re-run T1/T2 evaluation", agent: "worker", model: STRONG + ":high", cwd: WEVAL, task: reevalTask });
const reevalVerify = await runs.run("vreeval", { label: "Verify WP-K", agent: "worker", model: STRONG + ":high", cwd: WEVAL, task: reevalVerifyTask });

return {
  setup: { ok: setup.ok, output: setup.output },
  scan: { ok: lanes[0].ok, output: lanes[0].output },
  churn: { ok: lanes[1].ok, output: lanes[1].output },
  vscan: { ok: reviews[0].ok, output: reviews[0].output },
  vchurn: { ok: reviews[1].ok, output: reviews[1].output },
  integ2: { ok: integ2.ok, output: integ2.output },
  reeval: { ok: reeval.ok, output: reeval.output },
  reevalVerify: { ok: reevalVerify.ok, output: reevalVerify.output }
};
