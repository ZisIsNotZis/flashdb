// Wave 1b integration + independent review.
// Stage 1: one integrator worker (single writer) merges the five wave1 branches into master
//          and runs the combined gates. Stage 2: six fresh read-only reviewers on the integrated
//          revision (5 packages + 1 integration). Stage 3: one scribe worker records the review
//          outcomes in the program ticket. Writers never run concurrently.

const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";

const integrateTask = [
"You are the integrator for the flashdb wave1b resume plan. Repo /home/z/vibe/flashdb, branch master, currently at 7fc453a (a one-file program-ticket commit on top of base 93740b6). Run 'source $HOME/.cargo/env' first.",
"AUTHORITY: you are the single writer on master for this step. Do not use git rebase, git reset --hard, or history rewriting. Do not modify repo files outside what is stated below.",
"",
"MERGE these five branches, each of which is exactly one commit based on 93740b6 and touches a disjoint file set: wave1/instr (8b6c4d1), wave1/coloc (b515f7d), wave1/tenx (99fe90b), wave1/decisions (7589beb), wave1/tails (f3cf77c). Use 'git merge --no-ff <branch>' in that order. If a conflict appears, resolve it minimally and correctly (the file sets should be disjoint) and record exactly what you resolved.",
"Then commit the untracked control scripts .scratch/52-program/wave1b.js and .scratch/52-program/wave1-integrate-review.js (if present) with message '52: wave1b orchestration control scripts'.",
"",
"COMBINED GATES - run and capture verbatim into the ticket:",
"  source $HOME/.cargo/env; cargo build --workspace --offline   (must finish with 0 warnings)",
"  cargo test --workspace --offline                              (report the test-result lines and totals)",
"  cd harness && uv run --with pytest pytest -q                  (report the summary line)",
"  git diff --check                                              (must be silent)",
"",
"RECORD in .scratch/52-program/issues.md (append a new section, do not rewrite existing content): get the real clock with `date -Iseconds`; the five merge-commit hashes; the final HEAD hash; the verbatim gate results; any conflict resolution; and the integrated revision hash. Then commit that ticket update.",
"REPORT: final HEAD hash, one line per merge, the verbatim gate results, and any failure or conflict. Terse."
].join("\n");

const reviewCommon = [
"You are an independent reviewer in the flashdb project. Repo /home/z/vibe/flashdb (the integrated master). Run 'source $HOME/.cargo/env' first.",
"READ-ONLY: do not create, edit, or commit any file. Do not repair anything - report only. Inspect final artifacts and evidence, not completion claims.",
"Start with `git rev-parse HEAD` and bind your verdict to that hash. The packages were merged from base 93740b6; their code commits are named in your scope.",
"Attempt to FALSIFY acceptance: look for missing scope, regressions, stale or fabricated evidence, and unsupported claims. Paste the exact command output backing each finding. If you cannot verify something, write CANNOT-VERIFY and say why; never guess.",
"Use `cd harness && uv run --with pytest pytest -q -p no:cacheprovider <path>` for Python. Avoid running heavy `cargo test` unless it is the decisive check for your scope; if you do, note the concurrency.",
"End with a verdict line: `VERDICT: PASS` or `VERDICT: FAIL` or `VERDICT: CANNOT-VERIFY`, followed by numbered findings (location, violated criterion, reproduction/evidence), separating blocking from non-blocking."
].join("\n");

const instrReview = [
reviewCommon,
"SCOPE: package `instr` - code commit 8b6c4d1 on top of 93740b6. Files: engine/src/bin/flashdb-load.rs, harness/flashdb_harness/cold_0b.py, harness/tests/test_cold_0b.py, ticket .scratch/48-latency-instrumentation/issues.md.",
"ACCEPTANCE: (1) five new per-window fields exist with the claimed names (groups, mean_group_size, max_group_size, min_lag_us, groups_over_service_ceiling) and actually reach the --metrics-json window line; (2) cold_0b.py parses them and the per-rung report shows them; (3) the three new tests genuinely fail against the pre-change code and pass now; (4) the durable invariants from commits 744caaa and 965adf4 are preserved (bounded rung rate with 2 percent tolerance, no-straddle rung attribution, trailing window flush, per-window invalid/reject_reason, early_dispatches, group_elapsed_us).",
"REQUIRED CHECKS: inspect the driver diff and the JSON emission path; run `cd harness && uv run --with pytest pytest -q -p no:cacheprovider tests/test_cold_0b.py` and report the result.",
"BLOCKING: a claimed field that does not reach the JSON, a broken invariant, or a test that does not actually exercise the change."
].join("\n");

const colocReview = [
reviewCommon,
"SCOPE: package `coloc` - code commit b515f7d on top of 93740b6. Files: engine/src/keys.rs, engine/src/engine.rs, engine/src/tile.rs, engine/tests/family_colocation.rs, ticket .scratch/50-family-colocation/issues.md.",
"ACCEPTANCE: (1) the DEFAULT encoding is byte-identical to before (all pre-existing tests pass unchanged); (2) an explicit selector enables entity-first encoding; (3) under entity-first, one entity's P|X, U|X and R|X keys land in the same tile (the test must assert tile id or fence ranges, not merely ordering) for a range of entity values; (4) uniqueness, reverse lookup, duplicate-owner and reopen-recovery contracts still hold; (5) strict monotone keyspace ordering is preserved so tiles remain valid fenced ranges; (6) no existing key-order assumption in the engine or tests is silently broken.",
"REQUIRED CHECKS: inspect keys.rs/engine.rs/tile.rs diffs and the decision note; run `cargo test --offline -p flashdb-engine --test family_colocation` and report it. If affordable, also run `cargo test --workspace --offline`.",
"BLOCKING: default behavior changed, an encoding that breaks monotonicity/uniqueness/reverse contracts, or a co-location test that passes for the wrong reason."
].join("\n");

const tenxReview = [
reviewCommon,
"SCOPE: package `tenx` - measure-only commit 99fe90b on top of 93740b6. Files: harness/build_cold_51.py, ticket .scratch/51-instance-10x/issues.md. Instance described at /tmp/cold51/per_region-mixed.",
"ACCEPTANCE: (1) the claimed ratio is arithmetically correct - data_bytes / regime memory cap >= 10x (claimed data 1,643,925,811 B, regime cap 128 MiB, ratio 12.25x); (2) the instance actually exists and its metadata/size matches the claim (inspect it; do not rerun the full build); (3) the smoke result (800/800, 0 errors, cache-evicting at the cap) is supported by recorded evidence in the ticket; (4) the build-cap vs regime-cap separation is honestly justified; (5) the build script is coherent and could reproduce the instance.",
"REQUIRED CHECKS: stat/inspect /tmp/cold51/per_region-mixed and any run directory; read the ticket evidence. A single short smoke is acceptable but not required.",
"BLOCKING: a ratio that does not actually meet 10x, a missing/inconsistent instance, or unsupported smoke/readiness claims."
].join("\n");

const decisionsReview = [
reviewCommon,
"SCOPE: package `decisions` - docs commit 7589beb on top of 93740b6. Files: .scratch/15-objective-v0/pre-registration.md, .scratch/STATUS.md, docs/6_ARCHITECTURE.md, docs/glossary.md.",
"ACCEPTANCE: the four adopted decisions are recorded with their reasoning and are CONSISTENT across all touched documents: (1) the primary validity rule is the lag ceiling at the pre-registration's 10 ms, service latency reported separately; (2) T5/T6/T7 are adopted (not 'awaiting sign-off') with the superseded no-absolute-throughput clause removed and the sign-off list updated; (3) the 10x floor is a requirement, not a qualification, and sub-floor verdicts are provisional; (4) the tile-cap defect is fixed on both sides. Also: the first valid primary-metric evaluation (commit 9bae6fe) is recorded as NO sustained rung, latency-tail failure, and T1/T2/T4 underived.",
"CHECK FOR: stale or contradictory statements left in place, process narrative in fact documents, duplicated facts with different homes, and any claim not matching the committed STATUS/pre-registration. Cross-check docs/2_GOAL.md, which the base 93740b6 already touched.",
"BLOCKING: a contradiction with a higher-authority document, or a decision recorded with the wrong content."
].join("\n");

const tailsReview = [
reviewCommon,
"SCOPE: package `tails` - measure-only commit f3cf77c on top of 93740b6. Files: harness/cold49_tail_attribution.py, ticket .scratch/49-latency-tail-attribution/issues.md.",
"ACCEPTANCE: the ticket's headline attribution - 'dominant mechanism is compaction actuation' - is actually supported by the per-configuration, per-rung data: (1) the configurations are described with exact commands and use a frozen driver copy; (2) the per-rung service/lag tables and deltas are present and internally consistent; (3) group head-of-line, rotation/fsync and page-cache pressure are each shown NOT to be dominant by a configuration that separates them; (4) the residual (unseparated) tail is honestly labelled; (5) no cause is asserted that the configurations do not separate.",
"REQUIRED CHECKS: read the script and ticket; check the arithmetic/consistency of the tables; if cheap, re-run ONE configuration to confirm the tool works (note concurrency).",
"BLOCKING: an attribution contradicted by the data, a config that does not actually isolate its variable, or fabricated/missing numbers."
].join("\n");

const integrationReview = [
reviewCommon,
"SCOPE: the WHOLE integrated revision (all five commits merged into master, base 93740b6).",
"ACCEPTANCE: the combined revision (1) builds and passes the full Rust workspace tests with zero warnings and the full Python suite; (2) the five packages are mutually consistent - in particular the instrumented driver and the entity-first key encoding (default family-first) coexist without changing default behavior; (3) the lane tickets' claimed revisions match what is actually in the tree; (4) documentation matches code; (5) no cross-package regression was introduced by the merges.",
"REQUIRED CHECKS: `git log --oneline --graph -15`; `git diff --stat 93740b6..HEAD`; run `cargo build --workspace --offline`, `cargo test --workspace --offline`, and `cd harness && uv run --with pytest pytest -q -p no:cacheprovider`. Note that at most one other reviewer may run cargo concurrently (cargo serializes on the target lock).",
"BLOCKING: a build/test/reproducibility failure at the integrated revision, or a ticket claiming a revision that is not in the tree."
].join("\n");

const integ = await runs.run("integrate", { label: "Merge wave1 branches and run combined gates", agent: "worker", model: STRONG, cwd: REPO, task: integrateTask });
if (!integ.ok) {
  return { integrate: { ok: false, output: integ.output } };
}

const reviews = await runs.all([
  { key: "rev-instr", label: "Review latency instrumentation", agent: "reviewer", model: STRONG, cwd: REPO, task: instrReview },
  { key: "rev-coloc", label: "Review family co-location encoding", agent: "reviewer", model: STRONG, cwd: REPO, task: colocReview },
  { key: "rev-tenx", label: "Review 10x instance evidence", agent: "reviewer", model: STRONG, cwd: REPO, task: tenxReview },
  { key: "rev-decisions", label: "Review adopted decision records", agent: "reviewer", model: STRONG, cwd: REPO, task: decisionsReview },
  { key: "rev-tails", label: "Review latency-tail attribution", agent: "reviewer", model: STRONG, cwd: REPO, task: tailsReview },
  { key: "rev-integration", label: "Review integrated revision", agent: "reviewer", model: STRONG, cwd: REPO, task: integrationReview }
]);

const bundle = reviews.map(function (r) {
  return "### " + r.key + " (run " + (r.ok ? "ok" : "FAILED") + ")\n" + r.output;
}).join("\n\n");

const scribeTask = [
"You are the scribe for the flashdb wave1b review. Repo /home/z/vibe/flashdb, branch master. Get the real clock with `date -Iseconds` first.",
"TOUCH ONLY: append a new section to .scratch/52-program/issues.md titled 'wave1b independent review outcomes', citing the integrated HEAD via `git rev-parse HEAD`, with, for each review: its verdict line and its numbered blocking findings (if any). Then commit with message '52: record wave1b independent review outcomes'. Do not modify any other file. Do not repair code.",
"Here are the six reviewer results verbatim:",
"",
bundle
].join("\n");

const scribe = await runs.run("scribe", { label: "Record review verdicts in the program ticket", agent: "worker", model: STRONG, cwd: REPO, task: scribeTask });

return {
  integrate: { ok: integ.ok, output: integ.output },
  reviews: reviews.map(function (r) { return { key: r.key, ok: r.ok, output: r.output }; }),
  scribe: { ok: scribe.ok, output: scribe.output }
};
