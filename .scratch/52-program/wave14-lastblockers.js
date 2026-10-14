// Phase 14: remove the two remaining rung-50 lag-ceiling blockers so the +40-51% layout win
// becomes a clean lag-only SUSTAINED result: (O) learner burn-in/index-toggle churn, (P) the
// request-path device durability stall. Then integrate and re-evaluate under SS7.1 lag-only.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb";
const WO = W + "/wave14-churn2";
const WP = W + "/wave14-writestall";
const WE = W + "/wave15-eval";

const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master clean; HEAD is the wave12/accesspath integration + any SSOT commit -- verify with git log). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. Create both lane worktrees from master: `git worktree add -b wave14/churn2 " + WO + " master` and `git worktree add -b wave14/writestall " + WP + " master`. Verify and report both HEADs.",
"2. `git add` any untracked .scratch/52-program/*.js control scripts and commit.",
"REPORT: master HEAD, both worktree paths/HEADs."
].join("\n");

const churnTask = [
"flashdb work package O: remove the learner-side rung-50 lag churn so learned matches the frozen layout's lag. Single writer in " + WO + " (isolated worktree, branch wave14/churn2, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read .scratch/STATUS.md, tickets 64 and 65 first). WP-M added the learned secondary access path (`index_order_no` on `P|StockMovement`), exposed as a learner action; it gives a real +40-51% throughput win on the win workload and cuts the movement scan from 38-49 ms to 46-123 us. WP-N then measured that the LAG-ONLY sustained score is still 0 at rung 50 because: (i) the learner's 36-window rung-50 burn-in keeps changing the movement region's layout while the trace is live, so learned has 25-29/40 lag-rejected windows vs `static-derived`'s 3-4/40 -- i.e. the CHURN, not the layout, is the learner's remaining lag cost; and (ii) the learner toggles `index_order_no` ON for `P|StockMovement` even on the adversarial workload where the region's scan count is 0, burning 3-6 index builds for zero read benefit (the frozen `static-derived` correctly keeps it off).",
"",
"OBJECTIVE: make the learner's own transients stop costing lag, WITHOUT removing genuine adaptation. Concretely: (1) do not toggle/build the `order_no` index for a region whose observed scan traffic does not justify it (gate on measured demand, not on exploration), and (2) make the rung-50 burn-in not degrade the lag ceiling -- e.g. warm-start from a safe default, gate exploration on stable/quiet windows, bound the number of live-region changes per window, or defer a layout change while the region's traffic is in a live window. Preserve: the axis must still be discoverable and committed on the win workload (the +40-51% win must remain), and the adversarial rung-50 lag must stay at declared's level.",
"",
"DIAGNOSE FIRST with the committed artifacts: read `.scratch/65-eval-post-axis/seed*.json` and count, per window at rung 50, the learner's decisions and the lag-rej attribution for learned vs static-derived. Quantify how many of learned's 25-29 rung-50 breaches coincide with a layout change / index build. Choose the smallest sound fix; record rejected alternatives.",
"",
"MEASUREMENT: cp -a copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Harness `cold_0b.py`, `--workloads win adversarial --arms declared learned static-derived`, full ladder. At least 2 seeds + a repeat. Report per-rung achieved_rps, lag-rej, svc-rej, reorg bytes, and the learner decision/index-build counts. Record box load.",
"JUDGING: the author's ADOPTED reading is SS7.1 lag-only primary; report both that and both-ceilings. Do not edit the pre-registration.",
"ACCEPTANCE: on the win workload, learned's rung-50 lag-rej is within a small band of static-derived's (the churn cost removed) while the +40-51% throughput win over declared remains; on adversarial, learned builds no `order_no` index on the 0-scan region and its rung-50 lag-rej is within declared's band; the adversarial reorg ratio moves toward <=1.5x. OR the exact remaining cause with evidence. Never overstate; label pilot-grade.",
"TESTS: add harness tests that fail against the pre-change churn/toggle behaviour; keep all Python and Rust tests green.",
"TICKET .scratch/66-learner-burnin-churn/issues.md: contract, quantified diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const churnVerifyTask = [
"You are a fresh independent verifier for WP-O, worktree " + WO + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/edit-nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: harness/flashdb_harness/layout_learner.py, harness/flashdb_harness/cold_0b.py, harness/tests/, ticket .scratch/66-learner-burnin-churn/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) did the fix remove real adaptation (check the +40-51% win throughput is still produced and the axis is still committed on win)? (b) is the index-demand gate a genuine measured-demand rule, not a hardcoded workload special case? (c) are the rung-50 lag-rej / reorg / decision figures reproducible from the raw artifacts (state paths), with a real >=2-seed + repeat band? (d) does the ticket honestly state anything not achieved and keep the open author item? (e) do the new tests exercise the change and fail against pre-change behaviour? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const writeStallTask = [
"flashdb work package P: remove (or precisely bound) the request-path DEVICE durability stall that keeps rung 50 from reaching 0 lag-rejected windows. Single writer in " + WP + " (isolated worktree, branch wave14/writestall, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (measured, verified -- read .scratch/STATUS.md, tickets 54, 55, 56, 58, 64, 65 first). WP-E overlapped the WAL barrier so an ack happens only after a covering `fdatasync`, removing most of the fsync cascade; WP-G then showed the last lag breaches are a device-bound write stall: a slow `fdatasync` (e.g. 101 ms) makes the NEXT group's synchronous WAL `write()`/append block the single pacer (~74 ms), turning device latency into schedule lag. WP-N measured that even the frozen `static-derived` win layout (index on) still has 3-4/40 rung-50 lag-rejected windows from this path, so it is the last blocker between the +40-51% layout win and a clean lag-only SUSTAINED verdict.",
"",
"OBJECTIVE: make the single pacer not block on device write latency, while preserving the durability protocol exactly (a request is acked only after a covering fdatasync) and the single-writer commit semantics. Diagnose FIRST and report the exact blocking syscall/cause (append write vs fdatasync vs compaction I/O contention) with evidence from the per-window `group_phase` instrumentation, then implement the smallest sound mechanism -- e.g. a dedicated WAL-append writer thread the pacer enqueues to (off the dispatch path), a bounded append queue with backpressure, separating the compaction device traffic from the WAL, or batching group appends so a stall covers more work. Preserve: no request ack before a covering fdatasync; newest-first reads; disjoint tile ranges; no change to committed data. If the residual is genuinely the device's own latency floor below the 10 ms lag ceiling (i.e. no engine change can remove it), PROVE that with the device-latency distribution and say so -- that is a valid, valuable outcome.",
"",
"MEASUREMENT: cp -a copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Harness `cold_0b.py`, `--workloads win --arms declared learned static-derived` (adversarial if cheap), full ladder. At least 2 seeds + a repeat. Report per-rung achieved_rps, lag-rej, svc-rej, lag/svc p50/p95/p99, and the breach attribution. Record box load; the box is shared/noisy.",
"JUDGING: the author's ADOPTED reading is SS7.1 lag-only primary; report both that and both-ceilings. Do not edit the pre-registration.",
"ACCEPTANCE: on the win workload with the index adopted, rung-50 lag-rejected windows reach 0 (or the residual is proven device-bound with the device-latency distribution and the exact remaining count); throughput and write amplification do not regress; durability ordering unchanged. Never overstate.",
"TESTS: add tests that fail against the pre-change behaviour where meaningful (including that an ack still requires a covering fdatasync); keep all Rust and Python tests green.",
"TICKET .scratch/67-requestpath-write-stall/issues.md: contract, quantified diagnosis, design + rejected alternatives, invariants argument, change, tests, verbatim gates, before/after tables, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, headline numbers, honest verdict."
].join("\n");

const writeStallVerifyTask = [
"You are a fresh independent verifier for WP-P, worktree " + WP + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/wal.rs, engine/src/engine.rs (append/ack path), engine/src/bin/flashdb-load.rs, engine/tests/, ticket .scratch/67-requestpath-write-stall/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) does the change preserve the durability invariant -- is every ack still gated on a covering fdatasync (paste the code)? (b) single-writer commit semantics and read correctness preserved, no lost/duplicated writes under a stall? (c) does the pacer genuinely not block on device write latency now, and is the lag still dispatch-minus-due (not faked)? (d) are the before/after figures and the breach attribution reproducible from the raw artifacts (state paths, seed count)? (e) does the ticket honestly report anything not achieved (including a proven device-floor residual) and keep the open author item? (f) do the new tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master clean). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave14/churn2` then `git merge --no-ff wave14/writestall` into master (disjoint layers: harness learner vs engine write path; expect no code conflicts). Resolve minimally and record any conflict.",
"2. Create the re-evaluation worktree: `git worktree add -b wave15/eval " + WE + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gates + ".",
"4. Append an 'Integration record: wave14/churn2 + wave14/writestall' section to .scratch/52-program/issues.md (real clock, commits, gates). Commit.",
"REPORT: master HEAD, both merges, wave15 worktree path/HEAD, verbatim gates."
].join("\n");

const evalTask = [
"flashdb work package Q: the final SS7.1 lag-only evaluation after the churn and write-stall fixes. Single writer in " + WE + " (isolated worktree, branch wave15/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md and tickets 64 (the axis), 65 (WP-N), 66 (learner churn fix), 67 (write-stall fix). The layout axis gives a +40-51% throughput win; the goal now is to see whether rung 50 reaches 0 lag-rejected windows so the win becomes a clean lag-only SUSTAINED result, and to report T1/T2/T3 with the best available evidence.",
"",
"RUN: instance `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy). Arms declared + learned + static-derived. Workloads win (T1) and adversarial (T2). Full ladder 50/100/200/400. >=3 independent seeds + a repeat; more if wall-clock allows. One process at a time under the memory cap; record box load at every boundary.",
"",
"REPORT, honestly:",
"1. T1 (win): under the SS7.1 lag-only primary reading, is any arm rung-50 (or higher) lag-sustained now? Does learned beat declared by >15% in sustained load level -- or, if the ceiling is still breached, report the achieved-throughput comparison (declared vs learned) and the fraction of windows that are lag-valid, with seed count and a measured noise band. Label underivable if it truly is.",
"2. T2 (adversarial): rung-50 lag vs declared; the learned/declared reorg ratio (toward <=1.5x?); any index-build on the 0-scan region.",
"3. T3: learned/declared reorg ratio on win; does the single-region blowup recur?",
"4. Both judging views; flag the harness's both-ceilings 0/0 'sustain' artifact.",
"5. Any NEW defect; the slice-39 zero-gain status; a frank statement of what the evidence does and does not support for the pre-registered thesis.",
"TICKET .scratch/68-eval-final/issues.md: contract, exact commands, per-arm/rung tables, both judging views, T1/T2/T3 verdicts with seed counts and noise band, residual gaps. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and the honest verdict (pilot-grade labels; do NOT claim a 10-seed pre-registered verdict unless you actually ran it)."
].join("\n");

const evalVerifyTask = [
"You are a fresh independent verifier for WP-Q, worktree " + WE + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Confirm the measured revision contains BOTH wave14 fixes (paste evidence). Scope: ticket .scratch/68-eval-final/issues.md, the committed run artifacts, and the claimed code revision.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) are the T1/T2/T3 verdicts and the lag-valid fractions computed from raw committed artifacts with the true seed count and a measured noise band, under BOTH judging views? (b) does the ticket avoid claiming a 10-seed pre-registered verdict it did not earn, and is the pre-registration untouched? (c) do the numbers support the direction claim, and does the ticket state frankly what the evidence supports / does not support for the thesis? (d) any unstated regression or inflated number? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Create wave14 lane worktrees", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };

const lanes = await runs.all([
  { key: "churn", label: "WP-O: remove learner rung-50 churn", agent: "worker", model: STRONG + ":high", cwd: WO, task: churnTask },
  { key: "writestall", label: "WP-P: remove request-path device write stall", agent: "worker", model: STRONG + ":high", cwd: WP, task: writeStallTask }
]);

const reviews = await runs.all([
  { key: "vchurn", label: "Verify WP-O", agent: "worker", model: STRONG + ":high", cwd: WO, task: churnVerifyTask },
  { key: "vwritestall", label: "Verify WP-P", agent: "worker", model: STRONG + ":high", cwd: WP, task: writeStallVerifyTask }
]);

const integ = await runs.run("integ", { label: "Integrate wave14 and create wave15", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
if (!integ.ok) return { setup: { ok: setup.ok, output: setup.output }, churn: { ok: lanes[0].ok, output: lanes[0].output }, writestall: { ok: lanes[1].ok, output: lanes[1].output }, vchurn: { ok: reviews[0].ok, output: reviews[0].output }, vwritestall: { ok: reviews[1].ok, output: reviews[1].output }, integ: { ok: false, output: integ.output } };

const evalQ = await runs.run("eval", { label: "WP-Q: final lag-only evaluation", agent: "worker", model: STRONG + ":high", cwd: WE, task: evalTask });
const evalVerify = await runs.run("veval", { label: "Verify WP-Q", agent: "worker", model: STRONG + ":high", cwd: WE, task: evalVerifyTask });
return {
  setup: { ok: setup.ok, output: setup.output },
  churn: { ok: lanes[0].ok, output: lanes[0].output },
  writestall: { ok: lanes[1].ok, output: lanes[1].output },
  vchurn: { ok: reviews[0].ok, output: reviews[0].output },
  vwritestall: { ok: reviews[1].ok, output: reviews[1].output },
  integ: { ok: integ.ok, output: integ.output },
  evalQ: { ok: evalQ.ok, output: evalQ.output },
  evalVerify: { ok: evalVerify.ok, output: evalVerify.output }
};
