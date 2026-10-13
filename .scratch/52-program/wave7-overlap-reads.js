// Small hygiene (commit control scripts) + create wave7, then overlap request-path reads, then verify.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT7 = "/home/z/vibe/worktrees/flashdb/wave7-overlap-reads";

const setupTask = [
"You are the integration/hygiene worker. Repo /home/z/vibe/flashdb, master at 63e5f69. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git add` the untracked orchestration control scripts under .scratch/52-program/ (*.js) and commit as '52: wave4-6 orchestration control scripts'.",
"2. Create the next lane worktree: `git worktree add -b wave7/readsoverlap " + WT7 + " master`. Verify and report its HEAD.",
"3. COMBINED GATES, capture verbatim: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent).",
"REPORT: final HEAD, worktree path/HEAD, verbatim gates."
].join("\n");

const overlapReadsTask = [
"flashdb work package G: take the request-path reads off the pacer so a slow cold read does not become schedule lag. Single writer in " + WT7 + " (isolated worktree, branch wave7/readsoverlap, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis/design before implementing; commit before any long measurement.",
"",
"CONTEXT (verified): .scratch/57-cold-reads-and-colocation/issues.md found that after compaction (WP-B), the overlapped WAL barrier (WP-E) and the non-blocking window boundary (WP-F), the LAST rung-50 lag-ceiling breaches (cold44 6/40, cold51 4/40) are attributed to the request-path READS: the driver is single-threaded, so a slow cold point read on one request blocks the pacer from dispatching the next, turning read latency into schedule lag. The ticket's own next-step says: 'Move the request-path reads off the pacer (async/overlapped reads, or a read thread pool) is the remaining dominant mechanism and is a different work package'. Do that.",
"",
"OBJECTIVE: overlap request-path reads with dispatch/pacing so read latency becomes per-request SERVICE latency (reported separately) rather than schedule lag, while preserving every invariant: single-writer commit semantics, the arrivals model, the durability protocol (ack only after a covering fdatasync), disjoint tile ranges, and the read-path correctness (newest-first get, memtable-over-tile precedence).",
"Evaluate and choose among (record rejected alternatives): (a) a read worker thread pool / async reads with the pacer dispatching on schedule and collecting results; (b) software pipelining of the next group's reads while the current group is served; (c) io_uring / async file reads; (d) a two-stage split (read phase off the pacer, commit phase single-writer). Constrain the change to what actually helps the measured tail; do not rewrite the engine wholesale. If the engine's &mut self write path makes a design infeasible, say so precisely and report the bounded result rather than faking it.",
"",
"JUDGING: report per rung BOTH sustained-by-lag-only (lag p99 <= 10 ms, 0 lag-rejected windows) and sustained-by-both-ceilings; restate that the primary-vs-service-ceiling reconciliation is an OPEN AUTHOR ITEM (do not edit the pre-registration or docs).",
"MEASUREMENT: cp -a copies of /tmp/cold44/per_region-mixed (8.17x, iterate) and /tmp/cold51/per_region-mixed (12.25x, confirm). Build once, copy the binary, run only that copy, one process at a time. Before/after rungs 50/100/200/400: svc and lag p50/p95/p99, achieved, rejections per ceiling, and the boundary/read breach attribution.",
"ACCEPTANCE: durability unchanged and invariants preserved; and rung 50 sustained by the lag ceiling (0 lag-rejected windows) on cold44 AND c51 with no achieved-throughput or write-amp regression; OR, if unreachable, the exact remaining per-breach cause with evidence. Never overstate.",
"TESTS: add tests that fail against the pre-change behaviour where meaningful. Keep all Rust and Python tests green.",
"TICKET .scratch/58-overlapped-reads/issues.md: contract, diagnosis/design, rejected alternatives, change, tests, verbatim gates, before/after table, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash, headline numbers, honest verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-G, worktree " + WT7 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/engine.rs, engine/src/replay.rs, engine/src/bin/flashdb-load.rs, engine/src/wal.rs, engine/tests/, ticket .scratch/58-overlapped-reads/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY: (a) does the read-overlap change preserve single-writer commit semantics, the arrivals model, the durability ordering (ack only after a covering fdatasync), and read correctness? Look for any race or use of a stale snapshot. (b) does the driver's new dispatch genuinely not block on reads, and does it still measure each request's lag as dispatch-minus-due (not fake it)? (c) are the before/after figures and the breach attribution reproducible from the raw /tmp artifacts (state paths)? (d) is the verdict honest about anything not achieved, and is the open author item still flagged? (e) do the new tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Commit control scripts and create wave7", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) {
  return { setup: { ok: false, output: setup.output } };
}
const overlap = await runs.run("overlap-reads", { label: "Overlap request-path reads with dispatch", agent: "worker", model: STRONG + ":high", cwd: WT7, task: overlapReadsTask });
const verify = await runs.run("verify", { label: "Verify the overlapped-reads package", agent: "worker", model: STRONG + ":high", cwd: WT7, task: verifyTask });
return {
  setup: { ok: setup.ok, output: setup.output },
  overlap: { ok: overlap.ok, output: overlap.output },
  verify: { ok: verify.ok, output: verify.output }
};
