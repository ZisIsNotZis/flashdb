# 82 - controlled T5/T6/T7 verdict on the low-spec laptop (M1, decidable)

## Status: measured, decidable

5 paired blocks x rungs 50/100/200 x 3 arms at 12.546x of the 128 MiB cap, on AC / power profile
`performance` / EPP `performance` / 3.6 GHz, with a 0.2 ms machine lag floor and a 9.8 ms device
fsync floor, **re-measured after the two fairness fixes the f70d91c report named** (sqlite's WAL
checkpoint policy stated and probed; both SQL pacers made open loop like `flashdb-load`). All three
arms sustained 50 and 100 rps in 5/5 blocks; at 200 rps sqlite and postgres sustained 5/5 and
flashdb 3/5. **T5 PASS** (deficit 1.001 against 1.5x; flashdb sustained 50 rps in 5/5 blocks),
**T7 PASS**, **T6 FAIL** (1.00x against the pre-registered 3x, a ratio bounded at 1.0526 for any pair
of arms at one offered level). The verdicts, the per-metric defects and their smallest fixes are in
`report.md`; the machine-readable document is `report.json`; the raw evidence is `runs/`, including
the checkpoint probe under `runs/probe-checkpoint/`.

## Objective

Make T5/T6/T7 decidable at the pre-registered >=10x regime on the available machine (Intel i5-8350U,
4C/8T laptop) instead of another CANNOT-VERIFY. The previous "cannot verify" was a methodology gap:
the measurement ran while the box was on battery in power-saver at ~23% clock, and the ~8 h T6
estimate came from re-seeding each instance per run. Both are fixed here.

## Machine state (verified 2026-10-17 after the user's action)

- AC online = 1; power profile = `performance`; energy_performance_preference = `performance`;
  scaling_cur_freq = 3.60 GHz (max). The governor name still reads `powersave` because this platform
  drives frequency through EPP, so record **EPP + cur_freq + AC**, not the governor name alone.
- Desktop load ~1.2-2.0 (firefox, gnome, pi). This is why the protocol pairs arms within one window.

## Protocol

1. **Build the seeded (pre-load) instance once per arm** from the frozen seed trace, then snapshot it
   (a copy). Restore the snapshot before every arm-run; never re-seed inside the measurement loop.
2. **Paired blocks.** N blocks; within each block, for each rung in 50/100/200, run the arms
   `flashdb-declared`, `sqlite`, `postgres` interleaved (restore -> run -> restore -> run ...). A block
   is one complete crossing; report median and IQR across blocks per (arm, rung).
3. **Machine-state recording** before and after every arm-run: AC online, power profile, EPP,
   scaling_cur_freq, loadavg, top CPU processes.
4. **Device floor.** At the same machine state, run an idle pacer / fsync microbench with no engine;
   report the schedule-lag floor and fsync p50/p99. Report both the absolute 10 ms ceiling and a
   floor-adjusted reading (p99 lag / floor), but keep the pre-registered absolute ceiling as the
   primary rule.
5. **Same unified validity rule** as M1: lag p99 > 10 ms, the 3-consecutive-window growth clause, or
   any request error rejects a window; a rung is sustained iff achieved within 5% of offered and zero
   rejected windows. Engine-only 128 MiB cap; harness outside; one process at a time.
6. **No concurrent work.** No subagents, builds, or seed jobs during the measured phase; verify with
   `ps` and record loadavg.

## Instance and disclosure

Reuse the existing logical dataset (10.6M stock cells, 200k customers). Physical footprint per arm
differs by encoding: flashdb 1.68 GB (12.55x of 128 MiB), postgres 1.10 GB (8.22x), sqlite 0.55 GB
(4.07x). Report all three; do not inflate a baseline to meet the floor.

## Acceptance

- A decidable T5 (1x rung), T6 (highest common sustained level), T7 (declared passes T5) table with
  median/IQR across blocks, the machine state, the device floor, and a verdict per metric.
- Every unmeasured item is named with its exact remaining methodology defect and smallest fix — the
  words "undecidable" or "cannot verify" are not an acceptable conclusion.
- No change to the pre-registration thresholds in this ticket.

## Budget

- One worker for build/run/report; one fresh reviewer.
- Wall clock: 3 hours.
- Stop conditions: a snapshot restore cannot reproduce the seeded state; the box cannot sustain the
  required quiet window (record and retry once); any need to change a threshold (stop and escalate).
