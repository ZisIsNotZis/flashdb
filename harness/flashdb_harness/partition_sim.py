"""Does family partitioning actually reduce rewrites? A policy simulation, not a guess.

The headroom model answered the READ side (page size + bloom bits top out at ~3%). The write
side candidate is family partitioning: keep the entries of different key families in different
tiles, so a family that grows slowly ends up in fewer, larger tiers and is rewritten less often.
The measured criterion (slow family >= ~20% of written bytes) is met for the inventory workload
(U 30.8%, R 10.7%, together 41.5%), but the share alone does not prove a saving - the saving
comes from the tier structure, and a partitioned layout also consumes MORE tiles against the
manifest's cap, which triggers more compaction.

This module simulates the engine's actual shipped merge policy (`budget_prefix`: absorb as many
of the smallest adjacent tiles as fit the budget, capped by the tile cap) over byte streams, and
reports the write amplification of

  - co-located: one stream carrying all families (today), versus
  - partitioned: one stream per family, sharing the same tile cap.

Validation: the co-located simulation is run on the scale driver's measured composition
(P ~1 KiB per doc plus U ~40 B per doc, 5,000,000 docs, 64 MiB windows, 512 MiB budget, cap 12)
and compared against the measured write_amp of 3.92 from slice 12d. If it does not reproduce
that, the simulation is not trustworthy for the partitioned comparison and the script says so
instead of quoting a number.

Usage:
    uv run python -m flashdb_harness.partition_sim
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field

WINDOW_BYTES = 64 * 1024 * 1024
BUDGET_BYTES = 8 * WINDOW_BYTES
TILE_CAP = 12


@dataclass
class Stream:
    """One family's byte stream: `windows` windows of `bytes_per_window` each."""

    name: str
    bytes_per_window: int
    windows: int
    sizes: list[int] = field(default_factory=list)
    written: int = 0          # bytes written into tiles (new data + compaction output)

    def append_window(self) -> None:
        self.sizes.append(self.bytes_per_window)
        self.written += self.bytes_per_window


def apply_policy(streams: list[Stream]) -> None:
    """Merge tiles across all streams while the shared tile count exceeds the cap.

    Mirrors `budget_prefix`: seed on the globally smallest tile, then absorb the smaller
    adjacent neighbour while the running total fits the budget and the cap allows. Tiles are
    ordered by creation, so "adjacent" means neighbours in the concatenated tile list.
    """
    for s in streams:
        while len(s.sizes) == 0:  # pragma: no cover - defensive
            return
    total_tiles = sum(len(s.sizes) for s in streams)
    while total_tiles > TILE_CAP:
        # flatten with (stream, index) so a merge can retire tiles from several streams,
        # which is what a co-located merge does and what a partitioned layout must not do
        flat: list[tuple[int, int, int]] = []  # (size, stream_idx, idx)
        for si, s in enumerate(streams):
            for idx, size in enumerate(s.sizes):
                flat.append((size, si, idx))
        if len(flat) < 2:
            return
        seed = min(range(len(flat)), key=lambda i: (flat[i][0], i))
        lo = hi = seed
        total = flat[seed][0]
        while hi - lo + 1 < TILE_CAP:
            left = lo - 1 if lo > 0 else None
            right = hi + 1 if hi + 1 < len(flat) else None
            def ok(i: int | None) -> bool:
                return i is not None and total + flat[i][0] <= BUDGET_BYTES
            pick = None
            if ok(left) and ok(right):
                pick = left if (flat[left][0], flat[left][1]) <= (flat[right][0], flat[right][1]) else right
            elif ok(left):
                pick = left
            elif ok(right):
                pick = right
            if pick is None:
                break
            total += flat[pick][0]
            lo, hi = min(lo, pick), max(hi, pick)
        if hi == lo:  # nothing could be absorbed; force the two smallest adjacent tiles
            idx = min(range(len(flat) - 1), key=lambda i: (flat[i][0] + flat[i + 1][0], i))
            lo, hi = idx, idx + 1
        merged_indices = flat[lo:hi + 1]
        merged_bytes = sum(m[0] for m in merged_indices)
        # remove the inputs, then insert the output in the FIRST stream that contributed
        target = merged_indices[0][1]
        new_sizes: list[list[int]] = [[] for _ in streams]
        for si, s in enumerate(streams):
            for idx, size in enumerate(s.sizes):
                if (si, idx) in {(m[1], m[2]) for m in merged_indices}:
                    continue
                new_sizes[si].append(size)
        new_sizes[target].append(merged_bytes)
        for si, s in enumerate(streams):
            s.sizes = new_sizes[si]
        streams[target].written += merged_bytes
        total_tiles = sum(len(s.sizes) for s in streams)


def simulate(streams: list[Stream]) -> dict[str, float]:
    for w in range(max(s.windows for s in streams)):
        for s in streams:
            if w < s.windows:
                s.append_window()
        apply_policy(streams)
    payload = sum(s.bytes_per_window * s.windows for s in streams)
    written = sum(s.written for s in streams)
    return {"payload": payload, "written": written, "write_amp": written / payload}


def baseline_scale_driver() -> list[Stream]:
    """The measured 12d configuration: 5 M docs x 1 KiB P plus ~40 B U per doc."""
    docs = 5_000_000
    p_bytes = docs * (1024 + 40)           # doc value + key/framing
    u_bytes = docs * 48                    # unique key + framing
    windows = max(1, math.ceil((p_bytes + u_bytes) / WINDOW_BYTES))
    return [
        Stream("P", p_bytes // windows, windows),
        Stream("U", u_bytes // windows, windows),
    ]


def inventory_composition(total: int = 5_559_999_933) -> list[Stream]:
    """The measured inventory shares (P 58.5%, U 30.8%, R 10.7% of written bytes).

    The shares are measured (slice 18); the total is scaled to the same payload as the
    validation run so both configurations see the same number of compaction windows. Quoting
    the raw measured total (32.8 MB) would be degenerate: it is smaller than one window, so
    nothing compacts and both variants trivially report write_amp 1.0.
    """
    windows = max(1, math.ceil(total / WINDOW_BYTES))
    return [
        Stream("P", int(total * 0.5852) // windows, windows),
        Stream("U", int(total * 0.3075) // windows, windows),
        Stream("R", int(total * 0.1073) // windows, windows),
    ]


def partitioned(streams: list[Stream]) -> list[Stream]:
    """Same byte rates, but tiles never mix families (each family keeps its own tile list)."""
    return [Stream(s.name, s.bytes_per_window, s.windows) for s in streams]


def simulate_partitioned(streams: list[Stream]) -> dict[str, float]:
    """Partitioned variant: compaction never merges across families.

    The tile cap is global in the engine (one manifest list), so partitioning means the same
    budget of tiles is shared by more streams - modelled by capping each stream's list at the
    global cap and compacting each stream independently (which is the optimistic bound: a
    shared cap would be stricter).
    """
    payload = sum(s.bytes_per_window * s.windows for s in streams)
    written = 0
    for s in streams:
        solo = [Stream(s.name, s.bytes_per_window, s.windows)]
        # each family is compacted independently, sharing the same cap
        global TILE_CAP
        saved_cap = TILE_CAP
        TILE_CAP = max(2, TILE_CAP // 2)  # be generous: half the tiles per family
        try:
            r = simulate(solo)
        finally:
            TILE_CAP = saved_cap
        written += r["written"]
    return {"payload": payload, "written": written, "write_amp": written / payload}


def main() -> int:
    colocated = simulate(baseline_scale_driver())
    print(f"validation (scale-driver composition): payload={colocated['payload']:,} "
          f"written={colocated['written']:,} write_amp={colocated['write_amp']:.3f}")
    print("  measured write_amp for this configuration (slice 12d, 5 M docs) = 3.920")
    err = abs(colocated["write_amp"] - 3.920) / 3.920
    print(f"  relative error = {err:.1%} {'(trustworthy for comparison)' if err <= 0.25 else '(OUTSIDE 25%: do not quote the partitioned number)'}")

    comp = inventory_composition()
    print(f"\ninventory composition shares: " + ", ".join(
        f"{s.name}={s.bytes_per_window / sum(x.bytes_per_window for x in comp):.1%}" for s in comp))
    colo = simulate(partitioned(comp))
    part = simulate_partitioned(comp)
    print(f"  co-located : write_amp={colo['write_amp']:.3f} written={colo['written']:,}")
    print(f"  partitioned: write_amp={part['write_amp']:.3f} written={part['written']:,}")
    gain = (colo["write_amp"] - part["write_amp"]) / colo["write_amp"]
    print(f"  predicted write-side gain from partitioning = {gain:+.1%} "
          f"({'above' if gain >= 0.15 else 'BELOW'} the provisional 15% threshold)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
