#!/usr/bin/env python3
"""89 — score the paired 200 rps block with the project's own reducer and the decided rule.

    python3 .scratch/89-m3b-paired-200/score-pair.py <runs-dir>
    python3 .scratch/89-m3b-paired-200/score-pair.py --manifest <runs-dir>
    python3 .scratch/89-m3b-paired-200/score-pair.py --arm-facts <arm.out>
    python3 .scratch/89-m3b-paired-200/score-pair.py --pair-valid <declared.out> <indexed.out>

Each arm file is the driver's raw per-window JSON (one object per line) with a `#`-comment header
holding the restore-verification line and the start and end loadavg. The rows go through
`cold_0b.window_records` and `cold_0b.summarize_curve`, exactly the reducer the M1 record was scored
with, so "sustained" here is the same quantity that produced the 3-of-5; `sustained_primary` is the
decided lag-only rule (`docs/2_GOAL.md` §1) and is the one the verdict is read on.

Clause (b): "at 200 rps the indexed arm sustains at least as many paired blocks as declared".

WP-17's harvest rule is here as well as in `paired-200.sh`: the unit of evidence is a COMPLETE pair
whose two arms both started under the 2.0 trigger, recorded `errors=0`, and whose restore verified.
A pair that fails any of those is excluded and named; one to four valid pairs are never a verdict —
the pre-registered unit is five paired blocks (`docs/0_ROADMAP.md` WP-2).

Exit 0 = met, 1 = falsified, 2 = a helper argument/usage error, 3 = the data cannot decide it,
including `partial: N/5` for N < 5.
"""
from __future__ import annotations

import json
import re
import sys
from datetime import datetime
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "harness"))

from flashdb_harness.cold_0b import summarize_curve, window_records  # noqa: E402

WINDOW_MS = 1000
MIN_PAIRS = 5                     # WP-2's pre-registered unit: five paired blocks
LOADAVG_LIMIT = 2.0               # the granted quiet-window trigger
INVALID_MARKER = "# INVALID - do not cite"
START_RE = re.compile(r"^# loadavg at start:\s*(.*)$")
END_RE = re.compile(r"^# loadavg at end:\s*(.*)$")
SUMMARY_ERRORS_RE = re.compile(r"\berrors=(\d+)\b")
ARM_NAME_RE = re.compile(r"^p(\d+)-(declared|indexed)\.out$")


# --------------------------------------------------------------------------------------------
# What an arm file says about its own admissibility. One parser, so the shell, the manifest and
# the scorer cannot disagree about whether an arm started under the trigger with zero errors.
# --------------------------------------------------------------------------------------------
def _first_float(text: str | None) -> float | None:
    if not text:
        return None
    m = re.match(r"\s*(-?[0-9]+(?:\.[0-9]+)?)", text)
    return float(m.group(1)) if m else None


def arm_facts(path: Path) -> dict:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    start_line = next((m.group(1) for line in lines if (m := START_RE.match(line))), None)
    end_line = next((m.group(1) for line in reversed(lines) if (m := END_RE.match(line))), None)
    restore_line = next((line for line in lines if line.startswith("# restore:")), "")
    json_objects: list[dict] = []
    for line in lines:
        if not line.startswith("{"):
            continue
        try:
            json_objects.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    summary_errors = None
    summary_line = next((line for line in lines if line.startswith("load_summary ")), None)
    if summary_line is not None:
        m = SUMMARY_ERRORS_RE.search(summary_line)
        if m:
            summary_errors = int(m.group(1))
    json_errors = [int(o["errors"]) for o in json_objects if "errors" in o]
    errors = max(json_errors + ([summary_errors] if summary_errors is not None else []),
                 default=None)
    return {
        "file": path.name,
        "marked_invalid": bool(lines) and lines[0].startswith(INVALID_MARKER),
        "start_loadavg": _first_float(start_line),
        "start_line": start_line or "",
        "end_loadavg": _first_float(end_line),
        "restore": restore_line,
        "restore_verified": restore_line.startswith("# restore: verified"),
        "windows": len(json_objects),
        "errors": errors,
    }


def validate_pair(files: dict[str, Path]) -> tuple[bool, list[str]]:
    """A pair is evidence only when both arms exist and each passes every admissibility rule."""
    reasons: list[str] = []
    for variant in ("declared", "indexed"):
        if variant not in files:
            reasons.append(f"missing {variant} arm")
    for variant in ("declared", "indexed"):
        path = files.get(variant)
        if path is None:
            continue
        f = arm_facts(path)
        stem = path.stem
        if f["marked_invalid"]:
            reasons.append(f"{stem}: marked INVALID")
        if not f["restore_verified"]:
            reasons.append(f"{stem}: restore not verified")
        start = f["start_loadavg"]
        if start is None:
            reasons.append(f"{stem}: no start loadavg")
        elif start >= LOADAVG_LIMIT:
            reasons.append(f"{stem}: start loadavg {start:.2f} >= {LOADAVG_LIMIT:.1f}")
        errors = f["errors"]
        if errors is None:
            reasons.append(f"{stem}: no errors= reading")
        elif errors != 0:
            reasons.append(f"{stem}: errors={errors} != 0")
        if f["windows"] == 0:
            reasons.append(f"{stem}: no window JSON")
    return (not reasons, reasons)


def group_arms(runs: Path) -> dict[int, dict[str, Path]]:
    arms: dict[int, dict[str, Path]] = {}
    for f in sorted(runs.glob("p*-*.out")):
        m = ARM_NAME_RE.match(f.name)
        if not m:
            continue
        arms.setdefault(int(m.group(1)), {})[m.group(2)] = f
    return arms


# --------------------------------------------------------------------------------------------
# Scoring: the M1 reducer, unchanged.
# --------------------------------------------------------------------------------------------
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


def score(runs: Path) -> int:
    arms = group_arms(runs)
    if not arms:
        print(f"cannot decide: no p*-*.out under {runs}")
        return 3

    valid: dict[int, dict[str, Path]] = {}
    for pair in sorted(arms):
        ok, reasons = validate_pair(arms[pair])
        if ok:
            valid[pair] = arms[pair]
        else:
            print(f"excluded: pair {pair} — " + "; ".join(reasons))

    n = len(valid)
    print(f"valid paired blocks: {n} (the pre-registered unit is {MIN_PAIRS})")
    if n < MIN_PAIRS:
        print(f"partial: {n}/{MIN_PAIRS}")
        print(f"cannot decide: {n} complete valid pair(s) is below the {MIN_PAIRS}-pair unit; "
              "no verdict is read from a partial series")
        return 3
    nums = sorted(valid)
    if nums != list(range(1, n + 1)):
        print(f"cannot decide: valid pairs are {nums}, not the contiguous series 1..{n}; "
              "a gap means a pair is missing and the blocks were not taken as one unit")
        return 3

    print(f"{'pair':>4} {'arm':>9} {'achieved':>9} {'lag_rej':>7} {'reqs':>5} "
          f"{'sustained':>9} {'primary':>8}  loadavg start")
    declared_blocks = indexed_blocks = 0
    for pair in nums:
        for variant in ("declared", "indexed"):
            row = load_arm(valid[pair][variant])
            print(f"{pair:>4} {variant:>9} {row['achieved_rps']:>9.1f} "
                  f"{row['lag_ceiling_rejected_windows']:>7} {row['windows_seen']:>5} "
                  f"{str(row['sustained']):>9} {str(row['sustained_primary']):>8}  "
                  f"{row['loadavg_start'].split()[0] if row['loadavg_start'] else '?'}")
            if variant == "declared":
                declared_blocks += int(bool(row["sustained_primary"]))
            else:
                indexed_blocks += int(bool(row["sustained_primary"]))

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


# --------------------------------------------------------------------------------------------
# Manifest: the durable record of which pairs are evidence and why the others are not.
# --------------------------------------------------------------------------------------------
def _entry(pair: int, files: dict[str, Path], source: str) -> dict:
    ok, reasons = validate_pair(files)
    entry: dict = {
        "pair": pair,
        "source": source,
        "valid": ok,
        "reason": None if ok else "; ".join(reasons),
    }
    for variant in ("declared", "indexed"):
        path = files.get(variant)
        if path is None:
            entry[variant] = None
            continue
        f = arm_facts(path)
        entry[variant] = {
            "file": path.name,
            "start_loadavg": f["start_loadavg"],
            "errors": f["errors"],
            "restore_verified": f["restore_verified"],
            "windows": f["windows"],
            "marked_invalid": f["marked_invalid"],
        }
    return entry


def write_manifest(runs: Path) -> int:
    entries: list[dict] = []
    for pair, files in group_arms(runs).items():
        entries.append(_entry(pair, files, runs.name))
    # A refused run's files are moved into runs/invalid-<stamp>/ and marked; keep them named in the
    # manifest so the record says which pair failed and why, without letting them count.
    invalid_dirs = sorted(p for p in runs.glob("invalid-*") if p.is_dir())
    for d in invalid_dirs:
        groups: dict[int, dict[str, Path]] = {}
        for f in sorted(d.glob("p*-*.out")):
            m = ARM_NAME_RE.match(f.name)
            if m:
                groups.setdefault(int(m.group(1)), {})[m.group(2)] = f
        for pair, files in groups.items():
            entry = _entry(pair, files, d.name)
            marker = next(
                (line for f in files.values()
                 for line in f.read_text(encoding="utf-8", errors="replace").splitlines()[:1]
                 if line.startswith(INVALID_MARKER)),
                INVALID_MARKER,
            )
            entry["reason"] = f"{marker} — {entry['reason'] or 'incomplete pair'}"
            entries.append(entry)
    entries.sort(key=lambda e: (e["pair"], e["source"]))
    manifest = {
        "schema": "flashdb.paired-200.pairs.v1",
        "updated": datetime.now().astimezone().isoformat(timespec="seconds"),
        "unit_pairs": MIN_PAIRS,
        "loadavg_limit": LOADAVG_LIMIT,
        "pairs": entries,
    }
    out = runs / "pairs.json"
    out.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    valid = [e for e in entries if e["valid"]]
    print(f"manifest: {out} — {len(valid)} valid pair(s), "
          f"{len(entries) - len(valid)} invalid/incomplete record(s)")
    return 0


def pair_valid_cli(declared: Path, indexed: Path) -> int:
    files: dict[str, Path] = {}
    if declared.exists():
        files["declared"] = declared
    if indexed.exists():
        files["indexed"] = indexed
    ok, reasons = validate_pair(files)
    if ok:
        return 0
    print("; ".join(reasons), file=sys.stderr)
    return 2


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] == "--arm-facts":
        if len(argv) != 3:
            print("usage: score-pair.py --arm-facts <arm.out>", file=sys.stderr)
            return 2
        print(json.dumps(arm_facts(Path(argv[2]))))
        return 0
    if len(argv) > 1 and argv[1] == "--pair-valid":
        if len(argv) != 4:
            print("usage: score-pair.py --pair-valid <declared.out> <indexed.out>", file=sys.stderr)
            return 2
        return pair_valid_cli(Path(argv[2]), Path(argv[3]))
    if len(argv) > 1 and argv[1] == "--manifest":
        if len(argv) != 3:
            print("usage: score-pair.py --manifest <runs-dir>", file=sys.stderr)
            return 2
        return write_manifest(Path(argv[2]))
    runs = Path(argv[1] if len(argv) > 1 else ".scratch/89-m3b-paired-200/runs")
    return score(runs)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
