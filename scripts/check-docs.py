#!/usr/bin/env python3
"""One command that decides whether the doc base obeys the level rule.

    python3 scripts/check-docs.py                     # the standing checks: names, tiers, refs
    python3 scripts/check-docs.py --only preserved    # the migration's own acceptance, once
    python3 scripts/check-docs.py --only refs

Checks, each a property a newcomer can verify by reading this file:

  names      every `docs/*.md` is `<N>_<NAME>.md`, plus the one declared index (`glossary.md`)
  tiers      every level doc's H1 headings are the source tiers, ascending, no repeats, non-empty
  refs       every `docs/...md` path mentioned in a tracked text file exists, and no retired
             path (`docs/plan/`, `docs/objective.md`, ...) is still mentioned anywhere
  preserved  every non-blank line of the pre-migration doc base appears in the new one, byte for byte
             after whitespace collapsing, except lines listed in DROPPED (each with a reason). It is
             the migration's acceptance, run against `--base` (default: the commit before it), not a
             standing check: after the move, a doc's own history belongs to git.

Exit 0 when every check passes, 1 when any fails, 2 on a usage error.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DOCS = REPO / "docs"
TIERS = ["EXPLICIT", "CONFIRMED", "INFERRED"]
INDEX_DOCS = {"glossary.md"}
TIER_EXEMPT = INDEX_DOCS | {"0_STAGING.md"}
RETIRED = [
    "docs/objective.md",
    "docs/design.md",
    "docs/contracts.md",
    "docs/engine.md",
    "docs/learning.md",
    "docs/dev-loop.md",
    "docs/prior-art.md",
    "docs/roadmap.md",
    "docs/plan/",
]
DROPPED = REPO / ".scratch" / "91-docs-migration" / "docs-dropped.txt"
LEVEL_RE = re.compile(r"^\d+_[A-Z][A-Z_]*\.md$")
REF_RE = re.compile(r"docs/[A-Za-z0-9_./-]+\.md")
TEXT_SUFFIXES = {".md", ".sh", ".py", ".rs", ".toml", ".json", ".txt", ".yml", ".yaml"}


def tracked_text_files() -> list[Path]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=REPO, capture_output=True, text=True, check=True).stdout
    paths = []
    for rel in out.split("\0"):
        if not rel:
            continue
        p = REPO / rel
        if p.suffix in TEXT_SUFFIXES and p.is_file():
            paths.append(p)
    return paths


RECORD_SUFFIXES = {".out", ".err", ".tsv"}


def is_record(path: Path) -> bool:
    """A saved log or run dump is what a command printed at a moment; rewriting it would falsify the
    record, so a path that no longer exists there is history and not a broken reference."""
    return path.suffix in RECORD_SUFFIXES or "/logs/" in path.as_posix() or "runs" in path.parts


def norm(line: str) -> str:
    return " ".join(line.split())


def docs_files() -> list[Path]:
    return sorted(p for p in DOCS.rglob("*.md"))


def check_names() -> list[str]:
    bad = []
    for p in docs_files():
        rel = p.relative_to(DOCS).as_posix()
        if "/" in rel:
            bad.append(f"{p.relative_to(REPO)} sits in a subdirectory; the level is the filename")
        elif rel not in INDEX_DOCS and not LEVEL_RE.match(rel):
            bad.append(f"{p.relative_to(REPO)} is not <N>_<NAME>.md")
    for name in sorted(INDEX_DOCS):
        if not (DOCS / name).is_file():
            bad.append(f"declared index docs/{name} is missing")
    return bad


def check_tiers() -> list[str]:
    bad = []
    for p in docs_files():
        rel = p.relative_to(DOCS).as_posix()
        if rel in TIER_EXEMPT:
            continue
        lines = p.read_text(encoding="utf-8").splitlines()
        h1 = [(i, l[2:].strip()) for i, l in enumerate(lines) if l.startswith("# ")]
        if not h1:
            bad.append(f"{p.relative_to(REPO)} has no H1 tier zone")
            continue
        idx = -1
        for pos, h in h1:
            if h not in TIERS:
                bad.append(f"{p.relative_to(REPO)}: H1 {h!r} is not one of {TIERS}")
                continue
            i = TIERS.index(h)
            if i <= idx:
                bad.append(f"{p.relative_to(REPO)}: tier {h} is out of order or repeated")
            idx = i
        h2 = [l[3:].strip() for l in lines if l.startswith("## ")]
        for title in sorted({h for h in h2 if h2.count(h) > 1}):
            bad.append(f"{p.relative_to(REPO)}: heading {title!r} appears more than once, so a § citation cannot resolve")
        last = h1[-1][0]
        if not any(l.strip() for l in lines[last + 1 :]):
            bad.append(f"{p.relative_to(REPO)}: no content under its last tier {h1[-1][1]}")
    return bad


def check_refs() -> list[str]:
    bad = []
    for era in RETIRED:
        hits = subprocess.run(["git", "grep", "-l", "-F", era], cwd=REPO, capture_output=True, text=True)
        for f in hits.stdout.split():
            if f in {"scripts/check-docs.py", ".scratch/91-docs-migration/migrate.py"} or f.endswith("docs-dropped.txt"):
                continue
            if is_record(Path(f)):
                continue
            bad.append(f"retired path {era!r} still mentioned in {f}")
    for p in tracked_text_files():
        if p == Path(__file__) or is_record(p):
            continue
        if p.relative_to(REPO).as_posix().startswith(".scratch/91-docs-migration/"):
            continue  # the migration records the old paths on purpose: it is their obituary
        try:
            text = p.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for m in REF_RE.finditer(text):
            ref = m.group(0)
            if ref.startswith("docs/"):
                if not (REPO / ref).exists():
                    bad.append(f"{p.relative_to(REPO)} mentions missing {ref}")
    return bad


SWEPT = [
    ("docs/objective.md", "docs/2_GOAL.md"), ("docs/design.md", "docs/6_ARCHITECTURE.md"),
    ("docs/contracts.md", "docs/7_INTERFACE.md"), ("docs/engine.md", "docs/8_DETAIL.md"),
    ("docs/learning.md", "docs/5_PRODUCT.md"), ("docs/dev-loop.md", "docs/9_IMPLEMENTATION.md"),
    ("docs/prior-art.md", "docs/1_PROBLEM.md"), ("docs/roadmap.md", "docs/0_ROADMAP.md"),
    ("docs/plan/workplan.md", "docs/0_ROADMAP.md"), ("docs/plan/evidence.md", "docs/9_IMPLEMENTATION.md"),
    ("docs/plan/open-items.md", "docs/9_IMPLEMENTATION.md"), ("docs/plan/gap-analysis.md", "docs/6_ARCHITECTURE.md"),
    ("../objective.md", "2_GOAL.md"), ("../contracts.md", "7_INTERFACE.md"), ("../engine.md", "8_DETAIL.md"),
    ("../learning.md", "5_PRODUCT.md"), ("../dev-loop.md", "9_IMPLEMENTATION.md"), ("../roadmap.md", "0_ROADMAP.md"),
    ("../design.md", "6_ARCHITECTURE.md"), ("plan/workplan.md", "0_ROADMAP.md"), ("plan/evidence.md", "9_IMPLEMENTATION.md"),
    ("plan/open-items.md", "9_IMPLEMENTATION.md"), ("plan/gap-analysis.md", "6_ARCHITECTURE.md"), ("docs/plan/", "docs/"),
    ("workplan.md", "0_ROADMAP.md"), ("open-items.md", "9_IMPLEMENTATION.md"),
    ("gap-analysis.md", "6_ARCHITECTURE.md"), ("objective.md", "2_GOAL.md"), ("contracts.md", "7_INTERFACE.md"),
    ("engine.md", "8_DETAIL.md"), ("learning.md", "5_PRODUCT.md"), ("dev-loop.md", "9_IMPLEMENTATION.md"),
    ("prior-art.md", "1_PROBLEM.md"), ("design.md", "6_ARCHITECTURE.md"), ("roadmap.md", "0_ROADMAP.md"),
]


def check_preserved(base: str) -> list[str]:
    allow = set()
    if DROPPED.is_file():
        for line in DROPPED.read_text(encoding="utf-8").splitlines():
            if line.strip() and not line.startswith("# --- "):
                allow.add(norm(line))
    new_lines = set()
    for p in docs_files():
        for line in p.read_text(encoding="utf-8").splitlines():
            if line.strip():
                new_lines.add(norm(line))
    for extra in ("README.md", "AGENTS.md"):
        p = REPO / extra
        if p.is_file():
            for line in p.read_text(encoding="utf-8").splitlines():
                if line.strip():
                    new_lines.add(norm(line))
    bad = []
    for rel in subprocess.run(
        ["git", "ls-tree", "-r", "--name-only", base, "docs"], cwd=REPO, capture_output=True, text=True, check=True
    ).stdout.split():
        if not rel.endswith(".md"):
            continue
        try:
            text = subprocess.run(
                ["git", "show", f"{base}:{rel}"], cwd=REPO, capture_output=True, text=True, check=True
            ).stdout
        except subprocess.CalledProcessError:
            continue
        for line in text.splitlines():
            n = norm(line)
            if n in allow:
                continue
            swept = line
            for before, after in SWEPT:
                swept = swept.replace(before, after)
            if norm(swept) in new_lines:
                continue
            if not n:
                continue
            bad.append(f"lost from {rel}: {line[:160]}")
    return bad


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="3af8a04^", help="pre-migration revision for --only preserved")
    ap.add_argument("--only", choices=["names", "tiers", "refs", "preserved"])
    args = ap.parse_args()
    checks = {
        "names": check_names,
        "tiers": check_tiers,
        "refs": check_refs,
        "preserved": lambda: check_preserved(args.base),
    }
    todo = [args.only] if args.only else [c for c in checks if c != "preserved"]
    failed = 0
    for name in todo:
        problems = checks[name]()
        if problems:
            failed += 1
            print(f"FAIL {name} ({len(problems)})")
            for p in problems[:40]:
                print(f"  - {p}")
            if len(problems) > 40:
                print(f"  ... {len(problems) - 40} more")
        else:
            print(f"ok   {name}")
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except subprocess.CalledProcessError as e:  # git failed: a usage error, never a pass
        print(f"error: {e}", file=sys.stderr)
        sys.exit(2)
