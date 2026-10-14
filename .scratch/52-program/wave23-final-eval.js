// wave23: final combined evaluation bound to the FULLY integrated master (tilecap + driftshape + rewarm
// repair, HEAD 48b0488). This is the milestone's final-revision proof. Reuses the wave22 harness.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT = "/home/z/vibe/worktrees/flashdb/wave23-final-eval";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"flashdb setup for the final combined evaluation. Run 'source $HOME/.cargo/env'. Work in " + REPO + ".",
"Create a fresh worktree at /home/z/vibe/worktrees/flashdb/wave23-final-eval from the CURRENT master HEAD (`git -C " + REPO + " rev-parse HEAD`) on a new branch wave23/final-eval. Then in the new worktree: `cargo build --workspace --release --offline` (this is the binary to measure). Create a cold copy of the 12.25x instance: `cp -a /tmp/cold51/per_region-mixed /tmp/wave23-eval/per_region-mixed` (fresh, not reused). Report master HEAD, worktree path, release binary path, and the instance size.",
"Create .scratch/79-final-eval/issues.md with a dispatch header (objective, branch/HEAD, instance, protocol, AC) and append a dispatch entry to .scratch/52-program/issues.md. Commit.",
"REPORT concisely: master HEAD, worktree path, release binary path, instance bytes."
].join("\n");

const evalTask = [
"flashdb work package: FINAL combined post-integration evaluation, bound to the fully integrated master (tilecap + driftshape + rewarm repair). Single writer in " + WT + " (branch wave23/final-eval). Run 'source $HOME/.cargo/env' first. Do NOT cd to " + REPO + ".",
"",
"OBJECTIVE: give the final-revision proof of the thesis metrics, on the tree that actually ships. This is the milestone measurement.",
"PROTOCOL (cold, one process at a time, shared/noisy box): measure ONLY the release binary built in this worktree, one process at a time, against the cold copy /tmp/wave23-eval/per_region-mixed (12.25x, 1,643,925,811 B). Record `uptime`/loadavg and cgroup peak with each run. Do NOT edit the pre-registration .scratch/15-objective-v0/pre-registration.md.",
"ARMS: the decisive set, matching ticket 78: `--workloads win` and the write-spike drift (`--drift-shape write --drift-factor 10`) with arms `declared learned static-derived`. SEEDS: 42,43,44 + a seed-42 repeat. LOCK every measurement: `flock /tmp/wave21-measure.lock -c '<full command>'`.",
"REPORT per run: achieved_rps per rung, lag-rejected windows per rung, lag p99, both-ceilings sustained flags, compact_bytes_total, region counts, recovery_windows, index builds.",
"",
"JUDGING: the primary metric is the author's adopted SS7.1 lag-only reading; ALSO report the harness both-ceilings view. Do not invent thresholds; do not lower any. State what is pilot-grade vs a full 10-seed verdict (this is 3 seeds + repeat -- NOT a 10-seed verdict).",
"",
"ACCOUNTING (ticket 72 convention): the reorg metric MUST use `summary.compact_bytes_total` (includes the final drained merge), NOT a per-region/per-window sum which undercounts. State the convention in the ticket.",
"",
"DELIVERABLE: .scratch/79-final-eval/issues.md with the table(s), the T1/T2/T3/T4 verdicts and the SS7.1 lag-only vs both-ceilings reading, noise bands, box load, and honest limits. Write the raw per-run artifacts under .scratch/79-final-eval/. GATES, report verbatim: " + gates + ". Append a dispatch/results entry to .scratch/52-program/issues.md. Commit; report HEAD + the headline verdicts."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier of the WP-79 final evaluation in " + WT + ". Run 'source $HOME/.cargo/env'. READ-ONLY: write/commit nothing and do NOT re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it) and `git log --oneline -5`; confirm the tree contains tilecap + driftshape + rewarm-repair changes (e.g. `Engine::set_max_tiles`, `COMPACT_TILE_SYNC_EVERY_BYTES`, the drift-shape flag).",
"PASTE exact output for: " + gates + ".",
"FALSIFY, recomputing from the committed raw JSON in .scratch/79-final-eval/ with python3/jq: (a) do the lag-rej/throughput tables match the raw artifacts exactly? (b) are the T1/T2/T3/T4 verdict directions correct AND do they agree with WP-X (ticket 78) on the same arms -- if any direction CHANGED with the rewarm fix in the tree, flag it as material; (c) is the T3 reorg computed with `compact_bytes_total` (ticket 72 convention), and are the ratios honest? (d) is the SS7.1 lag-only primary vs both-ceilings both reported, and is any '10-seed' claim disclaimed? (e) pre-registration untouched? (f) is the rung-50 residual honestly stated (device-bound) rather than claimed closed? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integTask = [
"You are the integrator. Repo " + REPO + ". Run 'source $HOME/.cargo/env' first.",
"1. IF the WP-79 verifier PASSed, `git merge --no-ff wave23/final-eval` into master; if it FAILed, do NOT merge and record that. Resolve minimally.",
"2. COMBINED GATES verbatim: " + gates + ".",
"3. Append an 'Integration record: WP-79 final eval' section to .scratch/52-program/issues.md (real clock, verdict, merge-or-not, commit, gates, and the headline T1/T2/T3/T4 result). Commit.",
"REPORT: master HEAD, whether wave23/final-eval was merged, verbatim gates, and the headline verdicts."
].join("\n");

const setup = await runs.run("setup", { label: "Setup the final-eval worktree", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
const evalr = await runs.run("eval", { label: "WP-79 final combined evaluation", agent: "worker", model: STRONG + ":high", cwd: WT, task: evalTask });
const verify = await runs.run("verify", { label: "Verify WP-79 final evaluation", agent: "worker", model: STRONG + ":high", cwd: WT, task: verifyTask });
const integ = await runs.run("integ", { label: "Merge the final eval", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
return { setup: { ok: setup.ok, output: setup.output }, eval: { ok: evalr.ok, output: evalr.output }, verify: { ok: verify.ok, output: verify.output }, integ: { ok: integ.ok, output: integ.output } };
