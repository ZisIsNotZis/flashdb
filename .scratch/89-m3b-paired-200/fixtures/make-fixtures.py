#!/usr/bin/env python3
"""TEST FIXTURE ONLY — write driven-shaped `pN-<arm>.out` files for `score-pair.py`.

    python3 fixtures/make-fixtures.py <scenario> <out-dir>

Each file is the real format the box produces: `# paired-200`, `# restore: verified`,
`# loadavg at start: ...`, one window JSON object, the driver's `load_summary`, and
`# loadavg at end: ...`. Scenario `sustained` decides whether the window reaches the 95% rule.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path


def arm(pair: int, variant: str, *, start: float = 1.00, errors: int = 0,
        sustained: bool = True, restore: bool = True) -> str:
    completed = 200 if sustained else 100
    window = {
        "achieved_rps": 200.0, "completed": completed, "compact_bytes": 0,
        "early_dispatches": 0, "errors": errors, "group_elapsed_us": 5000, "groups": 4,
        "groups_over_service_ceiling": 0, "invalid": False, "lag_ceiling_breached": False,
        "lag_ceiling_us": 10000, "lag_p50_us": 100, "lag_p95_us": 500, "lag_p99_us": 1000,
        "max_group_size": 50, "mean_group_size": 50.0, "min_lag_us": 0, "p50_us": 1000,
        "p95_us": 2000, "p99_us": 3000, "rate_rps": 200.0, "reject_reason": None,
        "requests": 200, "service_ceiling_breached": False, "service_ceiling_us": None,
        "service_p50_us": 1000, "service_p95_us": 2000, "service_p99_us": 3000,
        "window": 0, "window_us": 1000000,
    }
    restore_line = ("# restore: verified sha256=fixture bytes=1" if restore
                    else "# restore: FAILED sha256=fixture bytes=1")
    return "\n".join([
        f"# paired-200 arm={variant} pair={pair} rev=fixture",
        restore_line,
        f"# loadavg at start: {start:.2f} 1.00 1.00 1/100 1234  (fixture)",
        json.dumps(window, separators=(",", ":")),
        "load_summary requests=200 completed=%d errors=%d else_used=0 late_total=0 "
        "early_dispatches=0 windows=1 invalid_windows=0 service_ceiling_rejected_windows=0 "
        "lag_ceiling_rejected_windows=0 ceiling_rejected_windows=0 last_valid_rate_rps=200 "
        "saturated_at_rps=none commit_batch=1 group_requests=64 elapsed_ms=1000 achieved_rps=200 "
        "compact_bytes_total=0 compact_merges=0 first_error=\"none\"" % (completed, errors),
        "# loadavg at end: 1.10 1.05 1.00 1/100 1234  (fixture)",
        "",
    ])


# scenario -> list of (pair, declared-kwargs, indexed-kwargs)
SCENARIOS: dict[str, list[tuple[int, dict, dict]]] = {
    "two": [(p, {}, {}) for p in (1, 2)],
    "four": [(p, {}, {}) for p in (1, 2, 3, 4)],
    "five-met": [(1, {}, {}), (2, {}, {}), (3, {}, {}), (4, {}, {}),
                 (5, {"sustained": False}, {})],
    "five-falsified": [(p, {}, {"sustained": p <= 3}) for p in (1, 2, 3, 4, 5)],
    "five-vacuous": [(p, {"sustained": False}, {"sustained": False}) for p in (1, 2, 3, 4, 5)],
    # 5 pairs, but p3's declared arm started above the trigger and p4's indexed arm errored:
    # three valid pairs remain, so the scorer must refuse a verdict and name both exclusions.
    "excluded": [(1, {}, {}), (2, {}, {}),
                 (3, {"start": 2.50}, {}),
                 (4, {}, {"errors": 1}),
                 (5, {}, {})],
}


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: make-fixtures.py <scenario> <out-dir>", file=sys.stderr)
        print("scenarios: " + ", ".join(sorted(SCENARIOS)), file=sys.stderr)
        return 2
    scenario, out_dir = argv[1], Path(argv[2])
    if scenario not in SCENARIOS:
        print(f"unknown scenario {scenario}", file=sys.stderr)
        return 2
    out_dir.mkdir(parents=True, exist_ok=True)
    for pair, declared_kw, indexed_kw in SCENARIOS[scenario]:
        (out_dir / f"p{pair}-declared.out").write_text(arm(pair, "declared", **declared_kw))
        (out_dir / f"p{pair}-indexed.out").write_text(arm(pair, "indexed", **indexed_kw))
    print(f"wrote {scenario} into {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
