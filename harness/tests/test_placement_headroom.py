"""Tests for the placement-headroom measurement.

Two things are pinned: the counting logic (dependent reads = 1 + tile changes,
hand-computed below) and the measured verdict (family co-location cuts dependent
random reads per request by ~35%, above a 15% read-level bar that is NOT the
pre-registration's throughput threshold; today's layout is a region count, so it
is insensitive to tile size).
"""

from __future__ import annotations

import sys
from collections import Counter
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from flashdb_harness import placement_headroom as ph  # noqa: E402


# ---------------------------------------------------------------------------
# Counting logic, by hand
# ---------------------------------------------------------------------------
def test_dependent_reads_counts_tile_runs():
    """Hand computation: dependent = 1 + tile changes.

    []            -> 0 reads
    [5]           -> 1 (the first read is always a random read)
    [5,5]         -> 1 (second read served from the same tile)
    [5,6]         -> 2 (6 != 5, one change)
    [5,6,6,7]     -> 1 + (5!=6) + (6!=6) + (6!=7) = 3
    """
    assert ph.dependent_reads([]) == 0
    assert ph.dependent_reads([5]) == 1
    assert ph.dependent_reads([5, 5]) == 1
    assert ph.dependent_reads([5, 6]) == 2
    assert ph.dependent_reads([5, 6, 6, 7]) == 3


# ---------------------------------------------------------------------------
# The three placements on a synthetic store, by hand
# ---------------------------------------------------------------------------
def _synth_store() -> set[ph.Key]:
    k = ph.Key
    cust_a = ("a@example.com",)
    cust_b = ("b@example.com",)
    stock = ("S000001", "L000")
    order = ("O00000001",)
    return {
        k("P", "Customer", cust_a, cust_a), k("U", "Customer", cust_a, ("email", cust_a[0])),
        k("P", "Customer", cust_b, cust_b), k("U", "Customer", cust_b, ("email", cust_b[0])),
        k("P", "Stock", stock, stock), k("U", "Stock", stock, ("sku_loc", stock)),
        k("P", "Order", order, order), k("U", "Order", order, ("order_no", order)),
    }


def _synth_reads() -> list[list[ph.Key]]:
    """req1 reads the U then the P of customer a and of stock s; req2 reads customer b."""
    k = ph.Key
    cust_a = ("a@example.com",)
    cust_b = ("b@example.com",)
    stock = ("S000001", "L000")
    return [
        [
            k("U", "Customer", cust_a, ("email", cust_a[0])),
            k("P", "Customer", cust_a, cust_a),
            k("U", "Stock", stock, ("sku_loc", stock)),
            k("P", "Stock", stock, stock),
        ],
        [
            k("U", "Customer", cust_b, ("email", cust_b[0])),
            k("P", "Customer", cust_b, cust_b),
        ],
    ]


def _counts(read_seqs):
    return Counter(k for seq in read_seqs for k in seq)


def test_today_is_a_region_count_per_request():
    """today, tile_entries=2. Region order (marker, entity): P|Customer, P|Order, P|Stock, U|Customer, U|Order, U|Stock.

    Hand computation of tile ids (a new tile at each region, split every 2):
      P|Customer[a]=0  P|Customer[b]=0   (two entries fill tile 0)
      P|Order[o]   =1  (new region -> tile 1)
      P|Stock[s]   =2  (new region -> tile 2)
      U|Customer[a]=3  U|Customer[b]=3   (new region, two entries -> tile 3)
      U|Order[o]   =4  (new region -> tile 4)
      U|Stock[s]   =5  (new region -> tile 5)
    req1 [U|Cust[a], P|Cust[a], U|Stock[s], P|Stock[s]] = [3,0,5,2] -> 1 + 3 changes = 4
    req2 [U|Cust[b], P|Cust[b]]                         = [3,0]     -> 1 + 1 change  = 2
    """
    universe, read_seqs = _synth_store(), _synth_reads()
    tiles = ph.tile_map(universe, "today", 2, _counts(read_seqs))
    total, per = ph.count(read_seqs, tiles)
    assert per == [4, 2]
    assert total == 6


def test_colocate_adjacent_row_keys_collapse():
    """colocate, tile_entries=2. Order (entity, row, marker): Customer rows a,b then Order row o then Stock row s.

    Hand computation of tile ids:
      [P|Cust[a], U|Cust[a], P|Cust[b], U|Cust[b], P|Order[o], U|Order[o], P|Stock[s], U|Stock[s]] -> positions 0..7
      tile_entries=2 -> tiles [0, 0, 1, 1, 2, 2, 3, 3]
    req1 [U|Cust[a], P|Cust[a], U|Stock[s], P|Stock[s]] = [0,0,3,3] -> 2
    req2 [U|Cust[b], P|Cust[b]]                         = [1,1]     -> 1
    total 3, i.e. 50% below today's 6.
    """
    universe, read_seqs = _synth_store(), _synth_reads()
    tiles = ph.tile_map(universe, "colocate", 2, _counts(read_seqs))
    total, per = ph.count(read_seqs, tiles)
    assert per == [2, 1]
    assert total == 3


def test_freq_boundaries_need_no_co_occurrence_here():
    """freq, tile_entries=2. All six read keys have access count 1; ties break by (marker, entity, tail).

    Hand computation of the read-key order (count-1 block; count-0 keys follow and
    never enter the two requests' tiles):
      P|Cust[a], P|Cust[b], P|Stock[s], U|Cust[a], U|Cust[b], U|Stock[s] -> positions 0..5
      tile_entries=2 -> tiles [0, 0, 1, 1, 2, 2]
    req1 [U|Cust[a], P|Cust[a], U|Stock[s], P|Stock[s]] = [1,0,2,1] -> 4
    req2 [U|Cust[b], P|Cust[b]]                         = [2,0]     -> 2
    total 6: a pure access-count order cannot collapse a U->P pair whose members
    are ordered apart, which is exactly why family co-location is its own rule.
    """
    universe, read_seqs = _synth_store(), _synth_reads()
    tiles = ph.tile_map(universe, "freq", 2, _counts(read_seqs))
    total, per = ph.count(read_seqs, tiles)
    assert per == [4, 2]
    assert total == 6


# ---------------------------------------------------------------------------
# The measured verdict, from the real generator
# ---------------------------------------------------------------------------
def test_measurement_is_deterministic():
    a = ph.build_store_and_trace(seed=7, requests=1_000, customers=200, skus=200, locs=8)
    b = ph.build_store_and_trace(seed=7, requests=1_000, customers=200, skus=200, locs=8)
    universe, reads = a
    counts = _counts(reads)
    r1 = ph.measure(universe, reads, "today", 256, counts)["total_dependent_reads"]
    r2 = ph.measure(b[0], b[1], "today", 256, _counts(b[1]))["total_dependent_reads"]
    assert r1 == r2


def test_today_count_is_independent_of_tile_size():
    universe, reads = ph.build_store_and_trace(seed=42, requests=2_000, customers=500, skus=500, locs=8)
    counts = _counts(reads)
    totals = {t: ph.measure(universe, reads, "today", t, counts)["total_dependent_reads"]
              for t in (64, 256, 4096)}
    assert len(set(totals.values())) == 1, f"no request reads two keys of one region: {totals}"


def test_colocate_clears_the_15pct_threshold_at_both_headline_tile_sizes():
    """The measured finding the report states, pinned as a test.

    Family co-location collapses each lookup's U->P pair, which is every read of
    order_flow (70%), stock_lookup (15%) and customer_lookup (10%); movements_scan
    is a single region read and cannot move. That is ~35% at both headline tile
    sizes, above the 15% read-level bar. That bar is a read-level criterion, NOT
    the pre-registration's threshold: the pre-registration's 15% is on sustained
    offered load (a throughput quantity), which this count cannot evaluate.
    """
    universe, reads = ph.build_store_and_trace(seed=42, requests=8_000, customers=5_000, skus=2_000, locs=8)
    counts = _counts(reads)
    for t in ph.TILE_ENTRIES:
        base = ph.measure(universe, reads, "today", t, counts)["total_dependent_reads"]
        colo = ph.measure(universe, reads, "colocate", t, counts)["total_dependent_reads"]
        gain = ph.reduction(colo, base)
        assert gain > 15.0, f"colocate should clear 15% at tile_entries={t}, got {gain:.2f}%"
