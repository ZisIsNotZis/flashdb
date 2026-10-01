"""Layout headroom analysis (0b prerequisite, oracle risk #2).

Question this answers, cheaply and before any learner is written: **do the layout knobs that
are safe to implement today have enough headroom for a >15% win?** If not, the layout SPACE is
the problem and the learner is irrelevant, so the space must be enlarged first (family
partitioning, then L2 co-location) - see `.scratch/14-layout-harness/layout-space-proposal.md`
and its "Orchestrator review" section.

Model, with every constant measured (never invented):

- Dependent read latency by page size (QD1): 4 KiB 96.3 us, 16 KiB 117.4 us, 64 KiB 185.8 us
  (0a-redo, fio and O_DIRECT Rust agree at 4 KiB; reduced-load window).
- Batched independent read: 203k IOPS at QD32 -> 4.9 us per completed read.
- Device sequential bandwidth: 831 MiB/s (fio 1 MiB QD8). Engine scan: 605 MiB/s at 4 KiB
  blocks, i.e. 0.728 of device rate, which is the per-block CRC + framing overhead.
- Bloom false positives: FPR = 0.5 ** round(bits * ln2) - the standard optimum; at 10 bits this
  is 0.0078, and 08d-2a measured 0.0079 on a real tile, so the formula is trustworthy for
  extrapolation. 0 bits means no bloom at all.
- Per-tile probe cost, fitted to the 08d-2b probe table (1.59 us at 12 tiles with a bloom,
  151.84 us without): `K * (c_hash + fpr * c_block)` with c_hash ~ 0.15 us and
  c_block ~ L_dep(page) - the measured numbers reproduce within ~10%.
- Patterns are declared, not sampled: each is (dependent reads, independent fan-out, bytes
  touched, whether it probes indexes, whether it scans). The mix below is the inventory shape
  from `docs/dev-loop.md` and `harness/flashdb_harness/generator.py`, with a Zipfian hot/cold
  split: hot regions are assumed to receive 90% of the traffic (documented assumption, swept).

Scoring uses the provisional objective in `.scratch/15-objective-v0/provisional-objective.md`
(piecewise penalty, CVaR over windows). The output is a spread, not a verdict: the question is
only whether the best candidate differs from the current default by more than the provisional
15% threshold.

Usage:
    uv run --with pytest python -m flashdb_harness.layout_headroom
"""

from __future__ import annotations

import argparse
import math
from dataclasses import dataclass

# ---------------------------------------------------------------------------
# Measured constants (`.scratch/05-engine-v0/evidence/0a-redo/`,
# `.scratch/08-tile-pages/`, `.scratch/11-tile-directory-ram/`, `.scratch/12-...`)
# ---------------------------------------------------------------------------
L_DEP_US = {4096: 96.3, 16384: 117.4, 65536: 185.8}
L_BAT_US = 4.9              # 203k IOPS at QD32
DEVICE_MIB_S = 831.0
SCAN_4K_FRACTION = 605.0 / 831.0   # measured engine scan at 4 KiB blocks
C_HASH_US = 0.15            # one in-RAM bloom check, fitted from the probe table
FENCE_BYTES_PER_GIB = 0.20 * 1024 * 1024

# Provisional objective (doc 15)
EPSILON = 0.05
SLO_INFLATION = 2.0
TAIL_FACTOR = 1.26          # measured device p99/mean at QD1


def l_dep_us(page_size: int) -> float:
    """Dependent read latency, log-interpolated between the measured page sizes."""
    points = sorted(L_DEP_US)
    if page_size <= points[0]:
        return L_DEP_US[points[0]]
    if page_size >= points[-1]:
        return L_DEP_US[points[-1]]
    for lo, hi in zip(points, points[1:]):
        if lo <= page_size <= hi:
            frac = math.log(page_size / lo) / math.log(hi / lo)
            return L_DEP_US[lo] + frac * (L_DEP_US[hi] - L_DEP_US[lo])
    raise AssertionError("unreachable")


def fpr(bits_per_key: int) -> float:
    """False-positive rate of an optimally tuned bloom. 0 bits means no bloom."""
    if bits_per_key <= 0:
        return 1.0
    k = max(1, round(bits_per_key * math.log(2)))
    return 0.5**k


def scan_fraction(page_size: int, large_page_scan_gain: float) -> float:
    """Fraction of device bandwidth a scan achieves at this page size.

    Measured only at 4 KiB (0.728, CRC-bound). The gain of a larger block is *unmeasured*
    (slice 17 is measuring it), so it is an explicit parameter: 1.0 means no gain, and the
    caller sweeps it. The term saturates at 1.0 (cannot exceed the device).
    """
    if page_size <= 4096:
        return SCAN_4K_FRACTION
    scale = math.log2(page_size / 4096)
    return min(1.0, SCAN_4K_FRACTION * (large_page_scan_gain**scale))


# ---------------------------------------------------------------------------
# Patterns and candidates
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class Pattern:
    name: str
    weight: float        # share of requests in a window
    dependent: int       # dependent random reads (hops)
    fanout: int          # independent, parallelisable reads
    bytes_touched: int
    probes: bool         # consults index prefixes (bloom/range filter applies)
    scans: bool


PATTERNS = (
    # order create: probe the unique order_no + the customer, then write lines + stock
    Pattern("order_create", 0.25, 3, 4, 4 * 1024, probes=True, scans=False),
    # order lookup by business key: unique probe then the primary doc
    Pattern("order_by_no", 0.30, 2, 0, 2 * 1024, probes=True, scans=False),
    # stock lookup by (sku, loc): reverse probe then the primary doc
    Pattern("stock_by_sku", 0.30, 2, 0, 2 * 1024, probes=True, scans=False),
    # movements scan for a SKU: a range scan over one entity
    Pattern("movements_scan", 0.15, 0, 0, 512 * 1024, probes=False, scans=True),
)


@dataclass(frozen=True)
class Layout:
    page_size: int
    bloom_bits: int

    def __str__(self) -> str:
        return f"page={self.page_size // 1024}K bloom={self.bloom_bits}"


def base_layout() -> Layout:
    """Today's shipped values: 4 KiB blocks (the engine's PAGE_SIZE), 10 bloom bits."""
    return Layout(4096, 10)


def candidates() -> list[Layout]:
    return [
        Layout(4096, 0), Layout(4096, 10), Layout(4096, 14),
        Layout(16384, 0), Layout(16384, 10), Layout(16384, 14),
        Layout(65536, 0), Layout(65536, 10), Layout(65536, 14),
    ]


# ---------------------------------------------------------------------------
# Cost model and objective
# ---------------------------------------------------------------------------
def service_us(pattern: Pattern, layout: Layout, tiles_probed: int, large_page_scan_gain: float) -> float:
    page = layout.page_size
    dep = pattern.dependent * l_dep_us(page)
    batched = math.ceil(pattern.fanout / 32) * L_BAT_US
    if pattern.scans:
        bw = DEVICE_MIB_S * scan_fraction(page, large_page_scan_gain)
        transfer = (pattern.bytes_touched / (1024 * 1024)) / bw * 1e6
    else:
        transfer = 0.0
    probe = 0.0
    if pattern.probes:
        # every active tile is asked; a bloom answers "maybe" for a false positive
        probe = tiles_probed * (C_HASH_US + fpr(layout.bloom_bits) * l_dep_us(page))
    return dep + batched + transfer + probe


def penalty(x_us: float, slo_us: float) -> float:
    """Provisional piecewise penalty (doc 15), CONTINUOUS at the SLO.

    Below the SLO a small linear gradient still rewards headroom; at the SLO the value is
    exactly `EPSILON` (not 0, which would be a discontinuity and would make 0.9xSLO score
    worse than 1.0xSLO), and above it the rescaled sigmoid rises smoothly to 1. A first draft
    put 0 at the SLO and this model's own test caught the resulting non-monotonicity.
    """
    if x_us < slo_us:
        return EPSILON * (x_us / slo_us)
    s = slo_us / 2.0
    sig = 1.0 / (1.0 + math.exp(-(x_us - slo_us) / s))
    sig0 = 1.0 / (1.0 + math.exp(0.0))
    rescaled = (sig - sig0) / (1.0 - sig0)
    return EPSILON + (1.0 - EPSILON) * rescaled


def angriness(layout: Layout, tiles_probed: int, large_page_scan_gain: float, slo: dict[str, float]) -> float:
    """Weighted, tail-inflated penalty over the pattern mix.

    One window; the CVaR aggregator needs many windows to differ from a mean, and the headroom
    question is about the *spread* between layouts, which a single-window score already
    exposes. Documented limitation, not an oversight.
    """
    total = 0.0
    for p in PATTERNS:
        x = service_us(p, layout, tiles_probed, large_page_scan_gain) * TAIL_FACTOR
        total += p.weight * penalty(x, slo[p.name])
    return total


def baseline_slo(tiles_probed: int, large_page_scan_gain: float) -> dict[str, float]:
    """SLO per pattern = SLO_INFLATION x the reference-load p99 of the DEFAULT layout."""
    base = base_layout()
    return {
        p.name: SLO_INFLATION * service_us(p, base, tiles_probed, large_page_scan_gain) * TAIL_FACTOR
        for p in PATTERNS
    }


def headroom(tiles_probed: int, large_page_scan_gain: float) -> list[tuple[Layout, float, float]]:
    slo = baseline_slo(tiles_probed, large_page_scan_gain)
    base_score = angriness(base_layout(), tiles_probed, large_page_scan_gain, slo)
    rows = []
    for layout in candidates():
        score = angriness(layout, tiles_probed, large_page_scan_gain, slo)
        rows.append((layout, score, (base_score - score) / base_score * 100.0))
    rows.sort(key=lambda r: r[1])
    return rows


# ---------------------------------------------------------------------------
# Write side: does family partitioning (the safe family axis) have enough headroom?
# ---------------------------------------------------------------------------
# The read-side knobs above cannot reach 15%, so the next candidate axis is keeping the
# entries of *different key families in different tiles* (allowed by I2: nothing is omitted,
# every entry is still published, the read path already merges all active tiles). Its benefit
# is on the WRITE side: a family that changes rarely stops being rewritten every time the hot
# family compacts. Whether that is enough depends on two things we cannot assume and must
# bound: the byte share of the slow family, and how much of the rewrite it can avoid.

def total_cost_us(read_angriness: float, write_bytes_per_ingested_byte: float,
                  write_weight_us_per_byte: float) -> float:
    """Read angriness plus a write term, in comparable units.

    `write_weight_us_per_byte` converts bytes written per ingested byte into the same
    penalty units as the read side. It is an exposed parameter because it is a policy choice
    (the objective's throughput/endurance weight, doc 15 sec. 5), not a measurement.
    """
    return read_angriness + write_weight_us_per_byte * write_bytes_per_ingested_byte


def partition_breakeven(slow_family_share: float, rewrite_avoided: float,
                        write_amp_colocated: float) -> float:
    """write_amp after partitioning families.

    `slow_family_share` = fraction of written bytes belonging to the family that is separated;
    `rewrite_avoided` = fraction of the rewrites that family would otherwise suffer (1.0 means
    it is never rewritten again while the hot family keeps compacting).
    """
    saved = write_amp_colocated * slow_family_share * rewrite_avoided
    return write_amp_colocated - saved


def partitioning_headroom(tiles_probed: int, gain: float, write_weight: float,
                          slow_shares: tuple[float, ...] = (0.05, 0.1, 0.2, 0.4),
                          rewrite_avoided: float = 1.0) -> None:
    slo = baseline_slo(tiles_probed, gain)
    best_read = angriness(Layout(4096, 14), tiles_probed, gain, slo)  # best safe read layout
    base_read = angriness(base_layout(), tiles_probed, gain, slo)
    write_amp = 3.92  # measured at 5 GB with budget_prefix (slice 12d)
    print(f"\n## write-side headroom for family partitioning (weight={write_weight:.3f} us/byte)")
    print(f"   read-side best is only {(base_read - best_read) / base_read * 100:.2f}% better than base")
    for share in slow_shares:
        amp = partition_breakeven(share, rewrite_avoided, write_amp)
        before = total_cost_us(base_read, write_amp, write_weight)
        after = total_cost_us(best_read, amp, write_weight)
        print(f"   slow-family share={share:4.0%} rewrite_avoided={rewrite_avoided:.0%} "
              f"write_amp {write_amp:.2f} -> {amp:.2f}  total gain vs base "
              f"{(before - after) / before * 100:+.2f}%")
    # What weight would be needed for a 15% total win at a given share?
    share = 0.2
    amp = partition_breakeven(share, rewrite_avoided, write_amp)
    required = 0.0
    for w in [x / 1000.0 for x in range(0, 2000)]:
        before = total_cost_us(base_read, write_amp, w)
        after = total_cost_us(best_read, amp, w)
        if (before - after) / before >= 0.15:
            required = w
            break
    print(f"   for a 15% total win at share={share:.0%}: need write_weight >= {required:.3f} "
          f"us/byte (i.e. an endurance weight that large relative to read penalties)")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="layout headroom analysis (deterministic)")
    ap.add_argument("--tiles-probed", type=int, default=12,
                    help="active tiles a probe must consult (measured cap is 12)")
    ap.add_argument("--large-page-scan-gain", type=float, nargs="+", default=[1.0, 1.15, 1.3, 1.5],
                    help="scan-rate multiplier per doubling of block size (UNMEASURED; swept)")
    args = ap.parse_args(argv)

    print(f"# base = {base_layout()} tiles_probed={args.tiles_probed}")
    print("# NOTE: the engine's real block size is 4 KiB, so base is also the PROBE-optimal")
    print("# page size; 16K/64K only help scans, which are a minority of the mix.")
    for gain in args.large_page_scan_gain:
        rows = headroom(args.tiles_probed, gain)
        best, best_score, best_gain = rows[0]
        print(f"\n## large-page scan gain per doubling = {gain:.2f}x")
        for layout, score, gain_pct in rows:
            marker = " <- base" if layout == base_layout() else ""
            print(f"  {str(layout):24s} angriness={score:.5f}  vs base {gain_pct:+6.2f}%{marker}")
        print(f"  best = {best} ({best_gain:+.2f}% vs base); threshold for a learner win is 15.00%")
    partitioning_headroom(args.tiles_probed, args.large_page_scan_gain[0], write_weight=0.02)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

# ---------------------------------------------------------------------------
# KNOWN CALIBRATION GAP (2026-09-30, must be closed before any verdict is quoted)
# ---------------------------------------------------------------------------
# The model above prices the DEVICE regime: dependent reads as L_dep(page) from 0a-redo. Slice
# 17 then measured the same layouts with a WARM page cache and found something the model does
# not represent:
#
#   present-key probe, 1 tile : 19.3 us @4 KiB -> 88.9 us @64 KiB   (+69.6 us)
#   present-key probe, 4 tiles: 24.12 us      -> 35.02 us
#   sequential scan           : 1387 MiB/s    -> 2013 MiB/s  (probe_cost scan)
#   scanbench (600k docs)     : 2231.7 MiB/s  -> 3154.9 MiB/s
#
# The probe penalty is ~1.16 us per extra KiB of block handled, i.e. these measurements are
# CPU-bound (block read into a buffer, CRC-32C, entry parse), not device-bound. One constant
# fitted from the probe delta cannot predict the scan numbers, because the scan path's
# per-byte CPU cost is lower than the probe path's (scan predicts ~0.86 GB/s, measured
# 1.4-2.0 GB/s). So:
#
#   - the device regime (cold cache) is what this model prices, and cold probes/scans at both
#     page sizes have NOT been measured;
#   - a warm/CPU term needs at least two path-specific constants (probe vs scan), and fitting
#     both on the same points would make the check circular;
#   - therefore review-01 F14's quantile-calibration error (the pre-registered 25% tolerance in
#     doc 15) CANNOT be evaluated yet. Closing it needs a cold-cache measurement: a working set
#     larger than RAM under a cgroup with the cache dropped, probes and scans at 4 KiB and
#     64 KiB, from which the device terms and the CPU terms can be separated.
#
# The headroom conclusion (the safe knob set cannot reach 15%) is NOT sensitive to this gap: it
# depends on the *ranking* of the layouts, and the warm measurements make larger pages look
# worse for probes, not better - i.e. they strengthen it.
