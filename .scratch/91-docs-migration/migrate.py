#!/usr/bin/env python3
"""91 — migrate the doc base into the level scheme, without retyping a single sourced line.

    python3 .scratch/91-docs-migration/migrate.py

The rule (`/home/z/vibe/AGENTS.md` §Docs): the level picks the file, `docs/<N>_<NAME>.md`; the H1
zoning inside each file is the source tier, `EXPLICIT` then `CONFIRMED` then `INFERRED`, lower index
first; one home per meaning, so a file that mixes levels is split and a section that restates a
higher level leaves.

What this script does is mechanical on purpose: it splits each source doc at its H2 headings, assigns
each section (by exact heading) to one target file and one tier in `PLAN`, copies the section's lines
byte for byte, and writes `docs-dropped.txt` for the sections `DROP` names with the reason. It refuses
to run if any source section is neither assigned nor dropped, so a silent loss is impossible; the
loss that is intended is written down where the checker reads it.

Rejected branches, recorded because they cost real time to price:

- *Rename only, no tier zoning* (keep every doc as it is, number the filenames): cheaper, and it leaves
  `design.md` at three levels at once and no source provenance anywhere, which is the defect the rule
  exists to remove. Rejected.
- *Full editorial rewrite of each level file* (merge sentences, drop restatements by hand across 2,078
  lines): the highest ceiling and the highest chance of losing a measured number or a revision pin in
  prose; the redo would not be reconstructible. Rejected in favour of section-granularity moves plus a
  short list of adjudicated deletions.
- *Sentence-level tier zoning*: the right end state for the few genuinely mixed sections, done by hand
  after this script runs; doing it for every section would re-edit text that one tier already covers.
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DOCS = REPO / "docs"
TIERS = ["EXPLICIT", "CONFIRMED", "INFERRED"]

# --- the plan: (target file, tier, source file, exact H2 title); `None` is the source's preamble.
# --- Ordered as the file should read inside each tier.
PLAN: list[tuple[str, str, str, str | None]] = [
    # 0 — the roadmap: milestones, gates, workpackages, decisions in force.
    ("0_ROADMAP.md", "EXPLICIT", "docs/roadmap.md", "Decisions (2026-10-19, delegated author authority)"),
    ("0_ROADMAP.md", "EXPLICIT", "docs/roadmap.md", "Non-goals for v0 (explicit)"),
    ("0_ROADMAP.md", "CONFIRMED", "docs/roadmap.md", "Decisions (2026-10-21, the node that now owns this roadmap)"),
    ("0_ROADMAP.md", "INFERRED", "docs/roadmap.md", None),
    ("0_ROADMAP.md", "INFERRED", "docs/roadmap.md", "Goal and done-criterion"),
    ("0_ROADMAP.md", "INFERRED", "docs/roadmap.md", "Where we are (measured)"),
    ("0_ROADMAP.md", "INFERRED", "docs/roadmap.md", "Milestones"),
    ("0_ROADMAP.md", "INFERRED", "docs/roadmap.md", "Current milestone"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", None),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "M3 — access-path parity (current)"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "M5 — fair external re-run (server leg advanced; the run waits on the quiet window)"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "M4 — gap attribution (after M3)"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "M3 status"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "Known blockers"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "Not planned (bounded backlog)"),
    ("0_ROADMAP.md", "INFERRED", "docs/plan/workplan.md", "Completion evidence"),
    # 1 — why the work is worth doing.
    ("1_PROBLEM.md", "INFERRED", "docs/design.md", None),
    ("1_PROBLEM.md", "INFERRED", "docs/design.md", "Thesis"),
    ("1_PROBLEM.md", "INFERRED", "docs/prior-art.md", None),
    ("1_PROBLEM.md", "INFERRED", "docs/prior-art.md", "What is established — and therefore not a contribution"),
    ("1_PROBLEM.md", "INFERRED", "docs/prior-art.md", "What may still be unclaimed"),
    ("1_PROBLEM.md", "INFERRED", "docs/prior-art.md", "The claim, demoted"),
    ("1_PROBLEM.md", "INFERRED", "docs/prior-art.md", "Verification and reading order"),
    # 2 — success metrics and scope.
    ("2_GOAL.md", "INFERRED", "docs/objective.md", None),
    ("2_GOAL.md", "EXPLICIT", "docs/objective.md", "0. The product bar: an end-to-end win over sqlite and postgres (author-decided)"),
    ("2_GOAL.md", "EXPLICIT", "docs/objective.md", "Endurance is a constraint, not a price (author, 2026-10-06)"),
    ("2_GOAL.md", "CONFIRMED", "docs/design.md", "Goals"),
    ("2_GOAL.md", "CONFIRMED", "docs/design.md", "Non-goals"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "1. The internal objective: angriness"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "Why a soft penalty and not a hard constraint"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "SLO(load): the anchor problem"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "The penalty curve"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "The other terms"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "Bursty load: weights are time series"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "Form of the decision"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "Worked example"),
    ("2_GOAL.md", "INFERRED", "docs/objective.md", "Operational reading"),
    # 5 — the product surface: what the learner does, and the decision forks still open.
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", None),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "One rule above all"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "The trace"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Pattern keys"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Weights are time series"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "The tier ladder"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Gray release"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Layout and schema changes are deploys"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Prediction and predication"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "The advisor"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Push and the human loop"),
    ("5_PRODUCT.md", "INFERRED", "docs/learning.md", "Off-policy evaluation"),
    ("5_PRODUCT.md", "INFERRED", "docs/design.md", "Open questions"),
    # 6 — subsystem boundaries, invariants, and the one architecture analysis there is.
    ("6_ARCHITECTURE.md", "CONFIRMED", "docs/design.md", "Invariants"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/design.md", "Architecture at a glance"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", None),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "1. Records and revision"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "2. Per-request phase ledger at 200 rps"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "3. Theoretical floor per phase"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "4. Residual and classification"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "5. Why the competitors are faster, per commit"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "6. Format forensics"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "7. Ranked fix list (ranking is inference; facts named)"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "8. What would falsify each attribution"),
    ("6_ARCHITECTURE.md", "INFERRED", "docs/plan/gap-analysis.md", "9. Arithmetic provenance"),
    # 7 — the contracts.
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", None),
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", "Contract 1 — Logical model"),
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", "Contract 2 — Transaction model"),
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", "Contract 3 — Durability and failure"),
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", "Contract 4 — Layout contract"),
    ("7_INTERFACE.md", "CONFIRMED", "docs/contracts.md", "Reserved interfaces (frozen now)"),
    # 8 — what is inside the engine.
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", None),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Key space — one LSM, four prefix families"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Foreground commit — one barrier (SS-01 resolved)"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Snapshots and validation (SS-05 resolved, honestly)"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Tiles, pages, access granule (SS-03 resolved)"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Compaction, manifest, checkpoint, recovery (SS-02, SS-21 resolved)"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Write path and budget (SS-04 resolved)"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "What the learner controls in v0"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Corrections recorded against earlier documents"),
    ("8_DETAIL.md", "INFERRED", "docs/engine.md", "Resolution checklist"),
    # 9 — how it is built, tested, measured, and what is open.
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", None),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", "Workloads"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", "Harness"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", "Baselines and the gap-attribution protocol"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", "Build order"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/dev-loop.md", "Evidence"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", None),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "The five layers that the old conflict collapsed"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "Claim → producer"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "Absent, with the consequence"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "Measured verdicts and their conditions"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "Statements corrected"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/evidence.md", "What stays unverified, and what would settle it"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", None),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Engine correctness"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Read/write path performance"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Memory and footprint"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Scale and capacity"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Benchmark instrument and measurement"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Learner / thesis"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Docs and SSOT"),
    ("9_IMPLEMENTATION.md", "INFERRED", "docs/plan/open-items.md", "Critical path to an end-to-end win over sqlite/postgres"),
]

# --- sections that leave the doc base, with the rule that removes them. The line text of each is
# --- computed here, so `docs-dropped.txt` is never hand-maintained and never drifts.
DROP: list[tuple[str, str | None, str]] = [
    ("docs/design.md", "Prior art", "restates prior-art.md, which is a section of the same level file: two homes for one meaning"),
    ("docs/plan/workplan.md", "Decisions in force (2026-10-19, delegated author authority)", "restates the roadmap's decisions 1-4 in the same level file: two homes for one meaning"),
]


DROP_RX = [
    (re.compile(r"^Budget: [\d,]+ lines / [\d,]+ chars"),
     "the per-source length budget retires with the source's filename; a level file's size is governed by its level, not by a line count"),
    (re.compile(r"^Reading order: this file \(thesis, invariants, open questions\)"),
     "navigation belongs in README's documentation index, which now lists the levels in reading order"),
]


def filter_lines(src: str, lines: list[str], dropped: list[str]) -> list[str]:
    keep = []
    for line in lines:
        hit = next((reason for rx, reason in DROP_RX if rx.match(line.strip())), None)
        if hit is None:
            keep.append(line)
        else:
            dropped.append(f"# --- {src} (line rule: {hit})")
            dropped.append(line)
    return keep


def source_text(rel: str) -> str:
    """Read a source doc as it stands at HEAD, so the migration stays reproducible after it lands."""
    return subprocess.run(["git", "show", f"HEAD:{rel}"], cwd=REPO, capture_output=True, text=True, check=True).stdout


def split_sections(path: Path | str, dropped: list[str] | None = None) -> list[tuple[str | None, list[str]]]:
    lines = (source_text(path) if isinstance(path, str) else path.read_text(encoding="utf-8")).splitlines()
    out: list[tuple[str | None, list[str]]] = []
    current: str | None = None
    buf: list[str] = []
    title_seen = False
    for line in lines:
        if line.startswith("# ") and not title_seen:
            title_seen = True  # the doc's own title: the filename carries the level, so it leaves
            if dropped is not None:
                dropped.append(f"# --- {path} (doc title: the filename carries the level and the H1 is the source tier)")
                dropped.append(line)
            continue
        if line.startswith("## "):
            out.append((current, buf))
            current = line[3:].strip()
            buf = [line]
            continue
        buf.append(line)
    out.append((current, buf))
    return out


# --- reference sweep: every mention of an old path becomes the path that now owns the meaning.
# --- Longest and most specific first; the bare-name rules refuse to match a name a more specific rule
# --- already rewrote (`(?<![\w/])`), so "docs/engine.md" cannot decay to "docs/" + "8_DETAIL.md" twice.
REF_MAP: list[tuple[str, str]] = [
    ("docs/plan/workplan.md", "docs/0_ROADMAP.md"),
    ("docs/plan/evidence.md", "docs/9_IMPLEMENTATION.md"),
    ("docs/plan/open-items.md", "docs/9_IMPLEMENTATION.md"),
    ("docs/plan/gap-analysis.md", "docs/6_ARCHITECTURE.md"),
    ("docs/objective.md", "docs/2_GOAL.md"),
    ("docs/contracts.md", "docs/7_INTERFACE.md"),
    ("docs/engine.md", "docs/8_DETAIL.md"),
    ("docs/learning.md", "docs/5_PRODUCT.md"),
    ("docs/dev-loop.md", "docs/9_IMPLEMENTATION.md"),
    ("docs/prior-art.md", "docs/1_PROBLEM.md"),
    ("docs/roadmap.md", "docs/0_ROADMAP.md"),
    ("docs/design.md", "docs/6_ARCHITECTURE.md"),
    ("../objective.md", "2_GOAL.md"),
    ("../contracts.md", "7_INTERFACE.md"),
    ("../engine.md", "8_DETAIL.md"),
    ("../learning.md", "5_PRODUCT.md"),
    ("../dev-loop.md", "9_IMPLEMENTATION.md"),
    ("../roadmap.md", "0_ROADMAP.md"),
    ("../design.md", "6_ARCHITECTURE.md"),
    ("plan/workplan.md", "0_ROADMAP.md"),
    ("plan/evidence.md", "9_IMPLEMENTATION.md"),
    ("plan/open-items.md", "9_IMPLEMENTATION.md"),
    ("plan/gap-analysis.md", "6_ARCHITECTURE.md"),
    ("docs/plan/", "docs/"),
]
BARE_MAP = {
    "workplan.md": "0_ROADMAP.md",
    "open-items.md": "9_IMPLEMENTATION.md",
    "gap-analysis.md": "6_ARCHITECTURE.md",
    "objective.md": "2_GOAL.md",
    "contracts.md": "7_INTERFACE.md",
    "engine.md": "8_DETAIL.md",
    "learning.md": "5_PRODUCT.md",
    "dev-loop.md": "9_IMPLEMENTATION.md",
    "prior-art.md": "1_PROBLEM.md",
    "design.md": "6_ARCHITECTURE.md",
    "roadmap.md": "0_ROADMAP.md",
}
SKIP = {"scripts/check-docs.py", ".scratch/91-docs-migration/migrate.py"}
SKIP_PREFIX = (".scratch/91-docs-migration/",)
SUFFIXES = {".md", ".sh", ".py", ".rs", ".toml", ".json", ".txt", ".yml", ".yaml", ".js"}


def sweep() -> int:
    changed = 0
    files = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, capture_output=True, text=True, check=True).stdout
    for rel in files.split("\0"):
        if not rel or rel in SKIP or rel.startswith(SKIP_PREFIX):
            continue
        path = REPO / rel
        if path.suffix not in SUFFIXES or not path.is_file():
            continue
        try:
            text = old = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for before, after in REF_MAP:
            text = text.replace(before, after)
        for before, after in BARE_MAP.items():
            text = re.sub(r"(?<![\w/])" + re.escape(before) + r"(?![\w])", after, text)
        if text != old:
            path.write_text(text, encoding="utf-8")
            changed += 1
    print(f"swept references in {changed} files")
    return 0


def main() -> int:
    dropped_lines: list[str] = [
        "# Lines removed from the doc base by `.scratch/91-docs-migration/migrate.py`,",
        "# each with the rule that removes it. Read by `scripts/check-docs.py --only preserved`.",
    ]
    sources: dict[str, list[tuple[str | None, list[str]]]] = {}
    for src in sorted({p[2] for p in PLAN} | {d[0] for d in DROP}):
        sources[src] = split_sections(src, dropped_lines)

    assigned: dict[tuple[str, str | None], list[tuple[str, str, list[str]]]] = {}
    used: set[tuple[str, str | None]] = set()
    for target, tier, src, title in PLAN:
        match = [s for s in sources[src] if s[0] == title]
        if len(match) != 1:
            print(f"error: {src} has {len(match)} sections titled {title!r}, expected 1", file=sys.stderr)
            return 2
        used.add((src, title))
        assigned.setdefault((target, tier), []).append((src, tier, match[0][1]))

    for src, title, reason in DROP:
        match = [s for s in sources[src] if s[0] == title]
        if len(match) != 1:
            print(f"error: DROP names {src} §{title!r} but it appears {len(match)} times", file=sys.stderr)
            return 2
        used.add((src, title))
        dropped_lines.append(f"# --- {src} §{title}: {reason}")
        dropped_lines.extend(l for l in match[0][1] if l.strip())

    missing = [(src, s[0]) for src, sections in sources.items() for s in sections if (src, s[0]) not in used]
    if missing:
        print("error: these sections are neither assigned nor dropped, so they would vanish:", file=sys.stderr)
        for src, title in missing:
            print(f"  {src} §{title}", file=sys.stderr)
        return 2

    # write one file per target, tiers ascending, the plan's order preserved inside a tier
    for target in sorted({p[0] for p in PLAN}):
        parts: list[str] = []
        for tier in TIERS:
            chunks = assigned.get((target, tier))
            if not chunks:
                continue
            parts.append(f"# {tier}")
            for _src, _tier, lines in chunks:
                parts.extend(lines)
        text = "\n".join(parts).rstrip("\n") + "\n"
        text = "\n".join(filter_lines(target, text.split("\n"), dropped_lines))
        text = "\n".join(("" if not l.strip() else l) for l in text.split("\n"))
        while "\n\n\n" in text:
            text = text.replace("\n\n\n", "\n\n")
        (DOCS / target).write_text(text, encoding="utf-8")
        print(f"wrote docs/{target}: {len(text.splitlines())} lines")

    (Path(__file__).resolve().parent / "docs-dropped.txt").write_text("\n".join(dropped_lines) + "\n", encoding="utf-8")
    print(f"dropped {len(dropped_lines)} lines, with reasons")
    sweep()
    return 0


if __name__ == "__main__":
    sys.exit(main())
