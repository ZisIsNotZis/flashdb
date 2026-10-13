// Integrate wave7/readsoverlap (WP-G), then run a bounded T1/T2/T4 evaluation PILOT, then verify.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT8 = "/home/z/vibe/worktrees/flashdb/wave8-eval-pilot";

const integrateTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at 5b4b78d. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave7/readsoverlap` (WP-G, verified PASS at 22eef3e: read-ahead tile warming takes cold point reads off the pacer; rung-50 lag 0/40 c51 and 0-1/40 cold44). Resolve conflicts minimally and record them.",
"2. Create the next lane worktree: `git worktree add -b wave8/eval " + WT8 + " master`. Verify and report its HEAD.",
"3. COMBINED GATES, capture verbatim: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent).",
"4. Append an 'Integration record: wave7/readsoverlap' section to .scratch/52-program/issues.md (real clock, merge commit, final HEAD, verbatim gates, wave8 path). Commit.",
"REPORT: final HEAD, merge status, wave8 worktree path/HEAD, verbatim gates."
].join("\n");

const pilotTask = [
"flashdb work package H: the T1/T2/T4 evaluation PILOT under the fixed engine. Single writer in " + WT8 + " (isolated worktree, branch wave8/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; commit the measurement plan before running anything long.",
"",
"CONTEXT: .scratch/STATUS.md is the current SSOT (read it first). The engine latency is now fixed enough that rung 50 is sustained by the LAG ceiling (c51 0/40, cold44 0-1/40; WP-G). The harness `harness/flashdb_harness/cold_0b.py` already implements the three arms (declared, static-derived, learned UCB1 driven live via --policy-file/--policy-reload), the win/adversarial/drift workloads, and the trustworthy aggregator (tickets 42/44/45/47). The pre-registration is .scratch/15-objective-v0/pre-registration.md.",
"",
"OBJECTIVE (a PILOT, not the final verdict): get fresh, honest data on whether the pre-registered thesis is still live under the fixed engine, and a measured noise band, so the full 10-seed campaign can be planned. Do NOT claim a 10-seed pre-registered verdict if you ran fewer seeds; label every number with its actual seed count and conditions.",
"",
"RUN (using the existing harness; state every exact command):",
"- Instance: a cp -a copy of /tmp/cold51/per_region-mixed (12.25x) or /tmp/cold44/per_region-mixed (8.17x); state which and why. One process at a time under the memory cap.",
"- Workloads: the win workload first (the T1 workload), then adversarial-uniform (the T2 workload) if time permits.",
"- Arms: declared, static-derived, learned. At least the rung-50 level; more rungs if the harness makes it cheap.",
"- Seeds: as many as fit your budget, at least 2 independent access seeds per (arm, workload); repeat at least one (arm, workload, seed) run twice to give a measured run-to-run noise band.",
"",
"REPORT, honestly:",
"1. Primary metric under the adopted STATUS section 7.1 reading (lag ceiling primary; service reported separately) AND under the pre-registration's both-ceilings reading, per (arm, workload, rung).",
"2. T1 direction: does learned beat declared by >15% in sustained load level on the win workload? Give the achieved numbers and the noise band; if underivable at this seed count, say so.",
"3. T2 direction: does learned lose >5% to declared on adversarial-uniform?",
"4. T3: reorganisation bytes learned vs declared (the 3x threshold).",
"5. Whether the measured zero-gain / write-amp problem from slice 39 persists, with numbers.",
"6. The exact seed count, the run-to-run spread, and what a full 10-seed campaign would cost in wall-clock.",
"If the harness cannot drive an arm (for example policy-reload fails), report exactly where and stop rather than improvising. Never overstate a pilot as a verdict.",
"",
"TICKET .scratch/59-eval-pilot/issues.md: contract, exact commands, per-arm/per-rung tables, the two judging views, T1-T4 directions with seed counts and noise band, readiness gaps, and the cost estimate for the full campaign. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash and the honest pilot verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-H, worktree " + WT8 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: harness/flashdb_harness/cold_0b.py and any new harness script, the raw run artifacts under /tmp, and ticket .scratch/59-eval-pilot/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY: (a) are the arm policies actually distinct and the learner genuinely running (policy-reload engaged), not a relabeled declared run? (b) is the primary-metric computation the real per-window criterion (no median hiding a tail), and are both judging views reported? (c) are the T1-T4 directions computed from the raw artifacts (state paths) and labelled with the true seed count and a real noise band? (d) does the pilot avoid claiming a pre-registered verdict it did not earn? (e) is the pre-registration untouched? (f) does any new harness code have tests? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-G and create wave8", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}
const pilot = await runs.run("pilot", { label: "Run the T1/T2/T4 evaluation pilot", agent: "worker", model: STRONG + ":high", cwd: WT8, task: pilotTask });
const verify = await runs.run("verify", { label: "Verify the evaluation pilot", agent: "worker", model: STRONG + ":high", cwd: WT8, task: verifyTask });
return {
  integrate: { ok: integ.ok, output: integ.output },
  pilot: { ok: pilot.ok, output: pilot.output },
  verify: { ok: verify.ok, output: verify.output }
};
