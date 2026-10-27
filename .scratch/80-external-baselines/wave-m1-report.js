const repo = "/home/z/vibe/flashdb";

const fixTask = `You are the final report-fix worker for milestone M1 of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m1-baselines, HEAD bb7c6f0, tree clean. Do not push. This is a report/artefact correction only: no re-measurement, no engine change, no threshold change.

Read the fresh re-review in full: /home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/0249adaa-cc1a-4b67-ba76-b443ecddd80b_reviewer_output.md . Then read .scratch/80-external-baselines/report.md, report.json, issues.md, and the committed run records under .scratch/80-external-baselines/runs/m1-10x/rung-50/.

Fix the one blocking P1 and the listed P2s, all from committed records:

P1 (blocking): report.md §5 says the serving peak is measured over the measured phase only and the bulk load is excluded. That is false for both SQL arms at the regime. Use the records:
- sqlite: engine_memory.start_bytes 134193152, peak_bytes 134217728, arm_scope_peak_bytes 134217728 (cap 134217728).
- postgres: start_bytes 134197248, peak_bytes 134217728, arm_scope_peak_bytes 134221824.
- flashdb: start_bytes 3547136, peak_bytes 134217728 (genuinely measured-phase).
State that both SQL arms entered the measured phase at ~99.98% of the cap, so their serving peaks are carried-over bulk-load memory rather than measured-phase growth, and that sqlite's serving peak is numerically its arm-scope lifetime peak. Add a peak-minus-start column (or equivalent) so a reader can see it. Keep the flashdb construction-scope disclosure.

P2 (all from the review):
#2 report.md:245 arithmetic: the declared shortfall vs 3x is 6x (3.0/0.5), the learned one 3x.
#3 remove the un-reproducible 12:49:34 timing and the ~8 h budget derivation based on it; use the committed show boundaries and state the measured seed/elapsed/oracle numbers actually recorded.
#4 remove the "kernel ignores a write to memory.peak ... verified" claim; no committed artefact records it.
#5 report.json.unverified must carry all 8 §7 items, not 4; do not keep a second hard-coded prose home that disagrees with report.md.
#6 give run-B T5/T7 a tri-state field (pass / fail / not-robustly-determined) so a JSON-only reader cannot read them as PASS.
#7 soften report.md:127 to what the committed loadavg readings support.
#8 report.md:190 heading overclaims "observed": flashdb's durability_observed is null; retitle.
#9 add the missing comparison figure required by docs/0_ROADMAP.md M1 (a chart of latency p99 vs offered rate with sustained markers, generated from committed per-rung records), save it under .scratch/80-external-baselines/ (e.g. figure.svg or .png) and reference it from §2/§3.
#10 state that run-flashdb-declared.json oracle_s 6.53e-06 is a pre-fix timer artifact and the real cost is the measured 38 min.
#12 label the flashdb-declared 100 rps vs flashdb-learned 200 rps pair as run-to-run variance of the same declared wiring, not two T6 readings.

Also correct the project gate fact: .scratch/STATUS.md §6 currently says cargo build --workspace --offline is 0 warnings. On this machine the literal --offline command fails at resolution (Cargo.lock pins the crates.io source; the cache is mirrors.aliyun.com). Replace it with the working command the repo runner uses (the pinned aliyun --config source replacement plus --offline) and record the condition, so the claim is reproducible.

Then run and record: cd harness && uv run --with pytest pytest -q; the cargo build/test command with the project's pinned aliyun config and --offline; git diff --check; and verify the figure file exists and is non-empty. Commit on wave/m1-baselines, tree clean.

Report: the commit hash, each P1/P2 with where it was fixed, the gate summary lines, the figure path, and anything still open. Stop instead of inventing if a claim cannot be reconciled with a committed record.`;

const reviewTask = `You are a fresh narrow reviewer for the final M1 report correction.

Repo /home/z/vibe/flashdb, branch wave/m1-baselines. Read-only; do not edit. Read the prior re-review at /home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/0249adaa-cc1a-4b67-ba76-b443ecddd80b_reviewer_output.md and the corrected report.md, report.json, issues.md, STATUS.md, and the committed run records.

Verify by falsification:
1) The blocking P1 is fixed: does §5 now state that both SQL arms entered the measured phase at ~99.98% of the cap (citing start_bytes), that their serving peaks are carried-over bulk-load memory, and that sqlite's serving peak equals its lifetime peak? Is there a peak-minus-start column?
2) Are P2 #2,#3,#4,#5,#6,#7,#8,#9,#10,#12 fixed as described?
3) Is the figure present, non-empty, generated from committed records, and referenced?
4) Is the STATUS.md gate claim now reproducible with the named command on this machine?
5) Does report.json now carry all 8 unverified items and a tri-state T5/T7 for run B?
Give VERDICT PASS / CANNOT-VERIFY / FAIL with blocking defects (file:line) and non-blocking findings. A finite verdict. Do not re-run the full suites; a gate runner's results at the same commit are acceptable evidence if the commit is named.`;

const fix = await runs.run("fix", {
  agent: "worker",
  label: "Fix M1 blocking P1 and report P2s",
  cwd: repo,
  task: fixTask,
  timeoutMs: 3600000,
});
if (!fix.ok) {
  return { stage: "fix", ok: false, output: fix.output };
}

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Narrow-review corrected M1 report",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 1800000,
});

return { stage: "done", ok: true, fix: fix.output, review: review.output };
