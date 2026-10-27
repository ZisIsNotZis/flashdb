// Phase 19: integrate the decisive WP-S eval; measure T4 (drift recovery); refresh the SSOT to the
// full T1-T4 state; verify. This completes the milestone (re-derive T1/T2/T4 on the >=10x instance).
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb";
const WT4 = W + "/wave19-drift";
const WS = W + "/wave20-ssot";

const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master = wave16 integration + wave18 scribe; verify with git log). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave17/eval` into master (carries the WP-S ticket 71 + artifacts; docs only). Resolve minimally and record any conflict.",
"2. Create the T4 worktree: `git worktree add -b wave19/drift " + WT4 + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gates + ".",
"4. Append an 'Integration record: wave17/eval' section to .scratch/52-program/issues.md (real clock, commit, gates). Commit any untracked .scratch/52-program/*.js.",
"REPORT: master HEAD, merge result, wave19 worktree path/HEAD, verbatim gates."
].join("\n");

const t4Task = [
"flashdb work package U: measure the pre-registered T4 (drift recovery) threshold on the >=10x instance. Single writer in " + WT4 + " (isolated worktree, branch wave19/drift, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md, .scratch/15-objective-v0/pre-registration.md (T4 verbatim), and tickets 29, 38, 39, 71. WP-S re-derived T1/T2/T3: the learned/static layout (the WP-M `order_no` index axis) sustains rung 100 lag-only (static 8/8 seeds, learned 6/8) while declared never sustains any rung, +42.7-52.9% throughput. T4 is the last underived threshold. The harness `cold_0b.py` supports `--workloads drift` with a fixed/auto drift rate; slices 38/39 recorded that the drift arm must run BELOW the engine's capacity for the pre-drift mix so recovery has a baseline (an above-capacity rate makes every arm saturate and T4 unevaluable).",
"",
"OBJECTIVE: measure whether the learner recovers to within 10% of its pre-drift throughput within 2 windows after the drift, while `static-derived` does not (or recovers measurably slower). First VALIDATE the drift arm is evaluable: choose a drift rate below the pre-drift (win-mix) capacity so at least one arm is not saturated at the pre-drift level (use the harness's `--drift-rate`/`auto` and `--drift-rate-fraction`); if no evaluable rate exists, PROVE it with the capacity curve and report T4 as underivable rather than faking it. Then run learned + static-derived (+ declared if cheap) at the evaluable rate, >=3 seeds + a repeat, serial (no concurrent measurement), on `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy).",
"",
"REPORT, honestly:",
"1. Is the drift arm evaluable? The pre-drift and post-drift capacity, and the chosen drift rate as a fraction of pre-drift capacity.",
"2. T4: per-window throughput around the drift point for learned vs static-derived; does learned recover to within 10% of its pre-drift throughput within 2 windows? Does static-derived fail to (or recover measurably slower)? Give the window-level numbers, seed count, and noise band.",
"3. Both judging views (SS7.1 lag-only primary; both-ceilings). Do not edit the pre-registration.",
"4. The honest overall T4 verdict, and any residual/gap.",
"MEASUREMENT: build once, copy the binary, run only that copy, one process at a time; record box load. Keep all Rust/Python tests green.",
"TICKET .scratch/73-t4-drift/issues.md: contract, evaluability proof, exact commands, per-window tables, both judging views, the T4 verdict with seed count and noise band, residual. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and the honest T4 verdict (pilot-grade; NOT a 10-seed claim unless you ran it)."
].join("\n");

const t4VerifyTask = [
"You are a fresh independent verifier for WP-U, worktree " + WT4 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: ticket .scratch/73-t4-drift/issues.md, the committed run artifacts, and the harness drift path.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the drift arm genuinely EVALUABLE (pre-drift rate below at least one arm's capacity, with the capacity evidence), or is an 'underivable' verdict honestly proven? (b) is the T4 comparison (learned recovers within 10% in 2 windows; static-derived does not / slower) computed from the raw per-window artifacts with the true seed count and a real noise band? (c) does the ticket avoid overclaiming and keep the open author item? (d) is the pre-registration untouched? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const ssotTask = [
"flashdb work package V: refresh the current-fact SSOT to the completed T1/T2/T3/T4 state. Single writer in " + WS + " (isolated worktree, branch wave20/ssot, based on integrated master AFTER the WP-U merge; the integrator creates this worktree). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Doc-only.",
"",
"READ the SSOT (.scratch/STATUS.md, docs/2_GOAL.md, docs/8_DETAIL.md) and the terminal tickets: 64 (learned access path), 65 (WP-N), 66 (learner churn), 67 (write stall), 68 (WP-Q), 69 (startup warm), 70 (oscillation), 71 (WP-S decisive), 73 (T4 drift). Verify against the tickets, not memory.",
"",
"OBJECTIVE: state, as current fact and nothing else, the re-derived thresholds on the >=10x instance under the author's adopted SS7.1 lag-only primary reading: (1) T1 -- the learned/static `order_no` access-path layout sustains rung 100 lag-only (static-derived 8/8 seeds, learned 6/8) while declared never sustains any rung, +42.7-52.9% throughput (median +48.6%, outside the 2.48% noise band); rung-50 0/40 is NOT reached (residual: compaction-adjacent cold reads + the device fsync tail); (2) T2 -- reorg median ~0.93x (<=1.5x), learned at/better than declared on adversarial, 0 index builds on the 0-scan region; (3) T3 -- win reorg median ~0.93x (<=3x), no single-region blowup; (4) T4 -- the WP-U verdict (positive or honestly underivable, per ticket 73); (5) the engine-latency facts (compaction off-path, overlapped WAL barrier + deferred append, non-blocking boundary, read-ahead warming incl. eager startup; 304 Rust / 140 Python tests); (6) the open author items: the SS7.1 primary-vs-service-ceiling reconciliation, the tile-cap fix (7.4, adopted-unimplemented), and any T4/residual gap. Point to the ticket paths for history.",
"",
"RULES: FACT document, rewritten in place; current facts only, one home per fact, no dated narrative/changelog/'was A now B'; keep existing structure and budget notes; do NOT touch .scratch/15-objective-v0/pre-registration.md; do NOT strengthen any verdict beyond what the tickets support (label pilot-grade / seed counts where the tickets do).",
"TICKET .scratch/74-ssot-final/issues.md: what changed, ticket sources, diff stat. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and the sections changed."
].join("\n");

const ssotVerifyTask = [
"You are a fresh independent verifier for WP-V (SSOT refresh), worktree " + WS + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: .scratch/STATUS.md, docs/2_GOAL.md, docs/8_DETAIL.md, .scratch/74-ssot-final/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) does every changed statement match its source ticket (64-73) -- spot-check T1 (static 8/8, learned 6/8 at rung 100; declared never; +42.7-52.9%), T2/T3 medians, the T4 verdict, the rung-50 residual, and the test counts? (b) current-fact-only, one home per fact, no stale contradiction? (c) pre-registration untouched? (d) open author items still open? (e) no unearned 10-seed claim? Report blocking defects with location and the contradicting ticket evidence.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings."
].join("\n");

const setup = await runs.run("setup", { label: "Integrate WP-S and create the T4 lane", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };

const t4 = await runs.run("t4", { label: "WP-U: measure T4 drift recovery", agent: "worker", model: STRONG + ":high", cwd: WT4, task: t4Task });
const t4v = await runs.run("vt4", { label: "Verify WP-U", agent: "worker", model: STRONG + ":high", cwd: WT4, task: t4VerifyTask });

const setup2Task = [
"You are the integrator. Repo /home/z/vibe/flashdb (master). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave19/drift` into master (carries the WP-U ticket 73 + artifacts; docs only). Resolve minimally.",
"2. Create the SSOT worktree: `git worktree add -b wave20/ssot " + WS + " master`. Report its HEAD.",
"3. Append an 'Integration record: wave19/drift' section to .scratch/52-program/issues.md (real clock, commit). Commit.",
"REPORT: master HEAD, merge result, wave20 worktree path/HEAD."
].join("\n");

const setup2 = await runs.run("setup2", { label: "Integrate WP-U and create the SSOT worktree", agent: "worker", model: STRONG, cwd: REPO, task: setup2Task });
if (!setup2.ok) return { setup: { ok: setup.ok, output: setup.output }, t4: { ok: t4.ok, output: t4.output }, t4v: { ok: t4v.ok, output: t4v.output }, setup2: { ok: false, output: setup2.output } };

const ssot = await runs.run("ssot", { label: "WP-V: refresh the final SSOT", agent: "worker", model: STRONG + ":high", cwd: WS, task: ssotTask });
const ssotv = await runs.run("vssot", { label: "Verify WP-V", agent: "worker", model: STRONG + ":high", cwd: WS, task: ssotVerifyTask });
return {
  setup: { ok: setup.ok, output: setup.output },
  t4: { ok: t4.ok, output: t4.output },
  t4v: { ok: t4v.ok, output: t4v.output },
  setup2: { ok: setup2.ok, output: setup2.output },
  ssot: { ok: ssot.ok, output: ssot.output },
  ssotv: { ok: ssotv.ok, output: ssotv.output }
};
