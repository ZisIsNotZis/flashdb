#!/usr/bin/env python3
"""Draw the M1 comparison figure from the committed per-rung arm records.

`docs/roadmap.md` M1 asks for "a comparison table + figure". The table is `report.md` §2-§3; this
script draws the figure: schedule-lag p99 against offered rate, one point per arm per rung, with the
sustained verdict as the marker fill.

Every number is read from the committed `run-<arm>.json` under each `rung-<rps>/` directory
(`curve[0].lag_p99_us`, `curve[0].sustained`, `curve[0].reject_reasons`, `data_bytes`), so the
figure is a view of the run records rather than a second home for them. No plotting dependency is
used: the SVG is emitted directly, so the file is regenerable from this script and the records on a
box with no Python plotting stack.

Usage (from the repo root)::

    uv run python scripts/m1-latency-figure.py

writes `.scratch/80-external-baselines/figure.svg`.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path

ARMS = ("flashdb-declared", "flashdb-learned", "sqlite", "postgres")
REGIME_ARMS = ("flashdb-declared", "sqlite", "postgres")
COLOURS = {
    "flashdb-declared": "#1f5fa8",
    "flashdb-learned": "#2a9d4a",
    "sqlite": "#d1701a",
    "postgres": "#b02418",
}

W, H = 1040, 660
TOP, BOTTOM = 152, 500
LEFT_X0, LEFT_X1 = 96, 590
RIGHT_X0, RIGHT_X1 = 680, 1000
Y_MIN_LOG, Y_MAX_LOG = 3.0, 6.5


def read_json(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


def load_rungs(root: Path) -> dict[int, dict]:
    """`offered_rps -> {arm: curve point}` from the committed per-rung records."""
    rungs: dict[int, dict] = {}
    for path in sorted(root.glob("rung-*"), key=lambda p: int(p.name.split("-")[1])):
        offered = int(path.name.split("-")[1])
        arms = {}
        for arm in ARMS:
            record = path / f"run-{arm}.json"
            if not record.exists():
                continue
            data = read_json(record)
            curve = data["curve"][0] if data["curve"] else {}
            arms[arm] = {
                "lag_p99_us": curve.get("lag_p99_us"),
                "sustained": curve.get("sustained"),
                "invalid": curve.get("invalid_windows"),
                "reject_reasons": curve.get("reject_reasons", {}),
                "data_bytes": data.get("data_bytes") or data.get("instance_bytes"),
                "engine_cap_mib": data.get("engine_cap_mib"),
            }
        rungs[offered] = arms
    return rungs


def data_to_cap(arm: dict) -> float | None:
    if arm.get("data_bytes") and arm.get("engine_cap_mib"):
        return arm["data_bytes"] / (arm["engine_cap_mib"] << 20)
    return None


def y_of(value_us: float) -> float:
    log = max(Y_MIN_LOG, min(Y_MAX_LOG, math.log10(value_us)))
    return BOTTOM - (log - Y_MIN_LOG) / (Y_MAX_LOG - Y_MIN_LOG) * (BOTTOM - TOP)


def fnum(value: float) -> str:
    return f"{value:.0f}"


def marker(x: float, y: float, colour: str, sustained: bool) -> list[str]:
    if sustained:
        return [f'<circle cx="{x:.1f}" cy="{y:.1f}" r="7" fill="{colour}"/>']
    return [f'<circle cx="{x:.1f}" cy="{y:.1f}" r="7" fill="#ffffff" stroke="{colour}"'
            f' stroke-width="2.5"/>']


def label(x: float, y: float, text: str, **attrs) -> str:
    extra = " ".join(f'{k}="{v}"' for k, v in attrs.items())
    return f'<text x="{x:.1f}" y="{y:.1f}" {extra}>{text}</text>'


def render(ladder_root: Path, regime_root: Path) -> str:
    ladder = load_rungs(ladder_root)
    regime = load_rungs(regime_root)
    levels = sorted(ladder)
    regime_level = sorted(regime)[0]
    parts: list[str] = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}"'
        f' viewBox="0 0 {W} {H}" font-family="DejaVu Sans, Helvetica, Arial, sans-serif">',
        f'<rect x="0" y="0" width="{W}" height="{H}" fill="#ffffff"/>',
        label(28, 34, "M1 - external baseline scoreboard: schedule-lag p99 vs offered rate",
              **{"font-size": 20, "font-weight": "bold", "fill": "#111111"}),
        label(28, 56, "One point per arm per rung, read from the committed run-&lt;arm&gt;.json"
                       " records. Filled marker = sustained under the pre-registered rule",
              **{"font-size": 11, "fill": "#333333"}),
        label(28, 72, "(lag p99 &lt;= 10,000 us, the 3-window growth streak, zero errors); hollow"
                       " marker = at least one rejected window (counts in report.md Table A).",
              **{"font-size": 11, "fill": "#333333"}),
        label(28, 88, "Dashed line = the 10,000 us lag ceiling. Markers at one offered level are"
                       " spread horizontally so they do not overprint; the tick marks the level.",
              **{"font-size": 11, "fill": "#333333"}),
    ]

    # Shared log y axis and grid.
    for exp in (3, 4, 5, 6):
        y = y_of(10 ** exp)
        parts.append(f'<line x1="{LEFT_X0}" y1="{y:.1f}" x2="{RIGHT_X1}" y2="{y:.1f}"'
                     f' stroke="#e3e3e3" stroke-width="1"/>')
        parts.append(label(LEFT_X0 - 12, y + 4, f"1e{exp}", **{"font-size": 11, "fill": "#444444",
                                                              "text-anchor": "end"}))
    ceiling = 10_000.0
    cy = y_of(ceiling)
    parts.append(f'<line x1="{LEFT_X0}" y1="{cy:.1f}" x2="{RIGHT_X1}" y2="{cy:.1f}"'
                 f' stroke="#777777" stroke-width="1.5" stroke-dasharray="7 5"/>')
    parts.append(label(RIGHT_X1 - 4, cy - 6, "10,000 us ceiling",
                       **{"font-size": 10, "fill": "#555555", "text-anchor": "end"}))
    parts.append(label(24, (TOP + BOTTOM) / 2, "lag p99 (us, log scale)",
                       **{"font-size": 12, "fill": "#222222", "text-anchor": "middle",
                          "transform": f"rotate(-90 24 {(TOP + BOTTOM) / 2:.1f})"}))

    # Legend: the four arms, then the two marker meanings.
    lx, ly = LEFT_X0, 120
    for i, arm in enumerate(ARMS):
        x = lx + i * 148
        parts.append(f'<line x1="{x}" y1="{ly}" x2="{x + 16}" y2="{ly}" stroke="{COLOURS[arm]}"'
                     f' stroke-width="2.5"/>')
        parts.extend(marker(x + 8, ly, COLOURS[arm], True))
        parts.append(label(x + 22, ly + 4, arm, **{"font-size": 11, "fill": "#222222"}))
    x = lx + 4 * 148
    parts.extend(marker(x + 8, ly, "#444444", True))
    parts.append(label(x + 20, ly + 4, "sustained", **{"font-size": 11, "fill": "#222222"}))
    parts.extend(marker(x + 88, ly, "#444444", False))
    parts.append(label(x + 100, ly + 4, "rejected", **{"font-size": 11, "fill": "#222222"}))

    # Panel A: the ladder (run A), one x position per offered level.
    flashdb_a = ladder[levels[0]].get("flashdb-declared", {})
    ratio_a = data_to_cap(flashdb_a)
    parts.append(label(LEFT_X0, TOP - 10,
                       f"A. Ladder - run A (flashdb data {ratio_a:.3f}x of the cap, PROVISIONAL)"
                       if ratio_a else "A. Ladder - run A (PROVISIONAL)",
                       **{"font-size": 12, "font-weight": "bold", "fill": "#111111"}))
    xs = {level: LEFT_X0 + 30 + i * (LEFT_X1 - LEFT_X0 - 60) / (len(levels) - 1)
          for i, level in enumerate(levels)}
    for level in levels:
        parts.append(f'<line x1="{xs[level]:.1f}" y1="{TOP - 4}" x2="{xs[level]:.1f}"'
                     f' y2="{BOTTOM + 6}" stroke="#f0f0f0" stroke-width="1"/>')
        parts.append(label(xs[level], BOTTOM + 26, fnum(level),
                           **{"font-size": 11, "fill": "#222222", "text-anchor": "middle"}))
    parts.append(label((LEFT_X0 + LEFT_X1) / 2, BOTTOM + 48,
                       "offered rate (rps) - one rung per column, 20 windows each",
                       **{"font-size": 12, "fill": "#222222", "text-anchor": "middle"}))
    # At one offered level three or four arms can sit within a few pixels of each other in p99
    # (postgres 8,472 us and flashdb-learned 7,623 us at 200 rps, for instance), so the markers are
    # spread horizontally around the column. The column tick still marks the offered level.
    offsets = {arm: (i - (len(ARMS) - 1) / 2) * 16 for i, arm in enumerate(ARMS)}
    for arm in ARMS:
        points = []
        for level in levels:
            point = ladder[level].get(arm)
            if point and point.get("lag_p99_us"):
                points.append((xs[level] + offsets[arm], y_of(point["lag_p99_us"]), point))
        if len(points) > 1:
            parts.append('<polyline points="' +
                         " ".join(f"{x:.1f},{y:.1f}" for x, y, _ in points) +
                         f'" fill="none" stroke="{COLOURS[arm]}" stroke-width="2"'
                         ' stroke-opacity="0.75"/>')
        for x, y, point in points:
            parts.extend(marker(x, y, COLOURS[arm], bool(point["sustained"])))

    # Panel B: the regime rung (run B), three arms at one offered level.
    regime_arms = [arm for arm in REGIME_ARMS if arm in regime[regime_level]]
    flashdb_b = regime[regime_level].get("flashdb-declared", {})
    ratio_b = data_to_cap(flashdb_b)
    parts.append(label(RIGHT_X0 - 30, TOP - 10,
                       f"B. Regime rung - run B (flashdb data {ratio_b:.3f}x of the cap)"
                       if ratio_b else "B. Regime rung - run B",
                       **{"font-size": 12, "font-weight": "bold", "fill": "#111111"}))
    rxs = {arm: RIGHT_X0 + 20 + i * (RIGHT_X1 - RIGHT_X0 - 40) / max(1, len(regime_arms) - 1)
           for i, arm in enumerate(regime_arms)}
    for arm in regime_arms:
        x = rxs[arm]
        point = regime[regime_level][arm]
        y = y_of(point["lag_p99_us"])
        parts.append(f'<line x1="{x:.1f}" y1="{TOP - 4}" x2="{x:.1f}" y2="{BOTTOM + 6}"'
                     f' stroke="#f0f0f0" stroke-width="1"/>')
        parts.extend(marker(x, y, COLOURS[arm], bool(point["sustained"])))
        parts.append(label(x, y - 14, f"{point['lag_p99_us']:,} us",
                           **{"font-size": 10, "fill": "#333333", "text-anchor": "middle"}))
        if not point["sustained"]:
            parts.append(label(x, y + 22, f"{point['invalid']} rej.",
                               **{"font-size": 10, "fill": "#666666",
                                  "text-anchor": "middle"}))
        short = arm.replace("flashdb-", "fdb-")
        parts.append(label(x, BOTTOM + 26, short,
                           **{"font-size": 11, "fill": "#222222", "text-anchor": "middle"}))
    parts.append(label((RIGHT_X0 + RIGHT_X1) / 2, BOTTOM + 48,
                       f"arm (one offered level: {fnum(regime_level)} rps, 20 windows)",
                       **{"font-size": 12, "fill": "#222222", "text-anchor": "middle"}))

    # Provenance footer: the records this view was computed from, and the rule it marks.
    parts.append(label(28, BOTTOM + 88,
                       f"Source: {ladder_root}/rung-*/run-&lt;arm&gt;.json and"
                       f" {regime_root}/rung-*/run-&lt;arm&gt;.json (curve[0]).",
                       **{"font-size": 10, "fill": "#555555"}))
    parts.append(label(28, BOTTOM + 102,
                       "Marker fill is each record's own sustained verdict; the rule is the"
                       " pre-registered one (10,000 us lag p99, the 3-window growth streak, zero"
                       " errors), scored per arm by cold_0b.summarize_curve.",
                       **{"font-size": 10, "fill": "#555555"}))
    parts.append(label(28, BOTTOM + 116,
                       "Regenerate: uv run python scripts/m1-latency-figure.py",
                       **{"font-size": 10, "fill": "#555555"}))
    parts.append("</svg>")
    return "\n".join(parts) + "\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description="draw the M1 lag-p99 figure from the run records")
    ap.add_argument("--ladder", default=".scratch/80-external-baselines/runs/m1-measure")
    ap.add_argument("--regime", default=".scratch/80-external-baselines/runs/m1-10x")
    ap.add_argument("--out", default=".scratch/80-external-baselines/figure.svg")
    args = ap.parse_args(argv)
    ladder_root = Path(args.ladder)
    regime_root = Path(args.regime)
    for root in (ladder_root, regime_root):
        if not any(root.glob("rung-*")):
            raise SystemExit(f"no rung-* records under {root}")
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(render(ladder_root, regime_root), encoding="utf-8")
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(__import__("sys").argv[1:]))
