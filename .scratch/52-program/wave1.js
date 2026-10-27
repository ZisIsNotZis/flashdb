const MODEL = "deepseek/deepseek-flash";

const instrTask = [
"Repo /home/z/vibe/flashdb, branch master, HEAD 9bae6fe. Run 'source $HOME/.cargo/env' first. Narrow deliverable; commit incrementally; if you approach your time limit commit what is complete and say exactly what is unfinished.",
"TOUCH ONLY: engine/src/bin/flashdb-load.rs, harness/flashdb_harness/cold_0b.py, harness/tests/, .scratch/48-latency-instrumentation/issues.md (create). Do not touch engine/src/engine.rs, keys.rs, manifest.rs, engine/tests/ or docs/ - other lanes own those.",
"",
"GOAL: close the two measurement gaps an independent verifier could not resolve, because the fields do not exist.",
"1. PER-WINDOW GROUP STATISTICS. The driver forms groups from already-ready requests (engine/src/bin/flashdb-load.rs, function ready_group and the dispatch loop). Record per window: the number of groups dispatched, the mean and max group size, and the number of requests (so mean size = requests/groups is derivable). Emit them in the --metrics-json window line and parse them in cold_0b.py, and show them per rung in the curve row.",
"2. PER-WINDOW MINIMUM LAG. Record both min_lag_us and the existing lag percentiles per window (the absolute minimum per-request lag is currently unverifiable because only clamped percentiles are kept).",
"3. PER-GROUP ELAPSED. Record the max group elapsed per window (group_elapsed_us already exists) AND the number of groups whose elapsed exceeded the service ceiling, so a verifier can tell whether the p99 tail is one group's serial work or many groups' independent stalls.",
"4. Keep every durable invariant from 744caaa and 965adf4: bounded rung rate with the 2 percent tolerance, rung attribution that never straddles an offered-rate change, trailing window flush, per-window invalid/reject fields with their named reason, early_dispatches counter.",
"5. TESTS in harness/tests/ that fail against the pre-change code: group statistics are consistent (sum of per-window requests equals the summary total; mean size lies between 1 and the group cap); min lag is present and non-negative; a window whose largest group elapsed exceeds the ceiling reports at least one group-exceeded count.",
"",
"TICKET .scratch/48-latency-instrumentation/issues.md: the fields added with their exact names, the tests, verbatim gate output, residual risk. Terse.",
"GATES, report verbatim: cargo test --workspace --offline (test result lines and total); cargo build --workspace --offline (zero warnings); cd harness && uv run --with pytest pytest -q (summary); git diff --check (silent). Commit one commit; report its hash and the exact new field names."
].join("\n");

const tailTask = [
"Repo /home/z/vibe/flashdb, branch master, HEAD 9bae6fe. Run 'source $HOME/.cargo/env' first. MEASURE-ONLY lane: do not change any existing source file.",
"TOUCH ONLY: one new script under harness/ (name it clearly), .scratch/49-latency-tail-attribution/issues.md (create). Do not touch engine/ or fleet any existing harness module. /tmp is free.",
"",
"GOAL: attribute the latency tail. The verified curve at .scratch/47-latency-trusted-curve/issues.md shows, for the adversarial/declared arm on a cold 8.17x instance: per-request service p50 5,930 / 6,153 / 9,157 / 19,638 us and service p95/p99 910,504/990,504 (rung 50) up to 412,788/445,288 (rung 400); lag p50 62/76/2,689/7,992 us. All 64 windows are rejected by a 10 ms ceiling. Throughput is fine (99.4-100 percent of offered, 0 errors). The tail is therefore not group aggregation alone and its cause is unverified.",
"",
"SETUP: the instance is /tmp/cold44/per_region-mixed (data 1,643,933,528 B, cap 201,326,592 B, 8.17x, region_mode=per_region). FIRST build the driver once and COPY the binary to your own directory (for example /tmp/cold49/flashdb-load) and run only that copy, because another lane is editing engine/src/bin/flashdb-load.rs concurrently. Make your own cp -a copy of the instance for each configuration so no run contaminates the next.",
"",
"RUN THESE CONFIGURATIONS, at least the two lowest offered levels each, and report per rung the per-request service p50/p95/p99 and lag p50/p95/p99 plus the achieved rate:",
"  (1) baseline as-is (group 64, 8 MiB cadence, compaction actuation at the driver's default budget);",
"  (2) group size 1 (--group-requests 1): isolates group head-of-line from engine service time;",
"  (3) compaction actuation disabled or set to zero budget (find the driver flag; if no such flag exists, say so and skip this configuration rather than inventing one);",
"  (4) a much larger checkpoint/rotate cadence than 8 MiB, so rotation fsync stalls cannot occur during the run (state the value and why it is large enough given the recorded WAL ingest of 979,080 B for this trace);",
"  (5) if time permits, group 64 but with the instance's memory cap doubled, to see whether page-cache pressure drives the tail.",
"",
"ANALYSIS REQUIRED: for each configuration, say whether the p95/p99 tail moved, and by how much. Then state which mechanism dominates: group head-of-line, rotation/fsync stalls, compaction stalls, page-cache pressure, or none of these. If two mechanisms are confounded, say so and state the configuration that would separate them. Do NOT assert a cause that the configurations do not separate.",
"TICKET .scratch/49-latency-tail-attribution/issues.md: the configurations with exact commands, the per-rung tables, the deltas, the attribution conclusion with its uncertainty, and what remains unseparated. Terse.",
"GATES, report verbatim: cd harness && uv run --with pytest pytest -q (summary if you added a test); git diff --check (silent). Commit one commit; report its hash and the headline deltas."
].join("\n");

const colocateTask = [
"Repo /home/z/vibe/flashdb, branch master, HEAD 9bae6fe. Run 'source $HOME/.cargo/env' first. Narrow deliverable; commit incrementally; if you approach your time limit commit what is complete and say exactly what is unfinished.",
"TOUCH ONLY: engine/src/keys.rs, engine/src/engine.rs, engine/src/tile.rs, engine/tests/, .scratch/50-family-colocation/issues.md (create). Do not touch engine/src/bin/, engine/src/manifest.rs, harness/ or docs/.",
"",
"WHY: an offline measurement (.scratch/43-placement-headroom/issues.md, verified in commit 3af49cf) shows that co-locating the derived U|X and R|X entries in the same tile as the P|X row for the same entity would cut dependent random reads per request by about 35 percent, stable across tile sizes. That is the one measured axis with headroom, and it is currently NOT expressible, because tiles are monotone key-range fenced and the key encoding orders keys by family prefix, so U|X sorts far from P|X. The learner's action space therefore cannot influence placement at all.",
"",
"DELIVERABLE: make family co-location expressible, behind a policy selector, without breaking any invariant.",
"1. Read how keys are encoded today (engine/src/keys.rs) and how tiles are fenced and region-partitioned (engine/src/engine.rs region_of_prefix, region_prefix, the tile writer, and the unique/reverse prefix helpers).",
"2. Work out the MINIMAL key-encoding change that makes one entity's P, U and R entries sort adjacently (for example entity-major ordering: marker, entity, family, rest) while preserving: strict monotone ordering of the whole keyspace (tiles are fenced ranges), the uniqueness prefix contract used by the unique-prefix bloom and by unique_lookup, the reverse-lookup contract, and every existing key-order assumption in the engine and its tests. Write the reasoning in the ticket as a short decision note with the rejected alternatives.",
"3. Implement it behind an explicit selector (a compile-time constant or a runtime mode with a default of today's encoding) so the old and new encodings can both be tested; the default must keep the current behaviour and all existing tests must pass unchanged.",
"4. Add tests in engine/tests/ proving: with the new encoding, one entity's P|X, U|X and R|X keys land in the SAME tile for a range of entity values (assert the tile id or the fence ranges, not just the ordering); the uniqueness and reverse lookups still return the right handles; the duplicate-owner invariant still holds; and a reopen after the change recovers the same state.",
"",
"TICKET .scratch/50-family-colocation/issues.md: the decision note, the change, the selector, the tests, verbatim gates, residual risk, and the exact measurement that would now be possible (dependent reads per request on a real run). Terse.",
"GATES, report verbatim: cargo test --workspace --offline (test result lines and total); cargo build --workspace --offline (zero warnings); git diff --check (silent). Commit one commit; report its hash and whether the entity-co-location test passes."
].join("\n");

const tenXTask = [
"Repo /home/z/vibe/flashdb, branch master, HEAD 9bae6fe. Run 'source $HOME/.cargo/env' first. MEASURE-ONLY lane: do not modify any existing source file.",
"TOUCH ONLY: one new build script under harness/ (name it clearly), .scratch/51-instance-10x/issues.md (create). /tmp is free.",
"",
"GOAL: produce a cold instance that satisfies the pre-registration's own floor, 'Data >= 10x the memory budget' (.scratch/15-objective-v0/pre-registration.md:52), because every published curve so far ran at 8.17x and each verdict had to be qualified.",
"1. Read how the existing instance was built: harness/build_cold_44.py and the slice 29/31 build path. Note the constraint discovered in slice 46: a per-region publish emits one tile per region, so the checkpoint/rotate cadence must be large enough that the 1024-tile cap is not exhausted during the run (8 MiB works; the runner's shipped 16 KiB default does not).",
"2. Build under /tmp/cold51 an instance with data at least 10x the cgroup memory cap. Choose the pair (data size, cap) so that the ratio comfortably exceeds 10x and the build stays inside your time budget; state both numbers and the exact ratio. If you must trade, prefer a smaller cap over a smaller dataset, and say so.",
"3. Record the exact build parameters, the seed-phase output, the openinfo line (active tiles, tile bytes, RSS, VmHWM), the WAL ingest expected for the 8000-request scored trace, and the ratio against the floor.",
"4. Verify the instance is usable: run a SHORT smoke load (for example one offered level, one arm) and report that it completes with 0 errors, or report the failure. Do not run the full four-rung curve.",
"TICKET .scratch/51-instance-10x/issues.md: parameters, seed output, openinfo, the ratio versus the 10x floor, the smoke result, residual risk (for example build time and whether the same trace ingest still keeps rotation rare). Terse.",
"GATES, report verbatim: git diff --check (silent). Commit one commit; report its hash, the instance path, the ratio and the smoke result."
].join("\n");

const decisionsTask = [
"Repo /home/z/vibe/flashdb, branch master, HEAD 9bae6fe. You are the DOCS writer recording decisions that the orchestrator has ADOPTED under the author's standing autonomy grant (the author stated on 2026-10-09 that the project has one milestone, that decisions should be made from the original philosophy rather than asked, and that work must not stop).",
"TOUCH ONLY: .scratch/15-objective-v0/pre-registration.md, .scratch/STATUS.md, docs/2_GOAL.md, docs/6_ARCHITECTURE.md (only the tagged open items this touches), docs/glossary.md, .scratch/active-work.md. Create NO new files. Do not touch engine/ or harness/.",
"",
"RECORD THESE FOUR DECISIONS with their reasoning, marked as adopted by the orchestrator on 2026-10-09 under the standing autonomy grant, reviewable by the author:",
"",
"1. THE PRIMARY VALIDITY RULE IS THE PRE-REGISTRATION'S OWN LAG RULE. Keep 10 ms. The pre-registration defines the primary metric as achieved throughput within 5 percent of offered, lag p99 <= 10 ms, no window rejected. The implementation now carries two named ceilings: a lag ceiling (default 10,000 us, the pre-registration's number) and a service-latency ceiling. The primary metric is judged on the LAG ceiling; per-request service latency is reported separately as the latency/efficiency measure, and the sign-off list's relative alternative (two times the reference-load p99) will be made concrete only once a reference-load p99 is measured. Rationale to record: moving the bound after seeing the failure would be moving the goalposts; the honest response to a latency failure is to attack the latency.",
"",
"2. T5/T6/T7 ARE ADOPTED as the external-baseline thresholds, taking the proposals from PROPOSED to agreed: T5 low-load non-inferiority - at the 1x rung flashdb must be within 1.5x of the slower of {sqlite, postgres}, with 2x as the fail line and a 10x gap a design failure; T6 high-load superiority - at the highest common offered level flashdb must reach at least 3x the best baseline's sustained throughput under the same latency rule; T7 initial-policy quality - the declared arm alone must already pass T5. Baselines: sqlite (WAL, synchronous stated), postgres (frozen disclosed config, no tuning race), and one new request-level harness driving every arm, every arm in the same memory-constrained regime. Update the pre-registration: move the proposals out of the 'awaiting sign-off' section, remove the now-superseded 'no absolute-throughput claim' clause, and update the sign-off list so it does not still ask for these.",
"",
"3. THE 10x FLOOR IS A REQUIREMENT, NOT A QUALIFICATION. Every verdict from an instance below 10x is provisional. A 10x instance is being built (ticket .scratch/51-instance-10x) and final verdicts must be re-derived on it. Record that the 8.17x runs stand as provisional and that 8.17x is warmer than 10x, so a failure at 8.17x cannot be explained away by the lower ratio.",
"",
"4. THE TILE-CAP DEFECT IS FIXED ON BOTH SIDES (.scratch/46-partitioned-publish-tile-cap): raise and make configurable the tile cap, AND make the run harness's default cadence compatible with per-region publishing, AND make a scored run REFUSE an incompatible cadence loudly instead of aborting mid-run. Rationale: the shipped default must be able to run the shipped engine in the documented regime.",
"",
"ALSO: record in STATUS that the first valid evaluation of the primary metric (commit 9bae6fe, verified) gives NO sustained rung for the adversarial/declared arm at 8.17x, that the failure is a latency tail (service p50 5,930-19,638 us; p95/p99 412,788-990,504 us) and not throughput (99.4-100 percent achieved, 0 errors), and that T1/T2/T4 remain underived pending the other arms and workloads.",
"",
"Keep the writing to decision records: no process narrative, no dates in fact documents beyond the provenance tag, one home per fact. GATES, report verbatim: cd harness && uv run --with pytest pytest -q (summary); git diff --check (silent). Commit one commit; report its hash and the files touched."
].join("\n");

const instr = await runs.run("instr", { agent: "worker", model: MODEL, task: instrTask });
const tail = await runs.run("tails", { agent: "worker", model: MODEL, task: tailTask });
const coloc = await runs.run("coloc", { agent: "worker", model: MODEL, task: colocateTask });
const tenx = await runs.run("tenx", { agent: "worker", model: MODEL, task: tenXTask });
const decisions = await runs.run("decisions", { agent: "worker", model: MODEL, task: decisionsTask });

return [instr, tail, coloc, tenx, decisions];
