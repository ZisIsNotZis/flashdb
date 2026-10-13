// Integrate wave4/lag (WP-D) and attempt the one remaining durability-preserving latency fix:
// overlapped group commit (dispatch must not block behind the previous group's fdatasync), then verify.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT5 = "/home/z/vibe/worktrees/flashdb/wave5-overlap";

const integrateTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at 8766c59. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave4/lag` (WP-D, verified PASS at 9cc224f; observational lag-stall instrumentation, no engine latency change). Resolve conflicts minimally and record them.",
"2. Create the next lane worktree: `git worktree add -b wave5/overlap " + WT5 + " master`. Verify and report its HEAD.",
"3. COMBINED GATES, capture verbatim: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent).",
"4. Append an 'Integration record: wave4/lag' section to .scratch/52-program/issues.md (real clock, merge commit, final HEAD, verbatim gates, wave5 path). Commit.",
"REPORT: final HEAD, merge status, wave5 worktree path/HEAD, verbatim gates."
].join("\n");

const overlapTask = [
"flashdb work package E: overlapped, durability-preserving group commit. Single writer in " + WT5 + " (isolated worktree, branch wave5/overlap, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint after each stage; commit before any long measurement.",
"",
"WHY (verified in .scratch/55-lag-stall-attribution/issues.md, WP-D PASS): the primary per-window lag ceiling (lag p99 <= 10 ms, no window rejected) fails because the driver's pacer is single-writer and SYNCHRONOUS: it cannot issue group N+1 until group N's WAL fdatasync returns. So a single group's device fdatasync tail (71-498 ms under load, vs 12-24 ms idle) lands on the NEXT group as schedule lag, batching the requests accumulated during the stall. WP-D correctly rejected 'async group commit (ack before the barrier)' because that weakens durability. But a DIFFERENT design preserves durability: allow the next group's append/dispatch to proceed while group N's fdatasync is in flight, and still require every request to be acknowledged only after an fdatasync covering its appends has completed. This converts the device tail from SCHEDULE LAG into per-request SERVICE latency (reported separately), leaving durability unchanged.",
"",
"STAGE 1 (engine, commit first): add an async/overlapped barrier to the group-commit path so the driver can begin and append group N+1 while group N's WAL barrier is in flight. Requirements, all mandatory: (a) a request/group is reported durable ONLY after an fdatasync covering its WAL appends has completed; (b) at most a bounded number of barriers in flight (ordering preserved); (c) an fdatasync error fails closed exactly as today (poison the WAL / require recovery), never a silent ack; (d) WAL rotation and segment-recovery semantics remain correct with a barrier in flight; (e) memory/state bounded. Add tests proving the ordering and the fail-closed behavior. If (a)-(e) cannot be satisfied, STOP, commit the analysis, and report — do not weaken durability.",
"STAGE 2 (driver + measurement): rework the load driver so dispatch does not block behind the barrier: form groups from already-due requests on schedule, submit appends, and record each group's completion when its covering barrier completes (so service latency includes the barrier, but schedule lag does not). Keep the arrivals model and every durable invariant from 744caaa/965adf4.",
"",
"JUDGING (do NOT edit the pre-registration or docs): the adopted decision .scratch/STATUS.md section 7.1 states the primary metric is judged on the LAG ceiling with service latency reported separately. Report per rung BOTH: (i) sustained-by-lag-only (lag p99 <= 10 ms and no window rejected by the lag ceiling) and (ii) sustained-by-both-ceilings. State clearly which the adopted decision implies and flag the pre-registration's service-ceiling-invalidates-window text as an open author reconciliation, without changing it.",
"",
"MEASUREMENT: cp -a copies of /tmp/cold44/per_region-mixed (8.17x, iterate) and /tmp/cold51/per_region-mixed (12.25x, confirm). Build once, copy the binary, run only that copy, one process at a time. Before/after rungs 50/100/200/400: per-rung svc and lag p50/p95/p99, achieved, rejections by each ceiling, and group/barrier phase stats. ACCEPTANCE: durability preserved with tests; and, if stage 1 is sound, at least rung 50 meets the lag ceiling (lag p99 <= 10 ms, no lag-rejected window) without regressing achieved throughput or write amp. If it still fails, report exactly why with per-group evidence. Never overstate.",
"",
"TICKET .scratch/56-overlapped-barrier/issues.md: contract, design + rejected alternatives, the durability argument, tests, verbatim gates, before/after table, the two judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash, headline numbers, honest verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-E, worktree " + WT5 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/engine.rs, engine/src/wal.rs, engine/src/bin/flashdb-load.rs, engine/tests/, ticket .scratch/56-overlapped-barrier/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY with special attention to DURABILITY, because this changes the group-commit barrier: (a) is every request still acked only after an fdatasync covering its appends completed? Find the exact ordering and any window where an ack or a driver completion could precede durability. (b) on fdatasync error, does it still fail closed (poison/recovery), with no silent ack? (c) with multiple barriers in flight, is ordering preserved and is WAL rotation/segment recovery still correct? (d) does the driver's new completion timing actually measure the barrier (not fake an early completion) and keep the arrivals model? (e) are the before/after figures reproducible from the raw /tmp artifacts, and is the 'sustained by lag' claim computed on the real per-window criterion, not a median? (f) is the pre-registration untouched and is the section-7.1 reconciliation presented as an open item, not a silent change? (g) do the new tests genuinely exercise the ordering/fail-closed behavior? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-D and create wave5", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}
const overlap = await runs.run("overlap", { label: "Overlapped durability-preserving group commit", agent: "worker", model: STRONG + ":high", cwd: WT5, task: overlapTask });
const verify = await runs.run("verify", { label: "Verify the overlapped-barrier package", agent: "worker", model: STRONG + ":high", cwd: WT5, task: verifyTask });
return {
  integrate: { ok: integ.ok, output: integ.output },
  overlap: { ok: overlap.ok, output: overlap.output },
  verify: { ok: verify.ok, output: verify.output }
};
