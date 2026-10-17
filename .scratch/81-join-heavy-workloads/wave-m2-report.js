const repo = "/home/z/vibe/flashdb";

const fixTask = `You are the final report-fix worker for milestone M2 of flashdb (join-heavy workload descriptors).

Repo /home/z/vibe/flashdb, branch wave/m2-join-heavy, tree clean. Do not push. Report/analysis correction only: no engine change, no threshold change, no new milestone scope decision.

Read the review in full: /home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/de648176-64af-41eb-bff3-6a5a7794cf9b_reviewer_output.md . Then .scratch/81-join-heavy-workloads/{issues.md,report.md,descriptors.json}, harness/flashdb_harness/{generator.py,join_descriptors.py}, harness/tests/test_join_shapes.py, docs/roadmap.md M2.

Fix the two blocking P1s:
1. Add an "M2 acceptance status" block to report.md (and reference it from issues.md) that states item by item: descriptors measured (met), recommendation (met), >=10x headroom measurement (UNMET), learned-vs-declared headroom with the latency rule (UNMET). Name the missing engine capabilities that block them (multi-row binding, cust->take validation, aggregate-to-write), point at M-B as the smallest deciding measurement, and record explicitly that closing the gate needs either that engine work or an author waiver of the >=10x item. Do not let the ticket's engine-free scope silently stand as the milestone gate.
2. Reword the star_fanout verdict so it does not contradict its own number: the ~1.2x is the 5-row tail case and a page-size-only route to 15% is confined to the tail and unproven; on the median it is a net loss. Remove "far below the 15% bar".

Fix the P2s that affect correctness or honest reading:
- regions_per_request is a lower bound, and rows without the key are uncounted (say both in the limits list).
- Pin the committed descriptor values with a test at measure(42,25) (not per_shape=5), or assert the JSON digest, so descriptors.json and the report tables cannot go stale silently.
- Move the decisive read-cost numbers (4.00 / 1216.32 / 2.88 / 107.64) into join_descriptors.py as a committed helper with a test, so they are not an untested heredoc.
- Fix the bloom counting basis (parent-row absent share vs deduplicated distinct probes): do not claim "<=13.3" unless established.
- Name the reduced co-location key explicitly (dev-loop's (sku, loc, time-bucket) vs the measured single field).
- Fix the dangling generator.JOIN_MIX -> JOIN_NAMES name.
- Declare the 2 KiB/row encoding assumption in the constants list.
- Normalise the anti_join fanout median type (1.0 vs 1).
- Verify the multi_hop median 614 claim; if it is a hot-sku tail outcome, phrase it as "the hot sku sets the width" rather than a median, and state the check.

Run: cd harness && uv run --with pytest pytest -q; regenerate the corpus and descriptors and confirm the committed sha256 and JSON; git diff --check. Commit on wave/m2-join-heavy, tree clean. Update docs/roadmap.md M2 status to: descriptors + recommendation delivered; gate UNMET (2/4); author decision needed on the >=10x/learned-vs-declared item.

Report the commit, each P1/P2 with where fixed, the pytest summary, the reconfirmed corpus sha256, and any item still open.`;

const reviewTask = `You are a fresh narrow reviewer for the M2 report correction.

Repo /home/z/vibe/flashdb, branch wave/m2-join-heavy. Read-only. Read the prior review at /home/z/.pi/agent/sessions/--home-z-vibe-flashdb--/subagent-artifacts/de648176-64af-41eb-bff3-6a5a7794cf9b_reviewer_output.md and the corrected report.md/issues.md/roadmap/descriptors/tests.

Verify: (1) the M2 acceptance-status block states all four roadmap items with met/unmet, names the missing engine capabilities, and escalates the scope cut to the author; (2) the star_fanout wording no longer contradicts the ~1.2x number; (3) the named P2s are fixed (regions lower bound, descriptor pinning at 42/25, committed read-cost helper, bloom basis, reduced key, JOIN_NAMES, 2 KiB/row constant, median type, multi_hop 614 phrasing). Give VERDICT PASS / CANNOT-VERIFY / FAIL with blocking defects and file:line. State plainly that this verdict covers the analysis slice only, not the unmet milestone gate.`;

const fix = await runs.run("fix", {
  agent: "worker",
  label: "Fix M2 P1/P2 report defects",
  cwd: repo,
  task: fixTask,
  timeoutMs: 3600000,
});
if (!fix.ok) {
  return { stage: "fix", ok: false, output: fix.output };
}

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Narrow-review corrected M2 report",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 1800000,
});

return { stage: "done", ok: true, fix: fix.output, review: review.output };
