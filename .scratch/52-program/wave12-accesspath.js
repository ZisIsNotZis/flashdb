// Phase 12: give the thesis its missing axis -- a LEARNED SECONDARY ACCESS PATH (the
// placement/co-location surface docs/learning.md says must exist). Measure deterministic
// headroom first (project rule: never add an axis by intuition); implement only on GO;
// then re-evaluate T1/T2/T3 under the author's adopted SS7.1 lag-only primary view.
const STRONG = "deepseek/deepseek-v4-pro";
const REPO = "/home/z/vibe/flashdb";
const W = "/home/z/vibe/worktrees/flashdb";
const WAX = W + "/wave12-accesspath";
const WEV = W + "/wave13-eval";

const gates = "cargo build --workspace --offline (0 warnings); cargo test --workspace --offline (totals); cd harness && uv run --with pytest pytest -q; git diff --check (silent)";

const setupTask = [
"You are the integrator. Repo /home/z/vibe/flashdb, master HEAD is the current SSOT-refresh HEAD (verify with git log). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. Create the axis worktree: `git worktree add -b wave12/accesspath " + WAX + " master`. Verify and report its HEAD.",
"2. Confirm master is clean; report the HEAD and the last 5 commits.",
"REPORT: master HEAD, worktree path/HEAD."
].join("\n");

const axisTask = [
"flashdb work package M: the axis the thesis is missing -- a LEARNED SECONDARY ACCESS PATH. Single writer in " + WAX + " (isolated worktree, branch wave12/accesspath, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit incrementally; COMMIT THE STAGE-1 DESIGN BEFORE building anything.",
"",
"CONTEXT (all measured/verified -- read .scratch/STATUS.md, docs/learning.md, and tickets 29, 58, 60, 62 first). The engine-side latency work is done (rung 50 lag-sustained on point-lookup workloads). The WP-K re-evaluation (ticket 62) measured that T1 is UNDERIVABLE because the win workload's `movements_scan` (`find StockMovement where order_no=X`) has no `order_no` index, so it runs a full-entity primary scan of 6 tiles / ~4.8 MB, JSON-parsing every movement doc on the single pacer (~89-94% of wall time). Both arms fail identically, so the pre-registered win workload offers NO layout-dependent headroom to the current learner action space (merge threshold / page size / bloom bits), and the learner churns (T2 partial, T3 FAIL). docs/learning.md already states the learned surface must be able to act on PLACEMENT/co-location and that a new axis is added only after its headroom is measured with deterministic counts, never by intuition. The author has granted autonomous advancement; you are authorized to choose and record the justified in-scope direction. Do NOT edit .scratch/15-objective-v0/pre-registration.md.",
"",
"STAGE 1 -- DETERMINISTIC HEADROOM (bounded; do this first and commit the design+measurement before implementing): measure the win scan's deterministic cost (blocks read, docs scanned, bytes, CPU parse time, per-request cost) and the headroom a SECONDARY ACCESS PATH keyed on the observed predicate (`order_no` -> StockMovement refs) would remove: i.e. how many dependent block reads / docs / ns per request it eliminates, and its write + space cost. Use the engine's own counters and the existing harness instrumentation; run deterministic counts, not wall-clock guessing. Record a GO/NO-GO with the numbers. If the deterministic headroom is not material, record NO-GO and STOP -- that is a valid outcome (the axis has no headroom, and the honest result is that the pre-registered thesis is not supported on this workload).",
"",
"STAGE 2 -- IMPLEMENT, ONLY IF STAGE 1 IS GO: add the secondary access path to the engine and expose it as a LEARNER ACTION in the harness. Design it as a physical-layout object built and maintained OFF the request path (e.g. a learned auxiliary tile/region family, maintained by the compaction/actuator path from the WAL), used by the authoritative read path ONLY when present and valid. Hard constraints: (i) layout is never on the correctness path -- a missing/stale index must fall back to the existing scan and never return a wrong answer; (ii) crash-consistent (reuse the WAL/manifest/tile machinery; a covering fdatasync before any ack that depends on it); (iii) it must not add work to the single-writer request path; (iv) newest-first memtable-over-tile semantics preserved; (v) bounded memory. Expose it as a learner-selectable action (which observed predicate(s) to serve / co-locate), scored by the existing unit-anchored, reorg-priced reward. Keep the change bounded -- do not rewrite the engine. If the design cannot be made correct-and-bounded with the existing machinery, say so precisely and record the blocking constraint rather than faking it.",
"",
"MEASUREMENT: cp -a copies of /tmp/cold51/per_region-mixed (12.25x). Build once, copy the binary, run only that copy, one process at a time. Harness `cold_0b.py`. Before/after on `--workloads win --arms declared learned` (and static-derived if cheap). Report per-rung achieved_rps, lag-rej, svc-rej, lag p99, reorg bytes, and the breach attribution. Box is shared/noisy -- record load and repeat a run for a noise band.",
"JUDGING: the author's ADOPTED reading is SS7.1 lag-only primary (a window is invalid iff its lag p99 exceeds the lag ceiling; service reported separately). Report BOTH that and the pre-registration's both-ceilings view. Do not edit the pre-registration.",
"ACCEPTANCE: Stage 1's deterministic headroom is measured and recorded (GO/NO-GO). If GO and implemented: all invariants preserved (state how each is preserved), all tests green, the axis is genuinely learnable (the learner can select it), and a measured before/after on the win workload shows material headroom (state the number and the seed count), OR an honest partial with the exact remaining cause. Never overstate.",
"TESTS: add tests that fail against the pre-change behaviour where meaningful (including a fallback/correctness test for a missing/stale access path). Keep all Rust and Python tests green.",
"TICKET .scratch/64-learned-access-path/issues.md: contract, Stage-1 deterministic headroom table, GO/NO-GO, design + rejected alternatives, invariants argument, change, tests, verbatim gates, before/after table, both judging views, acceptance verdict, residual items. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash, the Stage-1 headroom numbers, and the honest verdict."
].join("\n");

const axisVerifyTask = [
"You are a fresh independent verifier for WP-M, worktree " + WAX + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Scope: the new engine access-path code, engine/tests/, the harness learner action, harness/tests/, and ticket .scratch/64-learned-access-path/issues.md.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the Stage-1 headroom genuinely DETERMINISTIC and reproducible from committed artifacts/counters (not wall-clock guessing), and does the GO/NO-GO follow from it? (b) is layout strictly OFF the correctness path -- prove a missing/stale/invalid access path falls back to the existing scan and can never return a wrong answer (paste the fallback code and any test)? (c) crash-consistency and durability ordering preserved? (d) does the access path add zero work to the single-writer request path (or is the added cost honestly measured)? (e) is the axis genuinely learner-selectable, and are the before/after win figures reproducible from the raw artifacts (state paths, seed count, noise band)? (f) both judging views reported and the pre-registration untouched? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const integTask = [
"You are the integrator. Repo /home/z/vibe/flashdb (master clean). Run 'source $HOME/.cargo/env' first. Single writer; no rebase/reset/history rewrite.",
"1. `git merge --no-ff wave12/accesspath` into master. Resolve conflicts minimally and record them.",
"2. Create the evaluation worktree from integrated master: `git worktree add -b wave13/eval " + WEV + " master`. Report its HEAD.",
"3. COMBINED GATES verbatim: " + gates + ".",
"4. Append an 'Integration record: wave12/accesspath' section to .scratch/52-program/issues.md (real clock, commit, gates). Commit.",
"REPORT: master HEAD, merge result, wave13 worktree path/HEAD, verbatim gates."
].join("\n");

const evalTask = [
"flashdb work package N: the post-axis T1/T2/T3 evaluation under the author's adopted SS7.1 lag-only primary reading. Single writer in " + WEV + " (isolated worktree, branch wave13/eval, based on integrated master). Run 'source $HOME/.cargo/env' first. Do NOT cd to /home/z/vibe/flashdb. Commit the plan before any long run.",
"",
"CONTEXT: read .scratch/STATUS.md and tickets 62 (WP-K re-run) and 64 (WP-M access path). If WP-M recorded NO-GO, this package instead documents the NO-GO conclusion and re-confirms the ticket-62 numbers on the integrated revision; if WP-M implemented the axis, this is a fresh full evaluation.",
"",
"RUN: instance `/tmp/cold51/per_region-mixed` (12.25x; `cp -a` copy). Arms declared + learned (+ static-derived if cheap). Workloads win (T1) and adversarial-uniform (T2). Full ladder 50/100/200/400. At least 3 independent access seeds per (arm, workload) plus a repeat for a noise band; more if wall-clock allows. One process at a time under the memory cap; record box load at every run boundary.",
"",
"REPORT, honestly:",
"1. T1 (win): does learned beat declared by >15% in sustained load level (lag-only primary)? Is the win workload lag-sustained for any arm now? Give numbers + noise band + seed count; label underivable if still not measurable.",
"2. T2 (adversarial): does learned stay within declared's reorg band (<=1.5x) and remain rung-50 lag-sustained like declared?",
"3. T3: learned/declared reorg ratio; does the win seed-44-class blowup recur?",
"4. Both judging views (lag-only SS7.1 primary AND both-ceilings); flag the harness's both-ceilings 'sustain' artifact if it is a 0/0 disguise.",
"5. Any NEW defect the axis introduced; the slice-39 zero-gain status.",
"6. Wall-clock per seed and what a full 10-seed pre-registered campaign costs.",
"TICKET .scratch/65-eval-post-axis/issues.md: contract, exact commands, per-arm/rung tables, both judging views, T1/T2/T3 verdicts with seed counts and noise band, residual gaps, cost. Append a one-paragraph dispatch entry to .scratch/52-program/issues.md.",
"GATES, report verbatim: " + gates + ". Commit; report hash and the honest verdict (label pilot-grade where true; do NOT claim a 10-seed pre-registered verdict)."
].join("\n");

const evalVerifyTask = [
"You are a fresh independent verifier for WP-N, worktree " + WEV + ". Run 'source $HOME/.cargo/env' first. READ-ONLY: create/edit/commit nothing; do not re-run the long measurement.",
"Bind to `git rev-parse HEAD` (paste it); list new commits with `git log --oneline -8`. Confirm the measured revision contains WP-M's access-path change (paste evidence) or, if WP-M was NO-GO, that it honestly re-confirms on the integrated revision. Scope: ticket .scratch/65-eval-post-axis/issues.md, the committed run artifacts, and the claimed code revision.",
"PASTE exact output for: " + gates + ".",
"FALSIFY: (a) is the measured revision the one that actually contains the WP-M axis (or the honest NO-GO base)? (b) are the T1/T2/T3 verdicts computed from raw committed artifacts with the true seed count and a measured noise band, under BOTH judging views? (c) does the evaluation avoid claiming a pre-registered 10-seed verdict it did not earn, and is the pre-registration untouched? (d) do the numbers support the ticket's direction claim? Report blocking defects with location, violated criterion, reproduction.",
"End with `VERDICT: PASS|FAIL|CANNOT-VERIFY` and numbered findings (blocking vs non-blocking)."
].join("\n");

const setup = await runs.run("setup", { label: "Create the access-path worktree", agent: "worker", model: STRONG, cwd: REPO, task: setupTask });
if (!setup.ok) return { setup: { ok: false, output: setup.output } };
const axis = await runs.run("axis", { label: "WP-M: measure headroom and add the learned access path", agent: "worker", model: STRONG + ":high", cwd: WAX, task: axisTask });
const axisVerify = await runs.run("vaxis", { label: "Verify WP-M", agent: "worker", model: STRONG + ":high", cwd: WAX, task: axisVerifyTask });
const integ = await runs.run("integ", { label: "Integrate wave12 and create wave13", agent: "worker", model: STRONG, cwd: REPO, task: integTask });
if (!integ.ok) return { setup: { ok: setup.ok, output: setup.output }, axis: { ok: axis.ok, output: axis.output }, axisVerify: { ok: axisVerify.ok, output: axisVerify.output }, integ: { ok: false, output: integ.output } };
const evalN = await runs.run("eval", { label: "WP-N: post-axis T1/T2/T3 evaluation", agent: "worker", model: STRONG + ":high", cwd: WEV, task: evalTask });
const evalVerify = await runs.run("veval", { label: "Verify WP-N", agent: "worker", model: STRONG + ":high", cwd: WEV, task: evalVerifyTask });
return {
  setup: { ok: setup.ok, output: setup.output },
  axis: { ok: axis.ok, output: axis.output },
  axisVerify: { ok: axisVerify.ok, output: axisVerify.output },
  integ: { ok: integ.ok, output: integ.output },
  evalN: { ok: evalN.ok, output: evalN.output },
  evalVerify: { ok: evalVerify.ok, output: evalVerify.output }
};
