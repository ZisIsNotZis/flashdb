"""Placement headroom: dependent random reads per request, by key placement.

Why this exists
---------------
The learned surface today decides only the SHAPE of each region's storage (merge
threshold, page size, bloom bits). The request level supplies key co-occurrence,
and the physical decision that could exploit it is PLACEMENT - which key families
share a tile, and where region boundaries fall. Placement is currently a fixed
rule, so the co-occurrence signal has no action to influence; this is the leading
explanation of the measured zero throughput gain (slice 39: learner +0.0%,
4.13-4.27x write). Before any new axis is added, its headroom is measured here
with deterministic counts - never wall-clock time and never intuition.

Metric
------
**Dependent random reads per request**: walk a request's reads in the order the
request adapter (`engine/src/request.rs`) issues them; a read counts when it
cannot be served from the same tile as the previous read of that request. The
first read always counts, so a request whose every read lands in one tile counts
1 (*dependent depth = 1 + independent fan-out*, `docs/8_DETAIL.md`). This is a
count of tiles touched in sequence, not a latency.

The trace is the repository's own deterministic inventory workload
(`generator.Generator`, the same stream `inventory_workload.py` replays and
`load_profile.py` labels with `classify`): a self-implemented SplitMix64 rng and
an integer Zipf, so the same seed and dimensions produce a byte-identical request
sequence anywhere.

Placements compared
-------------------
(a) ``today``     the baseline. Keys ordered by their key-family prefix, cut at
                  per-region ``(marker, entity)`` boundaries: one tile starts at
                  every region change and every ``tile_entries`` entries within a
                  region (`RegionMode::PerRegion`-style,
                  `engine/src/keys.rs::region_prefix`). This is NOT the shipped
                  default - that is `RegionMode::Global` (`engine/src/engine.rs:57`),
                  one whole-keyspace tile per publish. The per-region construction
                  is kept because it encodes the region-axis placement decision that
                  co-location would change and is the conservative (highest-count)
                  baseline, so it cannot inflate the measured reduction. The verifier
                  confirmed the reduction does not depend on the choice: recomputed
                  as a Global-style whole-key-order placement the baseline is still
                  94,798 at every tile size and the reduction is unchanged
                  (34.76/34.76/34.77/34.82/37.08% at 64/256/1024/4096/16384). It is
                  also degenerate: because the module starts a new tile at every
                  region change and zero of 20,000 requests reads two keys of one
                  region, the baseline is mathematically forced to equal the raw
                  read count (94,798) and is flat at every tile size - a property of
                  the modelled workload, not an independent measurement of today's
                  layout (see ``.scratch/43-placement-headroom/issues.md``).
(b) ``colocate``  family co-location: a row's derived ``U|X`` and ``R|X`` entries
                  are placed in the same tile as its ``P|X`` row, so the row's
                  keys are adjacent. Rows are grouped by entity; a tile holds a
                  contiguous run of rows (``tile_entries`` entries).
(c) ``freq``      region boundaries chosen from the observed key distribution
                  instead of the fixed rule: keys are ordered by observed access
                  count (hottest first, ties broken by engine key order) and cut
                  into ``tile_entries``-entry tiles, so hot keys share tiles.

Tile-size assumption
--------------------
A tile is a fixed-capacity run of ``tile_entries`` **entries** in the placement's
key order. The answer depends on it, so every result is printed for a sweep
(default 64..16384; the headline sizes are 256 and 4096). The design's 8 MB tile
is ~4096 entries at ~2 KiB/entry. Sensitivity is shown, not asserted.

Key-space model
---------------
The read keys are derived from each emitted request exactly as the adapter reads
them (the unique lookup, then the primary row; see ``read_sequence``). Their tile
comes from their position in the placement's order over the whole store, which is
enumerated deterministically: every seeded Customer/Stock row and every
StockMovement/Order written by the trace, plus the (would-be) unique keys the
flow probes. The store composition is a documented assumption: it only shifts
positions, and every request reads keys of distinct regions, so the ``today``
count is a region count and is insensitive to composition and tile size. That also
makes it degenerate: with zero of 20,000 requests reading two keys of one region,
the ``today`` count is mathematically forced to the raw read count (94,798) at
every tile size - a property of the modelled workload, not an independent
measurement of today's layout.

Usage (from the harness directory)::

    uv run python -m flashdb_harness.placement_headroom
    uv run python -m flashdb_harness.placement_headroom --requests 20000 --tile-entries 256 4096
"""

from __future__ import annotations

import argparse
import sys
from collections import Counter
from dataclasses import dataclass

from flashdb_harness.generator import Generator, email_name, loc_name, sku_name
from flashdb_harness.load_profile import classify

# The two headline tile sizes (entries) plus a sweep for sensitivity. An 8 MB
# tile at ~2 KiB/entry is ~4096 entries; 256 is the small-tile end.
TILE_ENTRIES = (256, 4096)
TILE_SWEEP = (64, 256, 1024, 4096, 16384)

# A read-level willingness-to-pursue bar, explicitly NOT the pre-registration's
# throughput threshold: the pre-registration's 15% is on sustained offered load (a
# throughput quantity), which this count of dependent reads cannot evaluate.
READ_LEVEL_BAR = 15.0

# Byte order of the engine's key markers: P 0x50 < R 0x52 < U 0x55.
MARKER_RANK = {"P": 0, "R": 1, "U": 2}


@dataclass(frozen=True)
class Key:
    """One logical key: its region, its co-location row, and its byte-order tail.

    ``region`` is the engine's ``(marker, entity)`` partition unit. ``row`` is the
    logical row the key derives from, and is what family co-location groups on
    (a row's ``P``/``U``/``R`` keys share it). ``tail`` is the within-region sort
    suffix (a deterministic surrogate for the encoded key bytes).
    """

    marker: str
    entity: str
    row: tuple
    tail: tuple

    @property
    def region(self) -> str:
        return f"{self.marker}|{self.entity}"


def primary_key(entity: str, row: tuple) -> Key:
    """``P|<entity>`` row key; the tail orders rows the way handles do (in insertion order)."""
    return Key("P", entity, row, row)


def unique_key(entity: str, field: str, value: object, row: tuple) -> Key:
    """``U|<entity>`` key; the tail is ``(field, value)`` as the encoded key orders it."""
    return Key("U", entity, row, (field, value))


def reverse_key(entity: str, field: str, row: tuple) -> Key:
    """``R|<entity>`` key; never read by the inventory flow, kept so region sizes are real."""
    return Key("R", entity, row, (field,))


# ---------------------------------------------------------------------------
# Read sequences (mirrors engine/src/request.rs, the single source of read order)
# ---------------------------------------------------------------------------
def read_sequence(req: dict) -> list[Key]:
    """The keys one request reads, in the adapter's execution order."""
    blocks = req["blocks"]
    if "take" in blocks:
        # order_flow: customer lookup, then order_no uniqueness probes on Order and
        # Backorder, then the stock lookup. Each lookup is unique_key -> primary_key.
        p = req["params"]
        email, order_no, sku, loc = p["email"], p["order_no"], p["sku"], p["loc"]
        cust_row = (email,)
        stock_row = (sku, loc)
        return [
            unique_key("Customer", "email", email, cust_row),
            primary_key("Customer", cust_row),
            unique_key("Order", "order_no", order_no, (order_no,)),
            unique_key("Backorder", "order_no", order_no, (order_no,)),
            unique_key("Stock", "sku_loc", (sku, loc), stock_row),
            primary_key("Stock", stock_row),
        ]
    q = blocks["q"]
    if "get" in q:  # stock_lookup
        w = q["get"]["Stock"]["where"]
        stock_row = (w["sku"], w["loc"])
        return [
            unique_key("Stock", "sku_loc", (w["sku"], w["loc"]), stock_row),
            primary_key("Stock", stock_row),
        ]
    if "Customer" in q["find"]:  # customer_lookup
        email = q["find"]["Customer"]["email"]
        return [
            unique_key("Customer", "email", email, (email,)),
            primary_key("Customer", (email,)),
        ]
    # movements_scan: one sequential scan of the P|StockMovement region -> one
    # random positioning read (its many tile touches are sequential, not random).
    return [Key("P", "StockMovement", ("scan",), ("scan",))]


# ---------------------------------------------------------------------------
# Store enumeration (deterministic, from the same generator dimensions)
# ---------------------------------------------------------------------------
def build_store_and_trace(seed: int, requests: int, customers: int, skus: int, locs: int):
    """Return ``(universe, read_seqs)``: every store key and every request's read keys."""
    universe: set[Key] = set()

    def add(k: Key) -> None:
        universe.add(k)

    for i in range(customers):
        email = email_name(i)
        add(primary_key("Customer", (email,)))
        add(unique_key("Customer", "email", email, (email,)))
    for rank in range(1, skus + 1):
        sku = sku_name(rank)
        for j in range(locs):
            row = (sku, loc_name(j))
            add(primary_key("Stock", row))
            add(unique_key("Stock", "sku_loc", (sku, loc_name(j)), row))

    gen = Generator(seed, requests, customers, skus, locs)
    read_seqs: list[list[Key]] = []
    for req in gen.requests():
        seq = read_sequence(req)
        read_seqs.append(seq)
        for k in seq:
            add(k)
        if classify(req) == "order_flow":
            # Each committed order_flow writes a StockMovement and an Order row
            # (P/U/R each). The seeded on_hand (1e6) is never exhausted by this
            # trace, so no Backorder row is written; the flow still probes the
            # Backorder unique key, which is already in the read sequence.
            order_no = req["params"]["order_no"]
            movement_no = "M" + order_no[1:]
            for entity, value in (("StockMovement", movement_no), ("Order", order_no)):
                row = (value,)
                add(primary_key(entity, row))
                add(unique_key(entity, "movement_no" if entity == "StockMovement" else "order_no", value, row))
                add(reverse_key(entity, "stock" if entity == "StockMovement" else "buyer", row))
    return universe, read_seqs


# ---------------------------------------------------------------------------
# Placement -> key order -> tile ids
# ---------------------------------------------------------------------------
def sort_key(k: Key, placement: str, counts: Counter) -> tuple:
    if placement == "today":
        return (MARKER_RANK[k.marker], k.entity, k.tail)
    if placement == "colocate":
        return (k.entity, k.row, MARKER_RANK[k.marker])
    if placement == "freq":
        return (-counts.get(k, 0), MARKER_RANK[k.marker], k.entity, k.tail)
    raise ValueError(f"unknown placement {placement!r}")


def tile_map(universe, placement: str, tile_entries: int, counts: Counter) -> dict[Key, int]:
    """Assign every key a tile id under one placement and one tile capacity.

    ``today`` uses per-region boundaries: a new tile starts at every new
    ``(marker, entity)`` region and every ``tile_entries`` entries within a region.
    ``colocate``/``freq`` are derived arrangements (a reordered key space), so a
    tile is simply every ``tile_entries`` consecutive entries.
    """
    if tile_entries < 1:
        raise ValueError("tile_entries must be >= 1")
    ordered = sorted(universe, key=lambda k: sort_key(k, placement, counts))
    tiles: dict[Key, int] = {}
    if placement == "today":
        tid = -1
        prev_region = None
        used = 0
        for k in ordered:
            if k.region != prev_region:
                tid += 1
                used = 1
                prev_region = k.region
            elif used == tile_entries:
                tid += 1
                used = 1
            else:
                used += 1
            tiles[k] = tid
    else:
        for i, k in enumerate(ordered):
            tiles[k] = i // tile_entries
    return tiles


# ---------------------------------------------------------------------------
# The metric
# ---------------------------------------------------------------------------
def dependent_reads(tile_sequence) -> int:
    """Reads that cannot be served from the previous read's tile.

    ``1 + number of tile changes``; 0 for an empty sequence, 1 for a single read
    or a run inside one tile. This is the dependent depth of ``docs/8_DETAIL.md``.
    """
    if not tile_sequence:
        return 0
    return 1 + sum(1 for a, b in zip(tile_sequence, tile_sequence[1:]) if a != b)


def count(read_seqs, tiles: dict[Key, int]) -> tuple[int, list[int]]:
    per_request = [dependent_reads([tiles[k] for k in seq]) for seq in read_seqs]
    return sum(per_request), per_request


def measure(universe, read_seqs, placement: str, tile_entries: int,
            counts: Counter | None = None) -> dict:
    if counts is None:
        counts = Counter(k for seq in read_seqs for k in seq)
    tiles = tile_map(universe, placement, tile_entries, counts)
    total, per_request = count(read_seqs, tiles)
    return {
        "placement": placement,
        "tile_entries": tile_entries,
        "tiles": len(set(tiles.values())),
        "total_dependent_reads": total,
        "reads_per_request": total / len(read_seqs) if read_seqs else 0.0,
        "per_request": per_request,
    }


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------
def reduction(total: int, base: int) -> float:
    return (base - total) / base * 100.0 if base else 0.0


def report(rows_by_tile: dict[int, list[dict]]) -> None:
    print("# Placement headroom: dependent random reads per request")
    print("# workload = generator MIX (order_flow 70, stock_lookup 15, "
          "customer_lookup 10, movements_scan 5)")
    for tile_entries, rows in rows_by_tile.items():
        base = next(r for r in rows if r["placement"] == "today")
        print(f"\n## tile_entries={tile_entries}  (tiles={base['tiles']} today; "
              f"base dependent reads={base['total_dependent_reads']})")
        print(f"  {'placement':<10} {'dependent':>10} {'per_req':>8} {'vs today':>10}")
        for r in rows:
            print(f"  {r['placement']:<10} {r['total_dependent_reads']:>10} "
                  f"{r['reads_per_request']:>8.3f} {reduction(r['total_dependent_reads'], base['total_dependent_reads']):>9.2f}%")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--requests", type=int, default=20_000)
    ap.add_argument("--customers", type=int, default=5_000)
    ap.add_argument("--skus", type=int, default=10_000)
    ap.add_argument("--locs", type=int, default=8)
    ap.add_argument("--tile-entries", type=int, nargs="+", default=list(TILE_SWEEP))
    a = ap.parse_args(argv)

    universe, read_seqs = build_store_and_trace(
        a.seed, a.requests, a.customers, a.skus, a.locs)
    counts = Counter(k for seq in read_seqs for k in seq)
    print(f"# seed={a.seed} requests={len(read_seqs)} store_keys={len(universe)} "
          f"distinct_read_keys={len(counts)}")

    rows_by_tile: dict[int, list[dict]] = {}
    for tile_entries in a.tile_entries:
        rows_by_tile[tile_entries] = [
            measure(universe, read_seqs, placement, tile_entries, counts)
            for placement in ("today", "colocate", "freq")
        ]
    report(rows_by_tile)

    # Verdict, read-level only. The pre-registration's 15% threshold is on
    # sustained offered load - a throughput quantity - not on dependent reads, so
    # this count can neither clear nor fail it. The bar below is a read-level
    # criterion and is explicitly NOT the pre-registration's threshold.
    print(f"\n## verdict (dependent random reads per request; read-level bar = "
          f"{READ_LEVEL_BAR:g}%, not the pre-registration threshold)")
    for tile_entries in a.tile_entries:
        rows = rows_by_tile[tile_entries]
        base = next(r for r in rows if r["placement"] == "today")
        for r in rows:
            if r["placement"] == "today":
                continue
            gain = reduction(r["total_dependent_reads"], base["total_dependent_reads"])
            print(f"  tile_entries={tile_entries} {r['placement']:<10} "
                  f"{gain:+.2f}% vs today -> {'above' if gain >= READ_LEVEL_BAR else 'below'} "
                  f"{READ_LEVEL_BAR:g}% read-level bar")
    print("  verdict: family co-location would cut dependent random reads per request "
          "by about 35%, stable across tile sizes; whether that converts into a "
          "sustained-load gain is NOT established by this count and requires an "
          "engine-side experiment.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
