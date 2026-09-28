# flashdb

Experimental single-node document store for IO-bound workloads. Its research goal is to learn physical layout from workload traces rather than require users to declare a layout. **The learned-layout engine is not implemented yet.**

## Current state

- Rust prototype: versioned P/U/R keys, WAL with CRC and torn-tail recovery, memtable snapshots, per-block atomic commit, unique-value enforcement and reverse lookup. A narrowly scoped adapter executes the inventory generator's `cust → take` order-flow template (including stock-decrement and backorder branches); arbitrary request DAGs and operators are **not** supported. This is an in-memory index backed by a WAL, **not** a complete LSM: no tile files, compaction or manifest yet.
- Python harness: deterministic inventory request generator (SplitMix64, integer Zipf), with a frozen, hashed JSONL corpus. The experimental Rust replay command consumes all four generator request shapes from an **existing, pre-seeded WAL**; it does not bootstrap data or implement a general API.
- Device calibration: `bench/0a-redo.sh` runs fio and a Rust `O_DIRECT read_at` microbenchmark under a CPU-idle guard. The latest measurements are *reduced-load references*, not clean-idle calibration; see `.scratch/05-engine-v0/evidence/0a-redo/nvme-bench-analysis.md`.
- Design and open decisions are recorded in `docs/` and `.scratch/`. The proposed request contract is broader than the implemented prototype and is not yet a production API.

## Run the existing tests

Requires Rust 1.98.1. From the repository root:

```sh
cargo test --workspace
```

For the Python harness, use Python 3.12 and `uv`:

```sh
cd harness
uv run --with pytest pytest
```

The fio calibration additionally requires `fio` and an idle NVMe; do not run it on a busy host or assume its hard-coded benchmark path is safe for another environment.

## Documentation

| File | Purpose |
|---|---|
| `docs/design.md` | Research thesis, scope and invariants. |
| `docs/engine.md` | Intended LSM, snapshots, tiles and recovery. |
| `docs/contracts.md` | Proposed request, transaction and layout contracts; check status/open sections. |
| `docs/objective.md` | Cost objective; several author decisions remain open. |
| `docs/learning.md` | Offline learning and later adaptation. |
| `docs/dev-loop.md` | Workload, validation and experiment plan. |
| `docs/glossary.md` | Terminology. |
| `docs/prior-art.md` | Prior-art notes; verify citations before external use. |

Reference target: one NVMe and data:RAM ≥20:1 in a cgroup-limited experiment (for example 200 GB / 8 GB). Multi-node operation, online drift and SQL are outside v0 scope.
