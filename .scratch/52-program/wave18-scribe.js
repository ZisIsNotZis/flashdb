// Phase 18: repair the two documentation-accuracy FAILs (ticket 67 F1 mechanism wording;
// ticket 70 seed43 reorg figures / final-drain gap), fresh-verify, merge. Doc-only; no measurement.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb/wave18-scribe";

const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git worktree add -b wave18/scribe " + W + " master`. Report its HEAD.",
"REPORT: master HEAD, worktree path/HEAD."
].join("\n");

const repairTask = [
"flashdb work package T: correct two documentation-accuracy defects found by independent verifiers. Single writer in " + W + " (isolated worktree, branch wave18/scribe, based on master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Doc-only: do NOT change code, and do NOT re-run any load measurement.",
"",
"REPAIR A -- .scratch/67-requestpath-write-stall/issues.md (the WP-P F1 write-amplification paragraph). An independent verifier confirmed the six static-derived figures are true and recompute exactly (before 21.9/21.9/30.2 MB; after 26.4/94.9/98.0 MB), the non-regression criterion is correctly marked NOT substantiated, and the read-path follow-on is correctly exonerated -- but the VARIANCE ATTRIBUTION mechanism is FALSE. It currently says the excess is 'a single ~69-72 MB merge'. The truth (recompute it yourself from the committed .scratch/67 and .scratch/68 JSONs to confirm): it is a SINGLE-REGION MERGE OSCILLATION -- seed43new U|Customer emitted 6 merges of ~11.4-11.5 MB each (69.0 MB total); seed42repeatnew P|Customer emitted 7 merges of ~10.2-10.4 MB each (72.1 MB total); largest single merge ~11.5 MB, not ~70 MB. Also correct the '(the whole excess is a single ...)' overstatement: in seed43new P|Order (+5.9 MB) and U|StockMovement (+2.3 MB) also grew, offset by P|Stock (-4.2 MB). Rewrite that portion to state the single-region oscillation and the true per-merge sizes. Recompute and paste the numbers you use.",
"",
"REPAIR B -- .scratch/70-merge-oscillation/issues.md (the seed43 win-learned reorg figures). An independent verifier FAILed this: the ticket's `after seed43` win-learned reorg (6.88 MB, max 5.23 MB, T3 0.22x) SUMS the per-window `compaction_bytes` and therefore DROPS the driver's final DRAINED merge -- `flashdb-load.rs` applies the last in-flight job after the window loop and adds it to `summary.compact_bytes_total`/`compact_merges` but to no window. From seed43.json: summary compact_bytes_total = 18,256,145 and compact_merges = 6, while summed window per-region compaction_bytes = 6,883,598 (5 merges), so the 6th merge is 11,372,547 B. Correct the ticket: use the driver's summary totals (or explicitly disclose the gap), give the corrected seed43 learned/declared ratio (~0.59x) and the corrected T3 median (~0.575x, still <=3x), and correct the 'max single-window 5.23 MB' to the true max per-actuation ~11.37 MB (one pair, still not oscillation). Note that the same final-drain gap affects the ticket-68 'before' figures identically (apples-to-apples but both sides undercount by one merge). Confirm the fix's headline acceptance (#1/#2/#4) still holds at the corrected numbers. Recompute and paste the numbers you use.",
"",
"Also fix the two minor items the verifier named: the ticket-70 gate header says 'run at final HEAD 1b13c8b' but final HEAD is later (state the actual HEAD); and two declared rung-50 lag-rej cells written '39/40' where the curve rows have windows=39 (make it consistent or disclose).",
"",
"RULES: these are current-fact documents -- correct them in place, no changelog/'was A now B'. Do NOT touch .scratch/15-objective-v0/pre-registration.md. Do NOT change any verdict to a stronger claim; the honest verdicts stand.",
"TICKET .scratch/72-scribe-repairs/issues.md: the two repairs, the verified numbers used, and the commit. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-T (doc repairs), worktree " + W + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: .scratch/67-requestpath-write-stall/issues.md, .scratch/70-merge-oscillation/issues.md, .scratch/72-scribe-repairs/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) does ticket 67 now describe the variance as a single-region merge oscillation with the true per-merge sizes (recompute from the .scratch/67 and .scratch/68 JSONs) and keep the non-regression criterion honestly marked not-substantiated? (b) does ticket 70 now use the driver's compact_bytes_total / compact_merges (including the final drained merge) or explicitly disclose the gap, with the corrected seed43 ratio (~0.59x) and T3 median (~0.575x) and the true max actuation (~11.37 MB)? (c) did any verdict get strengthened, or any code changed (git diff must be docs only)? (d) is the pre-registration untouched? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave18/scribe` into master (docs only). Resolve minimally.",
"2. COMBINED GATES verbatim: " + gates + ".",
"3. Append an 'Integration record: wave18/scribe' section to .scratch/52-program/issues.md (real clock, commit, gates). Commit.",
"REPORT: master HEAD, merge result, verbatim gates."
].join("\n");

const setup = await runs.run("setup", { label: "Create the scribe worktree", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };
const repair = await runs.run("repair", { label: "WP-T: repair the two doc defects", agent: "worker", model: STRONG + ":high", cwd: W, task: repairTask });
const verify = await runs.run("verify", { label: "Verify WP-T", agent: "worker", model: STRONG + ":high", cwd: W, task: verifyTask });
const integ = await runs.run("integ", { label: "Integrate wave18/scribe", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
return { setup: { ok: setup.ok, output: setup.output }, repair: { ok: repair.ok, output: repair.output }, verify: { ok: verify.ok, output: verify.output }, integ: { ok: integ.ok, output: integ.output } };
