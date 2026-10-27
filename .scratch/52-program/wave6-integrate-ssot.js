// Integrate wave6/reads (WP-F), then refresh the project SSOT to the verified current state,
// then independently verify the SSOT against the tickets.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";

const integrateTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master at adba227. Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave6/reads` (WP-F, verified PASS at 17de3a3: non-blocking boundary drain, newest-first read, key-encoding selector + entity-first co-location arm). Resolve conflicts minimally and record them.",
"2. COMBINED GATES, capture verbatim into the ticket: cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent).",
"3. Append an 'Integration record: wave6/reads' section to .scratch/52-program/issues.md (real clock, merge commit, final HEAD, verbatim gates). Commit.",
"REPORT: final HEAD, merge status, verbatim gates."
].join("\n");

const ssotTask = [
"You are the SSOT (current-state) writer for flashdb. Repo /home/z/vibe/flashdb, branch master. Run 'source $HOME/.cargo/env' first. Get the real clock via `date -Iseconds`. Single writer.",
"This is a FACT-DOC rewrite: current state only, one home per fact, no dated narrative, no changelog, no 'was A later B', no duplication. Read the verified tickets before writing: .scratch/53-compaction-tail-latency/issues.md, .scratch/54-residual-tail/issues.md, .scratch/55-lag-stall-attribution/issues.md, .scratch/56-overlapped-barrier/issues.md, .scratch/57-cold-reads-and-colocation/issues.md, and the current .scratch/STATUS.md. Do NOT edit .scratch/15-objective-v0/pre-registration.md (author-owned). Do NOT edit any ticket.",
"",
"REWRITE .scratch/STATUS.md IN PLACE so it states the current verified facts, replacing stale content (it is now six waves out of date). It must include, accurately and with numbers:",
"- Engine: the load-path compaction is now off-request-path (background build + lazy apply, crash-consistency fsynced dirent) and the WAL group barrier is overlapped/durability-preserving (ack only after a covering fdatasync; fail-closed; MAX_PENDING_BARRIERS bound); the driver's window-boundary drain is non-blocking. Cite the commits from the tickets.",
"- Test counts: current Rust and Python totals from ticket 57's gates.",
"- The primary-metric status: rung 50 does NOT meet the lag ceiling (cold44 6/40, cold51 4/40 lag-rejected windows on the final binary); the ceiling is blocked by cold point reads under shared-device durability I/O (device WAL fdatasync tail 71-498 ms under load, 6.7/13.4 ms p50/p99 idle; reads 4.03 blocks/req); the service ceiling rejects nearly all windows and is device-bound.",
"- The co-location result: entity-first co-location reduces requests read volume 16.6% (4.03 -> 3.36 blocks/req) but raises compaction write amp ~12x (22 MB -> 265 MB) and its tail is worse (15/40); the offline -34.8% dependent-read figure was not converted to a load gain.",
"- The OPEN author decision (do NOT resolve it): the pre-registration's validity rule invalidates a window on either the lag ceiling or the separate service ceiling, while the adopted decision (STATUS section 7.1, 2026-10-09) says the primary metric is judged on the LAG ceiling with service reported separately. List the concrete options (lag-only primary per section 7.1; keep both ceilings and treat the host/device floor as the limit; move WAL to a sub-10ms-fsync device).",
"- Keep the sections that are still true (the 0b instrument trust fixes, the environment notes, reviewer hygiene) but correct any number now stale (for example the whole-run reorganisation figure).",
"",
"Then update ONLY the other owning current-fact docs that these waves made wrong, minimally: docs/8_DETAIL.md if its foreground-commit/barrier description no longer matches the implemented overlapped barrier, and docs/2_GOAL.md if its latency statement conflicts. Record in a short appended section of .scratch/52-program/issues.md what you changed and the source ticket for each change. Commit as one commit '52: refresh the current-state SSOT after waves 2-6'.",
"REPORT: files touched, the exact commit hash, and any fact you refused to change and why.",
"GATES, report verbatim: cd harness && uv run --with pytest pytest -q (summary); git diff --check (silent)."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier of the SSOT refresh. Repo /home/z/vibe/flashdb. Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it). Find the SSOT commit (`git log --oneline -4`) and inspect its diff.",
"FALSIFY: (1) every numeric and factual claim in .scratch/STATUS.md matches the verified tickets 53-57 (spot-check the rung-50 lag-reject counts 6/40 and 4/40, the read volumes 4.03/3.36 blocks/req, the write-amp 22->265 MB, the fsync tail 6.7/13.4 ms p50/p99 and 71-498 ms load tail, the test totals) and quote both sides; (2) no fact is duplicated across docs or left stale/contradictory; (3) the pre-registration was NOT edited; (4) the open author decision is stated as open, not resolved; (5) the file is current-state prose with no dated narrative/changelog; (6) gates: cd harness && uv run --with pytest pytest -q (summary) and git diff --check (silent).",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-F into master", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}
const ssot = await runs.run("ssot", { label: "Refresh the current-state SSOT", agent: "worker", model: STRONG + ":high", cwd: REPO, task: ssotTask });
const verify = await runs.run("verify", { label: "Verify the SSOT refresh", agent: "worker", model: STRONG + ":high", cwd: REPO, task: verifyTask });
return {
  integrate: { ok: integ.ok, output: integ.output },
  ssot: { ok: ssot.ok, output: ssot.output },
  verify: { ok: verify.ok, output: verify.output }
};
