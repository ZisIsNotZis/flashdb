# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`. Accepted: WAL-retained tile slice `ba8791c` (63 Rust), manifest `1c22318` (76 Rust), multi-tile + maybe_checkpoint `9de3cff` (82 Rust + 10 Python; reviewer OK-with-notes). Parent next: compaction worker (retention per corrected docs/engine.md baseline), then review + integrate. Snapshot lease/expiry and L-02 dedup retention remain author-owned gates for version dropping / WAL retirement only. No user decision pending.
