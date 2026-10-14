// wave24: refresh the SSOT (.scratch/STATUS.md) to the FINAL post-WP-79 state, then fresh-verify.
// Docs-only. This is the current-facts fact document (no history, one home per fact).
const STRONG = "deepseek/deepseek-v4-pro";
const B = "/home/z/vibe/flashdb";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const scribeTask = [
"flashdb SSOT refresh (docs-only) in " + B + " on master (HEAD will be reported by you; the final integrated master after the WP-79 merge). Run 'source $HOME/.cargo/env' first.",
"",
"TASK: rewrite .scratch/STATUS.md IN PLACE so it is the current single source of truth for the project state as of NOW. It is a FACT document: current facts only -- NO dated narrative, NO changelog, NO 'was A, later B', no errata, no dashboard timestamps. Rewrite in place; do not append history. The change/process history lives in the tickets, not here.",
"",
"FACTS TO REFLECT (verify each against the committed tickets/artifacts before writing; do not trust this prose blindly):",
"1. Milestone state: the pre-registered T1/T2/T3/T4 re-derivation on the >=10x instance is COMPLETE. Bound revision = the final integrated master (record the actual `git rev-parse HEAD`).",
"2. Instance: the 12.25x instance = 1,643,925,811 B / 128 MiB cap = >=10x data:memory (the earlier 'below the 10x floor' harness wording is a KNOWN harness bug only if not yet fixed -- check and state the current behaviour honestly).",
"3. Final measured verdicts (pilot-grade: 3 seeds 42/43/44 + seed-42 repeat; NOT a 10-seed verdict) from ticket 79 (.scratch/79-final-eval/issues.md):",
"   - Primary (author-adopted SS7.1 lag-only): rung 50 NOT lag-sustained (static-derived 2-3/40 lag-rejected, learned 3-6/40, declared 39-40/40); rung 100 lag-sustained (static-derived 4/4 seeds, learned 3/4); rung 200 lag-sustained (static-derived 4/4, learned 2/4).",
"   - T1 SUPPORTED: learned vs declared aggregate achieved throughput +41.3% to +50.7% (median +47.6%), outside the noise band.",
"   - T2 SUPPORTED: adversarial-uniform reorg median 0.933x (max 0.995x) <= 1.5x; 0 index builds on the 0-scan region.",
"   - T3 SUPPORTED: win reorg median 0.767x (max 0.941x) <= 3x; no single-region blowup (U|Stock 5.23 MB / 1 merge).",
"   - T4 (write-spike drift) EVALUABLE and measured NOT SUPPORTED: a real ~8-10% capacity drop, but NEITHER arm recovers and learned ~= static post-drift; the write-spike floor is policy-independent.",
"   - Harness both-ceilings view: sustained=false on all win/adversarial rows -- the service ceiling is device-bound; SS7.1 lag-only is the author-adopted primary reading (the SS7.1 primary-vs-service-ceiling reconciliation remains an OPEN AUTHOR ITEM).",
"4. The two physical-layout mechanisms now in the tree (current state): the learner-selectable order_no secondary access path (index axis, demand-gated one-shot sticky); the tile cap raised/configured (MAX_TILES 4096, Engine::set_max_tiles, flashdb-load --max-tiles); the write-spike drift shape (--drift-shape); the compaction build's periodic sync_data cadence (COMPACT_TILE_SYNC_EVERY_BYTES).",
"5. Honest limit: the rung-50 residual is the compaction build/device fdatasync tail; the shared device's own latency floor sits above the 10 ms ceiling (proven: pacer WAL fdatasync tails 77-166 ms even with NO build concurrent). This is device-bound, not a learner gap.",
"6. Evidence quality: tests 308 Rust / 146 Python, 0 warnings. All measurement cold (cp -a instance, one process at a time under systemd-run MemoryMax, flock-serialized).",
"",
"ACCOUNTING: the reorg metric uses summary.compact_bytes_total (ticket 72 convention; the final drained merge is included; per-region sums undercount).",
"OPEN AUTHOR ITEMS (keep visible; do NOT resolve silently): (a) SS7.1 primary-vs-service-ceiling reconciliation; (b) the 10-seed / 15% / 5% / 3x thresholds 'as written' vs the shared-box reality; (c) T5-T7 baseline harness -- postgres is NOT installed and installing system deps needs user authority.",
"",
"Do NOT edit .scratch/15-objective-v0/pre-registration.md. GATES, report verbatim: " + gates + ". Append a short 'SSOT refresh (WP-79)' entry to .scratch/52-program/issues.md. Commit; report the new HEAD and paste the final STATUS.md."
].join("\n");

const verifyTask = [
"You are a fresh independent verifier of the SSOT refresh in " + B + ". Run 'source $HOME/.cargo/env'. READ-ONLY: write/commit nothing.",
"Bind to `git rev-parse HEAD` (paste it) and `git log --oneline -3`.",
"FALSIFY: (a) does every numeric claim in .scratch/STATUS.md match the committed tickets/raw JSON, in particular .scratch/79-final-eval/issues.md (run the numbers yourself with python3/jq where possible: T1 median, T2 median, T3 median, rung-50/100/200 lag-rej, T4 pre/post)? (b) is STATUS.md a pure current-facts document (no dated narrative/changelog/'was A later B'/timestamps)? (c) are the open author items (SS7.1 reconciliation, 10-seed thresholds, T5-T7 postgres) present and NOT falsely resolved? (d) is the instance honestly >=10x (1,643,925,811 B / 128 MiB)? (e) pre-registration untouched?",
"PASTE exact output for: " + gates + ". Also print the full .scratch/STATUS.md so the review is inspectable.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const scribe = await runs.run("scribe", { label: "SSOT refresh to final state", agent: "worker", model: STRONG + ":high", cwd: B, task: scribeTask });
const verify = await runs.run("verify", { label: "Verify the SSOT refresh", agent: "worker", model: STRONG + ":high", cwd: B, task: verifyTask });
return { scribe: { ok: scribe.ok, output: scribe.output }, verify: { ok: verify.ok, output: verify.output } };
