// Keep the current-fact SSOT in step with the verified state through tickets 48-62.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT = "/home/z/vibe/worktrees/flashdb/wave11-ssot";

const setupTask = [
"You are the integrator/scribe-setup worker. Repo /home/z/vibe/flashdb, master HEAD e625e00 (already includes the wave8/eval docs plus both wave9 lane merges, resolved). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git worktree add -b wave11/ssot " + WT + " master`. Verify and report its HEAD.",
"2. Append an 'Integration record: wave9 lanes + wave10 re-eval' if not already present to .scratch/52-program/issues.md, and commit any untracked .scratch/52-program/*.js control scripts. Commit.",
"REPORT: master HEAD, worktree path/HEAD."
].join("\n");

const ssotTask = [
"flashdb work package L: bring the current-fact SSOT up to date with the verified state, based ONLY on the committed tickets. Single writer in " + WT + " (isolated worktree, branch wave11/ssot, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally.",
"",
"READ the current-fact SSOT and the tickets that changed the facts: .scratch/STATUS.md (the SSOT), docs/objective.md, docs/engine.md, and tickets .scratch/48 through .scratch/62 (especially 53 compaction-latency-safe, 54 residual/service tail, 55 lag-stall, 56 overlapped WAL barrier, 57 boundary+reads+co-location, 58 overlapped reads, 59 eval-pilot, 60 scan-readahead, 61 learner-churn, 62 eval-rerun). Verify claims against those tickets, not memory.",
"",
"OBJECTIVE: make the SSOT state, as current facts and nothing else, (a) the engine latency work now complete and verified (compaction off the request path; overlapped durability-preserving WAL barrier; non-blocking window boundary; background read-ahead tile warming; scan-shape warm path), including that rung 50 is lag-sustained on the point-lookup workloads (c51 0/40, cold44 0-1/40); (b) the evaluation status: the three arms exist (declared/static-derived/learned UCB1) and were re-measured at 3 seeds + a repeat on the 12.25x instance; T1 (win) UNDERIVABLE because the win workload's scan is CPU-bound on the pacer (no order_no index) so no arm lag-sustains rung 50 and both arms fail identically; T2 (adversarial) PARTIAL (rung-50 lag churn fixed, reorg churn not robust: 0.13-2.88x median 1.16x); T3 FAIL on win seed44 (8.37x); the slice-39 zero-gain persists; (c) the open items: the pre-registration primary-vs-service-ceiling reconciliation (STATUS 7.1 lag-only working reading vs the both-ceilings rule) remains an OPEN AUTHOR ITEM, and the tile-cap fix (7.4) remains adopted-but-unimplemented, plus the NEW open item that the pre-registered win workload exposes no layout-dependent headroom (ticket 29's finding, now measured).",
"",
"RULES: This is a FACT document, rewritten in place: current facts only, one home per fact, no dated narrative, no changelogs, no 'was A now B'. Keep the existing structure and any budget notes. Do NOT touch .scratch/15-objective-v0/pre-registration.md. Do NOT change any threshold or claim a verdict the tickets do not support (label pilot-grade where the tickets do). Point to the ticket paths for the change history instead of retelling it.",
"",
"Report the exact sections changed and paste the final diff stat. If a fact is ambiguous in the tickets, write the ambiguity explicitly rather than smoothing it.",
"TICKET .scratch/63-ssot-refresh/issues.md: what changed and why (short), the ticket sources, and the diff stat. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-L (SSOT refresh), worktree " + WT + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: .scratch/STATUS.md, docs/objective.md, docs/engine.md, .scratch/63-ssot-refresh/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY: (a) does every changed factual statement match the source tickets (48-62) -- spot-check the rung-50 lag status, the T1/T2/T3 directions and seed counts, and the 'CPU-bound scan' claim against .scratch/60 and .scratch/62? (b) is the document still current-fact-only (no dated narrative/changelog, no stale contradicting statement, one home per fact)? (c) is the pre-registration untouched? (d) are the open author items (7.1 reconciliation, 7.4 tile-cap, the no-headroom win-workload finding) still explicitly open? (e) does it avoid claiming a pre-registered 10-seed verdict? Report blocking defects with location and the contradicting ticket evidence.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Create the SSOT worktree", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };
const ssot = await runs.run("ssot", { label: "WP-L: refresh the current-fact SSOT", agent: "worker", model: STRONG + ":high", cwd: WT, task: ssotTask });
const verify = await runs.run("verify", { label: "Verify WP-L", agent: "worker", model: STRONG + ":high", cwd: WT, task: verifyTask });
return { setup: { ok: setup.ok, output: setup.output }, ssot: { ok: ssot.ok, output: ssot.output }, verify: { ok: verify.ok, output: verify.output } };
