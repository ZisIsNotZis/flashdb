# 88 — M3's remaining notes, closed on their own evidence

Status 2026-10-22, by the node that owns this roadmap. Two things arrived and did not agree with the
documents: WP-4's review of ticket 84 (mail `2026-10-22-wp4-review-of-ticket-84.md`,
`.scratch/86-index-mechanism-review/issues.md`) and WP-12's re-read of the M1 records (mail
`2026-10-21-wp12-m1-decided-rule.md`, `.scratch/87-m1-decided-rule/`). Both reports said "decision
needed: none" and both asked for text edits by the owner of the files. This ticket is those edits and
the two runs that back them.

## The review's note 1 was cured by making the claim true, not by weakening the sentence

M3's clause (c) said the no-index arm returns "byte-identical answers", but `arm()` in
`engine/tests/index_defect_mechanism.rs` recorded `format!("{}:{}", status, rows.len())` — status and
row **count**, never row content. The review offered two fixes: compare the rows, or reword. The first
is stronger and cheap, so the comparison now carries the serialized row
(`out["q"].row.to_string()`, `-` when absent) and a non-vacuity assertion that every one of the 188
answers carries a row, so two arms cannot agree over a string of `-`.

- Result: the test passes with before, after (index) and after-tiled answers equal **byte for byte**
  over all 188 requests — `logs/index-defect-mechanism-rowbytes.out`, exit 0, 1 passed.
- The claim in `docs/roadmap.md` (M3 row) and `docs/plan/workplan.md` §M3 status (c) is now what the
  test measures. The pre-fix failure is unchanged and still the entries assertion (`left: 0, right:
  2785`), which is what ticket 84's record shows.

## The review's note 2: (a)'s other halves had no citation, and now do

Ticket 84 measures the **entries** half of clause (a) and nothing of `coverage_csn` or
`startup_warm scan_entries`; `grep -n coverage engine/tests/index_defect_mechanism.rs` returns 0 hits,
which the review reported as an undisclosed absence. The tests that do bind those halves existed and
were green; the documents were simply not pointing at them. They are now cited in place:

- `coverage_csn` = max covered csn → `flashdb-load`'s
  `a_stale_adopted_index_is_rebuilt_and_coverage_advances` (`engine/src/bin/flashdb-load.rs:3065`):
  coverage equals the csn the build snapshotted and advances to the rebuild's.
- `startup_warm scan_entries` for a memtable-only region →
  `readahead::tests::warm_startup_counts_rows_that_live_only_in_the_memtable`
  (`engine/src/readahead.rs:585`): `scan_entries == 2` with `active_tile_regions()` empty.
- Both pass at `f38ec63` in `logs/workspace-head.out`.

## The review's note 3: the policy-off reading is restated with its limit

`arm=before` is genuinely the no-index path through `execute_request` (no adoption yet,
`lookup_movement_by_order_no` returns `None`, the scan falls through to `scan_primary`), but it is one
in-process arm, not a run through the driver's policy gate. §M3 status (c) now says both, and names
`movement_scan_falls_back_to_full_scan_when_index_absent` as what binds the fallback path.

## WP-12: the headline is rule-invariant, and the record says so

`0 of 45` block rows and `0 of 47` arm-runs change verdict under the decided lag-only rule, because no
saved M1 arm-run was ever scored with a service ceiling (`"service_ceiling_us": null` in 45 of 45
window files; both producers call `summarize_curve(windows, None)`). Landed:

- `README.md` §Status of the thesis: one bullet stating the invariance, the two counts, the cause, and
  the command that recomputes it (`.scratch/87-m1-decided-rule/run.sh`), so the 3 of 5 no longer reads
  as a both-ceilings figure waiting to be restated.
- `docs/plan/open-items.md` row `T-84`: the premise "every published rung verdict was produced by a
  reducer that treats a service-ceiling breach as a rejection" was **false for these records** and is
  retracted with its reason; what was actually missing is the decided reading (`sustained_primary`
  absent from 45 of 45 `run-*.json`), which now exists and is re-derivable.
- `docs/plan/open-items.md` leverage item 2: rewritten from "what remains is scoring the records" to
  what remains after scoring — the two single-window lag breaches (30,430 and 33,747 us) inside one
  ~573 ms group, with service p99 reported and rejecting nothing. The list's numbering had no item 4
  since it was written at `f584ecc`; it is now 1-8.

## What this ticket did not do

- No arm, rung or engine behaviour was run: no throughput or latency number is measured, claimed or
  implied anywhere in these edits, and `logs/workspace-head.out` starts with the loadavg (13.20) so a
  reader can see the box was not quiet.
- WP-4's non-claim notes (the averages in ticket 84's "122 B of block padding" and "3,951 B average
  block", the inferred entry count in the tiled arm, the rule-out rows being cross-run recombinations)
  are left as ticket-level text: they do not touch a claim the roadmap or README makes.

## Reproduce

```sh
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --workspace --offline                     # 340 passed / 0 failed at f38ec63
cargo --config 'source.crates-io.replace-with="aliyun"' \
      --config 'source.aliyun.registry="sparse+https://mirrors.aliyun.com/crates.io-index/"' \
      test --offline -p flashdb-engine --test index_defect_mechanism -- --nocapture
```
