#!/usr/bin/env python3
"""89 — score the paired 200 rps block with the project's own reducer and the decided rule.

    uv run --project harness python .scratch/89-m3b-paired-200/score-pair.py <runs-dir>

Each arm file is the driver's raw per-window JSON (one object per line) with a `#`-comment header
holding its start and end loadavg. The rows go through `cold_0b.window_records` and
`cold_0b.summarize_curve`, exactly the reducer the M1 record was scored with, so "sustained" here is
the same quantity that produced the 3-of-5; `sustained_primary` is the decided lag-only rule
(`docs/objective.md` §1) and is the one the verdict is read on.

Clause (b): "at 200 rps the indexed arm sustains at least as many paired blocks as declared".
Exit 0 = met, 1 = falsified, 3 = the data cannot decide it (say which file and why).
"""
from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness"))

from flashdb_harness.cold_0b import summarize_curve, window_records  # noqa: E402

WINDOW_MS = 1000


def load_arm(path: Path) -> dict:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    start = next((l for l in lines if "loadavg at start" in l), "# loadavg at start: (not recorded)")
    end = next((l for l in reversed(lines) if "loadavg at end" in l), "# loadavg at end: (not recorded)")
    rows = window_records([l for l in lines if l.startswith("{")], WINDOW_MS)
    if not rows:
        raise SystemExit(f"cannot decide: {path} holds no window JSON")
    curve = summarize_curve(rows, None)
    if len(curve) != 1:
        raise SystemExit(f"cannot decide: {path} holds {len(curve)} offered rates, not one rung")
    row = dict(curve[0])
    row["loadavg_start"] = re.sub(r"^# loadavg at start:\s*", "", start).strip()
    row["loadavg_end"] = re.sub(r"^# loadavg at end:\s*", "", end).strip()
    row["windows_seen"] = len(rows)
    return row


def main() -> int:
    runs = Path(sys.argv[1] if len(sys.argv) > 1 else ".scratch/89-m3b-paired-200/runs")
    files = sorted(runs.glob("p*-*.out"))
    if not files:
        print(f"cannot decide: no p*-*.out under {runs}")
        return 3
    arms: dict[str, dict[str, dict]] = {}
    for f in files:
        m = re.match(r"p(\d+)-(declared|indexed)$", f.stem)
        if not m:
            continue
        pair, variant = int(m.group(1)), m.group(2)
        arms.setdefault(pair, {})[variant] = load_arm(f)

    print(f"{'pair':>4} {'arm':>9} {'achieved':>9} {'lag_rej':>7} {'reqs':>5} "
          f"{'sustained':>9} {'primary':>8}  loadavg start")
    declared_blocks = indexed_blocks = 0
    for pair in sorted(arms):
        for variant in ("declared", "indexed"):
            row = arms[pair].get(variant)
            if row is None:
                print(f"cannot decide: pair {pair} is missing its {variant} arm")
                return 3
            print(f"{pair:>4} {variant:>9} {row['achieved_rps']:>9.1f} "
                  f"{row['lag_ceiling_rejected_windows']:>7} {row['windows_seen']:>5} "
                  f"{str(row['sustained']):>9} {str(row['sustained_primary']):>8}  "
                  f"{row['loadavg_start'].split()[0] if row['loadavg_start'] else '?'}")
            if variant == "declared":
                declared_blocks += int(bool(row["sustained_primary"]))
            else:
                indexed_blocks += int(bool(row["sustained_primary"]))

    n = len(arms)
    print()
    print(f"blocks sustained under the decided lag-only rule: declared {declared_blocks}/{n}, "
          f"indexed {indexed_blocks}/{n}")
    if declared_blocks + indexed_blocks == 0:
        # A vacuous 0 >= 0 is not a reading: it says the box or the run was unusable, not that the
        # index sustains what declared does. Refuse rather than meet the clause on it.
        print("cannot decide: no block sustained by either arm — the box was not quiet or the run "
              "broke; this is not a reading for or against the fix")
        return 3
    if indexed_blocks >= declared_blocks:
        print("VERDICT: clause (b) MET — the indexed arm sustains at least as many paired blocks as declared")
        return 0
    print("VERDICT: clause (b) FALSIFIED — the indexed arm sustains fewer paired blocks than declared")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
