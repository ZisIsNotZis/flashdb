const repo = "/home/z/vibe/flashdb";

const reviewTask = `You are a fresh independent reviewer for ticket 82 (the controlled M1 verdict at f70d91c).

Repo /home/z/vibe/flashdb, branch wave/m1-controlled, HEAD f70d91c. Read-only. Read .scratch/82-controlled-m1/{issues.md,report.md,report.json}, the committed run records under runs/ (per-block run-<arm>.json and windows-<arm>.jsonl, floor records, machine-state), scripts/m1-controlled-run.sh, scripts/m1-controlled-score.py, harness/flashdb_harness/baseline_0b.py, and the pre-registration T5/T6/T7.

Falsify:
1. Was the paired protocol actually executed (snapshot restore + digest check, arm rotation, N=5, 45 crossings), and are the medians/IQRs recomputable from the committed records?
2. Is machine state recorded at both boundaries (AC, EPP, cur_freq, loadavg) and was the measured phase free of concurrent subagents/builds?
3. Is the device floor measured at the same state, and is the floor-adjusted reading reported beside the absolute 10 ms rule?
4. Are T5/T6/T7 computed from the sustained rule (not the achieved column), with exact arithmetic? Is T6's "no denominator" a fair statement, and is it labelled FAIL rather than undecidable?
5. Are the two systematic baseline handicaps (sqlite's 254 ms checkpoint stall; the closed-loop SQL pacer vs flashdb's open loop) correctly diagnosed and disclosed, with the smallest fix named? Does either defect make the T6 FAIL an artifact rather than a finding?
6. Are the per-arm physical footprints (flashdb 12.55x, postgres 8.22x, sqlite 4.07x of 128 MiB) disclosed?
7. Is anything marked PASS that is unverified (e.g. the flashdb 10x oracle deferred)?

Report VERDICT PASS / CANNOT-VERIFY / FAIL for the ticket-82 deliverable, blocking defects with file:line, non-blocking findings, and a finite verdict. State plainly whether T5/T7 are decidable and whether T6 is a fair FAIL or still method-limited.`;

const fixTask = (review) => `You are the fairness-fix worker for ticket 82 (controlled M1) of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m1-controlled, HEAD f70d91c, tree clean. Do not push. Do not change the pre-registration, the roadmap thresholds, or engine code.

The independent review of f70d91c is below. Read it, then .scratch/82-controlled-m1/{issues.md,report.md,report.json} §9 and the committed records.

APPLY THE TWO CHEAP FAIRNESS FIXES the report itself names (do not rebuild instances or re-measure the floor; reuse the committed snapshots and floor):
1. sqlite WAL checkpoint policy: set an explicit PRAGMA wal_autocheckpoint (or journal_size_limit) and record the value per run, so the 254 ms stall is not an unstated default. First run one short sqlite probe with and without the setting to confirm the checkpoint hypothesis (the report's §9.2 admits it is inference, not a probe). Record the probe.
2. Asynchronous SQL pacer: make the sqlite and postgres pacers submit the durable commit and dispatch the next ready batch, acking on completion, so the lag rule is not applied to a closed-loop client while flashdb runs open-loop. Keep the same durability (one durable commit per batch, the same B) and record the mean/max group size.

Then re-run the 5 paired blocks (rungs 50/100/200, arms flashdb-declared, sqlite, postgres) using the existing snapshots, restored and digest-checked, with the same machine-state recording, floor, one-process-at-a-time lock, and N=5. Re-score and update .scratch/82-controlled-m1/report.md and report.json with the corrected T5/T6/T7.

Do NOT change any pre-registered threshold. If T6 is still FAIL after the fixes, report it as a fair FAIL and name any remaining methodological limit (notably the per-arm >=10x physical footprint: sqlite 4.07x, postgres 8.22x) with the smallest fix. If you find the report's T5/T7 read is wrong, correct it and say why.

Run the gates (cd harness && uv run --with pytest pytest -q; pinned-aliyun cargo build/test --offline; git diff --check) and commit on wave/m1-controlled, tree clean. Report the commit, the corrected T5/T6/T7 table, the checkpoint probe result, the pacer change, and anything still open.

INDEPENDENT REVIEW OF f70d91c:
${review}`;

const review2Task = `You are a fresh independent reviewer of the fairness-fixed controlled M1 result in /home/z/vibe/flashdb, branch wave/m1-controlled.

Read .scratch/82-controlled-m1/{issues.md,report.md,report.json}, the committed run records, the sqlite checkpoint probe, the pacer change, and the pre-registration T5/T6/T7.

Falsify: (1) was the sqlite checkpoint hypothesis actually probed, and is the setting recorded? (2) is the SQL pacer now asynchronous like flashdb's, with equal durability? (3) are the 5 paired blocks re-run with the same snapshots/floor/lock, and are the medians recomputable? (4) are T5/T6/T7 computed from the sustained rule with exact arithmetic? (5) if T6 is still FAIL, is that a fair FAIL with the remaining footprint limit named, not hidden? (6) is anything PASS that is unverified?
Give VERDICT PASS / CANNOT-VERIFY / FAIL, blocking defects with file:line, and a finite verdict.`;

const review1 = await runs.run("review1", {
  agent: "reviewer",
  label: "Review controlled M1 verdict",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 1800000,
});
if (!review1.ok) {
  return { stage: "review1", ok: false, output: review1.output };
}

const fix = await runs.run("fix", {
  agent: "worker",
  label: "Fix baseline fairness and re-run paired blocks",
  cwd: repo,
  task: fixTask(review1.output),
  timeoutMs: 10800000,
});
if (!fix.ok) {
  return { stage: "fix", ok: false, review1: review1.output, output: fix.output };
}

const review2 = await runs.run("review2", {
  agent: "reviewer",
  label: "Review fairness-fixed M1 result",
  cwd: repo,
  task: review2Task,
  timeoutMs: 1800000,
});

return { stage: "done", ok: true, review1: review1.output, fix: fix.output, review2: review2.output };
