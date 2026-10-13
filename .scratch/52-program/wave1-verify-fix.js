// Wave1b: independent shell-capable verification, then review corrections, then re-verification.
// The builtin `reviewer` has no shell tool, so the decisive build/test gates could not be
// independently executed. This uses read-only `worker` verifiers (project STATUS section 8:
// "read-only worker-as-verifier that must paste command output and must write CANNOT-VERIFY
// rather than guess"). One writer applies the one P1 and the minor P2 corrections.

const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";

const verifyTask = [
"You are an independent verifier for the flashdb wave1b integrated revision. Repo /home/z/vibe/flashdb. Run 'source $HOME/.cargo/env' first.",
"READ-ONLY: do not create, edit or commit any file. Report only.",
"Bind everything to `git rev-parse HEAD` and paste that output. PASTE the exact command output for every check (the point is independent execution, not narrative). If a check cannot run, write CANNOT-VERIFY with the exact command and why; never guess.",
"",
"Run and paste:",
"  1. git rev-parse HEAD ; git log --oneline -12 ; git status --porcelain",
"  2. source $HOME/.cargo/env; cargo build --workspace --offline            (must be 0 warnings)",
"  3. cargo test --workspace --offline                                       (paste the total passed/failed)",
"  4. cd harness && uv run --with pytest pytest -q -p no:cacheprovider       (paste the summary line)",
"  5. git diff --check                                                       (must be silent)",
"",
"Package-specific checks (paste output):",
"  6. instr: cargo test --offline -p flashdb-engine --bin flashdb-load ; and cd harness && uv run --with pytest pytest -q -p no:cacheprovider tests/test_cold_0b.py",
"  7. coloc: cargo test --offline -p flashdb-engine --test family_colocation",
"  8. decisions: read .scratch/47-latency-trusted-curve/curve.json (compact_bytes_total, compact_merges) and .scratch/STATUS.md section 3 'Whole run' line, and state whether the STATUS reorganisation figure matches the verified 9bae6fe curve (the reviewer claims STATUS has 299,390,660 B / 63 merges but the verified curve has 317,915,511 / 64). Quote both.",
"  9. tenx: du -sb /tmp/cold51/per_region-mixed and compare with the claimed 1,643,925,811 B data; and confirm the smoke evidence file (for example /tmp/cold51/smoke.out) shows 800/800, 0 errors. State the raw numbers.",
"  10. tails: confirm harness/cold49_tail_attribution.py and the ticket exist and that /tmp/cold49/*/curve.json exist; spot-check one ticket number against a curve.json value (state which).",
"",
"Then give a per-package verdict: `VERDICT <package>: PASS|CANNOT-VERIFY` with the decisive evidence. Finally one overall line `OVERALL: PASS|CANNOT-VERIFY`. Report any blocking defect with location, violated criterion and reproduction."
].join("\n");

const fixTask = [
"You are the single writer applying the wave1b review corrections. Repo /home/z/vibe/flashdb, branch master. Run 'source $HOME/.cargo/env' first.",
"TOUCH ONLY the files named below. Do not touch engine/ or harness/. Get the real clock with `date -Iseconds` before editing.",
"Each correction must be verified against its authoritative source before you write it; if a source disagrees with the instruction, record the disagreement instead of guessing.",
"",
"1. .scratch/STATUS.md section 3 'Whole run' line. Verify against .scratch/47-latency-trusted-curve/curve.json (compact_bytes_total, compact_merges) and run.stdout.txt. If the verified 9bae6fe curve gives 317,915,511 B over 64 merges, replace the stale '299,390,660 B over 63 merges' with those values. If the source says otherwise, leave it and record why.",
"2. docs/glossary.md entry 'External baseline'. Make its parenthetical list match the authoritative adopted B1 list in .scratch/15-objective-v0/pre-registration.md and STATUS.md section 7.2 (if B1 names sqlite, postgres, lmdb and rocksdb, list all four; do not invent baselines).",
"3. .scratch/52-program/issues.md integration record. The line 'Final HEAD hash (after merges + control-scripts commit): bef4f14...' is stale: the true final HEAD is later (38e9242 at review recording). Reword so the record distinguishes the 'Integrated revision hash (code under gate): bef4f14...' from the then-current HEAD, without deleting history.",
"4. .scratch/49-latency-tail-attribution/issues.md. (a) Fix the stale source citation for the --compact-bytes 0 -> omit mapping from cold_0b.py:846 to cold_0b.py:883-886. (b) Fix the overstated bound '>= 4.2x' to the value the data supports (the ticket's own rung-400 example gives 590,578/141,402 = 4.18x; keep the claim honest, e.g. '>= 4.17x'). Do not change any measured number.",
"",
"Also `git add` the untracked control script .scratch/52-program/wave1-verify-fix.js so the tree is clean.",
"Then append a short 'wave1b review corrections applied' section to .scratch/52-program/issues.md listing each file, the exact change, and the source used; and commit everything as one commit '52: apply wave1b review corrections'.",
"REPORT verbatim: git rev-parse HEAD, the diffstat, and `git diff --check` (must be silent). Terse."
].join("\n");

const reverifyTask = [
"You are a fresh independent verifier of the wave1b review corrections. Repo /home/z/vibe/flashdb. Run 'source $HOME/.cargo/env' first. READ-ONLY: do not create, edit or commit any file.",
"Run `git rev-parse HEAD` and bind to it. Find the commit titled '52: apply wave1b review corrections' (git log --oneline -6) and inspect `git show --stat <that commit>`.",
"Verify, pasting raw output:",
"  1. STATUS.md section 3 reorganisation figure now equals .scratch/47-latency-trusted-curve/curve.json compact_bytes_total/compact_merges (quote both).",
"  2. docs/glossary.md 'External baseline' list matches the B1 baselines in .scratch/15-objective-v0/pre-registration.md (quote both).",
"  3. .scratch/49-latency-tail-attribution/issues.md no longer cites cold_0b.py:846 and its tail multiple is not overstated (quote the line).",
"  4. The corrected tree still passes: cd harness && uv run --with pytest pytest -q -p no:cacheprovider (paste summary) and git diff --check (silent).",
"  5. No measured number in any ticket was silently altered by the correction commit: inspect the diff of the commit and confirm it changed only wording/citations/reorg-figure-as-instructed.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const verify = await runs.run("verify", { label: "Independently execute integration gates", agent: "worker", model: STRONG, cwd: REPO, task: verifyTask });

const fix = await runs.run("fix", { label: "Apply wave1 review corrections", agent: "worker", model: STRONG, cwd: REPO, task: fixTask });

const reverify = await runs.run("reverify", { label: "Re-verify review corrections", agent: "worker", model: STRONG, cwd: REPO, task: reverifyTask });

return {
  verify: { ok: verify.ok, output: verify.output },
  fix: { ok: fix.ok, output: fix.output },
  reverify: { ok: reverify.ok, output: reverify.output }
};
