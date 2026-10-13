// Integrate wave3/residual (WP-C) into master, then attack the primary-metric blocker:
// the rare single-request lag-ceiling stalls. Then independent verification.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT4 = "/home/z/vibe/worktrees/flashdb/wave4-lag";

const integrateTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at af86df1. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave3/residual`. It is WP-C (residual-tail attribution + honesty repair), independently verified PASS at 55c26ae. Resolve conflicts minimally and record them.",
"2. Commit the untracked control scripts under .scratch/52-program/ (*.js) with message '52: wave2/wave3 orchestration control scripts'.",
"3. Create the next lane worktree: `git worktree add -b wave4/lag " + WT4 + " master`. Verify it and report its HEAD.",
"4. COMBINED GATES, capture verbatim into the ticket: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q (summary); git diff --check (silent).",
"5. Append an 'Integration record: wave3/residual' section to .scratch/52-program/issues.md (real clock, merge commit, final HEAD, verbatim gates, wave4 path). Commit.",
"REPORT: final HEAD, merge status, wave4 worktree path/HEAD, verbatim gates, any failure."
].join("\n");

const lagTask = [
"flashdb work package D: attribute and remove the primary-metric (lag-ceiling) stalls. Single writer in " + WT4 + " (isolated worktree, branch wave4/lag, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis before any fix; commit before any long measurement.",
"",
"CONTEXT (verified): the pre-registration (.scratch/15-objective-v0/pre-registration.md) judges the primary metric on PER-WINDOW lag p99 <= 10 ms with NO window rejected. WP-B made compaction off-request-path; WP-C (.scratch/54-residual-tail/issues.md) showed the service tail is the per-group WAL fdatasync (device p50 6,669 / p99 13,396 us on the 8.17x instance; 9,107/26,391 on 12.25x) and that the LAG ceiling is ALSO breached at rung 50 in 9/40 windows (baseline) / 7/40 (c51). Per-window inspection by WP-C's repair found every lag-breaching window is a SINGLE-request stall (max_group_us == max_latency_us, up to 724 ms) with normal read_bytes and majflt=0. That single-request stall is now the exact blocker of the primary metric, and its cause is NOT attributed.",
"",
"PART 1 - DIAGNOSIS (commit before fixing). Instrument the request path so a lag breach is attributable without a rerun: per-group phase breakdown (time from due to dispatch, WAL append, apply, group fdatasync (already available via group_fsync_us), completion) and the OS view (involuntary/voluntary context switches, a per-window max). Then determine what the ~hundreds-of-ms single-request stall is: a device/fdatasync spike, lock contention with the WP-B compaction background worker, an OS scheduler preemption, page cache reclaim, or an artefact of the driver's pacer/window bookkeeping (for example a group that straddles an idle window or a rung transition). Name it with per-group numbers. Separate at least: (a) with the compaction worker disabled, (b) with writes but no compaction, (c) a read-only run (known to show ~300 ms spikes too), and (d) a control that keeps the process busy but does no engine I/O, to see whether the stall is intrinsic to the host.",
"PART 2 - FIX any engine or driver cause (for example head-of-line blocking behind the fdatasync, lock contention with the worker, or a pacer artefact). Do NOT weaken durability; do NOT edit the pre-registration or docs. If the cause is environmental (host/device), do not fake a fix: characterize it precisely (what event, how often, distribution) and state the smallest honest conclusion, with options as PROPOSALS only.",
"",
"MEASUREMENT/ACCEPTANCE: cp -a copy of /tmp/cold44/per_region-mixed (8.17x) for iteration, confirm on /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Before/after rungs 50/100/200/400: per-rung svc and lag p50/p95/p99, achieved, windows rejected by each ceiling, and the single-stall distribution. ACCEPTANCE: a named cause with per-group evidence, engine/driver causes fixed, and — if the cause is addressable — at least rung 50 sustained (lag p99 <= 10 ms, no window rejected) without weakening durability; otherwise a bounded honest characterization of what blocks the metric and why no engine/driver change removes it. Never overstate.",
"TESTS: add tests that fail against the pre-change behaviour where meaningful. Keep all Rust and Python tests green.",
"TICKET .scratch/55-lag-stall-attribution/issues.md: contract, diagnosis with numbers, decision + rejected alternatives, change, tests, verbatim gates, before/after table, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash, headline numbers, honest verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-D, worktree " + WT4 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list the new commits with `git log --oneline -8`. Scope: engine/src/bin/flashdb-load.rs, engine/src/engine.rs, engine/src/wal.rs, engine/tests/, ticket .scratch/55-lag-stall-attribution/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY: (a) does the per-group instrumentation actually measure the phases it claims, and does the separation config isolate the cause (not confound it)? (b) if a fix was made, does it preserve durability and the pre-registration, and is it the cause of the improvement? (c) are the before/after figures reproducible from the raw /tmp artifacts (state paths) and internally consistent? (d) is the acceptance verdict honest, including any irreducible residual? (e) do the new tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-C and create wave4", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}
const lag = await runs.run("lag", { label: "Attribute and remove the lag-ceiling stalls", agent: "worker", model: STRONG + ":high", cwd: WT4, task: lagTask });
const verify = await runs.run("verify", { label: "Verify the lag-stall package", agent: "worker", model: STRONG + ":high", cwd: WT4, task: verifyTask });
return {
  integrate: { ok: integ.ok, output: integ.output },
  lag: { ok: lag.ok, output: lag.output },
  verify: { ok: verify.ok, output: verify.output }
};
