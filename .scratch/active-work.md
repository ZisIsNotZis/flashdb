# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`; WAL-retained safe tile slice integrated through `83bcb2c`, with reviewed post-open corruption qualification pending final commit/test (`engine/src/tile.rs`, `engine/tests/tile_read.rs`, README/docs/ticket). Parent next: finish validation and record revision; then design A/B root+directory manifest and fault matrix without WAL retirement. Snapshot/dedup retention choices deferred. No user decision needed for next slice. No user decision pending.
