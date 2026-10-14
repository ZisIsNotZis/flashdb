// Repair wave21/rewarm (verifier FAIL): correct the falsified diagnosis and attempt a sound,
// bounded fix for the compaction-BUILD device tail; fresh-verify; merge only if it PASSes.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const B = "/home/z/vibe/worktrees/flashdb/wave21-rewarm";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";
const lock = "Before every measurement command, wrap it in `flock /tmp/wave21-measure.lock -c '<full command>'`.";

const repairTask = [
"flashdb work package W2-repair: correct the falsified diagnosis in ticket 76 and make the rung-50 residual a sound, bounded fix or a proven device floor. Single writer in " + B + " (branch wave21/rewarm, unmerged, based on master 8a4ea20). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally.",
"",
"CONTEXT: your predecessor's package was independently verified FAIL. Read the verifier's findings in the ticket/repo history and the raw data. The falsified claims: (1) the diagnosis said the rung-50 breaches are post-publish COLD READS; the raw WP-S data shows they are the compaction BUILD windows -- e.g. seed43 win/static-derived rung-50 window 16 has `compact_bytes=10,165,563` AND the lag breach, while window 17 (post-publish) has `lag_p99=107 us` and is clean; same in seed44 (w15 build breach, w16 clean) and the repeat (build breaches, post-publish clean). (2) the acceptance claim 'POST-publish windows clean in all four runs' is false: seed42 w18 applies a merge and w19 IS a lag breach (lag_p99=12,580us). So the residual is the compaction BUILD's own device I/O + `fdatasync` tail starving the pacer, NOT a read-warm gap.",
"",
"DO: (a) CORRECT the ticket 76 diagnosis in place to the build-window device-tail cause, with the raw per-window evidence (build window `compact_bytes` vs its lag; clean post-publish windows); remove the false acceptance claim. (b) Then attempt a SOUND bounded fix for that cause -- the compaction build already runs off the request path (ticket 53), so the issue is its device I/O competing with the pacer's WAL/read I/O. Options to evaluate and record: throttle/rate-limit the build's write+fsync burst; split a large merge build into smaller bounded chunks that yield; lower the build's I/O priority (ionice) or interleave; reduce the per-window compaction budget so fewer large builds land during a scored window; or align builds to window boundaries with a drain awareness. Choose the smallest sound mechanism, preserve correctness (compaction is off the correctness path; durability untouched), and do NOT game the metric. If a measured, evidenced case shows the residual is genuinely the device's own latency floor below the 10 ms lag ceiling (prove it with the device fsync distribution while the build runs), say so and stop -- that is a valid outcome. Either way REVERT or keep the prior ineffective warm-cursor change only if it is justified; document it.",
"",
"MEASUREMENT: `cp -a` copy of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Harness `cold_0b.py --workloads win --arms declared learned static-derived`; >=3 seeds + a repeat. Report per-rung achieved_rps, lag-rej, lag p99, and the BUILD-window vs post-publish attribution. " + lock + " Record box load.",
"JUDGING: SS7.1 lag-only primary; report both. Do not edit the pre-registration.",
"ACCEPTANCE: static-derived win rung-50 lag-rej reaches 0/40 across >=3 seeds with no throughput regression, OR the residual is corrected to the build-window cause and proven device-bound with the fsync-during-build distribution. Never overstate.",
"TESTS: keep/adjust tests; add one that pins the corrected behaviour where meaningful; keep all Rust and Python tests green.",
"TICKET: amend .scratch/76-compaction-rewarm/issues.md (corrected diagnosis + fix/verdict + honest acceptance). Append a dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, corrected diagnosis, and honest verdict."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier for the WP-W2 repair in " + B + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: engine/src/readahead.rs, engine/src/engine.rs, the compaction build path, engine/tests/, and ticket .scratch/76-compaction-rewarm/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the corrected diagnosis actually reproduced by the raw per-window data (build window `compact_bytes` coincides with the breach; post-publish windows clean -- recompute from .scratch/71 or the new run JSONs)? (b) if a fix is claimed: is it sound, off the correctness path, not metric-gaming, and does it demonstrably reduce the rung-50 lag-rej with a real >=3-seed + repeat band? (c) if device-bound is claimed: is it proven by the fsync-during-build distribution? (d) is the false 'post-publish clean' claim removed and the ticket honest? (e) pre-registration untouched; tests exercise the change? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master has wave21 tilecap+driftshape; wave21/rewarm unmerged). Run 'source $HOME/.cargo/env' first.",
"1. IF the repair verifier PASSed, `git merge --no-ff wave21/rewarm` into master; if it FAILed, do NOT merge and record that. Resolve minimally.",
"2. COMBINED GATES verbatim: " + gates + ".",
"3. Append an 'Integration record: wave21/rewarm repair' section to .scratch/52-program/issues.md (real clock, verdict, merge-or-not, commit, gates). Commit.",
"REPORT: master HEAD, whether wave21/rewarm was merged, verbatim gates."
].join("\n");

const repair = await runs.run("repair", { label: "WP-W2 repair: correct diagnosis + bounded fix", agent: "worker", model: STRONG + ":high", cwd: B, task: repairTask });
const verify = await runs.run("verify", { label: "Verify the WP-W2 repair", agent: "worker", model: STRONG + ":high", cwd: B, task: verifyTask });
const integ = await runs.run("integ", { label: "Merge the repaired lane if PASSed", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
return { repair: { ok: repair.ok, output: repair.output }, verify: { ok: verify.ok, output: verify.output }, integ: { ok: integ.ok, output: integ.output } };
