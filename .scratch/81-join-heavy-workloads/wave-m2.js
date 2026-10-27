const repo = "/home/z/vibe/flashdb";

const buildTask = `You are the build worker for milestone M2 (join-heavy workload descriptors) of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m2-join-heavy (create it from the current HEAD if it does not exist; work only on it). Do not push.

READ FIRST: .scratch/81-join-heavy-workloads/issues.md (your acceptance), docs/0_ROADMAP.md M2, docs/9_IMPLEMENTATION.md (the four workload hypotheses), .scratch/29-workload-shapes/issues.md (the finding that the current generator has no join-heavy shapes), harness/flashdb_harness/generator.py, seed.py, load_profile.py, and the engine request dialect in engine/src/request.rs.

GOAL: add the four join-heavy shapes to the deterministic generator and measure their structural descriptors. This slice does NOT execute the engine and does NOT implement L2.

REQUIRED:
1. Add four shapes to harness/flashdb_harness/generator.py, reusing the existing Rng/Zipf only (no random module, no float accumulation in the data path), each emitting a request in the existing JSONL dialect:
   - star_fanout: order line -> product/supplier/lead-time fan-out (replenishment/days-of-cover).
   - multi_hop: lot -> products -> order_lines -> orders -> customers -> returns (recall/traceability).
   - as_of: fact joined to a time-versioned SCD-2 dimension on (sku, t).
   - anti_join: demanded SKUs with no supply movement (lost sales).
2. A deterministic corpus: same seed -> byte-identical trace; write the trace and its sha256 into the ticket; add pytest tests proving determinism for every new shape and that the existing corpus hash is unchanged.
3. A descriptor measurement (a committed script and its output) per shape, each number naming its method: fan-out width, multi-hop depth, dependent-read share, and co-location potential (define the method concretely: e.g. the share of dependent reads whose target rows would share a region/tile under a stated co-location key, versus the share whose cost is set by per-region parameters).
4. Append a "Build record" to .scratch/81-join-heavy-workloads/issues.md with the exact commands, the corpus sha256, the descriptor table, and the pytest summary. Commit on wave/m2-join-heavy; tree clean.

Do NOT invent a descriptor the generator does not emit. If a shape needs an engine capability that does not exist, record it as unmeasured and say why. Do not change the pre-registration, the roadmap thresholds, or engine code.

Report: the branch/commit, the corpus sha256, the descriptor table, the pytest summary, and what is unmeasured.`;

const analyzeTask = `You are the analysis worker for milestone M2 of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m2-join-heavy. Do not push. Do not change engine code or the pre-registration.

READ: .scratch/81-join-heavy-workloads/issues.md (including the Build record and descriptor table), docs/9_IMPLEMENTATION.md, .scratch/29-workload-shapes/issues.md, the pre-registration's layout space and "What is NOT claimed", and the committed descriptor output.

TASK: decide, with evidence and honest limits, whether the per-region layout space (page size, bloom bits, compaction trigger, per-tile parameters) can move the cost of each join-heavy shape, or whether the cost is dominated by where related rows live (the L2 co-location question). Then write .scratch/81-join-heavy-workloads/report.md:
- Conclusion first: the recommendation (per-region parameters sufficient for shape X / L2 required for shape Y / not determinable), and what would falsify it.
- The measured descriptor table, each number with its method.
- The reasoning that connects each descriptor to the layout axis, using the measured headroom from ticket 29 and a stated model; state the model's calibration limit explicitly.
- What is unverified: anything needing a real engine run, a calibrated simulator, or an L2 implementation.
- No claim about L2 beyond the analysis, and no change to the objective.

Also update docs/0_ROADMAP.md's M2 status line with the recommendation (one line) and commit on wave/m2-join-heavy. Report the commit and the decisive recommendation.

If the descriptors are insufficient to decide, say so and name the smallest measurement that would decide it, rather than inventing a conclusion.`;

const reviewTask = `You are a fresh independent reviewer for milestone M2 of flashdb.

Repo /home/z/vibe/flashdb, branch wave/m2-join-heavy. Read-only; do not edit.

Read .scratch/81-join-heavy-workloads/{issues.md,report.md}, the generator changes, the descriptor script and its committed output, docs/9_IMPLEMENTATION.md, .scratch/29-workload-shapes/issues.md, and docs/0_ROADMAP.md M2. Try to falsify:
1. Are the four shapes genuinely deterministic (same seed -> same bytes) and in the existing dialect? Do the tests prove it, and is the existing corpus hash unchanged?
2. Is each descriptor measured, with a named method, and not invented where the generator does not emit it?
3. Does the recommendation follow from the descriptors and the cited measured headroom, with the model's calibration limit and the unverified items stated?
4. Is any L2 claim made without a real run or a calibrated model?
5. Is the M2 acceptance in the ticket met, and is anything marked PASS that is actually unverified?

Report VERDICT PASS / CANNOT-VERIFY / FAIL, blocking defects with file:line and the criterion, and non-blocking findings. A finite verdict.`;

const build = await runs.run("build", {
  agent: "worker",
  label: "Add join-heavy shapes and descriptors",
  cwd: repo,
  task: buildTask,
  timeoutMs: 7200000,
});
if (!build.ok) {
  return { stage: "build", ok: false, output: build.output };
}

const analyze = await runs.run("analyze", {
  agent: "worker",
  label: "Analyze per-region headroom vs L2",
  cwd: repo,
  task: analyzeTask,
  timeoutMs: 3600000,
});
if (!analyze.ok) {
  return { stage: "analyze", ok: false, build: build.output, output: analyze.output };
}

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Review M2 descriptors and recommendation",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 2700000,
});

return { stage: "done", ok: true, build: build.output, analyze: analyze.output, review: review.output };
