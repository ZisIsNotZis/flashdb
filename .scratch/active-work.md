# Active work

- [07-persistent-tiles](07-persistent-tiles/issues/07-persistent-tiles.md) — claimed from `77537a0`. Accepted: WAL-retained tile `ba8791c`, manifest `1c22318`, multi-tile + maybe_checkpoint `9de3cff`, compaction `9003550` (87 Rust + 10 Python; reviewers OK-with-notes each). Disk-path mechanism closed loop done with WAL retained. Parent next: compaction lifecycle hardening, then prepare the author-owned proposal (snapshot lease/expiry + L-02 dedup retention) that gates version dropping / WAL retirement.
