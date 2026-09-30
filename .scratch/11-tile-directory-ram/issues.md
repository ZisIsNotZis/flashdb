# 11 — in-RAM tile directory (fence set) is proportional to tile bytes

Opened 2026-09-30 from slice 10c-1's measurement, which fixed the *big* compaction peak but
exposed the residual: `Tile::open` loads **every** fence record of a tile into RAM
(`fences: Vec<Fence>`, one `Fence { first_key: Vec<u8>, offset: u64, len: u32 }` per data
block, i.e. one small heap allocation per 4 KiB block).

Measured (10c-1, after the streaming fix): compaction peak is now ~0.07× tile bytes and no
longer 3.28×, with the residual explicitly attributed to fence allocation — 135 k small
`vec![]`s for 554 MB of tiles (≈ 40 MiB), and the worker flagged that it would approach the
50 MiB target again near ~700 MB of tiles.

## Why this blocks the data:RAM objective

The residual scales with tile bytes, so it does not disappear at scale; it just moved out of
compaction:

- 20 GiB of tiles ≈ 5.2 M data blocks ≈ 5.2 M fences ≈ hundreds of MB to > 1 GiB of RAM
  depending on key size, independent of how little data the workload touches.
- Every open tile in the active list (up to `manifest::MAX_TILES = 8`) pays this, and the
  fence set is held for the engine's whole lifetime, not just during an operation.

That is incompatible with any honest data:RAM ratio at multi-GB scale, and it is the same
class of defect as ticket 10 (memory proportional to data rather than to the working set).

## Direction (needs a design decision)

- Keep the directory **on disk** as the authoritative structure (it already is: CRC-protected
  directory blocks) and hold only a **sparse** RAM index (e.g. the first fence of every
  directory block, or every Nth fence), reading the directory block needed for a binary
  search on demand. The read path already tolerates a "wrong candidate" (a cursor re-verifies
  the block and its fence), so a sparse index costs at most one extra block read.
- Alternative, cheaper but weaker: keep all fences but store them in one arena
  (`Vec<u8>` + `(offset, len)` pairs, no per-fence `Vec`/allocation). Bounds the constant
  factor roughly 3-5×, does not change the asymptotics.
- Either way, the acceptance test is a stated number: fence RAM per GiB of tile data, and a
  measurement at ≥ some GiB of tiles showing it stays inside a declared budget.

## Related

- Ticket 10 (`10-write-path-memory`) — the write path; this is the read/open side of the
  same discipline.
- Ticket 08 (`08-tile-pages`) — FDBTILE2 already writes the directory this ticket would seek
  into; no format change is expected.
