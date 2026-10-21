#!/usr/bin/env python3
"""WP-12: read the saved M1 records (ticket 82) under the decided lag-only rule.

Inputs, all committed evidence, nothing re-run and nothing copied from prose:

    .scratch/82-controlled-m1/runs/b<block>/rung-<rung>/run-<arm>.json          one arm-run record
    .scratch/82-controlled-m1/runs/b<block>/rung-<rung>/windows-<arm>.jsonl     its per-window samples

Three readings per (block, rung, arm) row, all from `cold_0b.summarize_curve`:

    sustained                          the both-ceilings reading every record was scored with,
                                       recomputed from the row's own windows and compared with the
                                       record's stored `curve[0].sustained`
    sustained_primary                  the decided lag-only rule (objective.md §1 / T-84)
    service_ceiling_reported_windows   windows the SERVICE ceiling rejected, reported, never a veto

The counterfactual in `--ceiling-us` (default 10,000, cold_0b's own fixed service ceiling) is NOT the
decided rule: it answers "what would the both-ceilings reducer have said had the service ceiling been
switched on for these runs", because the M1 harness always calls `summarize_curve(windows, None)`
(`baseline_0b.py:1869`, `:1228`), so the recorded service branch never fired.

Outputs (all under this ticket): `rows.json`, `rows.md`, `table-decided.md`. Read-only on ticket 82.
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness"))

from flashdb_harness.baseline_0b import (  # noqa: E402
    DEFAULT_WINDOW_MS,
    RECOMPUTE_FIELDS,
    recompute_records,
    saved_window_rows,
)
from flashdb_harness.cold_0b import summarize_curve  # noqa: E402

RUNS = REPO / ".scratch/82-controlled-m1/runs"
ARMS = ("flashdb-declared", "sqlite", "postgres")


def as_recorded(root: Path) -> dict:
    """The recorded reading, re-derived from each row's own windows.

    `sustained` and `sustained_primary` both come out of the same `summarize_curve(windows, None)`
    call, which is the call the M1 harness makes; `sustained` is then compared with the record's
    stored value so the agreement is shown, not asserted.
    """
    rows = recompute_records(root)
    out = {}
    for row in rows:
        key = (row["workdir"], row["arm"], float(row["offered_rps"]))
        out[key] = row
    return out


def counterfactual(root: Path, ceiling_us: int) -> dict:
    """`summarize_curve(windows, ceiling_us)` per arm-run: the both-ceilings reading with the
    service ceiling switched ON. Not the decided rule; used only to size the service branch."""
    out = {}
    for path in sorted(root.glob("b*/rung-*/run-*.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        if "curve" not in record or "arm" not in record:
            continue
        windows_file = path.parent / str(record["windows_file"])
        windows = saved_window_rows(windows_file.read_text(encoding="utf-8").splitlines(),
                                   int(record.get("window_ms") or DEFAULT_WINDOW_MS))
        curve = summarize_curve(windows, ceiling_us)
        workdir = str(path.parent.relative_to(root))
        for row in curve:
            out[(workdir, record["arm"], float(row["offered_rps"]))] = {
                "sustained_with_ceiling": bool(row["sustained"]),
                "service_rejected_windows": row["service_ceiling_rejected_windows"],
                "lag_rejected_windows": row["lag_ceiling_rejected_windows"],
                "error_windows": row["error_windows"],
                "service_p99_us": row["p99_us"],
                "lag_p99_us": row["lag_p99_us"],
                "windows": row["windows"],
                "reject_reasons": row["reject_reasons"],
                "achieved_rps": round(row["achieved_rps"], 3),
                "offered_rps": row["offered_rps"],
            }
    return out


def rejected_window_detail(root: Path) -> dict:
    """Every window a saved arm-run rejected, with the numbers that rejected it."""
    out: dict = {}
    for path in sorted(root.glob("b*/rung-*/run-*.json")):
        record = json.loads(path.read_text(encoding="utf-8"))
        if "curve" not in record or "arm" not in record:
            continue
        windows_file = path.parent / str(record["windows_file"])
        windows = saved_window_rows(windows_file.read_text(encoding="utf-8").splitlines(),
                                   int(record.get("window_ms") or DEFAULT_WINDOW_MS))
        workdir = str(path.parent.relative_to(root))
        for rate in sorted({w["rate_rps"] for w in windows}):
            key = (workdir, record["arm"], float(rate))
            bad = [{"window": w["window"], "reason": w.get("reject_reason"),
                    "lag_p99_us": w.get("lag_p99_us"),
                    "service_p99_us": w.get("service_p99_us", w.get("p99_us")),
                    "windows": sum(1 for x in windows if x["rate_rps"] == rate)}
                   for w in windows
                   if w["rate_rps"] == rate
                   and (w.get("invalid") or w.get("errors") or w.get("reject_reason"))]
            if bad:
                out[key] = bad
    return out


def build(root: Path, ceiling_us: int) -> dict:
    recorded = as_recorded(root)
    cf = counterfactual(root, ceiling_us)
    rejects = rejected_window_detail(root)

    keys = sorted(recorded, key=lambda k: (k[0], float(recorded[k]["offered_rps"]), k[1]))
    rows = []
    for key in keys:
        rec = recorded[key]
        c = cf[key]
        rows.append({
            "workdir": rec["workdir"],
            "block": int(rec["workdir"].split("/")[0][1:]),
            "rung": float(rec["offered_rps"]),
            "arm": rec["arm"],
            "achieved_rps": rec["achieved_rps"],
            "record_sustained": rec["recorded_sustained"],
            "recomputed_sustained": rec["sustained"],
            "sustained_primary": rec["sustained_primary"],
            "service_ceiling_reported_windows": rec["service_ceiling_reported_windows"],
            "lag_p99_us": rec["lag_p99_us"],
            "service_p99_us": rec["service_p99_us"],
            "reject_reasons": c["reject_reasons"],
            "rejected_windows": rejects.get(key, []),
            "windows": c["windows"],
            "counterfactual": c,
            "record_file": f"runs/{rec['workdir']}/run-{rec['arm']}.json",
            "windows_file": f"runs/{rec['workdir']}/windows-{rec['arm']}.jsonl",
        })

    mismatch = [row for row in rows
                if any(recorded[(row["workdir"], row["arm"], row["rung"])][new]
                       != recorded[(row["workdir"], row["arm"], row["rung"])][old]
                       for new, old in RECOMPUTE_FIELDS)]
    differ = [row for row in rows if row["recomputed_sustained"] != row["sustained_primary"]]
    cf_differ = [row for row in rows
                 if row["sustained_primary"] != row["counterfactual"]["sustained_with_ceiling"]]
    return {"rows": rows, "mismatch": mismatch, "differ": differ,
            "counterfactual_differ": cf_differ, "ceiling_us": ceiling_us}


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------
def fmt(value) -> str:
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, float) and value.is_integer():
        return f"{value:.0f}"
    if isinstance(value, float):
        return f"{value:.1f}"
    return str(value)


def per_row_table(rows: list[dict]) -> str:
    out = [
        "| block | rung | arm | `sustained` (both ceilings, as recorded) | `sustained_primary`"
        " (lag rule) | `service_ceiling_reported_windows` | lag p99 us | service p99 us |"
        " windows | where each value comes from |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for row in rows:
        out.append(
            f"| {row['block']} | {row['rung']:.0f} | {row['arm']}"
            f" | {fmt(row['recomputed_sustained'])}"
            f" | {fmt(row['sustained_primary'])}"
            f" | {row['service_ceiling_reported_windows']}"
            f" | {row['lag_p99_us']} | {row['service_p99_us']} | {row['windows']}"
            f" | `{row['record_file']}` field `curve[0].sustained` / `.lag_p99_us` / `.p99_us`;"
            f" `{row['windows_file']}` via `cold_0b.summarize_curve(w, None)` |")
    return "\n".join(out)


def per_cell_table(rows: list[dict]) -> str:
    cells: dict = {}
    for row in rows:
        cell = cells.setdefault((row["arm"], row["rung"]), [])
        cell.append(row)
    lines = [
        "| arm | rung | blocks | `sustained` (both ceilings) | `sustained_primary` (lag rule)"
        " | `service_ceiling_reported_windows` (sum) | lag p99 us per block | verdicts differ |",
        "|---|---|---|---|---|---|---|---|",
    ]
    for (arm, rung) in sorted(cells, key=lambda key: (key[1], ARMS.index(key[0]))):
        block_rows = sorted(cells[(arm, rung)], key=lambda row: row["block"])
        s = sum(1 for row in block_rows if row["recomputed_sustained"])
        p = sum(1 for row in block_rows if row["sustained_primary"])
        svc = sum(row["service_ceiling_reported_windows"] for row in block_rows)
        lags = ", ".join(str(row["lag_p99_us"]) for row in block_rows)
        differ = sum(1 for row in block_rows
                     if row["recomputed_sustained"] != row["sustained_primary"])
        lines.append(f"| {arm} | {rung:.0f} | {len(block_rows)} | {s}/{len(block_rows)}"
                     f" | {p}/{len(block_rows)} | {svc} | {lags} | {differ} |")
    return "\n".join(lines)


def median(values: list[float]) -> float | None:
    return statistics.median(values) if values else None


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--root", default=str(RUNS))
    ap.add_argument("--out", default=str(Path(__file__).resolve().parent))
    ap.add_argument("--ceiling-us", type=int, default=10_000,
                    help="the service ceiling the both-ceilings counterfactual is taken with")
    args = ap.parse_args(argv)

    root = Path(args.root).resolve()
    out = Path(args.out)
    result = build(root, args.ceiling_us)
    rows = result["rows"]

    (out / "rows.json").write_text(json.dumps(result, sort_keys=True, indent=2) + "\n",
                                   encoding="utf-8")
    (out / "rows.md").write_text(per_row_table(rows) + "\n", encoding="utf-8")
    (out / "table-decided.md").write_text(per_cell_table(rows) + "\n", encoding="utf-8")

    print(f"{len(rows)} rows re-derived from their own window samples")
    print(f"  {len(result['mismatch'])} differ from the recorded curve"
          f" (achieved_rps, lag p99, service p99, sustained)")
    print(f"  {len(result['differ'])} rows where sustained != sustained_primary")
    print(f"  {len(result['counterfactual_differ'])} rows where sustained_primary differs from the"
          f" both-ceilings reading at a {args.ceiling_us} us service ceiling")
    print()
    print(per_cell_table(rows))
    print()
    print("service ceiling reported windows, total:"
          f" {sum(row['service_ceiling_reported_windows'] for row in rows)} of {len(rows)} rows"
          " non-zero")
    print("rung-level service p99 above 10000 us:"
          f" {sum(1 for row in rows if row['service_p99_us'] > 10_000)} of {len(rows)} rows")
    print("rung-level service p99 above 19821 us (10 ms + the 9821 us fsync floor):"
          f" {sum(1 for row in rows if row['service_p99_us'] > 19_821)} of {len(rows)} rows")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
