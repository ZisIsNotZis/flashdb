const repo = "/home/z/vibe/flashdb";

const fixTask = `You are the fix worker for milestone M1 of flashdb (external baseline scoreboard).

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines (clean). Do not push. Do not change the pre-registration, the roadmap thresholds, or engine code. Work only where the defects below require it.

A fresh independent reviewer returned FAIL on the M1 measurement report. Read the full review at /home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/cf6cda41-7451-452c-9693-aa934a1da62a_reviewer_output.md, then docs/0_ROADMAP.md (M1), .scratch/80-external-baselines/report.md, report.json, issues.md, env.md, and harness/flashdb_harness/baseline_0b.py.

Fix every P1 defect and the P2 items that affect correctness or honest disclosure:

P1.1 Same latency rule on every arm. The pre-registration's validity rule includes a 3-consecutive-window lag-growth clause; flashdb-load implements it as lag_saturation (see engine/src/bin/flashdb-load.rs around late_growth_windows/saturated/reject_reason), but the SQL window reducer only rejects on lag_ceiling/errors. Implement the same growth clause in the SQL reducer (WindowSamples/_reject_reason) so the rule is identical for all arms, and re-score the committed windows. Add a test with a window series that flashdb-load marks saturated and that the SQL reducer must also mark (a real agreement check). If an affected rung's verdict changes, report the change.

P1.2 T6 arithmetic. T6 is sustained-vs-sustained at the highest common offered level. The current 1.999/2.00 uses flashdb's ACHIEVED 399.75 at 400 rps (sustained=false) against sqlite's sustained 200 at a different rung. Correct it: report the sustained reading (declared 99.95 vs sqlite 200 = 0.50x for run A), state that the top rung has no baseline denominator, and wire or remove the dead sustained_verdict() in scripts/m1-ladder-score.py. Update report.md, report.json and the T6 verdict text.

P1.3 sqlite durability observed. Read back PRAGMA journal_mode and PRAGMA synchronous after the bulk load and before the measured phase, assert wal/2, record both in run-sqlite.json, and add a test. Do not rely on the return value of the setter.

P1.4 Run B provenance. Add --seed-cap-mib to scripts/m1-baseline-run.sh, record argv in spec.json, and re-run run B (the 12.5x regime, 1x rung, 50 rps) through the locked runner with the lock held, capturing loadavg at the boundaries. Cite the actual command that produced the committed run in report.md §9, not an alternative. The re-run must regenerate the committed run records; do not hand-edit them.

P1.5 Engine memory peak. Record engine_cap_mib for every arm. Measure the measured-phase cgroup peak only (reset memory.peak at the start of the measured phase, or snapshot memory.current), and disclose that the seed/construction scope is separate. Restate the comparison so a seeding peak is never presented as serving pressure.

P2 (fix these): per-arm mean group size at rung 400 and an explicit statement that the equal-durability batch condition does not hold there (postgres ~10.78 vs ~1.0-1.2); the report numbers with no source or wrong value (5.16 loadavg, "20 windows" vs postgres 21, "0.6-1.2x" footprint, "10x inside the 2x line"); add a negative-control pytest for oracle_flashdb (a synthetic replayed iterable that must fail); fix the contradictory postgres-scope statement in issues.md; align the budget statement (3h vs 4h); soften the unsupported "synchronous SQL pacer is the main reason" causal claim.

Also: re-run the flashdb oracle at 10x now that the aggregate-join oracle is fast, and record it as PASS or the exact failure (removes the DEFERRED acceptance gap for run B).

Then run and record the gates: cargo build --workspace --offline; cargo test --workspace --offline; cd harness && uv run --with pytest pytest -q; git diff --check. Commit all changes on wave/m1-baselines and leave the tree clean.

Report: the commit hash, each P1 with its evidence and where it was fixed, the corrected T5/T6/T7 verdict table, the re-run command and its results, the gate summary lines, and anything still unverified. If a fix cannot be made without changing a pre-registered threshold or engine code, STOP and report it instead.`;

const gatesTask = `You are a fresh mechanical-gate runner for milestone M1 of flashdb.

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines. Do NOT edit any file. Run exactly these commands at the current HEAD and report the verbatim command, exit status and decisive output line for each:
1. git rev-parse HEAD and git status --porcelain
2. cargo build --workspace --offline
3. cargo test --workspace --offline
4. cd harness && uv run --with pytest pytest -q
5. git diff --check
6. Recheck the two committed raw records the report now relies on: the flashdb 10x oracle result in .scratch/80-external-baselines/runs/m1-10x/rung-50/run-flashdb-declared.json, and the sqlite durability fields in the corresponding run-sqlite.json. Print the relevant fields.
Report each result as a one-line PASS/FAIL with the exact evidence. If a command cannot run, say exactly why; do not claim a pass.`;

const reviewTask = `You are a fresh independent re-reviewer for milestone M1 of flashdb.

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines. Do not edit files. You may run read-only commands.

Read the previous review (/home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/cf6cda41-7451-452c-9693-aa934a1da62a_reviewer_output.md), the corrected report.md/report.json/issues.md, the pre-registration's "External baselines and thresholds" and "Metric" sections, docs/0_ROADMAP.md M1, and the harness/scripts changes. A separate fresh gate runner produced .scratch/80-external-baselines/gates.txt; read it rather than re-running the full suites.

Judge whether each previous P1 is actually fixed, trying to falsify:
1. Is the latency/sustained rule now provably identical for flashdb and the SQL arms, including the 3-consecutive-window growth clause, and is the SQL reducer tested against the flashdb semantics?
2. Is T6 now sustained-vs-sustained at the highest common level, with the dead code resolved and the no-denominator case stated?
3. Is sqlite's durability now observed (journal_mode/synchronous read back and recorded), not asserted?
4. Does the committed run B now have verifiable provenance (argv, seed cap, lock, loadavg) and does the reproduce command match it?
5. Is the engine memory peak now the measured-phase peak with engine_cap_mib recorded for every arm, and the seed scopes disclosed rather than conflated?
6. Are the P2 number/source/disclosure defects corrected?

Then state, per the M1 acceptance: T5, T6, T7 each with its exact condition, the measured numbers, and a verdict; every unverified criterion (notably T6 at >=10x and any single-seed limitation) disclosed and not marked PASS. Give VERDICT: PASS / CANNOT-VERIFY / FAIL for the M1 deliverable, blocking defects with file:line, and non-blocking findings. A finite verdict, not a request to run more.`;

const fix = await runs.run("fix", {
  agent: "worker",
  label: "Fix M1 P1 defects and re-run regime",
  cwd: repo,
  task: fixTask,
  timeoutMs: 10800000,
});
if (!fix.ok) {
  return { stage: "fix", ok: false, output: fix.output };
}

const gates = await runs.run("gates", {
  agent: "worker",
  label: "Run mechanical gates at fixed HEAD",
  cwd: repo,
  task: gatesTask,
  timeoutMs: 1800000,
});

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Re-review corrected M1 report",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 3600000,
});

return {
  stage: "done",
  ok: true,
  fix: fix.output,
  gates: gates.output,
  review: review.output,
};
