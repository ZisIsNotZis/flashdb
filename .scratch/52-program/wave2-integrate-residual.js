// Integrate WP-B (wave2/tailfix) into master, then run the residual-tail work package
// (wave3/residual) and independently verify it. One writer at a time; the review is
// read-only and fresh.

const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const WT3 = "/home/z/vibe/worktrees/flashdb/wave3-residual";

const integrateTask = [
"You are the integrator for flashdb WP-B. Repo /home/z/vibe/flashdb, branch master at e11bf6c. Run 'source $HOME/.cargo/env' first. Single writer on master; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave2/tailfix`. It is the off-request-path compaction fix, already independently verified at e9e2ae5 (verdict PASS). Resolve any conflict minimally and record it.",
"2. Create the next lane worktree branch for the residual-tail package: `git worktree add -b wave3/residual " + WT3 + " master`. Verify it exists on the new master and report its HEAD.",
"3. COMBINED GATES at the merged master, capture verbatim into the ticket:",
"   source $HOME/.cargo/env; cargo build --workspace --offline   (0 warnings)",
"   cargo test --workspace --offline                              (totals)",
"   cd harness && uv run --with pytest pytest -q                  (summary)",
"   git diff --check                                              (silent)",
"4. Append an 'Integration record: wave2/tailfix' section to .scratch/52-program/issues.md (real clock via `date -Iseconds`, merge commit, final HEAD, verbatim gates, the new worktree path). Commit.",
"REPORT: final HEAD hash, merge status, the wave3 worktree path/HEAD, verbatim gates, any failure. Terse."
].join("\n");

const residualTask = [
"flashdb work package C: separate and remove the residual latency tail. You are the single writer in " + WT3 + ", an isolated worktree on branch wave3/residual based on integrated master. Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; checkpoint the diagnosis BEFORE starting the fix, and the ticket before any long measurement.",
"",
"CONTEXT (verified): WP-B/.scratch/53-compaction-tail-latency/issues.md made load-path compaction latency-safe (background build + lazy apply), cutting rung-200 svc p99 ~11x and write amp 5.8x. But ticket .scratch/49-latency-tail-attribution/issues.md showed that even with compaction AND WAL maintenance removed, the median per-window svc p95 was still 11.8/17.0/39.5/41.1 ms at rungs 50/100/200/400 and no rung sustained on the 10 ms lag ceiling. That RESIDUAL tail is now the only thing between the engine and a genuinely sustained rung, and it is un-attributed: candidates are cold-cache read page-ins (1.64 GB data under a 192 MiB cgroup cap), allocator (the driver calls malloc_trim(0)/trim_heap() once per window at engine/src/bin/flashdb-load.rs:1027, on the request thread), and OS scheduler jitter. The read path uses pread (engine/src/tile.rs read_exact_at), not mmap.",
"",
"PART 1 - MEASURED DIAGNOSIS (commit this before any fix): attribute the residual. Add the minimal per-window instrumentation needed (for example getrusage major/minor faults, /proc/self/io read_bytes, elapsed time of trim_heap, and per-window stall maxima), then run controlled configurations that separate the candidates, e.g.: (a) baseline residual with compaction enabled (the WP-B code) and WAL maintenance on; (b) trim_heap removed or made periodic; (c) a config that changes read behaviour (readahead/O_DIRECT/larger cap) ONLY to separate cold-page-in cost, clearly labelled as a separation probe, not the shipped regime; (d) warm vs cold as a diagnostic only. Name the dominant sub-mechanism with numbers and say what remains unseparated. Do not guess.",
"PART 2 - FIX the dominant mechanism if it is an engine/driver defect (for example a trim_heap stall on the request thread, a read-amplification/page-in storm, or an avoidable synchronous cost), WITHOUT changing the cold regime required by the pre-registration (.scratch/15-objective-v0/pre-registration.md) and WITHOUT editing the pre-registration. If the residual proves to be irreducible OS/allocator jitter, do not fake a fix: characterize it (distribution, per-window), state the smallest honest conclusion, and propose (do not implement) how the pre-registration might treat it, with evidence. Evaluate alternatives and record the chosen one plus rejected alternatives.",
"",
"MEASUREMENT/ACCEPTANCE: use a cp -a copy of /tmp/cold44/per_region-mixed (8.17x) for iteration and confirm the headline on /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Before/after at rungs 50/100/200/400: per-rung per-request svc p50/p95/p99, lag p50/p95/p99, achieved, windows rejected and by which ceiling. ACCEPTANCE: at least the two lowest rungs sustain (lag p99 <= 10 ms, no window rejected) with required maintenance enabled, and the fix does not regress throughput or write amp; OR, if irreducibly blocked, a bounded characterization plus the smallest honest statement of what blocks the metric. Never overstate.",
"",
"TESTS: add tests that fail against the pre-change behaviour where meaningful. Keep all Rust and Python tests green.",
"TICKET .scratch/54-residual-tail/issues.md: contract, diagnosis with numbers, decision + rejected alternatives, change, tests, verbatim gates, before/after table, acceptance verdict, residual/unseparated items. Append a one-paragraph dispatch/contract entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: cargo test --workspace --offline (totals); cargo build --workspace --offline (0 warnings); cd harness && uv run --with pytest pytest -q; git diff --check (silent). Commit; report the hash, the headline before/after deltas, and an honest acceptance verdict."
].join("\n");

const verifyTask = [
"You are an independent verifier for the flashdb residual-tail work package (WP-C), worktree " + WT3 + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long load measurement.",
"Bind to `git rev-parse HEAD` and paste it. Package = the new commits since integrated master (find them with `git log --oneline -8`); scope = engine/src/bin/flashdb-load.rs, engine/src/engine.rs, engine/src/tile.rs, engine/tests/, ticket .scratch/54-residual-tail/issues.md, and the driver telemetry.",
"PASTE exact command output:",
"1. git rev-parse HEAD ; git log --oneline -8 ; git status --porcelain",
"2. cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check",
"FALSIFY: (a) does the specified separation actually isolate the mechanism it claims (e.g. a trim_heap config that also changes the cold regime is confounded)? (b) does the fix remove the stall without disabling required maintenance or touching the pre-registration? (c) are the before/after numbers reproducible from the raw artifacts under /tmp (state which paths) and internally consistent? (d) is the acceptance claim honest, including any residual it does not fix? (e) do the new tests genuinely exercise the change? Report a blocking defect with location, violated criterion and reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge WP-B and create wave3", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}

const residual = await runs.run("residual", { label: "Diagnose and remove the residual tail", agent: "worker", model: STRONG + ":high", cwd: WT3, task: residualTask });
const verify = await runs.run("verify", { label: "Verify the residual-tail package", agent: "worker", model: STRONG + ":high", cwd: WT3, task: verifyTask });

return {
  integrate: { ok: integ.ok, output: integ.output },
  residual: { ok: residual.ok, output: residual.output },
  verify: { ok: verify.ok, output: verify.output }
};
