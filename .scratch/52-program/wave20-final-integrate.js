// Final: merge the verified SSOT refresh (wave20/ssot) into master and run the combined gates.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const task = [
"You are the integrator. Repo /home/z/vibe/flashdb (master HEAD ~a8094be). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave20/ssot` into master (carries the verified SSOT refresh: .scratch/STATUS.md rewrite + ticket 74; docs only). Resolve minimally and record any conflict.",
"2. COMBINED GATES verbatim: " + gates + ".",
"3. Append a short 'Integration record: wave20/ssot' section to .scratch/52-program/issues.md (real clock, merge commit, final master HEAD, verbatim gates). Commit any untracked .scratch/52-program/*.js control scripts too.",
"REPORT: final master HEAD, merge result, verbatim gates, and `git log --oneline -5`."
].join("\n");

return runs.run("integ", { label: "Merge wave20/ssot into master", agent: "worker", model: STRONG, cwd: REPO, task: task });
