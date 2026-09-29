# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`; WAL-retained tile slice accepted at `ba8791c` (63 Rust + 10 Python tests). Manifest slice in progress: isolated worker `7f73db2d-2c5f-48d4-a363-821b1af181f0` builds A/B manifest publish/discover with fault injection; parent next: independent review then integrate. WAL still fully retained; no rotation/compaction. Snapshot lease/dedup-retention choices deferred to author. No user decision pending.
