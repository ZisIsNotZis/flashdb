# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`. Accepted: WAL-retained tile `ba8791c`, manifest `1c22318`, multi-tile + maybe_checkpoint `9de3cff`, compaction `9003550`, latest-only reads + superseded-version compaction `315b49e` (88 Rust + 10 Python). Author decisions resolved: no historical reads, idempotency via business uniqueness (L-02 closed). Parent next: WAL rotation slice (suffix-only replay, fault-injected), then the cgroup data:RAM experiment. No user decision pending.
