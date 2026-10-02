"""Pure parsers for the open-footprint controlled experiment (no engine run needed)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import open_footprint as of  # noqa: E402

OPENINFO = (
    "phase=openinfo ok=true dir=/tmp/x active_tiles=6 serving_memtable_entries=0 "
    "tile_bytes_total=118650044 rss_kb=5252 vm_hwm_kb=87668 vsize_kb=77260\n"
)

SEED_OUTPUT = (
    "phase=seed-inventory ok=true blocks=400 docs=800000 customers=400000 stocks=400000 "
    "csn=400 publishes=6 rotations=5 tiles=6 serving_memtable_entries=0\n"
)

TIME_STDERR = (
    "\tMaximum resident set size (kbytes): 142308\n"
    "\tElapsed (wall clock) time (h:mm:ss or m:ss): 0:11.24\n"
)


def test_parse_openinfo_reads_every_field() -> None:
    parsed = of.parse_openinfo(OPENINFO)
    assert parsed == {
        "active_tiles": 6,
        "serving_memtable_entries": 0,
        "tile_bytes_total": 118650044,
        "rss_kb": 5252,
        "vm_hwm_kb": 87668,
        "vsize_kb": 77260,
    }


def test_parse_openinfo_absent_line_is_all_none() -> None:
    parsed = of.parse_openinfo("nothing here\n")
    assert all(v is None for v in parsed.values())


def test_parse_seed_peak_and_docs() -> None:
    assert of.parse_seed_peak_bytes(TIME_STDERR) == 142308 * 1024
    assert of.seed_docs(SEED_OUTPUT) == 800000
    assert of.parse_seed_peak_bytes("") is None
    assert of.seed_docs("") == 0


def test_format_table_has_one_row_per_variant() -> None:
    row = {"name": "base", "axis": "base", "active_tiles": 22, "rows": 800000,
           "data_bytes": 118 << 20, "seed_peak_bytes": 42 << 20,
           "vm_hwm_kb": 9800, "rss_kb": 5200, "vsize_kb": 11000}
    table = of.format_table([row])
    lines = table.splitlines()
    assert len(lines) == 2
    assert lines[0].split()[0] == "variant"
    assert lines[1].split()[0] == "base"
