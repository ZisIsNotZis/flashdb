// Phase 21: close the three non-author-gated resume-plan items in parallel lanes:
//  A tile-cap fix (SS7.4); B rung-50 compaction re-warm residual; C T4 drift-shape gap.
// Each lane measures under a shared flock; then integrate and run the final combined eval.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb";
const A = W + "/wave21-tilecap";
const B = W + "/wave21-rewarm";
const C = W + "/wave21-driftshape";
const E = W + "/wave22-eval";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";
const lock = "Before every measurement command, wrap it in `flock /tmp/wave21-measure.lock -c '<full command>'` so only one lane measures at a time on this shared box; record which runs were lock-protected.";

const setupTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master HEAD 8a4ea20, clean). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"Create the three lane worktrees from master: `git worktree add -b wave21/tilecap " + A + " master`, `git worktree add -b wave21/rewarm " + B + " master`, `git worktree add -b wave21/driftshape " + C + " master`. Report all three HEADs and the master HEAD.",
"REPORT: master HEAD, three worktree paths/HEADs."
].join("\n");

const tilecapTask = [
"flashdb work package W1: implement the adopted tile-cap fix (STATUS SS7.4). Single writer in " + A + " (isolated worktree, branch wave21/tilecap, based on master 8a4ea20). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the plan/design before implementing.",
"",
"CONTEXT: read .scratch/STATUS.md SS6/SS7.4 and ticket .scratch/46-partitioned-publish-tile-cap. `manifest::MAX_TILES` is a hardcoded 1024; the scored runner ships a 16 KiB cadence that exhausts it under per-region publishing; the regression test `per_region_cadence_exhausts_the_tile_cap_and_the_publish_fails` pins the CURRENT (unfixed) behaviour. The adopted decision: raise/make the cap configurable, make the run harness's default cadence compatible with per-region publishing, and make a scored run refuse an incompatible cadence LOUDLY instead of aborting mid-run.",
"",
"OBJECTIVE: implement exactly that, preserving correctness (no silent tile loss; publish still fails closed if the cap is genuinely exceeded). Then UPDATE the regression test to pin the fixed behaviour (or replace it with a test that the shipped default cadence completes a scored run) while keeping a test that an over-cap publish still fails closed.",
"ACCEPTANCE: the shipped default cadence runs a per-region scored workload to completion without exhausting the cap; the cap is raised/configured; an incompatible cadence is refused loudly up front; a genuine over-cap publish still fails closed; all tests green. OR the exact remaining cause with evidence.",
"JUDGING: no threshold change. Do not edit the pre-registration. " + lock,
"TICKET .scratch/75-tile-cap-fix/issues.md: contract, design, change, tests, verbatim gates, acceptance verdict, residual. Append a dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and honest verdict."
].join("\n");

const rewarmTask = [
"flashdb work package W2: close the rung-50 lag residual (compaction-adjacent cold reads). Single writer in " + B + " (isolated worktree, branch wave21/rewarm, based on master 8a4ea20). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing.",
"",
"CONTEXT: read .scratch/STATUS.md SS3 and tickets 69 and 71. WP-S measured win rung-50 lag-rej static-derived median 2/40 (1-3/40), learned median 5/40; every residual breach is mid-run (windows 28-36), adjacent to larger compaction merges, with cold page-ins on the pacer (0.4-1.3 MB device reads) plus occasional device fdatasync tails. The startup warm-up was already removed (ticket 69). The remaining systematic cause is that when compaction publishes a NEW tile (a new snapshot), the read-ahead worker's warmed blocks are for the OLD snapshot, so the first reads of the new tiles are cold. rung 100 already sustains (WP-S); rung 50 does not, solely because of this residual.",
"",
"OBJECTIVE: warm newly published compaction output off the pacer so a post-compaction read is not a cold page-in. Diagnose the exact path first (which reads go cold after a publish: the movement scan prefix, the order_no index tile, both?), then implement the smallest sound fix -- e.g. have the read-ahead worker react to a new published snapshot by warming the new tiles' relevant prefixes, or warm the replacement tile during/after the off-path apply. Preserve all invariants: warm path read-only and off the correctness path; authoritative reads stay inline; durability untouched; bounded work. Do NOT game the metric.",
"",
"MEASUREMENT: `cp -a` copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Harness `cold_0b.py --workloads win --arms declared learned static-derived`; >=3 seeds + a repeat. Report per-rung achieved_rps, lag-rej, lag p99, and the mid-run window attribution (28-36). " + lock + " Record box load.",
"JUDGING: SS7.1 lag-only primary; report both. Do not edit the pre-registration.",
"ACCEPTANCE: static-derived win rung-50 lag-rej reaches 0/40 across >=3 seeds (learned within a small band), with no throughput regression. OR the exact remaining cause with evidence (e.g. a proven device floor). Never overstate; label pilot-grade.",
"TESTS: add a test failing against the pre-change cold-after-publish behaviour where meaningful; keep all tests green.",
"TICKET .scratch/76-compaction-rewarm/issues.md: contract, diagnosis, design + rejected alternatives, change, tests, verbatim gates, before/after tables, both judging views, acceptance verdict, residual. Append a dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and honest verdict."
].join("\n");

const driftTask = [
"flashdb work package W3: close the T4 evaluability gap by adding a non-scan drift shape. Single writer in " + C + " (isolated worktree, branch wave21/driftshape, based on master 8a4ea20). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the design before implementing.",
"",
"CONTEXT: read tickets 73 (T4 underivable) and 29, and STATUS SS5/SS7. T4 is underivable because the harness drift is hardcoded to `scan` weight x10, which LOWERS the write share and therefore does not reduce capacity for the indexed layout (post-drift capacity >= pre-drift). The pre-registration also names other drift shapes: 'a shape's weight multiplied, or one key's popularity spiked'. A WRITE-spike drift (raise the `write` weight) should reduce capacity (writes are the fsync-bound bottleneck) and could create a genuine recovery event; a key-popularity spike is another candidate.",
"",
"OBJECTIVE: add a configurable drift SHAPE to the harness `cold_0b.py` drift workload (at least a write-weighted drift, ideally also a key-popularity spike), so a capacity-reducing drift is reachable, then re-test T4: does the learner recover to within 10% of its pre-drift throughput within 2 windows while `static-derived` does not (or recovers measurably slower)? Keep the existing scan-drift behaviour selectable. Do NOT change the pre-registration or any threshold.",
"",
"MEASUREMENT: `cp -a` copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. First validate evaluability (find a drift rate below pre-drift capacity for the new shape; if none, prove it). Then run learned + static-derived (+ declared if cheap), >=3 seeds + a repeat. " + lock + " Record box load.",
"JUDGING: SS7.1 lag-only primary; report both. Do not edit the pre-registration.",
"ACCEPTANCE: a capacity-reducing drift shape exists and is selectable; T4 is evaluated (recovery windows for learned vs static-derived, with the throughput series and seed count) OR is honestly proven still-underivable for the new shape with evidence. Never overstate; label pilot-grade.",
"TESTS: add harness tests for the new drift shape; keep all tests green.",
"TICKET .scratch/77-drift-shape/issues.md: contract, design, change, tests, verbatim gates, evaluability proof, per-window tables, both judging views, the T4 verdict with seed count and noise band, residual. Append a dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and honest T4 verdict."
].join("\n");

const verifyTemplate = (pkg, wt, scope, extra) => [
"You are a fresh independent verifier for " + pkg + ", worktree " + wt + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: " + scope + ".",
"PASTE exact output for: " + gates + ".",
"FALSIFY: " + extra + " Does the ticket honestly report anything not achieved and keep the open author item? Is the pre-registration untouched? Do the new tests exercise the change and fail pre-change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const vTilecap = verifyTemplate("WP-W1 (tile-cap fix)", A, "engine/src/manifest.rs + the publish path, engine/src/bin, harness/, engine/tests/, ticket .scratch/75-tile-cap-fix/issues.md",
"(a) is the cap raised/configurable and is a genuinely over-cap publish still fail-closed (no silent tile loss)? (b) does the shipped default cadence now complete a per-region scored run, and is an incompatible cadence refused loudly up front? (c) are the acceptance numbers reproducible?");
const vRewarm = verifyTemplate("WP-W2 (compaction re-warm)", B, "engine/src/readahead.rs, engine/src/engine.rs, ticket .scratch/76-compaction-rewarm/issues.md",
"(a) is the warm path still strictly read-only/off the correctness path and bounded? (b) is the diagnosis (cold-after-publish) reproduced from the raw per-window attribution? (c) are the rung-50 lag-rej/repro figures from raw artifacts with >=3 seeds + repeat, and is the metric not gamed?");
const vDrift = verifyTemplate("WP-W3 (drift shape)", C, "harness/flashdb_harness/cold_0b.py, harness/tests/, ticket .scratch/77-drift-shape/issues.md",
"(a) is the new drift shape genuine and selectable (not a relabel)? (b) is the evaluability proof real (a rate below pre-drift capacity that saturates post-drift)? (c) is the T4 verdict computed from raw per-window artifacts with the true seed count and noise band, and is it either a real evaluation or an honest proof of continued underivability?");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master clean). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. Merge whichever of wave21/tilecap, wave21/rewarm, wave21/driftshape branch(es) the verifiers PASSed, with `git merge --no-ff` (disjoint areas; expect no code conflicts). Record each merge and any conflict.",
"2. Create the eval worktree: `git worktree add -b wave22/eval " + E + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gates + ".",
"4. Append an 'Integration record: wave21 lanes' section to .scratch/52-program/issues.md (real clock, commits, gates, which lanes merged). Commit any untracked control scripts.",
"REPORT: master HEAD, which lanes merged, eval worktree path/HEAD, verbatim gates."
].join("\n");

const evalTask = [
"flashdb work package X: the combined post-wave21 evaluation (rung-50 residual + T4 + T1/T2/T3 regression). Single writer in " + E + " (isolated worktree, branch wave22/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md and tickets 71, 73, and whichever of 75/76/77 merged. WP-S re-derived T1/T2/T3 (rung-100 lag-only sustain; rung-50 residual = compaction-adjacent cold reads). This run confirms the integrated result.",
"RUN: `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy). Arms declared + learned + static-derived. Workloads win (T1) + adversarial (T2) + the new drift shape (T4). >=3 seeds + a repeat; serial, one process at a time under the memory cap; record box load.",
"REPORT: (1) primary metric lag-only per arm per rung -- is rung 50 sustained now? rung 100? (2) T1 throughput; (3) T2 adversarial; (4) T3 win reorg; (5) T4 with the new shape; (6) both judging views; (7) a frank overall statement at the achieved seed count. New defects; slice-39 status. " + lock,
"JUDGING: SS7.1 lag-only primary; report both. Do not edit the pre-registration.",
"TICKET .scratch/78-eval-wave21/issues.md: contract, exact commands, per-arm/rung tables, both judging views, T1-T4 verdicts with seed counts and noise band, overall statement, residual. Append a dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and honest verdict (pilot-grade)."
].join("\n");

const vEval = [
"You are a fresh independent verifier for WP-X, worktree " + E + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Confirm the measured revision contains the merged wave21 fixes (paste `git merge-base --is-ancestor` evidence).",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) are the T1-T4 verdicts and lag-valid fractions computed from raw committed artifacts with the true seed count and a real noise band, under BOTH judging views? (b) is any verdict overstated or any metric gamed? (c) is the pre-registration untouched and no unearned 10-seed claim made? (d) are the wave21 fixes present in the measured revision? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings."
].join("\n");

const setup = await runs.run("setup", { label: "Create wave21 lane worktrees", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };

const lanes = await runs.all([
  { key: "tilecap", label: "WP-W1: tile-cap fix", agent: "worker", model: STRONG + ":high", cwd: A, task: tilecapTask },
  { key: "rewarm", label: "WP-W2: compaction re-warm", agent: "worker", model: STRONG + ":high", cwd: B, task: rewarmTask },
  { key: "driftshape", label: "WP-W3: drift shape", agent: "worker", model: STRONG + ":high", cwd: C, task: driftTask }
]);

const reviews = await runs.all([
  { key: "vtilecap", label: "Verify WP-W1", agent: "worker", model: STRONG + ":high", cwd: A, task: vTilecap },
  { key: "vrewarm", label: "Verify WP-W2", agent: "worker", model: STRONG + ":high", cwd: B, task: vRewarm },
  { key: "vdrift", label: "Verify WP-W3", agent: "worker", model: STRONG + ":high", cwd: C, task: vDrift }
]);

const integ = await runs.run("integ", { label: "Integrate passing wave21 lanes", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
if (!integ.ok) return { setup: { ok: setup.ok, output: setup.output }, lanes, reviews, integ: { ok: false, output: integ.output } };

const evalX = await runs.run("eval", { label: "WP-X: combined post-wave21 evaluation", agent: "worker", model: STRONG + ":high", cwd: E, task: evalTask });
const evalVerify = await runs.run("veval", { label: "Verify WP-X", agent: "worker", model: STRONG + ":high", cwd: E, task: vEval });
return {
  setup: { ok: setup.ok, output: setup.output },
  tilecap: { ok: lanes[0].ok, output: lanes[0].output },
  rewarm: { ok: lanes[1].ok, output: lanes[1].output },
  driftshape: { ok: lanes[2].ok, output: lanes[2].output },
  vtilecap: { ok: reviews[0].ok, output: reviews[0].output },
  vrewarm: { ok: reviews[1].ok, output: reviews[1].output },
  vdrift: { ok: reviews[2].ok, output: reviews[2].output },
  integ: { ok: integ.ok, output: integ.output },
  evalX: { ok: evalX.ok, output: evalX.output },
  evalVerify: { ok: evalVerify.ok, output: evalVerify.output }
};
