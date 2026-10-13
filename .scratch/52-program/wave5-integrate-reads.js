// Integrate wave5/overlap (WP-E) and attack the last rung-50 residual: boundary drain + cold reads.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT6 = "/home/z/vibe/worktrees/flashdb/wave6-reads";

const integrateTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at 56faf20. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave5/overlap` (WP-E, verified PASS at 1cc629a: durability-preserving overlapped WAL barrier). Resolve conflicts minimally and record them.",
"2. Create the next lane worktree: `git worktree add -b wave6/reads " + WT6 + " master`. Verify and report its HEAD.",
"3. COMBINED GATES, capture verbatim: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent).",
"4. Append an 'Integration record: wave5/overlap' section to .scratch/52-program/issues.md (real clock, merge commit, final HEAD, verbatim gates, wave6 path). Commit.",
"REPORT: final HEAD, merge status, wave6 worktree path/HEAD, verbatim gates."
].join("\n");

const readsTask = [
"flashdb work package F: remove the LAST rung-50 lag breaches. Single writer in " + WT6 + " (isolated worktree, branch wave6/reads, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; diagnose/checkpoint before fixing; commit before any long measurement.",
"",
"CONTEXT (verified): WP-E (.scratch/56-overlapped-barrier/issues.md, PASS) made the WAL barrier overlapped and durability-preserving, removing the fsync-tail cascade (rung-50 lag breaches 9/40 -> 5/40 on cold44, 2/40 on c51). The remaining breaches are now attributed to (1) DOMINANT: synchronous cold point reads on the request path (a request's dependent random reads stall behind cold preads, especially under concurrent write/compaction I/O); (2) SECONDARY: the window-boundary barrier drain (the driver blocks at a window boundary waiting for in-flight barriers, delaying the next window's first group).",
"",
"PART 1 - DIAGNOSE then FIX the boundary drain first (cheap): make window finalization non-blocking for dispatch (keep groups pipelined across the boundary; drain only for accounting without delaying the next group). Add a per-window counter proving how many lag breaches came from the boundary vs from cold reads.",
"PART 2 - reduce the cold-read stall. This is the layout thesis axis: ticket 43 measured today's placement at 4.740 dependent random reads/request and entity-first family co-location at 3.092 (-34.8%). Determine, with per-request read counts and per-read latency, whether reducing dependent reads reduces rung-50 lag breaches. Evaluate at least: (a) a bounded read-path optimization (prefetch of the next block(s) / read-ahead / batching the dependent reads of one request) if it does not weaken any invariant; (b) ENABLE the existing entity-first key encoding (engine/src/keys.rs KeyEncoding selector) and build a cold instance with it, then measure dependent reads and the rung-50 tail against the family-first instance. This is the first conversion of the -34.8% read reduction into a latency measurement; report it as such. Do NOT change the pre-registration.",
"ALSO fix the two non-blocking WP-E engine issues the verifier found: the submit_group in-flight-cap doc/behavior mismatch (engine.rs ~2992-3020), and end_group's missing ensure_no_pending_barrier guard (engine.rs ~2919); add tests.",
"",
"JUDGING: report per rung BOTH sustained-by-lag-only (lag p99 <= 10 ms, no window rejected by the lag ceiling) and sustained-by-both-ceilings, and restate that the primary-vs-service-ceiling reconciliation is an OPEN AUTHOR ITEM (do not edit the pre-registration or docs).",
"MEASUREMENT: cp -a copies of /tmp/cold44/per_region-mixed (8.17x, iterate) and /tmp/cold51/per_region-mixed (12.25x, confirm); for the co-location arm build an entity-first instance from the same generator/trace and state the exact build path. Build once, copy the binary, run only that copy, one process at a time. Before/after rungs 50/100/200/400: svc and lag p50/p95/p99, achieved, rejections per ceiling, per-request dependent-read counts, boundary-vs-read breach attribution.",
"ACCEPTANCE: durability unchanged; and rung 50 sustained by the lag ceiling (lag p99 <= 10 ms, 0 lag-rejected windows) on cold44 AND c51, with no achieved-throughput or write-amp regression; OR, if unreachable, the exact remaining per-breach cause with numbers. Never overstate. Report the co-location read-count and tail delta honestly even if small.",
"TICKET .scratch/57-cold-reads-and-colocation/issues.md: contract, diagnosis, fix + rejected alternatives, tests, verbatim gates, before/after table, the co-location conversion result, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report hash, headline numbers, honest verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for WP-F, worktree " + WT6 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/engine.rs, engine/src/keys.rs, engine/src/bin/flashdb-load.rs, engine/tests/, ticket .scratch/57-cold-reads-and-colocation/issues.md.",
"PASTE exact output for: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check.",
"FALSIFY: (a) is the boundary-drain fix genuinely non-blocking for dispatch and does its counter attribute breaches correctly (not post-hoc)? (b) does any read-path optimization preserve every invariant (no weakened durability/ordering)? (c) is the entity-first co-location arm real (a truly entity-first instance, the selector actually engaged) and are the read-count/tail deltas measured against a comparable family-first run (same trace/regime, not a confound)? (d) are the before/after and breach-attribution figures reproducible from the raw /tmp artifacts (state paths)? (e) is the verdict honest about anything not achieved, and is the §7.1/open-author item still flagged rather than silently resolved? (f) do the new tests exercise the changes? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-E and create wave6", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}
const reads = await runs.run("reads", { label: "Remove last rung-50 breaches and test co-location", agent: "worker", model: STRONG + ":high", cwd: WT6, task: readsTask });
const verify = await runs.run("verify", { label: "Verify the cold-read/co-location package", agent: "worker", model: STRONG + ":high", cwd: WT6, task: verifyTask });
return {
  integrate: { ok: integ.ok, output: integ.output },
  reads: { ok: reads.ok, output: reads.output },
  verify: { ok: verify.ok, output: verify.output }
};
