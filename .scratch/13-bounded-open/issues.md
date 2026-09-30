# 13 — engine open is O(total data) (unbounded recovery time)

Opened 2026-09-30 from the 5 GB end-to-end run (ticket 12).

## Observation

`open_impl` verifies every published tile against the retained WAL projection
(`verify_projection` reads all of a tile's entries) and replays the WAL suffix. The WAL side
is bounded by the rotation window after 07's work, but the tile side is not: opening an
engine reads **every entry of every active tile**. In the 5 GB run the `verify` phase (which
includes opening the engine) took 34 s, and the seed phase's own restarts would pay the same
cost. There is no bound on open time in terms of data size.

This is the last structural O(data) cost in the read/open path after tickets 07, 08 and 10-12
(probes are indexed, scans are streaming, memory is window-bounded, compaction is O(log N)).
It is also a *recovery* claim: "recovery replays only the suffix" is true for the WAL, but
not for tile verification.

## Why it matters

- Recovery time on a 100 GB dataset would be minutes of pure verification before serving,
  and it grows linearly forever.
- It couples restart cost to dataset size, which is exactly the property the design set out
  to avoid ("no bounded-RAM claim was made, but the layout should be learned, not declared").

## Direction (needs a design decision)

The tiles were verified en bloc when they were published and are immutable, covered by
per-block CRCs and a WAL-digest binding. So the honest options are:

1. **Trust-on-open, verify-on-read**: at open, validate structure only (superblock, directory,
   trailer, CRCs of metadata) and rely on per-block CRC checks during reads, plus the digest
   binding as a cheap identity check. That is what the tile reader already does per block;
   the open-time full projection comparison is the redundant part. Risk: a tile that was
   silently corrupted *between* publications is only detected when a read touches the
   affected block, i.e. detection becomes lazy rather than eager.
2. **Verify new tiles only** (the original 08e idea): keep the eager check for tiles published
   since the last open (they are bounded by the window) and trust older ones.
3. **Background verification**: open validates structure and serves immediately, then a
   verifier walks tiles in the background, failing the engine closed if it finds damage.

Also in scope: the same eager verification happens on every publish for the tile being
written (fine, it is one window) and on every compaction (bounded by the merge set).

## Acceptance

A stated open time for a multi-GB engine (measured), decoupled from data size, plus an
explicit statement of what is verified eagerly versus lazily and what corruption classes each
option catches. Anything that weakens the eager check must say so in the contract docs.
