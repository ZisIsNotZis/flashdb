"""Pure parsers and trace helpers for the cold-axis comparison (no engine run needed)."""

from __future__ import annotations

import json
import sys
import types
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import cold_axis_compare as cac  # noqa: E402

LOAD_OUTPUT = """\
device_fsync_p50_us=5602 device_fsync_p99_us=18139
engine_open target=/tmp/x discover=true active_tiles=12 serving_memtable_entries=0
engine_policy policy={"default":{"bloom_bits_per_key":10,"merge_min_tiles":2,"page_size":4096},"regions":{}}
{"completed":100,"current":{"patterns":[],"regions":[{"bytes_read":95,"bytes_written":0,"count":5,"mean_ns":40000.0,"p50_ns":33000.0,"p99_ns":70000.0,"probe_misses":0,"probes":5,"region":"P|Stock"},{"bytes_read":113500000,"count":5,"p50_ns":216000.0,"p99_ns":566000.0,"probes":5,"region":"U|Stock"}]},"errors":0,"p50_us":216,"p99_us":566,"rate_rps":300.0,"requests":100,"window":0}
load_summary requests=30000 completed=30000 errors=0 else_used=3000 late_total=0 windows=10 invalid_windows=0 last_valid_rate_rps=300 saturated_at_rps=none commit_batch=1 group_requests=none elapsed_ms=100000 achieved_rps=300 first_error="none"
"""

TILEINFO = """\
phase=tileinfo ok=true dir=/tmp/x active_tiles=240 serving_memtable_entries=0 region_mode=per_region
phase=tileinfo tile=tile-2021.r0.tile bytes=265697 cutoff=2021 entries=2000 page_size=4096 bloom_len=17524 region=P|Customer
phase=tileinfo tile=tile-2021.r2.tile bytes=367816 cutoff=2021 entries=2000 page_size=65536 bloom_len=17524 region=P|StockMovement
phase=tileinfo ok=true tiles=240 tile_bytes_total=1880000000 page_sizes={4096, 65536} bloom_tiles=240
"""

TIME_STDERR = """\
\tCommand being timed: "flashdb-load ..."
\tMaximum resident set size (kbytes): 83200
\tElapsed (wall clock) time (h:mm:ss or m:ss): 0:12.34
"""
TIME_STDERR_MULTI = (
    "Maximum resident set size (kbytes): 150000\n"
    "Maximum resident set size (kbytes): 189652\n"
)
COMPACT_OUTPUT = """\
phase=compact ok=true shape=budget_prefix region_mode=per_region merges=5 compaction_bytes=12629247 tile_bytes_before=4287849 tile_bytes_after=4863653 rewrite_factor=2.945 active_tiles_before=240 active_tiles_after=8 tiles_per_region_before=P|Customer:60 dir=/tmp/x
"""


def test_parse_load_output_reads_metrics_json_regions():
    parsed = cac.parse_load_output(LOAD_OUTPUT)
    assert parsed["device_fsync_p50_us"] == 5602
    assert parsed["active_tiles"] == 12
    assert parsed["requests"] == 30000 and parsed["completed"] == 30000
    assert parsed["else_used"] == 3000
    assert parsed["commit_rate"] == 1.0
    # P|Stock and U|Stock are reported independently in the JSON region rows.
    assert parsed["regions"]["P|Stock"]["p50_us"] == 33.0
    assert parsed["regions"]["P|Stock"]["bytes_read"] == 95
    assert parsed["regions"]["U|Stock"]["p50_us"] == 216.0
    assert parsed["regions"]["U|Stock"]["bytes_read"] == 113500000


def test_parse_load_output_reads_peak_report_lines():
    text = (
        "engine_open target=/tmp/x discover=true active_tiles=36 serving_memtable_entries=0\n"
        "load_peak stage=open hwm_kb=243244\n"
        "load_peak stage=end hwm_kb=86460\n"
        "load_summary requests=6000 completed=6000 errors=0 else_used=0 late_total=0 "
        "windows=19 invalid_windows=0 last_valid_rate_rps=300 saturated_at_rps=none "
        "commit_batch=1 group_requests=none elapsed_ms=20000 achieved_rps=300 first_error=\"none\"\n"
    )
    parsed = cac.parse_load_output(text)
    assert parsed["open_peak_kb"] == 243244
    assert parsed["load_peak_kb"] == 86460


def test_parse_rss_peak_takes_max_across_blocks():
    assert cac.parse_rss_peak_bytes(TIME_STDERR) == 83200 * 1024
    assert cac.parse_rss_peak_bytes(TIME_STDERR_MULTI) == 189652 * 1024
    assert cac.parse_rss_peak_bytes("nothing here") is None


def test_parse_scope_peak_reads_cgroup_line():
    text = "phase=seed-inventory ok=true\ncgroup_peak_bytes=268435456\n"
    assert cac.parse_scope_peak(text) == 268435456
    assert cac.parse_scope_peak("no peak") is None


def test_parse_tileinfo_pages_and_regions():
    ti = cac.parse_tileinfo(TILEINFO)
    assert ti["active_tiles"] == 240
    assert ti["page_sizes"] == [4096, 65536]
    assert ti["regions"] == {"P|Customer": 1, "P|StockMovement": 1}
    assert ti["tile_bytes_total"] == 1880000000


def test_parse_compact_extracts_write_side_numbers():
    c = cac.parse_compact(COMPACT_OUTPUT)
    assert c["merges"] == 5
    assert c["compaction_bytes"] == 12629247
    assert c["rewrite_factor"] == 2.945
    assert c["active_tiles_before"] == 240
    assert c["active_tiles_after"] == 8
    assert cac.parse_compact("phase=compact ok=false") == {}


def test_ratio_reports_achieved_vs_target():
    assert cac.ratio(2_000_000_000, 200_000_000) == 10.0
    assert cac.ratio(500_000_000, 200_000_000) == 2.5
    assert cac.ratio(1, 0) == 0.0


def test_steady_state_skips_first_window():
    parsed = cac.parse_load_output(LOAD_OUTPUT)
    steady = cac.steady_state(parsed)
    assert steady["p50_us"] == 216 and steady["p99_us"] == 566
    assert steady["windows"] == 1
    # A JSON-only run (no bare `window=` line) still yields steady-state percentiles.
    assert [w for w in parsed["windows"] if w["p50_us"] is not None]


def test_read_trace_mix_is_deterministic_and_forces_else(tmp_path):
    args = types.SimpleNamespace(
        seed=7, read_requests=10, customers=100, skus=50, locs=2, on_hand=1_000_000,
        stock_share=0.5, customer_share=0.2, scan_share=0.2, write_requests=20,
    )
    p1, p2 = tmp_path / "r1.jsonl", tmp_path / "r2.jsonl"
    m1 = cac.emit_read_trace(p1, args)
    m2 = cac.emit_read_trace(p2, args)
    assert m1 == m2
    assert m1["counts"] == {"stock_lookup": 5, "customer_lookup": 2,
                            "movements_scan": 2, "order_flow": 1}
    assert p1.read_bytes() == p2.read_bytes()
    # The single order_flow request must carry qty above on_hand so the guard's else fires, and
    # its order_no must sit above the build's committed range (write_requests) so it is new work.
    for line in p1.read_text().splitlines():
        if '"take"' in line:
            assert f'"qty":{args.on_hand + 1}' in line
            cfg = json.loads(line)
            order_no = cfg["params"]["order_no"]
            assert int(order_no[1:]) > args.write_requests, order_no


def test_write_trace_is_all_order_flow(tmp_path):
    args = types.SimpleNamespace(
        seed=3, write_requests=4, customers=10, skus=10, locs=2,
    )
    path = tmp_path / "w.jsonl"
    n = cac.emit_write_trace(path, args)
    assert n == 4
    for line in path.read_text().splitlines():
        assert '"take"' in line and '"cust"' in line
