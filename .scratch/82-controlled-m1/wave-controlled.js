const repo = "/home/z/vibe/flashdb";

const runTask = `You are the controlled-measurement worker for ticket 82 (M1 decidable verdict) of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m1-controlled (create it from master if absent; work only on it). Do not push. Do not change the pre-registration, roadmap thresholds, or engine code.

READ FIRST: .scratch/82-controlled-m1/issues.md (the protocol and acceptance), .scratch/80-external-baselines/{report.md,issues.md,env.md}, and the existing harness/scripts (harness/flashdb_harness/baseline_0b.py, scripts/m1-baseline-run.sh, scripts/m1-ladder-run.sh, scripts/m1-ladder-score.py).

Machine state is already fixed by the user: AC online, power profile performance, EPP performance, 3.6 GHz. RECORD it (AC, profile, EPP, scaling_cur_freq, loadavg, top CPU procs) before and after every arm-run; do not rely on the governor name (it still says powersave on this intel_pstate box).

GOAL: produce a decidable T5/T6/T7 table at >=10x under a controlled paired protocol, as specified in ticket 82:
1. Build the seeded pre-load instance once per arm from the frozen seed trace (runs/m1-10x/rung-50/seed.jsonl), then snapshot it. Restore the snapshot before every arm-run; never re-seed inside the loop.
2. Paired blocks: N>=5 blocks; within a block, for rungs 50/100/200, run flashdb-declared, sqlite, postgres interleaved (restore -> 20 s run -> restore ...). Report median/IQR per (arm, rung) across blocks.
3. Machine-state recording and a device-floor microbench (idle schedule-lag/fsync, no engine) at the same state. Report the absolute 10 ms ceiling and a floor-adjusted reading.
4. Same unified validity rule (lag p99 > 10 ms OR 3-consecutive-window growth OR any error; sustained = within 5% and zero rejected windows). Engine-only 128 MiB cap; harness outside; one process at a time under flock.
5. No subagents, builds, or seed jobs during the measured phase; verify with ps and record loadavg. The desktop apps (firefox/gnome/pi) may stay; pairing is the control.
6. Report the physical footprint per arm (flashdb 12.55x, postgres 8.22x, sqlite 4.07x of 128 MiB) and do not inflate a baseline.

DELIVERABLE: .scratch/82-controlled-m1/report.md + report.json with:
- the per (arm, rung) median/IQR table: achieved rps, lag p99, service p99, rejected windows and reasons, errors, machine state, engine peak;
- the device floor and the floor-adjusted reading;
- T5 at 50 rps, T6 at the highest common sustained level, T7 (declared passes T5), each with its exact condition and arithmetic;
- per metric a verdict: PASS / FAIL / a named remaining methodology defect + the smallest fix. Do NOT use "undecidable" or "cannot verify" as a conclusion.
- an honest summary: what the numbers support and do not support.

Run the gates: cd harness && uv run --with pytest pytest -q; the cargo build/test command with the project's pinned aliyun config and --offline; git diff --check. Commit on wave/m1-controlled, tree clean.

Report the commit, the decisive table, the floor, the machine state, the gate lines, and anything still open. If a snapshot restore cannot reproduce the seeded state, or the box cannot hold the quiet window, stop and report the exact failure rather than faking a run.`;

const reviewTask = `You are a fresh independent reviewer for ticket 82 (the controlled M1 verdict).

Repo /home/z/vibe/flashdb, branch wave/m1-controlled. Read-only. Read .scratch/82-controlled-m1/{issues.md,report.md,report.json}, the harness/scripts changes, the committed run records, and the pre-registration T5/T6/T7 conditions.

Falsify:
1. Was the paired protocol actually followed (restore snapshot, interleave arms, N blocks), and is the median/IQR computed from the committed per-run records rather than asserted?
2. Is the machine state recorded (AC, profile, EPP, cur_freq, loadavg) and was the measured phase free of concurrent subagents/builds?
3. Is the device floor measured at the same state, and is the floor-adjusted reading reported alongside the absolute 10 ms rule?
4. Are T5/T6/T7 computed from the sustained rule (not the achieved column), with the exact condition and arithmetic?
5. Is any "not determinable" left without a named methodology defect and smallest fix? Is anything marked PASS that is unverified?
6. Are the per-arm physical footprints disclosed?

Report VERDICT PASS / CANNOT-VERIFY / FAIL for the ticket-82 deliverable, blocking defects with file:line, non-blocking findings, and a finite verdict. State whether the T5/T6/T7 verdicts are now decidable, and if not, the exact remaining defect.`;

const run = await runs.run("run", {
  agent: "worker",
  label: "Paired controlled M1 measurement at 10x",
  cwd: repo,
  task: runTask,
  timeoutMs: 10800000,
});
if (!run.ok) {
  return { stage: "run", ok: false, output: run.output };
}

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Review controlled M1 verdict",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 2700000,
});

return { stage: "done", ok: true, run: run.output, review: review.output };
