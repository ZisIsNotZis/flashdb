# 72 - scribe repairs: correct two documentation-accuracy defects (tickets 67 + 70)

## Contract

Doc-only scribe repair of two current-fact defects found by independent verifiers. No code change
and no re-run of any load measurement. The honest verdicts stand; only the false mechanism and the
false figures are corrected in place (no "was A now B" changelog). The pre-registration
(`.scratch/15-objective-v0/pre-registration.md`) is untouched.

## REPAIR A — `.scratch/67-requestpath-write-stall/issues.md` (write-amplification variance attribution)

The six static-derived write-amp figures are true and recompute exactly (verified): before
seed42=21.9 / seed43=21.9 / seed42-repeat=30.2 MB; after seed42new=26.4 / seed43new=94.9 /
seed42repeatnew=98.0 MB. The non-regression criterion is correctly marked NOT substantiated and
the read-path follow-on is correctly exonerated. The corrected part is only the variance
mechanism: the excess is NOT a single ~69-72 MB merge — it is a single-region merge oscillation.

Recomputed from `.scratch/67-requestpath-write-stall/{seed43new,seed42repeatnew}.json`
(`win/static-derived`, per-window region `compaction_bytes`):

- seed43new `U|Customer`: 6 merges, 11,372,547 + 5×11,524,169 = 68,993,392 B ≈ 69.0 MB
  (per-merge 11.37-11.52 MB).
- seed42repeatnew `P|Customer`: 7 merges, 10,165,563 + 5×10,300,227 + 10,413,309 = 72,080,007 B
  ≈ 72.1 MB (per-merge 10.17-10.41 MB).
- Largest single merge 11,524,169 B ≈ 11.5 MB, not ~70 MB.
- In seed43new the Customer oscillation is not the whole excess: `P|Order` +5,926,679 B (+5.9 MB)
  and `U|StockMovement` +2,282,634 B (+2.3 MB) also grew, offset by `P|Stock` −4,237,001 B
  (−4.2 MB). All of it carries zero index output.

## REPAIR B — `.scratch/70-merge-oscillation/issues.md` (seed43 win-learned reorg figures)

The ticket's `after seed43` win-learned reorg (6.88 MB, max 5.23 MB, T3 0.22x) summed per-window
`compaction_bytes` and dropped the driver's final DRAINED merge. `flashdb-load` applies the last
in-flight job after the window loop and adds it to `summary.compact_bytes_total`/`compact_merges`
but to no window or region.

Recomputed from `.scratch/70-merge-oscillation/seed43.json` (`win/learned@1.0`):

- summary compact_bytes_total = 18,256,145 B, compact_merges = 6.
- summed per-window per-region compaction_bytes = 6,883,598 B (5 merges).
- 6th (final drained) merge = 18,256,145 − 6,883,598 = 11,372,547 B ≈ 11.37 MB (one pair).

Corrected figures: `after seed43` win-learned reorg 18.26 MB; learned/declared ratio 0.594x
(≈0.59x); max single merge 11.37 MB (not the 5.23 MB window actuation). Win T3 (learned/declared)
ratios 0.557 / 0.594 / 0.222 / 0.918 (seed42 / 43 / 44 / 42-repeat) give median 0.5758x — still
<=3x, no run exceeds 1x. The ticket-68 "before" figures are per-region sums and undercount their
own final drained merge the same way, so before-vs-after is apples-to-apples in merge count (both
sides undercount by one merge).

Also fixed: the gate header now states the actual final HEAD (`d63e294`, wave16 integration HEAD
`853633a`) instead of `1b13c8b`; the seed43 declared rung-50 lag-rej cell is corrected to 39/39
(the curve has 39 windows, not 40) with a footnote, and seed42-repeat declared is disclosed as a
genuine 39/40 over 40 windows.

Acceptance at the corrected numbers still holds: #1 single-region blowup bounded (PASS — `U|Stock`
5.23 MB / 1 merge on both seed42 and the same-trace repeat); #2 win T3 median 0.5758x <= 3x
(PASS); #4 no throughput regression + legitimate compaction still happens (PASS — learned 107 rps
on all four runs, compacts 6.83-28.20 MB).

## Gates (run at the repair commit)

1. `cargo build --workspace --offline` — 0 warnings.
2. `cargo test --workspace --offline` — **304 passed; 0 failed; 0 ignored**.
3. `cd harness && uv run --with pytest pytest -q` — **140 passed in 5.21s**.
4. `git diff --check` — silent (exit 0).

## Commit

Repair commit: `1e42fdf9849d4a4aef201359b4235976a8887080`.
