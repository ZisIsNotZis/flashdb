// Phase 16 (last mile): close the two named residuals so the +43-57% layout win becomes a clean
// lag-only SUSTAINED result -- (R1) eager startup warm so windows 0-2 stop breaching; (R2) bound
// the single-region merge oscillation. Also repair the WP-P F1 write-amp claim. Then final eval.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb";
const R1 = W + "/wave16-startwarm";
const R2 = W + "/wave16-oscillation";
const WE = W + "/wave17-eval";

const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"You are the integrator+scribe. Repo /home/z/vibe/flashdb (master is at the wave14 integration HEAD + WP-Q ticket; verify with git log). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave15/eval` into master (carries the WP-Q ticket 68 + artifacts; docs only). Resolve minimally.",
"2. SCRIBE REPAIR (the WP-P verifier's blocking finding F1): in .scratch/67-requestpath-write-stall/issues.md, the write-amplification non-regression paragraph quotes the DECLARED arm's 8.0-8.9 MB as if it were static-derived and says the after-runs match the before-runs. Correct it to the true static-derived figures from the committed JSONs: before seed42=21.9 / seed43=21.9 / seed42-repeat=30.2 MB; after seed42new=26.4 / seed43new=94.9 / seed42repeatnew=98.0 MB (2 of 3 after-runs 3.2-4.3x higher), attributed to a single ~69-72 MB U|Customer/P|Customer merge, plausibly shared-box timing noise, NOT causally tied to the read-path-only follow-on; mark the non-regression criterion as NOT substantiated by the current evidence pending the WP-R re-measure. Commit the correction with a clear message.",
"3. Create both lane worktrees from integrated master: `git worktree add -b wave16/startwarm " + R1 + " master` and `git worktree add -b wave16/oscillation " + R2 + " master`. Report both HEADs.",
"4. Commit any untracked .scratch/52-program/*.js control scripts.",
"REPORT: master HEAD, merge result, the F1 correction commit, both worktree paths/HEADs."
].join("\n");

const startWarmTask = [
"flashdb work package R1: eagerly warm the win scan + its index at run start so the read-ahead startup window stops breaching the lag ceiling. Single writer in " + R1 + " (isolated worktree, branch wave16/startwarm, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read .scratch/STATUS.md and tickets 64, 65, 67, 68 first). The engine latency work is done and the WP-M learned secondary access path gives a robust +43-57% throughput win on the win workload. The ONLY remaining rung-50 lag-ceiling breaches for the frozen `static-derived` layout are now the read-ahead worker's STARTUP warm-up: at the first windows (0-2) the scan prefix and the adopted `order_no` index tiles are not yet warmed, so the first scans take cold page-ins on the pacer (`lag_breach_attribution reads=N boundary=0`). WP-Q measured static-derived win rung-50 = 3/2/2/3 out of 40, all in windows 0-2; learned 13/4/3/3 with the extra seed42 blowup from a separate merge-oscillation (the sibling lane R2).",
"",
"OBJECTIVE: eliminate the startup warm-up breaches with the smallest sound change: e.g. warm the relevant scan prefix (and the adopted index tile) EAGERLY at engine open / run start, before the first window, rather than waiting for the background worker's first cadence; or make the driver perform a bounded synchronous warm-up pass before the timed windows begin. Preserve all invariants: the warm path is read-only and off the correctness path; authoritative reads stay inline; durability untouched; the warm work is bounded (do not add a second full scan to the request path). Do NOT game the metric (do not extend or skip the timed window, do not pre-issue requests).",
"",
"MEASUREMENT: cp -a copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time (no other measurement running). Harness `cold_0b.py`, `--workloads win --arms declared learned static-derived` (adversarial if cheap). At least 3 seeds + a repeat. Report per-rung achieved_rps, lag-rej, svc-rej, lag p99, and the breach attribution for the first 3 windows specifically. Record box load.",
"JUDGING: the author's ADOPTED reading is SS7.1 lag-only primary; report both that and both-ceilings. Do not edit the pre-registration.",
"ACCEPTANCE: on the win workload with the index adopted, `static-derived` rung-50 lag-rej reaches 0/40 (across >=3 seeds), and learned is within a small band of static-derived (its extra breaches being the separate R2 oscillation); throughput must not regress. OR the exact remaining cause with evidence. Never overstate; label pilot-grade.",
"TESTS: add a test that fails against the pre-change (cold-start) behaviour where meaningful; keep all Rust and Python tests green.",
"TICKET .scratch/69-startup-warm/issues.md: contract, diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables (first-3-window attribution), both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const oscTask = [
"flashdb work package R2: bound the single-region merge oscillation so one region cannot blow up the reorganisation and the lag. Single writer in " + R2 + " (isolated worktree, branch wave16/oscillation, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read .scratch/STATUS.md and tickets 37/38, 64, 65, 66, 67, 68 first). WP-Q measured a recurring single-region blowup: on identical seed 42 the win `U|Stock` region emitted ~75 MB / 8 merges (0 MB on the same-trace repeat), learned win rung-50 lag-rej hit 13/40 on that run, and the win T3 ratio blew up; a similar `U|Customer`/`P|Customer` oscillation drove the adversarial seed-42 2.85x outlier and the WP-P write-amp variance. The known cause is documented in the tickets: the driver calls the engine's `maybe_compact_shape` once per window with a global 67 MB budget, and the engine chooses which region compacts; a per-region reward/threshold cannot see that deferral in one region funnels merges into an eager neighbour, and a tile-count threshold oscillating around its boundary can emit several large merges.",
"",
"OBJECTIVE: make a single region's per-window reorganisation bounded and stable, so a threshold sitting near the tile-count boundary cannot repeatedly emit large merges. Diagnose first from the committed JSONs/`group_phase`/region metrics which mechanism fires, then implement the smallest sound fix -- e.g. a per-region per-window output cap, a hysteresis/deadband on the tile-count threshold, a minimum payback before re-merging a region, or a fair-share cap in the global actuator. Do not remove legitimate compaction (the write path must still fold tiles), and do not special-case a workload; the rule must be general.",
"",
"MEASUREMENT: cp -a copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time (no other measurement running). Harness `cold_0b.py`, `--workloads win adversarial --arms learned static-derived` (declared if cheap). At least 3 seeds + a repeat. Report per-region reorg totals, the max single-window actuation, learned/declared and static/declared reorg ratios (T3), and rung-50 lag-rej. Record box load.",
"JUDGING: SS7.1 lag-only primary; report both. Do not edit the pre-registration.",
"ACCEPTANCE: the single-region blowup no longer recurs (the same-trace repeat consistency of the max per-region output is bounded); win T3 ratio median <=3x and no single-run blowup; learned win rung-50 lag-rej improves toward static-derived's band; no throughput regression and legitimate compaction still happens. OR the exact remaining cause with evidence. Never overstate.",
"TESTS: add a test that fails against the pre-change oscillation where meaningful; keep all tests green.",
"TICKET .scratch/70-merge-oscillation/issues.md: contract, diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const startWarmVerifyTask = [
"You are a fresh independent verifier for WP-R1, worktree " + R1 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/readahead.rs, engine/src/engine.rs, engine/src/bin/flashdb-load.rs, engine tests, ticket .scratch/69-startup-warm/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the warm-up strictly read-only/off the correctness path and BOUNDED (not a second full scan on the request path)? (b) did the change game the metric -- is the timed window unchanged, no pre-issued requests, lag still dispatch-minus-due? (c) are the first-3-window attributions and the 0/40 claim reproducible from the raw artifacts (state paths, >=3 seeds)? (d) does the ticket honestly report anything not achieved and keep the open author item? (e) do the new tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const oscVerifyTask = [
"You are a fresh independent verifier for WP-R2, worktree " + R2 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: the engine compaction/actuator path, engine tests, ticket .scratch/70-merge-oscillation/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the fix a GENERAL rule (no workload special case)? (b) does legitimate compaction still fold tiles (no data loss, no unbounded tile growth)? (c) are the blowup/reorg/lag figures reproducible from the raw artifacts (state paths, >=3 seeds + repeat)? (d) does the ticket honestly report anything not achieved and keep the open author item? (e) do the new tests exercise the change and fail pre-change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const f1VerifyTask = [
"You are a fresh independent verifier of the WP-P F1 documentation repair, worktree " + REPO + " (master). Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD`; find the F1-repair commit with `git log --oneline -12`. READ .scratch/67-requestpath-write-stall/issues.md's write-amplification paragraph.",
"FALSIFY: does the corrected paragraph now state the TRUE static-derived before/after figures (recomputable from the committed .scratch/67 and .scratch/68 JSONs: before 21.9/21.9/30.2 MB; after 26.4/94.9/98.0 MB), correctly attribute the variance (a single ~70 MB U|Customer/P|Customer merge, plausibly shared-box noise, not the read-path follow-on), and mark the non-regression criterion as not substantiated pending re-measure? Recompute the six figures yourself with the harness's own function and paste the numbers. Is anything else still misleading?",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings."
].join("\n");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master clean). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave16/startwarm` then `git merge --no-ff wave16/oscillation` into master (disjoint engine areas; expect no code conflicts). Resolve minimally and record any conflict.",
"2. Create the final evaluation worktree: `git worktree add -b wave17/eval " + WE + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gates + ".",
"4. Append an 'Integration record: wave16/startwarm + wave16/oscillation' section to .scratch/52-program/issues.md (real clock, commits, gates). Commit.",
"REPORT: master HEAD, both merges, wave17 worktree path/HEAD, verbatim gates."
].join("\n");

const evalTask = [
"flashdb work package S: the final SS7.1 lag-only evaluation after the startup-warm and merge-oscillation fixes. Single writer in " + WE + " (isolated worktree, branch wave17/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md and tickets 64-70. The learned access path gives a robust +43-57% throughput win; WP-Q left static-derived win rung-50 at 3/2/2/3 out of 40 (startup warm-up) and a single-region merge oscillation. WP-R1 and WP-R2 target those. This is the decisive final measurement.",
"",
"RUN: instance `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy). Arms declared + learned + static-derived. Workloads win (T1) and adversarial (T2). Full ladder 50/100/200/400. >=3 independent seeds + a repeat; run as many as wall-clock allows (aim for more, given this is the decisive run). One process at a time under the memory cap; record box load at every boundary; a quiet box preferred.",
"",
"REPORT, honestly:",
"1. T1 (win): under SS7.1 lag-only primary, is any arm rung-50 (or higher) lag-sustained now? The rung-50 lag-rej table per arm per seed. Does learned beat declared by >15% in the sustained load level -- if 0 breaches is reached, give the sustained levels; if not, give the achieved-throughput comparison (+43-57%?) and the lag-valid fraction, with seed count and noise band.",
"2. T2 (adversarial): learned vs declared rung-50 lag; reorg ratio (<=1.5x?); index builds on the 0-scan region.",
"3. T3 (win reorg ratio): median and max; does the single-region blowup still recur?",
"4. Both judging views; flag the harness's both-ceilings 0/0 artifact.",
"5. A FRANK overall statement: what the evidence does and does not support for the pre-registered thesis (T1/T2/T3/T4), at the achieved seed count, under the adopted SS7.1 reading. New defects; slice-39 zero-gain status.",
"TICKET .scratch/71-eval-decisive/issues.md: contract, exact commands, per-arm/rung tables, both judging views, T1/T2/T3 verdicts with seed counts and noise band, overall thesis statement, residual gaps. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and the honest verdict (pilot-grade labels; do NOT claim a 10-seed pre-registered verdict unless you actually ran it)."
].join("\n");

const evalVerifyTask = [
"You are a fresh independent verifier for WP-S, worktree " + WE + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Confirm the measured revision contains BOTH wave16 fixes (paste `git merge-base --is-ancestor` evidence). Scope: ticket .scratch/71-eval-decisive/issues.md, the committed run artifacts, and the claimed code revision.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) are the T1/T2/T3 verdicts and the lag-valid fractions computed from raw committed artifacts with the true seed count and a real noise band, under BOTH judging views? (b) does the ticket avoid claiming a 10-seed pre-registered verdict it did not earn, and is the pre-registration untouched? (c) does the overall thesis statement match the evidence (no overclaim either way)? (d) any unstated regression, inflated number, or metric gaming? (e) are the WP-R1/R2 fixes actually present in the measured binary's revision? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Integrate wave15, repair F1, create wave16 lanes", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };

const lanes = await runs.all([
  { key: "startwarm", label: "WP-R1: eager startup warm", agent: "worker", model: STRONG + ":high", cwd: R1, task: startWarmTask },
  { key: "oscillation", label: "WP-R2: bound the single-region merge oscillation", agent: "worker", model: STRONG + ":high", cwd: R2, task: oscTask }
]);

const reviews = await runs.all([
  { key: "vstartwarm", label: "Verify WP-R1", agent: "worker", model: STRONG + ":high", cwd: R1, task: startWarmVerifyTask },
  { key: "voscillation", label: "Verify WP-R2", agent: "worker", model: STRONG + ":high", cwd: R2, task: oscVerifyTask },
  { key: "vf1", label: "Verify the F1 repair", agent: "worker", model: STRONG, cwd: REPO, task: f1VerifyTask }
]);

const integ = await runs.run("integ", { label: "Integrate wave16 and create wave17", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
if (!integ.ok) return { setup: { ok: setup.ok, output: setup.output }, startwarm: { ok: lanes[0].ok, output: lanes[0].output }, oscillation: { ok: lanes[1].ok, output: lanes[1].output }, reviews, integ: { ok: false, output: integ.output } };

const evalS = await runs.run("eval", { label: "WP-S: decisive final evaluation", agent: "worker", model: STRONG + ":high", cwd: WE, task: evalTask });
const evalVerify = await runs.run("veval", { label: "Verify WP-S", agent: "worker", model: STRONG + ":high", cwd: WE, task: evalVerifyTask });
return {
  setup: { ok: setup.ok, output: setup.output },
  startwarm: { ok: lanes[0].ok, output: lanes[0].output },
  oscillation: { ok: lanes[1].ok, output: lanes[1].output },
  vstartwarm: { ok: reviews[0].ok, output: reviews[0].output },
  voscillation: { ok: reviews[1].ok, output: reviews[1].output },
  vf1: { ok: reviews[2].ok, output: reviews[2].output },
  integ: { ok: integ.ok, output: integ.output },
  evalS: { ok: evalS.ok, output: evalS.output },
  evalVerify: { ok: evalVerify.ok, output: evalVerify.output }
};
