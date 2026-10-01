"""Tests for the layout headroom model.

The model is only as good as its constants and its arithmetic, so these tests pin the parts
that are derived rather than measured: the bloom false-positive formula, the latency
interpolation between measured page sizes, the scan-fraction saturation, the provisional
penalty's shape, and the partition break-even arithmetic.
"""

from __future__ import annotations

import pytest

from flashdb_harness import layout_headroom as lh


def test_fpr_matches_the_measured_value_at_10_bits():
    # 08d-2a measured 0.0079 on a real tile at 10 bits/key; the optimum formula gives 0.0078.
    assert lh.fpr(10) == pytest.approx(0.0078, abs=1e-4)


def test_fpr_is_monotone_and_zero_bits_means_no_bloom():
    assert lh.fpr(0) == 1.0
    assert lh.fpr(14) < lh.fpr(10) < lh.fpr(4)
    assert lh.fpr(14) == pytest.approx(0.5**10)


def test_l_dep_interpolates_the_measured_points_and_is_monotone():
    assert lh.l_dep_us(4096) == pytest.approx(96.3)
    assert lh.l_dep_us(16384) == pytest.approx(117.4)
    assert lh.l_dep_us(65536) == pytest.approx(185.8)
    assert lh.l_dep_us(8192) == pytest.approx((96.3 + 117.4) / 2, rel=0.05)
    assert lh.l_dep_us(2048) == 96.3, "below the smallest measured size, clamp"
    assert lh.l_dep_us(1 << 20) == 185.8, "above the largest measured size, clamp"
    assert lh.l_dep_us(4096) < lh.l_dep_us(16384) < lh.l_dep_us(65536)


def test_scan_fraction_saturates_at_the_device_rate():
    assert lh.scan_fraction(4096, 1.5) == pytest.approx(lh.SCAN_4K_FRACTION)
    assert lh.scan_fraction(65536, 1.0) == pytest.approx(lh.SCAN_4K_FRACTION)
    assert lh.scan_fraction(65536, 1.5) <= 1.0
    assert lh.scan_fraction(65536, 4.0) == 1.0, "cannot exceed the device bandwidth"


def test_penalty_is_continuous_at_the_slo_and_bounded():
    slo = 200.0
    assert lh.penalty(slo, slo) == pytest.approx(lh.EPSILON)
    assert lh.penalty(slo / 2, slo) == pytest.approx(lh.EPSILON * 0.5)
    assert lh.EPSILON < lh.penalty(slo * 1.5, slo) < 1.0
    assert lh.penalty(slo * 1e6, slo) <= 1.0
    # strictly increasing on both branches
    assert lh.penalty(0.9 * slo, slo) < lh.penalty(slo, slo) < lh.penalty(1.1 * slo, slo)


def test_partition_breakeven_is_linear_in_the_slow_family_share():
    assert lh.partition_breakeven(0.0, 1.0, 3.92) == pytest.approx(3.92)
    assert lh.partition_breakeven(0.5, 1.0, 4.0) == pytest.approx(2.0)
    assert lh.partition_breakeven(0.2, 0.5, 4.0) == pytest.approx(3.6)
    assert lh.partition_breakeven(1.0, 1.0, 3.92) == pytest.approx(0.0)


def test_headroom_ranking_is_deterministic_and_base_scores_zero_gain():
    rows = lh.headroom(tiles_probed=12, large_page_scan_gain=1.0)
    again = lh.headroom(tiles_probed=12, large_page_scan_gain=1.0)
    assert [str(l) for l, _, _ in rows] == [str(l) for l, _, _ in again]
    base = [g for l, _, g in rows if l == lh.base_layout()]
    assert base == [pytest.approx(0.0)]
    # documented finding: the safe knob set cannot reach the 15% threshold
    assert max(g for _, _, g in rows) < 15.0


# --- family partitioning: the write-side axis, simulated against the shipped policy ---------

from flashdb_harness import partition_sim as psim  # noqa: E402


def test_partition_simulation_reproduces_the_measured_write_amp():
    """The simulation is only usable if it matches a real measurement.

    Validated against slice 12d's measured write_amp of 3.920 at 5 M docs x 1 KiB with the
    scale driver's composition; the acceptance is the pre-registered 25% calibration gate.
    """
    result = psim.simulate(psim.baseline_scale_driver())
    assert result["write_amp"] == pytest.approx(3.920, rel=0.25)


def test_family_partitioning_loses_at_the_current_tile_cap():
    """The measured decision: partitioning is counterproductive with a 12-tile cap.

    Partitioning multiplies the tile count against a fixed manifest cap, so each family
    reaches the cap sooner and compacts its own smaller tiers more often. This is the finding
    that closed the axis, so it is pinned as a test rather than left to prose.
    """
    saved = psim.TILE_CAP
    try:
        psim.TILE_CAP = 12
        comp = psim.inventory_composition()
        colocated = psim.simulate(psim.partitioned(comp))["write_amp"]
        partitioned = psim.simulate_partitioned(comp)["write_amp"]
    finally:
        psim.TILE_CAP = saved
    assert partitioned > colocated, "at cap 12 partitioning must be worse, per the measurement"


def test_a_larger_tile_cap_reduces_write_amplification_substantially():
    """The other half of the finding: cap headroom is the real writing lever."""
    saved = psim.TILE_CAP
    try:
        amps = {}
        for cap in (12, 96):
            psim.TILE_CAP = cap
            amps[cap] = psim.simulate(psim.inventory_composition())["write_amp"]
    finally:
        psim.TILE_CAP = saved
    assert amps[96] < amps[12] * 0.6, f"cap headroom should cut writes by >40%: {amps}"
