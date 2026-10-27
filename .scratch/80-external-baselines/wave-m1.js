const repo = "/home/z/vibe/flashdb";

const buildTask = `You are the build worker for milestone M1 (external baseline scoreboard) of the flashdb research project.

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines (already checked out, clean). Work only in this repo; never push.

READ FIRST (do not open the multi-MB *.json reports):
- docs/0_ROADMAP.md (milestone M1)
- .scratch/80-external-baselines/issues.md (your acceptance) and .scratch/80-external-baselines/env.md (verified environment)
- harness/flashdb_harness/generator.py (the four request shapes and the deterministic Rng/Zipf)
- harness/flashdb_harness/seed.py (the seeded dimensions)
- harness/flashdb_harness/load_profile.py (offered-load manifest and window/segment model)
- harness/flashdb_harness/cold_0b.py — grep for build_trace and the window/ceiling parsing; reuse its JSONL dialect and validity rule, not its flashdb-only runner
- engine/src/request.rs — the block/op semantics the adapter must implement

GOAL: a committed, tested request-level harness at harness/flashdb_harness/baseline_0b.py that runs the SAME deterministic request trace against three arms and records comparable windows: flashdb-declared, sqlite, postgres. (flashdb-learned may reuse the flashdb wiring under a second arm name.)

REQUIRED BEHAVIOUR:
1. build_trace_baseline(...) emits (a) the JSONL trace in the existing dialect using Generator and (b) a manifest with sha256, segments and offered rates. Same seed -> byte-identical trace.
2. Seed every arm from the same dimensions (seed.py SeedGenerator semantics) so the oracle is meaningful. Implement the four shapes per arm:
   - order_flow: find Customer by email; then atomically (a) conditional Stock decrement where sku/loc/on_hand>=qty, (b) insert StockMovement, (c) insert Order; else insert Backorder. One durable commit per request batch.
   - stock_lookup: get Stock by (sku, loc).
   - customer_lookup: find Customer by email.
   - movements_scan: find StockMovement where order_no, ordered by movement_no, limit 100.
3. Closed-loop pacer: schedule request i at its segment's offered rate; measure per-request schedule lag (scheduled -> start) and service latency (start -> done). Aggregate per window. Apply the project rule exactly: a window is rejected if its schedule-lag p99 > 10,000 us or any request errored; a rung is sustained iff achieved rate is within 5% of offered AND zero rejected windows. Record rejected-window reasons/counts.
4. Equal durability with one disclosed batch size B for every arm: sqlite WAL + synchronous=FULL; postgres synchronous_commit=on with the env.md config; flashdb --group-requests B. State B.
5. Post-run inventory oracle per arm: for every (sku, loc), on_hand == on_hand_initial - sum(delta) over committed movements; on_hand >= 0; no duplicate order_no; movement count == committed take blocks; backorder count consistent. Fail loudly at the first violation.
6. Add pytest coverage in harness/tests/ for (a) trace determinism (sha256 stable across two calls) and (b) the oracle on a tiny in-memory trace. Dependencies: stdlib + psycopg only.

SCALE FOR THIS WORK PACKAGE: prove correctness and the pacer on a 2,000-request trace at 50 rps with small dimensions (e.g. 200 customers / 500 skus / 4 locs), each arm under systemd-run --user --scope -p MemoryMax=128M and flock /tmp/m1-measure.lock. Do NOT build the 1.6 GB cold instance here.

CONSTRAINTS:
- Do not change any pre-registered threshold, the pre-registration file, or engine code. Do not push.
- One process at a time under the lock. Commit incrementally on wave/m1-baselines.
- Add scripts/baseline-env.sh that reproducibly downloads (aliyun apt mirror), extracts and starts the user-space postgres from env.md; idempotent, no root.

ACCEPTANCE before you report done:
- cd harness && uv run --with pytest pytest -q passes (record the summary line).
- A committed run proves sqlite oracle PASS, postgres oracle PASS, flashdb oracle PASS on the 2,000-request trace; each arm's windows recorded; the trace sha256 printed and stable.
- Append a "Build record" section to .scratch/80-external-baselines/issues.md with the exact commands, trace sha256, B, durability per arm, oracle result per arm, the pytest summary, and the one-line invocation the measurement worker should use. Commit it.
- Report the commit hash, the invocation, and any arm that could not meet a criterion.

STOP AND REPORT (do not invent): an oracle that cannot pass, an arm that cannot fit 128 MiB, or any need to change a pre-registered threshold. Leave the branch committed and the tree clean.`;

const measureTask = `You are the measurement worker for milestone M1 of flashdb.

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines. Do not push.

READ: docs/0_ROADMAP.md (M1), .scratch/80-external-baselines/issues.md (including the Build record), .scratch/80-external-baselines/env.md, and the committed harness harness/flashdb_harness/baseline_0b.py. Do not open multi-MB json reports.

TASK: run the committed harness on a cold, cache-evicting instance at >=10x data:memory and report T5/T6/T7 exactly per .scratch/15-objective-v0/pre-registration.md ("External baselines and thresholds"):
- Arms: flashdb-declared, flashdb-learned, sqlite, postgres.
- Offered ladder: reference 50 rps x [1,2,4,8] = 50/100/200/400.
- Regime: 128 MiB cap for every arm, one process at a time under flock /tmp/m1-measure.lock and systemd-run --user --scope -p MemoryMax=128M; record cgroup peak and loadavg at boundaries.
- Instance: >=10x data:memory. Prefer an existing cold instance if present; otherwise build one with: cd harness && uv run python build_cold_51.py --release --workdir /tmp/m1-cold. If that build cannot finish inside your budget, run a smaller provisional instance and label every number PROVISIONAL with the reason.
- Apply the SAME latency/sustained rule to every arm (schedule-lag p99 <= 10 ms, no errors, achieved within 5% of offered). Never use a different rule for a baseline.
- Verify the inventory oracle for every arm on the measured run, not only the self-test.

DELIVERABLE: .scratch/80-external-baselines/report.md and report.json containing:
- per arm x rung table: achieved rps, schedule-lag p99, service-latency p99, rejected windows and reasons, errors, cgroup peak;
- the oracle result per arm;
- T5 at the 1x rung (within 1.5x of the slower of sqlite/postgres; 2x is the fail line);
- T6 at the highest common offered level (>=3x the best baseline's sustained throughput under the same latency rule);
- T7 (the declared arm alone passes T5);
- each threshold's exact condition, what is unverified, and a verdict per metric;
- a one-paragraph honest summary: what the numbers support and do not support.

CONSTRAINTS: no engine or threshold changes; no push; commit the report and raw JSON on the branch. Respect a 3-hour wall-clock cap and stop-and-report incomplete at the cap.

Report the commit hash and the decisive table inline (it is small).`;

const reviewTask = `You are a fresh independent reviewer for milestone M1 of flashdb.

Repo: /home/z/vibe/flashdb. Branch: wave/m1-baselines. You may run commands and read the raw report.json; do not edit files.

READ: docs/0_ROADMAP.md (M1), .scratch/80-external-baselines/issues.md, .scratch/80-external-baselines/env.md, .scratch/80-external-baselines/report.md, the pre-registration section "External baselines and thresholds", and harness/flashdb_harness/baseline_0b.py.

Judge, actively trying to falsify:
1. Does the harness execute the SAME deterministic trace (sha256) and the SAME latency/sustained rule for every arm?
2. Is the inventory oracle correct, and did it actually pass for every measured arm?
3. Is the equal-durability claim true per arm (sqlite WAL+FULL, postgres synchronous_commit=on, flashdb group-requests B), with differences disclosed rather than hidden?
4. Is the memory regime the same for every arm, and is the >=10x claim true?
5. Are T5/T6/T7 computed correctly from the pre-registered thresholds and the measured numbers, with conditions named? Is any number quoted from the wrong column (achieved vs offered, aggregate vs per-rung)?
6. Are defects, unverified criteria and the scale (measured vs provisional) disclosed honestly?

Report: VERDICT (PASS / CANNOT-VERIFY / FAIL), blocking defects with file:line and the criterion violated, non-blocking findings, and the exact evidence/commands. Do not restate completion claims. Give a finite verdict on the report, not a suggestion to run more.`;

const build = await runs.run("build", {
  agent: "worker",
  label: "Build request-level baseline harness",
  cwd: repo,
  task: buildTask,
  timeoutMs: 5400000,
});
if (!build.ok) {
  return { stage: "build", ok: false, output: build.output };
}

const measure = await runs.run("measure", {
  agent: "worker",
  label: "Measure flashdb vs sqlite vs postgres",
  cwd: repo,
  task: measureTask,
  timeoutMs: 10800000,
});
if (!measure.ok) {
  return { stage: "measure", ok: false, build: build.output, output: measure.output };
}

const review = await runs.run("review", {
  agent: "reviewer",
  label: "Review baseline harness and T5/T6/T7",
  cwd: repo,
  task: reviewTask,
  timeoutMs: 2700000,
});

return {
  stage: "done",
  ok: true,
  build: build.output,
  measure: measure.output,
  review: review.output,
};
