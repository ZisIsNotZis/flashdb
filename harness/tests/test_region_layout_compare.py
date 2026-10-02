"""Pure parsers for the region-layout comparison (no engine run needed)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import region_layout_compare as rlc  # noqa: E402

LOAD_OUTPUT = """\
device_fsync_p50_us=4846 device_fsync_p99_us=15700
engine_open target=/tmp/x discover=true active_tiles=45 serving_memtable_entries=0
window=0 rate_rps=25 requests=24 completed=24 errors=0 lag_p50_us=0 lag_p99_us=0 behind=false late_total=0 errors_window=0 p50_us=16000 p99_us=21000 invalid=false ceiling_us=none
pattern=scan:StockMovement count=24 p50_ns=16000000 p99_ns=21000000 mean_ns=16200000
window=1 rate_rps=25 requests=25 completed=49 errors=0 lag_p50_us=0 lag_p99_us=0 behind=false late_total=0 errors_window=0 p50_us=15000 p99_us=20000 invalid=false ceiling_us=none
load_summary requests=400 completed=400 errors=0 else_used=0 late_total=0 windows=2 invalid_windows=0 last_valid_rate_rps=25 saturated_at_rps=none commit_batch=1 group_requests=none elapsed_ms=16000 achieved_rps=25 first_error=None
"""

TILEINFO = """\
phase=tileinfo ok=true dir=/tmp/x active_tiles=45 serving_memtable_entries=0 region_mode=per_region
phase=tileinfo tile=tile-2021.r0.tile bytes=265697 cutoff=2021 entries=2000 page_size=4096 bloom_len=17524 bits_per_key=10 hash_count=7 dir_len=2311 fences=59 region=P|Order
phase=tileinfo tile=tile-2021.r2.tile bytes=367816 cutoff=2021 entries=2000 page_size=65536 bloom_len=17524 bits_per_key=10 hash_count=7 dir_len=245 fences=5 region=P|StockMovement
phase=tileinfo ok=true tiles=45 tile_bytes_total=4699128 page_sizes={4096, 65536} bloom_tiles=45
"""


def test_parse_load_output_extracts_schema_fields():
    parsed = rlc.parse_load_output(LOAD_OUTPUT)
    assert parsed["device_fsync_p50_us"] == 4846
    assert parsed["active_tiles"] == 45
    assert parsed["serving_memtable_entries"] == 0
    assert parsed["discover"] == "true"
    assert parsed["achieved_rps"] == 25.0
    assert parsed["else_used"] == 0
    assert parsed["requests"] == 400 and parsed["completed"] == 400
    assert len(parsed["windows"]) == 2


def test_steady_state_skips_warmup_and_takes_window_tail():
    parsed = rlc.parse_load_output(LOAD_OUTPUT)
    steady = rlc.steady_state(parsed)
    assert steady["p50_us"] == 15000
    assert steady["p99_us"] == 20000
    assert steady["windows"] == 1


def test_parse_tileinfo_reports_pages_regions_and_memtable():
    ti = rlc.parse_tileinfo(TILEINFO)
    assert ti["active_tiles"] == 45
    assert ti["serving_memtable_entries"] == 0
    assert ti["page_sizes"] == [4096, 65536]
    assert ti["regions"] == {"P|Order": 1, "P|StockMovement": 1}
    assert ti["tiles"] == 45
    assert ti["tile_bytes_total"] == 4699128


def test_kv_ignores_space_bearing_trailing_field():
    # first_error can contain spaces; only fixed-schema tokens are read.
    line = 'load_summary requests=3 achieved_rps=4 first_error=Some("boom here")'
    assert rlc._kv(line, "achieved_rps") == "4"
    assert rlc._kv(line, "requests") == "3"
