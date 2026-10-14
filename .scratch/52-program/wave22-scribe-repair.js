// WP-X (wave22/eval) report VERDICT FAIL: repair the T3 reorg accounting + the minor false-evidence
// claims in the ticket/report, then fresh-verify. Docs-only (no code).
const STRONG = "deepseek/deepseek-v4-pro";
const B = "/home/z/vibe/worktrees/flashdb/wave22-eval";
const repo = "/home/z/vibe/flashdb";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const repairTask = [
"flashdb scribe repair of the WP-X evaluation record in " + B + " (branch wave22/eval, HEAD 4a0317d). Run 'source $HOME/.cargo/env'. Do NOT cd to " + repo + ". READ-ONLY measurement: do NOT re-run the eval; repair the RECORD against the committed raw JSON artifacts. Commit.",
"",
"CONTEXT: the WP-X report (ticket .scratch/78-eval-wave21/issues.md and the per-seed .md files) was independently verified FAIL, one BLOCKING finding. A fresh reviewer must pass this before acceptance.",
"",
"FIX THE BLOCKING FINDING (T3 reorg metric gamed/understated). Ticket 72 (.scratch/72-scribe-repairs/issues.md) established that per-window/per-region reorg byte SUMS UNDERCOUNT the true reorg cost and that `summary.compact_bytes_total` (which includes the final drained merge) is the correct total. The WP-X report used the region sum for the win/learned T3 ratio, dropping the final-drained merge:",
"  - seed 42 win/learned@1.0: summary.compact_bytes_total=28,905,853 vs region sum 17,533,306 (diff 11,372,547 = final-drained U|Customer merge).",
"  - seed 42r: 28,496,094 vs 17,123,547 (same 11,372,547 diff).",
"  - Correct T3 ratios: 0.554 (seed43) / 0.918 / 0.928 / 0.941; MEDIAN 0.923x, MAX 0.941x. The claim 'bimodal (~0.55-0.57x in 3/4, 0.918x in 1/4)' is INVERTED: truth is 1/4 at 0.554x and 3/4 at 0.92-0.94x.",
"Do: recompute every T3 reorg ratio in the report (win/static-derived and win/learned@1.0, all seeds) from `summary.compact_bytes_total` (state the accounting convention explicitly and cite ticket 72); fix the T3 table, the median, the bimodal sentence, and any mirrored per-seed .md 'reorg bytes' columns. The T3 VERDICT (<=3x PASS, every run <=1x) is unchanged -- keep it, but make the reported advantage and narrative honest.",
"",
"FIX THE MINOR FINDINGS: (2) the 'New defects: achieved fraction <= 1.0005 at every rung' claim is false -- raw JSON has seed43 win/learned@1.0 rung-200 achieved_fraction=1.1315 (rate_exceeded=true) and seed42r rung-400=1.004; correct the sentence. (3) the T4 wording 'neither returns to the 10% band of 1450' is stronger than the data (post-drift steady values straddle 1305 rps; drop range true low end ~9.4%, not -9.9%); soften to match the raw values. Keep the T4 verdict (EVALUABLE, measured NOT SUPPORTED).",
"",
"Also verify the ticket's stated seed/artifact set, noise bands, and the 'NOT a 10-seed verdict' disclosure remain correct. Do not touch .scratch/15-objective-v0/pre-registration.md.",
"GATES, report verbatim: " + gates + ". Append a 'WP-X record repair' entry to .scratch/52-program/issues.md. Commit; report the new HEAD and a one-paragraph summary of corrected numbers."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier of the WP-X record repair in " + B + ". Run 'source $HOME/.cargo/env'. READ-ONLY: write/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it) and `git log --oneline -5`. Recompute INDEPENDENTLY from the committed raw JSON in .scratch/78-eval-wave21/*.json (use python3/jq): for every win/learned@1.0 and win/static-derived seed, print summary.compact_bytes_total and the region sum, and recompute the T3 ratio = leaned@1.0 total / static-derived total; confirm the corrected table (expect ratios ~0.554/0.918/0.928/0.941, median 0.923x) and that the bimodal narrative is corrected. Also confirm: the 'achieved_fraction' sentence now matches the raw (seed43 rung-200 = 1.1315; seed42r rung-400 = 1.004); the T4 wording matches the raw post-drift values; the T1/T2/T4 verdicts and the NOT-10-seed disclosure are unchanged; pre-registration untouched.",
"PASTE exact output for: " + gates + ".",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const repair = await runs.run("repair", { label: "WP-X record scribe repair", agent: "worker", model: STRONG + ":high", cwd: B, task: repairTask });
const verify = await runs.run("verify", { label: "Verify the WP-X record repair", agent: "worker", model: STRONG + ":high", cwd: B, task: verifyTask });
return { repair: { ok: repair.ok, output: repair.output }, verify: { ok: verify.ok, output: verify.output } };
