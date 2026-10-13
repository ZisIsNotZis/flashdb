// Repair the WP-C acceptance-claim honesty defect (verifier blocking finding) and re-verify.
const STRONG = "deepseek/deepseek-v4-pro";
const WT3 = "/home/z/vibe/worktrees/flashdb/wave3-residual";

const repairTask = [
"You are the single writer repairing the flashdb WP-C ticket after an independent verifier returned FAIL. Worktree " + WT3 + ", branch wave3/residual, HEAD e791cec. Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally.",
"",
"THE BLOCKING FINDING (verifier, reproduced with the committed /tmp/cold54/analyze.py): the ticket .scratch/54-residual-tail/issues.md acceptance verdict / 'Smallest honest statement' / proposal #1 falsely claims the pre-registration's PRIMARY lag ceiling is met at rung 50 ('lag p99 median 2.2-2.7 ms'). The pre-registration metric is PER-WINDOW 'lag p99 <= 10 ms, no window rejected'. The artifacts show the lag ceiling (lag_p99_us > 10000) is violated in 9/40 windows (baseline rung 50), 7/40 (c51 rung 50), 10/20 (baseline rung 100), 3/20 (c51 rung 100). A median hides the per-window tail. The rare un-attributed scheduler/disk spikes are what cause the PRIMARY lag ceiling to fail, and proposal #1's premise (judging on lag alone would pass rung 50) is therefore invalid.",
"",
"REPAIR (evidence first; paste raw output):",
"1. Re-run `python3 /tmp/cold54/analyze.py /tmp/cold54/baseline.stdout` and the c51 baseline, count the lag-ceiling-breached windows per rung, and paste the commands and counts.",
"2. Rewrite the acceptance verdict / smallest-honest-statement / proposal #1 so they state the truth: at rung 50 NEITHER the service ceiling NOR the per-window lag ceiling is met; the lag breaches (N/40) trace to the rare un-attributed scheduler/disk spikes listed in the residual; the per-window median lag is small but the per-window tail decides the metric. If proposal #1 (how the pre-registration might treat the ceiling) is retained, restate its premise correctly and mark it as a proposal only.",
"3. Fix the transcription errors the verifier found: Config-4 'neither' p95 med (actual from artifacts 11,588/10,578/19,817/26,522, not 11,150/12,371/17,659/34,128); device fsync p50/p99 (actual baseline 6,669/13,396 per baseline.stdout, c51 9,107/26,391, not 5,176/10,528). Also correct the same fsync figures if they were copied into .scratch/52-program/issues.md.",
"4. Optionally strengthen the group_fsync unit test to assert non-zero accumulation for a non-empty group where portable; if not portable, say so.",
"",
"Do NOT edit .scratch/15-objective-v0/pre-registration.md or docs/. Do not change any measured number to a value the artifacts do not support. Commit and report the hash and the corrected per-rung lag-breach counts.",
"GATES: cd harness && uv run --with pytest pytest -q (summary); cargo build --workspace --offline (0 warnings); git diff --check (silent)."
].join("\n");

const reverifyTask = [
"You are a FRESH independent verifier of the WP-C repair. Worktree " + WT3 + ", branch wave3/residual. Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it). Inspect `git log --oneline -4`. Re-run `python3 /tmp/cold54/analyze.py /tmp/cold54/baseline.stdout` (and c51 baseline) and independently count the lag-ceiling-breached windows per rung.",
"Verify: (1) the ticket's acceptance verdict / smallest honest statement / proposal #1 no longer claim the lag ceiling is met and now state the per-window truth with the breach counts; (2) the Config-4 'neither' numbers and the device fsync figures now match the raw artifacts (quote both); (3) no other measured claim was changed to an unsupported value; (4) the pre-registration and docs were not edited; (5) gates: cargo build --workspace --offline (0 warnings), cargo test --workspace --offline (totals), cd harness && uv run --with pytest pytest -q, git diff --check (silent).",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking) with evidence."
].join("\n");

const repair = await runs.run("repair", { label: "Correct WP-C acceptance claims", agent: "worker", model: STRONG + ":high", cwd: WT3, task: repairTask });
const reverify = await runs.run("reverify", { label: "Re-verify WP-C honesty repair", agent: "worker", model: STRONG + ":high", cwd: WT3, task: reverifyTask });
return {
  repair: { ok: repair.ok, output: repair.output },
  reverify: { ok: reverify.ok, output: reverify.output }
};
