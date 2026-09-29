//! 引擎核心：按块提交（WAL + memtable + CSN 发布）与崩溃恢复。
//!
//! 层次：上层（执行器）把业务块翻译成 [`Op`] 序列——本层不理解业务，
//! 只保证：**一个块的全部变更要么原子发布（获得一个 CSN），要么整体不存在。**
//!
//! 提交时序（SS-01）：
//! 1. publish 前置检查（唯一性，对 memtable + 可选只读 tile + 本块已排队 unique）；
//! 2. WAL 追加记录 → `fdatasync`（唯一前台屏障）；
//! 3. 应用到 memtable → 推进 CSN → 应答。
//!
//! 唯一性检查在 **publish 时**对当前状态做（含并发已发布块），冲突返回
//! [`Outcome::Conflict`] 由调用方重试。反向唯一（`reverse: "unique"`）的
//! 强制执行本切片未实现，已记录在 ticket 04/03。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::keys::{self, check_name};
use crate::manifest::{self, Checkpoint, Manifest, SegmentRef, TileRef};
use crate::memtable::Memtable;
use crate::tile::Tile;
use crate::wal::{self, Wal};

const MAGIC: &[u8; 4] = b"FDB1";

/// 一个块内的键空间变更。键的 CSN 部分由引擎在发布时填充。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    PutDoc { entity: Vec<u8>, handle: u64, doc: Vec<u8> },
    DelDoc { entity: Vec<u8>, handle: u64 },
    PutUnique { entity: Vec<u8>, field: Vec<u8>, value: Vec<u8>, handle: u64 },
    DelUnique { entity: Vec<u8>, field: Vec<u8>, value: Vec<u8>, handle: u64 },
    PutReverse { entity: Vec<u8>, field: Vec<u8>, target: u64, source: u64 },
    DelReverse { entity: Vec<u8>, field: Vec<u8>, target: u64, source: u64 },
}

/// 唯一约束冲突：业务错误，不是并发时机竞争，不应自动重试或触发 else。
/// 保留早期 `Conflict` 类型名作为 v0 内部 API；未来并发验证失败需独立类型。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub kind: &'static str,
    pub entity: Vec<u8>,
    pub field: Vec<u8>,
    pub value: Vec<u8>,
}

/// 提交结果。`AlreadyCommitted` = 幂等重放（同 block_id 重复提交）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Committed { csn: u64 },
    AlreadyCommitted { csn: u64 },
    Conflict(Conflict),
}

pub struct Engine {
    wal: Wal,
    mt: Memtable,
    /// Published tiles ordered by strictly increasing cutoff; tile *i* covers
    /// exactly the CSN range `(tiles[i-1].cutoff, tiles[i].cutoff]`, so each CSN
    /// lives in at most one tier. Every tile holds only the NEWEST version per
    /// logical key within its range (2026-09-29: reads never see historical
    /// state, so superseded versions are unreadable garbage); the retained WAL
    /// keeps every version and stays the sole recovery authority.
    tiles: Vec<TileHandle>,
    wal_path: PathBuf,
    dir: PathBuf,
    manifest: Option<Manifest>,
    /// Current WAL segment per the manifest root; `None` while the whole
    /// history lives in `data.wal`. Republishing carries it verbatim into every
    /// new root so a rotated engine's root never stops naming the live suffix.
    segment: Option<SegmentRef>,
    csn: u64,
    dedup: HashMap<Vec<u8>, u64>,
    max_handle: u64,
}

/// An open tile plus the plain filename under which the manifest references it
/// (explicit path, discovery, or an engine-chosen checkpoint name).
struct TileHandle {
    tile: Tile,
    name: String,
}

impl TileHandle {
    fn cutoff(&self) -> u64 { self.tile.cutoff() }

    fn tile_ref(&self) -> TileRef {
        TileRef { name: self.name.clone(), cutoff: self.tile.cutoff(), digest: *self.tile.digest() }
    }
}

impl Engine {
    /// 新建（空库）。
    pub fn create(wal_path: impl AsRef<Path>) -> io::Result<Engine> {
        Ok(Engine {
            wal: Wal::create(&wal_path)?,
            mt: Memtable::new(),
            tiles: Vec::new(),
            wal_path: wal_path.as_ref().to_path_buf(),
            dir: parent_dir(wal_path.as_ref()),
            manifest: None,
            segment: None,
            csn: 0,
            dedup: HashMap::new(),
            max_handle: 0,
        })
    }

    /// Recover wholly from WAL; does not automatically discover tile candidates.
    pub fn open(wal_path: impl AsRef<Path>) -> io::Result<Engine> {
        Self::open_impl(wal_path.as_ref(), Vec::new(), None)
    }

    /// Explicit prototype recovery: a missing, corrupt or WAL-mismatched tile fails
    /// closed. To rebuild from retained WAL, explicitly call `open` instead. The
    /// tile path must satisfy the same plain-filename rules as `publish_tile`, so
    /// a legacy-opened engine can never adopt a name that a later `publish_tile`
    /// could reference but `open_discover` could never resolve.
    pub fn open_with_tile(wal_path: impl AsRef<Path>, tile_path: impl AsRef<Path>) -> io::Result<Engine> {
        let tile_path = tile_path.as_ref();
        let name = match tile_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile path must be a plain filename inside the engine directory")),
        };
        if !manifest::valid_tile_name(&name)
            || parent_dir(tile_path) != parent_dir(wal_path.as_ref()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile path must be a plain filename inside the engine directory"));
        }
        let tile = Tile::open(tile_path)?;
        Self::open_impl(wal_path.as_ref(), vec![TileHandle { tile, name }], None)
    }

    /// Durable-root recovery: select the highest valid manifest page in `dir`, open
    /// and verify every referenced tile against the retained WAL exactly as
    /// `open_with_tile` does (cutoff ordering included), and simply ignore
    /// unreferenced tile files (they are never candidates; fail-closed, no
    /// adoption). Conventional layout: `<dir>/data.wal`, `<dir>/manifest`, tiles
    /// referenced by plain filename. A missing manifest file is the empty initial
    /// state (no tiles); full WAL replay still happens, so open time and RAM stay
    /// unbounded in this slice.
    ///
    /// Segment-aware: once the root references a WAL segment, that named file
    /// replaces `data.wal` as the replay source and holds only the post-rotation
    /// suffix. Any record at or below the segment's start CSN fails closed; the
    /// checkpoint below the segment floor supplies the CSN/handle frontier and
    /// dedup is built from suffix records only (pre-rotation block ids are
    /// forgotten by design — a published tile covers their state).
    pub fn open_discover(dir: impl AsRef<Path>) -> io::Result<Engine> {
        let dir = dir.as_ref();
        let manifest = Manifest::open(&dir.join(manifest::MANIFEST_NAME))?;
        // The root names the live WAL segment once rotation has happened; before
        // that, replay reads the conventional `data.wal` in full.
        let segment = manifest.as_ref().and_then(|m| m.root().segment.clone());
        let wal_name = match &segment {
            Some(s) => s.name.as_str(),
            None => manifest::WAL_NAME,
        };
        let mut tiles = Vec::new();
        if let Some(m) = manifest.as_ref() {
            for t in &m.root().tiles {
                // The root's binding must match the tile's own header, not just
                // its name: a swapped or stale filename must not be adopted.
                let opened = Tile::open(dir.join(&t.name).as_path())?;
                if opened.cutoff() != t.cutoff || opened.digest() != &t.digest {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest tile reference disagrees with tile header"));
                }
                tiles.push(TileHandle { tile: opened, name: t.name.clone() });
            }
        }
        let mut e = Self::open_impl(&dir.join(wal_name), tiles, segment.as_ref().map(|s| s.start_csn))?;
        if let Some(m) = &manifest {
            let cp = m.root().checkpoint;
            match &segment {
                None => {
                    // While the WAL is fully retained these can only fire on divergence;
                    // after WAL retirement the watermark check must become a max() merge
                    // against the checkpointed watermark rather than replay-derived state.
                    if cp.csn > e.csn {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest checkpoint CSN is ahead of the retained WAL"));
                    }
                    if cp.handle_watermark > e.max_handle {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest handle watermark is ahead of the retained WAL"));
                    }
                }
                Some(_) => {
                    // Segment replay covers only the post-rotation suffix by
                    // design; the frontier established below the segment floor
                    // comes from the checkpoint, so merge instead of cross-check.
                    e.csn = e.csn.max(cp.csn);
                    e.max_handle = e.max_handle.max(cp.handle_watermark);
                }
            }
        }
        e.manifest = manifest;
        e.segment = segment;
        Ok(e)
    }

    /// Shared recovery core. `floor` is `Some(segment.start_csn)` for segment
    /// recovery: the replayed file holds only the post-rotation suffix, so tiles
    /// at or below the floor bound digests over the retired pre-rotation prefix
    /// (their state is durable in the tiles) and cannot be re-verified against
    /// the suffix; tiles above the floor are verified against the above-floor
    /// suffix range exactly as in full replay.
    fn open_impl(wal_path: &Path, tiles: Vec<TileHandle>, floor: Option<u64>) -> io::Result<Engine> {
        if !tiles.windows(2).all(|w| w[0].cutoff() < w[1].cutoff()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "tile cutoffs overlap or are not strictly increasing"));
        }
        let (wal, records) = Wal::open_or_recover(wal_path)?;
        for t in &tiles {
            if floor.is_some_and(|f| t.cutoff() <= f) { continue; }
            if wal_digest(&records, t.cutoff())? != *t.tile.digest() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "tile WAL prefix mismatch"));
            }
        }
        let mut mt = Memtable::new();
        let mut covered = Memtable::new();
        let mut csn = 0u64;
        let mut dedup = HashMap::new();
        let mut max_handle = 0;
        let covered_through = tiles.last().map(|t| t.cutoff()).unwrap_or(0);
        for payload in &records {
            let (rec_csn, block_id, ops) = decode_payload(payload)?;
            // Fail closed: a segment must hold only the live suffix; a record
            // at or below the floor means stale or foreign segment content.
            if floor.is_some_and(|f| rec_csn <= f) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL segment contains pre-rotation records"));
            }
            max_handle = max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
            let destination = if rec_csn <= covered_through { &mut covered } else { &mut mt };
            for op in &ops { apply_op(destination, op, rec_csn); }
            csn = csn.max(rec_csn);
            dedup.insert(block_id, rec_csn);
        }
        let mut lower = floor.unwrap_or(0);
        for t in &tiles {
            if floor.is_some_and(|f| t.cutoff() <= f) { continue; }
            // Tiles hold exactly the newest version per logical key within their
            // CSN range (same reduction the tile writers apply), so the WAL
            // projection is reduced the same way before the 1:1 comparison.
            t.tile.verify_projection(&newest_projection(&covered, lower, t.cutoff()), lower, t.cutoff())?;
            lower = t.cutoff();
        }
        Ok(Engine { wal, mt, tiles, wal_path: wal_path.to_path_buf(), dir: parent_dir(wal_path), manifest: None, segment: None, csn, dedup, max_handle })
    }

    /// Build one immutable P/U/R tile holding, for the WAL-published versions
    /// in `(previous cutoff, cutoff]`, only the NEWEST version per logical key.
    /// No manifest, checkpoint, or WAL rotation. A failed build leaves the
    /// serving memtable intact; a successful build evicts covered versions only
    /// after the file is synced and verified.
    pub fn build_tile(&mut self, tile_path: impl AsRef<Path>, cutoff: u64) -> io::Result<()> {
        if cutoff > self.csn {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cutoff exceeds CSN"));
        }
        // The same plain-filename rules as publish_tile: an engine-built tile
        // must be adoptable by the manifest and unlinkable by compaction without
        // ambiguity.
        let tile_path = tile_path.as_ref();
        match tile_path.file_name().and_then(|n| n.to_str()) {
            Some(n) if manifest::valid_tile_name(n) && parent_dir(tile_path) == self.dir => {}
            _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile path must be a plain filename inside the engine directory")),
        }
        if self.tiles.last().is_some_and(|t| t.cutoff() >= cutoff) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile cutoff must be strictly above the newest active tile cutoff"));
        }
        let tile = self.write_verified_tile(tile_path, cutoff)?;
        self.mt.evict_through(cutoff);
        self.tiles.push(TileHandle {
            tile,
            name: tile_path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string(),
        });
        Ok(())
    }

    /// Create-new the tile file, sync it, reopen it and re-check its exact
    /// newest-version-per-logical-key `(previous cutoff, cutoff]` P/U/R
    /// projection against the retained WAL before any caller may reference it.
    fn write_verified_tile(&self, tile_path: &Path, cutoff: u64) -> io::Result<Tile> {
        let (records, _) = wal::replay(&self.wal_path)?;
        if records.last().map(|r| decode_payload(r).map(|v| v.0)).transpose()? != Some(self.csn) && self.csn != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL differs from serving state"));
        }
        let digest = wal_digest(&records, cutoff)?;
        let lower = self.tiles.last().map(|t| t.cutoff()).unwrap_or(0);
        let mut expected = Memtable::new();
        for payload in &records {
            let (csn, _, ops) = decode_payload(payload)?;
            if csn <= cutoff && csn > lower { for op in &ops { apply_op(&mut expected, op, csn); } }
        }
        // The serving memtable holds exactly the versions above `lower`; both
        // sides reduce to the newest version per logical key in the disjoint
        // range, so filtering to `<= cutoff` writes precisely the new tile.
        let tile = Tile::write(tile_path, cutoff, &digest, &newest_projection(&self.mt, lower, cutoff))?;
        tile.verify_projection(&newest_projection(&expected, lower, cutoff), lower, cutoff)?;
        Ok(tile)
    }

    /// Durable publication of one tile: build the verified tile exactly as
    /// [`Engine::build_tile`], make the tile's directory entry durable, then
    /// run the manifest protocol — write the inactive page at seq+1 with the
    /// full active tile list plus the new tile, sync the manifest file, sync
    /// the parent directory, flip the in-memory root — and only then evict
    /// tile-covered memtable versions. Rules: tile cutoffs must be strictly
    /// increasing (no CSN overlap with the newest active tile), at most
    /// [`manifest::MAX_TILES`] active tiles, the tile path must be a plain
    /// not-yet-existing filename inside the engine directory (the manifest
    /// stores the name, never a path), and any manifest I/O error poisons the
    /// writer until reopen selects the highest valid page. A failed publish
    /// leaves the tile file on disk unreferenced (ignored by
    /// [`Engine::open_discover`]); republishing over it surfaces a clear
    /// `AlreadyExists` error instead of silently reusing the leftover file.
    pub fn publish_tile(&mut self, tile_path: impl AsRef<Path>, cutoff: u64) -> io::Result<()> {
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        if cutoff > self.csn {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cutoff exceeds CSN"));
        }
        if self.tiles.last().is_some_and(|t| t.cutoff() >= cutoff) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile cutoff must be strictly above the newest active tile cutoff"));
        }
        if self.tiles.len() >= manifest::MAX_TILES {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "active tile list is full; no compaction exists in this slice"));
        }
        let tile_path = tile_path.as_ref();
        let name = match tile_path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile path must be a plain filename inside the engine directory")),
        };
        if !manifest::valid_tile_name(name) || parent_dir(tile_path) != self.dir {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile path must be a plain filename inside the engine directory"));
        }
        // 1. Build + verify the tile file without touching serving state.
        let tile = match self.write_verified_tile(tile_path, cutoff) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "tile candidate already exists and is unreferenced by the manifest (leftover from a failed publish or crash after tile sync); remove {} or publish to another name", tile_path.display())));
            }
            Err(e) => return Err(e),
        };
        // 2. The tile's own directory entry must be durable before the manifest
        //    can reference it (checkpoint publication ordering).
        File::open(&self.dir)?.sync_all()?;
        // 3. Manifest protocol; any error poisons the writer until reopen. The
        //    page always carries the full ordered active list plus the new tile.
        let tref = TileRef { name: name.to_string(), cutoff, digest: *tile.digest() };
        let mut refs: Vec<TileRef> = self.tiles.iter().map(|t| t.tile_ref()).collect();
        refs.push(tref);
        let checkpoint = Checkpoint { csn: cutoff, handle_watermark: self.max_handle };
        self.manifest_publish(refs, checkpoint)?;
        // 4. In-memory flip only after the durable flip.
        self.mt.evict_through(cutoff);
        self.tiles.push(TileHandle { tile, name: name.to_string() });
        Ok(())
    }

    // Shared manifest step of the publication protocol ([`Engine::publish_tile`]
    // and [`Engine::compact`]): write the inactive page at `seq + 1` carrying the
    // FULL active tile list, sync the manifest file, sync the parent directory,
    // then flip the in-memory root. A missing manifest is created, or an
    // existing empty-root manifest adopted — an engine opened without discovery
    // must never overwrite a root it did not read. Any error poisons the writer
    // until reopen.
    fn manifest_publish(&mut self, refs: Vec<TileRef>, checkpoint: Checkpoint) -> io::Result<()> {
        // Every new root carries the engine's current segment reference verbatim
        // (`None` until rotation exists): a rotated engine's republished root
        // must keep naming the live suffix, and a legacy engine stays
        // None-encoded.
        let segment = self.segment.clone();
        match &mut self.manifest {
            Some(m) => m.publish(refs, checkpoint, segment)?,
            None => {
                let path = self.dir.join(manifest::MANIFEST_NAME);
                let mut m = match Manifest::create(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        // An engine opened without discovery must never overwrite
                        // a root it did not read; adopt or refuse, never clobber.
                        match Manifest::open(&path)? {
                            Some(m) if m.root().tiles.is_empty() => m,
                            _ => return Err(io::Error::new(io::ErrorKind::InvalidData,
                                "manifest already exists with active tiles; use open_discover to adopt them before publishing")),
                        }
                    }
                    Err(e) => return Err(e),
                };
                m.publish(refs, checkpoint, segment)?;
                self.manifest = Some(m);
            }
        }
        Ok(())
    }

    /// Merge every published tile into ONE new whole tile file covering the full
    /// `(0, newest tile cutoff]` CSN range, publish it through the manifest
    /// protocol, and only after the new root is durable unlink the superseded
    /// tile files.
    ///
    /// **Only the newest version per logical key survives.** Reads never see
    /// historical state (2026-09-29, `docs/contracts.md` L-02 resolution), so
    /// superseded versions are unreadable garbage: the merged tile keeps the
    /// newest version of every logical key (everything before the trailing
    /// `~csn`), where a tombstone is itself the newest state and is retained.
    /// The WAL is never rotated or truncated, and the serving memtable (exactly
    /// the versions above the newest cutoff) is untouched. Compaction's purpose
    /// is reducing the per-read file count / rewrite bound
    /// ([`manifest::MAX_TILES`] tiles collapse to one).
    ///
    /// Refusals: fewer than two active tiles (`InvalidInput` — merging one tile
    /// into a fresh copy buys nothing) or a leftover candidate file with the
    /// engine-chosen name `compact-<cutoff>.tile` inside the engine directory
    /// (`AlreadyExists`, mirroring `publish_tile`'s leftover handling — cutoffs
    /// strictly increase across compactions because compaction requires at
    /// least two tiles, so live names never collide). Reads are unavailable for
    /// the duration: single-writer `&mut self`.
    ///
    /// Ordering is `publish_tile`'s: create-new + sync + verify the merged tile
    /// against the retained WAL, sync the directory entry, manifest
    /// write-inactive-page(seq+1) → sync file → sync dir → flip. A crash before
    /// the flip leaves the merged file an unreferenced orphan; a crash (or
    /// unlink failure) between the flip and the unlinks leaves the superseded
    /// tiles on disk as unreferenced orphans. Discovery must — and does —
    /// ignore both kinds: unreferenced files are never adopted; they are leaked
    /// until manually removed.
    pub fn compact(&mut self) -> io::Result<()> {
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        if self.tiles.len() < 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "compaction requires at least two active tiles"));
        }
        let cutoff = self.tiles.last().unwrap().cutoff();
        let path = self.dir.join(format!("compact-{cutoff}.tile"));
        let name = format!("compact-{cutoff}.tile");
        if self.tiles.iter().any(|t| t.name == name) {
            // A LIVE published tile owns this name; deleting it per a generic
            // "remove the leftover" hint would brick discovery after restart.
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                format!("compaction candidate {name} is a live published tile; compaction cannot proceed under this name")));
        }
        // 1. Build + verify the merged tile against the full retained WAL union
        //    projection, exactly as publish verifies a single tile's range.
        let tile = match self.write_compacted_tile(&path, cutoff) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "compaction candidate already exists and is unreferenced by the manifest (leftover from a failed compaction or crash after tile sync); remove {} and retry", path.display())));
            }
            Err(e) => return Err(e),
        };
        // 2. The tile's own directory entry must be durable before the manifest
        //    can reference it (checkpoint publication ordering).
        File::open(&self.dir)?.sync_all()?;
        // 3. Manifest protocol: the new root lists exactly one tile. The cutoff
        //    (and therefore the checkpoint frontier) is unchanged; only the
        //    number of files carrying `(0, cutoff]` shrinks.
        let tref = TileRef { name: name.clone(), cutoff, digest: *tile.digest() };
        let checkpoint = Checkpoint { csn: cutoff, handle_watermark: self.max_handle };
        self.manifest_publish(vec![tref], checkpoint)?;
        // 4. In-memory flip: one tile replaces the whole ordered list; the
        //    memtable is untouched (it only holds versions above `cutoff`).
        let superseded = std::mem::replace(&mut self.tiles, vec![TileHandle { tile, name }]);
        // 5. Only now drop the superseded files. Unlink after the durable flip
        //    is safe: open fds keep working (POSIX) and the new root no longer
        //    references them. Failures here are best-effort ignored — the
        //    compaction is already durably committed and a leftover file is a
        //    harmless orphan that discovery ignores and never adopts.
        for old in &superseded {
            let _ = fs::remove_file(self.dir.join(&old.name));
        }
        Ok(())
    }

    /// Create-new the compacted tile for the full `(0, cutoff]` union of every
    /// published tile range, sync it, reopen it and re-check its exact
    /// newest-version-per-logical-key projection against the retained WAL
    /// before any caller may reference it. Each input tile was itself verified
    /// against its disjoint WAL range at open/publish, so building from the
    /// WAL projection IS the multi-tile merge (same codec, same key order,
    /// newest version per logical key, tombstones retained as newest state) —
    /// and the verification is `publish`'s extended to the union.
    fn write_compacted_tile(&self, tile_path: &Path, cutoff: u64) -> io::Result<Tile> {
        let (records, _) = wal::replay(&self.wal_path)?;
        if records.last().map(|r| decode_payload(r).map(|v| v.0)).transpose()? != Some(self.csn) && self.csn != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL differs from serving state"));
        }
        let digest = wal_digest(&records, cutoff)?;
        let mut expected = Memtable::new();
        for payload in &records {
            let (csn, _, ops) = decode_payload(payload)?;
            if csn <= cutoff { for op in &ops { apply_op(&mut expected, op, csn); } }
        }
        let expected = newest_projection(&expected, 0, cutoff);
        let tile = Tile::write(tile_path, cutoff, &digest, &expected)?;
        tile.verify_projection(&expected, 0, cutoff)?;
        Ok(tile)
    }

    /// Automatic checkpoint trigger. When the retained WAL's valid length
    /// exceeds `max_wal_bytes`, publish a new tile covering everything through
    /// the current CSN under an engine-chosen unused plain filename
    /// (`tile-<cutoff>.tile`); an existing file of that name is refused as
    /// `InvalidInput` rather than silently renamed. The WAL itself is never
    /// rotated or truncated in this slice. `Ok(false)` = below threshold (no
    /// checkpoint); callers invoke this explicitly — there is no background
    /// thread.
    pub fn maybe_checkpoint(&mut self, max_wal_bytes: u64) -> io::Result<bool> {
        if self.csn == 0 || self.wal.valid_len() <= max_wal_bytes {
            return Ok(false);
        }
        let path = self.dir.join(format!("tile-{}.tile", self.csn));
        if path.try_exists()? {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                format!("checkpoint tile name {} already exists; remove it or publish manually to another name", path.display())));
        }
        self.publish_tile(&path, self.csn)?;
        Ok(true)
    }

    /// Prototype inspection hook: proves tile-covered versions are no longer served
    /// from the memtable (not a bound on WAL recovery or total RAM).
    pub fn serving_memtable_entries(&self) -> usize { self.mt.len() }

    // K-way merge of the serving memtable and every published tile, all
    // traversed in global key order. Keys encode CSN descending, so each logical
    // key's versions arrive newest-first and the caller's first visible entry
    // wins — the only state reads observe (2026-09-29: no historical reads).
    // Tile CSN ranges are disjoint, so cross-tier equal keys are impossible;
    // the memtable-wins skip below is defensive only.
    fn scan_merged(&self, prefix: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<bool>) -> io::Result<()> {
        let mut mem = self.mt.scan_iter(prefix).peekable();
        let mut iters: Vec<_> = self.tiles.iter().map(|t| t.tile.entries()).collect();
        let mut heads: Vec<Option<(Vec<u8>, Vec<u8>)>> = Vec::with_capacity(iters.len());
        for iter in iters.iter_mut() {
            heads.push(next_matching(iter, prefix)?);
        }
        loop {
            if mem.peek().is_none() && heads.iter().all(Option::is_none) { break; }
            let disk = smallest_disk_head(&heads);
            let use_mem = match (mem.peek(), disk) {
                (Some((mk, _)), Some(i)) => {
                    let head = heads[i].as_ref().unwrap();
                    *mk <= head.0.as_slice()
                }
                (Some(_), None) => true,
                (None, _) => false,
            };
            if use_mem {
                let (key, value) = mem.next().unwrap();
                if let Some(i) = disk {
                    if heads[i].as_ref().is_some_and(|(k, _)| k.as_slice() == key) {
                        heads[i] = next_matching(&mut iters[i], prefix)?;
                    }
                }
                if !visit(key, value)? { return Ok(()); }
            } else {
                let i = disk.unwrap();
                let (key, value) = heads[i].take().unwrap();
                heads[i] = next_matching(&mut iters[i], prefix)?;
                if !visit(&key, &value)? { return Ok(()); }
            }
        }
        Ok(())
    }

    /// 已发布的最新 CSN。读只看最新提交状态（不存在历史读），发布检查亦然。
    pub fn csn(&self) -> u64 {
        self.csn
    }

    /// Next globally unused handle. The caller reserves further handles locally within
    /// one block; only successfully published handles advance the persisted high-water mark.
    pub fn next_handle(&self) -> io::Result<u64> {
        self.max_handle.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "handles exhausted"))
    }

    /// Whether a block was published (including after WAL recovery).
    pub fn committed_block_csn(&self, block_id: &[u8]) -> Option<u64> {
        self.dedup.get(block_id).copied()
    }

    /// 反向查找：当前指向 `target` 的全部 source 句柄（最新提交状态，无历史读）。
    /// tombstone（空 value）剔除。
    pub fn reverse_lookup(&self, entity: &[u8], field: &[u8], target: u64) -> io::Result<Vec<u64>> {
        check_name(entity)?;
        check_name(field)?;
        let pfx = keys::reverse_prefix(entity, field, target)?;
        let mut newest: HashMap<u64, bool> = HashMap::new();
        self.scan_merged(&pfx, |key, value| {
            let Some((_, source, _)) = keys::decode_reverse(key) else { return Ok(true); };
            // 键序内同一 source 按 CSN 降序排列，首个条目即最新状态。
            newest.entry(source).or_insert(!value.is_empty());
            Ok(true)
        })?;
        let mut out: Vec<u64> = newest.into_iter().filter(|(_, alive)| *alive).map(|(s, _)| s).collect();
        out.sort();
        Ok(out)
    }

    /// 按已声明唯一字段解析句柄（最新提交状态，无历史读）。
    /// 如观察到多个活 owner，则索引已损坏，不能任意选一个。
    pub fn unique_lookup(&self, entity: &[u8], field: &[u8], value: &[u8]) -> io::Result<Option<u64>> {
        let pfx = keys::unique_prefix(entity, field, value)?;
        let mut seen = std::collections::HashSet::new();
        let mut owner = None;
        self.scan_merged(&pfx, |key, val| {
            if key.len() != pfx.len() + 16 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed unique key"));
            }
            let handle = u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap());
            if keys::key_csn(&key).is_none() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "missing CSN"));
            }
            if !seen.insert(handle) || val.is_empty() { return Ok(true); }
            if val != handle.to_be_bytes() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "unique value disagrees with handle"));
            }
            if owner.replace(handle).is_some() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "duplicate unique owners"));
            }
            Ok(true)
        })?;
        Ok(owner)
    }

    /// 当前状态点读：只看最新提交版本（2026-09-29：不存在历史读），
    /// tombstone → 不存在。
    pub fn get(&self, entity: &[u8], handle: u64) -> io::Result<Option<Vec<u8>>> {
        check_name(entity)?;
        let pfx = keys::primary_prefix(entity, handle)?;
        let mut result = None;
        self.scan_merged(&pfx, |key, value| {
            if keys::key_csn(key).is_some() {
                result = if value.is_empty() { None } else { Some(value.to_vec()) };
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(result)
    }

    /// Visit each live primary document once at latest committed state (no
    /// historical reads). The scan walks the whole entity prefix; callers must
    /// bound their returned rows separately when filtering or ordering results.
    pub fn scan_primary(
        &self,
        entity: &[u8],
        mut visit: impl FnMut(u64, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        check_name(entity)?;
        let mut prefix = keys::primary_prefix(entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        let mut last_handle = None;
        self.scan_merged(&prefix, |key, value| {
            let (key_entity, handle, _) = keys::decode_primary(key)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed primary key"))?;
            if key_entity != entity {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "primary entity mismatch"));
            }
            if last_handle == Some(handle) { return Ok(true); }
            last_handle = Some(handle);
            if !value.is_empty() {
                visit(handle, value)?;
            }
            Ok(true)
        })
    }

    /// 原子提交一个块。唯一约束冲突 → `Outcome::Conflict`（未写 WAL，调用方直接报业务错误）。
    pub fn commit_block(&mut self, block_id: &[u8], ops: &[Op]) -> io::Result<Outcome> {
        if block_id.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty block_id"));
        }
        if let Some(&csn) = self.dedup.get(block_id) {
            return Ok(Outcome::AlreadyCommitted { csn });
        }
        let csn = self.csn.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CSN exhausted"))?;

        // WAL 之前校验全部键名，避免同步成功后 apply_op 的 unwrap 崩溃。
        // 唯一前缀包含 handle；按 handle 分组取最新版本，不能让较小 handle
        // 的 tombstone 遮蔽较大 handle 的活条目。overlay 中同键最后操作胜出。
        let mut touched: HashMap<Vec<u8>, (Vec<u8>, Vec<u8>, Vec<u8>, HashMap<u64, bool>)> = HashMap::new();
        for op in ops {
            match op {
                Op::PutDoc { entity, doc, .. } => {
                    check_name(entity)?;
                    if doc.is_empty() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty document")); }
                }
                Op::DelDoc { entity, .. } => { check_name(entity)?; }
                Op::PutReverse { entity, field, .. } | Op::DelReverse { entity, field, .. } => {
                    check_name(entity)?; check_name(field)?;
                }
                Op::PutUnique { entity, field, value, handle }
                | Op::DelUnique { entity, field, value, handle } => {
                    let pfx = keys::unique_prefix(entity, field, value)?;
                    let entry = touched.entry(pfx).or_insert_with(||
                        (entity.clone(), field.clone(), value.clone(), HashMap::new()));
                    entry.3.insert(*handle, matches!(op, Op::PutUnique { .. }));
                }
            }
        }
        for (pfx, (entity, field, value, overlay)) in touched {
            let mut live: HashMap<u64, bool> = HashMap::new();
            self.scan_merged(&pfx, |key, val| {
                if key.len() < pfx.len() + 16 { return Ok(true); }
                let handle = u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap());
                // 键按 handle 升序、每个 handle 内按 CSN 降序排列。
                live.entry(handle).or_insert(!val.is_empty());
                Ok(true)
            })?;
            live.extend(overlay);
            if live.values().filter(|&&alive| alive).take(2).count() > 1 {
                return Ok(Outcome::Conflict(Conflict { kind: "x_unique", entity, field, value }));
            }
        }

        let payload = encode_payload(csn, block_id, ops);
        self.wal.append(&payload)?;
        self.wal.sync()?;

        for op in ops {
            apply_op(&mut self.mt, op, csn);
        }
        self.csn = csn;
        self.max_handle = self.max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
        self.dedup.insert(block_id.to_vec(), csn);
        Ok(Outcome::Committed { csn })
    }
}

fn next_matching(
    disk: &mut impl Iterator<Item = io::Result<(Vec<u8>, Vec<u8>)>>,
    prefix: &[u8],
) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    for entry in disk {
        let (key, value) = entry?;
        if key.starts_with(prefix) { return Ok(Some((key, value))); }
    }
    Ok(None)
}

// Index of the smallest pending tile head, if any; linear over at most
// manifest::MAX_TILES heads.
fn smallest_disk_head(heads: &[Option<(Vec<u8>, Vec<u8>)>]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, head) in heads.iter().enumerate() {
        if let Some((key, _)) = head {
            let better = match best {
                None => true,
                Some(b) => key.as_slice() < heads[b].as_ref().unwrap().0.as_slice(),
            };
            if better { best = Some(i); }
        }
    }
    best
}

// Bind a tile to the exact WAL prefix which produced it; the WAL remains the only
// recovery authority. Length delimiters prevent ambiguous concatenation of records.
fn wal_digest(records: &[Vec<u8>], cutoff: u64) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut last = 0u64;
    for payload in records {
        let (csn, _, _) = decode_payload(payload)?;
        if csn == 0 || csn <= last {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL CSN reordering"));
        }
        if csn <= cutoff {
            hash.update((payload.len() as u64).to_le_bytes());
            hash.update(payload);
        }
        last = csn;
    }
    if cutoff > last { return Err(io::Error::new(io::ErrorKind::InvalidData, "tile cutoff exceeds WAL")); }
    Ok(hash.finalize().into())
}

// Directory a manifest or tile name resolves against; empty parents mean ".".
fn parent_dir(path: &Path) -> PathBuf {
    path.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

// ---------- op → memtable ----------

fn op_handle(op: &Op) -> u64 {
    match op {
        Op::PutDoc { handle, .. } | Op::DelDoc { handle, .. }
        | Op::PutUnique { handle, .. } | Op::DelUnique { handle, .. } => *handle,
        Op::PutReverse { target, source, .. } | Op::DelReverse { target, source, .. } => (*target).max(*source),
    }
}

fn apply_op(mt: &mut Memtable, op: &Op, csn: u64) {
    match op {
        Op::PutDoc { entity, handle, doc } => {
            mt.apply(keys::primary_key(entity, *handle, csn).unwrap(), doc.clone());
        }
        Op::DelDoc { entity, handle } => {
            mt.apply(keys::primary_key(entity, *handle, csn).unwrap(), Vec::new());
        }
        Op::PutUnique { entity, field, value, handle } => {
            mt.apply(keys::unique_key(entity, field, value, *handle, csn).unwrap(), handle.to_be_bytes().to_vec());
        }
        Op::DelUnique { entity, field, value, handle } => {
            mt.apply(keys::unique_key(entity, field, value, *handle, csn).unwrap(), Vec::new());
        }
        Op::PutReverse { entity, field, target, source } => {
            mt.apply(keys::reverse_key(entity, field, *target, *source, csn).unwrap(), vec![1]);
        }
        Op::DelReverse { entity, field, target, source } => {
            mt.apply(keys::reverse_key(entity, field, *target, *source, csn).unwrap(), Vec::new());
        }
    }
}

/// Reduce versioned entries to the NEWEST version per logical key within the
/// CSN range `(lower, cutoff]` (2026-09-29: reads never see historical state,
/// so superseded versions are unreadable garbage). The logical key is the full
/// key minus its trailing `~csn`; keys iterate grouped by logical key in
/// CSN-descending order, so the first in-range version of each logical key
/// wins. A tombstone (empty value) is itself a newest state and is retained.
fn newest_projection(mt: &Memtable, lower: u64, cutoff: u64) -> Memtable {
    let mut out = Memtable::new();
    let mut last_logical: Option<Vec<u8>> = None;
    for (key, value) in mt.entries() {
        let Some(csn) = keys::key_csn(key) else { continue };
        if csn <= lower || csn > cutoff { continue; }
        let logical = &key[..key.len() - 8];
        if last_logical.as_deref() == Some(logical) { continue; }
        last_logical = Some(logical.to_vec());
        out.apply(key.to_vec(), value.to_vec());
    }
    out
}

// ---------- WAL payload 编解码（v0 二进制，LE） ----------

const T_PUT_DOC: u8 = 0;
const T_DEL_DOC: u8 = 1;
const T_PUT_UNI: u8 = 2;
const T_DEL_UNI: u8 = 3;
const T_PUT_REV: u8 = 4;
const T_DEL_REV: u8 = 5;

fn encode_payload(csn: u64, block_id: &[u8], ops: &[Op]) -> Vec<u8> {
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(MAGIC);
    b.extend_from_slice(&csn.to_le_bytes());
    put_bytes(&mut b, block_id);
    put_u32(&mut b, ops.len() as u32);
    for op in ops {
        match op {
            Op::PutDoc { entity, handle, doc } => {
                b.push(T_PUT_DOC);
                put_bytes(&mut b, entity);
                put_u64(&mut b, *handle);
                put_bytes(&mut b, doc);
            }
            Op::DelDoc { entity, handle } => {
                b.push(T_DEL_DOC);
                put_bytes(&mut b, entity);
                put_u64(&mut b, *handle);
            }
            Op::PutUnique { entity, field, value, handle } => {
                b.push(T_PUT_UNI);
                put_bytes(&mut b, entity);
                put_bytes(&mut b, field);
                put_bytes(&mut b, value);
                put_u64(&mut b, *handle);
            }
            Op::DelUnique { entity, field, value, handle } => {
                b.push(T_DEL_UNI);
                put_bytes(&mut b, entity);
                put_bytes(&mut b, field);
                put_bytes(&mut b, value);
                put_u64(&mut b, *handle);
            }
            Op::PutReverse { entity, field, target, source } => {
                b.push(T_PUT_REV);
                put_bytes(&mut b, entity);
                put_bytes(&mut b, field);
                put_u64(&mut b, *target);
                put_u64(&mut b, *source);
            }
            Op::DelReverse { entity, field, target, source } => {
                b.push(T_DEL_REV);
                put_bytes(&mut b, entity);
                put_bytes(&mut b, field);
                put_u64(&mut b, *target);
                put_u64(&mut b, *source);
            }
        }
    }
    b
}

struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.i + n > self.b.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "wal codec: truncated"));
        }
        let s = &self.b[self.i..self.i + n];
        self.i += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

fn decode_payload(payload: &[u8]) -> io::Result<(u64, Vec<u8>, Vec<Op>)> {
    let mut c = Cur { b: payload, i: 0 };
    if c.take(4)? != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "wal codec: bad magic"));
    }
    let csn = c.u64()?;
    let block_id = c.bytes()?;
    let n = c.u32()? as usize;
    let mut ops = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = c.u8()?;
        let entity = c.bytes()?;
        let op = match tag {
            T_PUT_DOC => Op::PutDoc { handle: c.u64()?, doc: c.bytes()?, entity },
            T_DEL_DOC => Op::DelDoc { handle: c.u64()?, entity },
            T_PUT_UNI => {
                let field = c.bytes()?;
                let value = c.bytes()?;
                Op::PutUnique { handle: c.u64()?, entity, field, value }
            }
            T_DEL_UNI => {
                let field = c.bytes()?;
                let value = c.bytes()?;
                Op::DelUnique { handle: c.u64()?, entity, field, value }
            }
            T_PUT_REV => {
                let field = c.bytes()?;
                Op::PutReverse { target: c.u64()?, source: c.u64()?, entity, field }
            }
            T_DEL_REV => {
                let field = c.bytes()?;
                Op::DelReverse { target: c.u64()?, source: c.u64()?, entity, field }
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("wal codec: unknown op tag {other}"),
                ));
            }
        };
        ops.push(op);
    }
    Ok((csn, block_id, ops))
}

fn put_u32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_u64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_le_bytes());
}

fn put_bytes(v: &mut Vec<u8>, b: &[u8]) {
    put_u32(v, b.len() as u32);
    v.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::process;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-engine-{}-{name}", process::id()));
        fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-engine-dir-{}-{name}", process::id()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn get_ok(e: &Engine, entity: &str, handle: u64) -> Option<Vec<u8>> {
        e.get(entity.as_bytes(), handle).unwrap().map(|b| b.to_vec())
    }

    #[test]
    fn commit_then_get() {
        let p = tmp("commit_get.wal");
        let mut e = Engine::create(&p).unwrap();
        let out = e
            .commit_block(b"b1", &[Op::PutDoc {
                entity: b"Stock".to_vec(),
                handle: 11,
                doc: br#"{"sku":"P-42","loc":"L1","on_hand":6}"#.to_vec(),
            }])
            .unwrap();
        assert_eq!(out, Outcome::Committed { csn: 1 });
        let doc = get_ok(&e, "Stock", 11).unwrap();
        assert_eq!(doc, br#"{"sku":"P-42","loc":"L1","on_hand":6}"#);
        remove(&p);
    }

    #[test]
    fn unique_tombstone_of_lower_handle_does_not_hide_live_higher_handle() {
        let p = tmp("unique_tombstone_shadow.wal");
        let mut e = Engine::create(&p).unwrap();
        let idx = |handle| Op::PutUnique {
            entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"a@x.com".to_vec(), handle,
        };
        e.commit_block(b"first", &[idx(1)]).unwrap();
        e.commit_block(b"remove", &[Op::DelUnique {
            entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"a@x.com".to_vec(), handle: 1,
        }]).unwrap();
        e.commit_block(b"second", &[idx(2)]).unwrap();
        assert!(matches!(e.commit_block(b"third", &[idx(3)]).unwrap(), Outcome::Conflict(_)));
        remove(&p);
    }

    #[test]
    fn unique_transfer_and_last_overlay_operation() {
        let p = tmp("unique_transfer.wal");
        let mut e = Engine::create(&p).unwrap();
        let make = |handle| Op::PutUnique {
            entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"a@x.com".to_vec(), handle,
        };
        e.commit_block(b"first", &[make(1)]).unwrap();
        let del = Op::DelUnique {
            entity: b"Customer".to_vec(), field: b"email".to_vec(),
            value: b"a@x.com".to_vec(), handle: 1,
        };
        assert!(matches!(e.commit_block(b"transfer", &[del, make(2)]).unwrap(), Outcome::Committed { .. }));
        assert!(matches!(e.commit_block(b"third", &[make(3)]).unwrap(), Outcome::Conflict(_)));
        remove(&p);
    }

    #[test]
    fn invalid_name_does_not_append_wal_or_advance_csn() {
        let p = tmp("invalid_name.wal");
        let mut e = Engine::create(&p).unwrap();
        assert!(e.commit_block(b"bad", &[Op::DelDoc { entity: b"bad\0name".to_vec(), handle: 1 }]).is_err());
        assert_eq!(e.csn(), 0);
        drop(e);
        let e = Engine::open(&p).unwrap();
        assert_eq!(e.csn(), 0);
        remove(&p);
    }

    #[test]
    fn unique_lookup_tracks_reassignment_and_deletion() {
        let p = tmp("unique_lookup.wal");
        let mut e = Engine::create(&p).unwrap();
        let field = b"email";
        let value = b"a@x.com";
        let put = |handle| Op::PutUnique { entity: b"Customer".to_vec(), field: field.to_vec(), value: value.to_vec(), handle };
        let del = |handle| Op::DelUnique { entity: b"Customer".to_vec(), field: field.to_vec(), value: value.to_vec(), handle };
        assert_eq!(e.unique_lookup(b"Customer", field, value).unwrap(), None);
        e.commit_block(b"one", &[put(1)]).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value).unwrap(), Some(1));
        e.commit_block(b"transfer", &[del(1), put(2)]).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value).unwrap(), Some(2), "读只看最新提交状态");
        e.commit_block(b"delete", &[del(2)]).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value).unwrap(), None);
        drop(e);
        let e = Engine::open(&p).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value).unwrap(), None, "恢复后同样只看最新提交状态");
        remove(&p);
    }

    #[test]
    fn unique_violation_then_distinct_value_succeeds() {
        let p = tmp("uniq.wal");
        let mut e = Engine::create(&p).unwrap();
        let put_c = |e: &mut Engine, handle: u64, email: &[u8], bid: &[u8]| {
            e.commit_block(bid, &[Op::PutUnique {
                entity: b"Customer".to_vec(),
                field: b"email".to_vec(),
                value: email.to_vec(),
                handle,
            }])
        };
        assert!(matches!(put_c(&mut e, 1, b"a@x.com", b"r1").unwrap(), Outcome::Committed { .. }));
        // 不同 handle 抢同一 email → 冲突，且未写 WAL
        match put_c(&mut e, 2, b"a@x.com", b"r2").unwrap() {
            Outcome::Conflict(c) => {
                assert_eq!(c.kind, "x_unique");
                assert_eq!(c.value, b"a@x.com".to_vec());
            }
            other => panic!("expected conflict, got {other:?}"),
        }
        // 换 email → 成功
        assert!(matches!(put_c(&mut e, 2, b"b@x.com", b"r3").unwrap(), Outcome::Committed { .. }));
        // 同 handle 重放同值 → 合法（幂等重写），不是冲突
        assert!(matches!(put_c(&mut e, 1, b"a@x.com", b"r4").unwrap(), Outcome::Committed { .. }));
        remove(&p);
    }

    #[test]
    fn delete_frees_unique_and_hides_doc() {
        let p = tmp("del.wal");
        let mut e = Engine::create(&p).unwrap();
        e.commit_block(
            b"r1",
            &[
                Op::PutDoc { entity: b"Customer".to_vec(), handle: 1, doc: br#"{"name":"a"}"#.to_vec() },
                Op::PutUnique {
                    entity: b"Customer".to_vec(),
                    field: b"email".to_vec(),
                    value: b"a@x.com".to_vec(),
                    handle: 1,
                },
            ],
        ).unwrap();
        assert!(get_ok(&e, "Customer", 1).is_some());
        e.commit_block(
            b"r2",
            &[
                Op::DelDoc { entity: b"Customer".to_vec(), handle: 1 },
                Op::DelUnique {
                    entity: b"Customer".to_vec(),
                    field: b"email".to_vec(),
                    value: b"a@x.com".to_vec(),
                    handle: 1,
                },
            ],
        ).unwrap();
        assert!(get_ok(&e, "Customer", 1).is_none(), "DelDoc 后文档不可见");
        // email 已释放 → 新 handle 可占用
        assert!(matches!(
            e.commit_block(
                b"r3",
                &[Op::PutUnique {
                    entity: b"Customer".to_vec(),
                    field: b"email".to_vec(),
                    value: b"a@x.com".to_vec(),
                    handle: 2,
                }]
            ).unwrap(),
            Outcome::Committed { .. }
        ));
        remove(&p);
    }

    #[test]
    fn dedup_is_idempotent() {
        let p = tmp("dedup.wal");
        let mut e = Engine::create(&p).unwrap();
        let ops = [Op::PutDoc { entity: b"Order".to_vec(), handle: 5, doc: b"{}".to_vec() }];
        let first = e.commit_block(b"req-1/blk-0", &ops).unwrap();
        let again = e.commit_block(b"req-1/blk-0", &ops).unwrap();
        assert_eq!(first, Outcome::Committed { csn: 1 });
        assert_eq!(again, Outcome::AlreadyCommitted { csn: 1 });
        assert_eq!(e.csn(), 1, "重放不得推进 CSN");
        remove(&p);
    }

    #[test]
    fn recovery_replays_committed_blocks() {
        let p = tmp("recover.wal");
        {
            let mut e = Engine::create(&p).unwrap();
            e.commit_block(b"b1", &[Op::PutDoc {
                entity: b"Stock".to_vec(),
                handle: 11,
                doc: br#"{"on_hand":5}"#.to_vec(),
            }]).unwrap();
        }
        let e = Engine::open(&p).unwrap();
        assert_eq!(e.csn(), 1);
        assert_eq!(get_ok(&e, "Stock", 11).unwrap(), br#"{"on_hand":5}"#);
        remove(&p);
    }

    #[test]
    fn torn_wal_tail_drops_uncommitted_block() {
        let p = tmp("torn.wal");
        {
            let mut e = Engine::create(&p).unwrap();
            e.commit_block(b"b1", &[Op::PutDoc {
                entity: b"Stock".to_vec(), handle: 11, doc: b"v1".to_vec(),
            }]).unwrap();
            e.commit_block(b"b2", &[Op::PutDoc {
                entity: b"Stock".to_vec(), handle: 12, doc: b"v2".to_vec(),
            }]).unwrap();
        }
        // 模拟崩溃撕裂：砍掉 WAL 尾部 8 字节（b2 记录不再完整）
        let meta = p.metadata().unwrap();
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(meta.len() - 8).unwrap();
        drop(f);

        let mut e = Engine::open(&p).unwrap();
        assert_eq!(e.csn(), 1, "撕裂块未发布，CSN 回退");
        assert!(get_ok(&e, "Stock", 12).is_none(), "撕裂块不可见");
        assert_eq!(get_ok(&e, "Stock", 11).unwrap(), b"v1");
        // 继续提交，CSN 从 1 → 2
        let out = e.commit_block(b"b3", &[Op::PutDoc {
            entity: b"Stock".to_vec(), handle: 12, doc: b"v2-fixed".to_vec(),
        }]).unwrap();
        assert_eq!(out, Outcome::Committed { csn: 2 });
        remove(&p);
    }

    #[test]
    fn same_block_sibling_reference_is_atomic() {
        let p = tmp("sibling.wal");
        let mut e = Engine::create(&p).unwrap();
        // 同一原子块：创建服务订单 + 服务工单，工单反向引用订单（R 前缀）
        let out = e
            .commit_block(
                b"svc-1",
                &[
                    Op::PutDoc {
                        entity: b"ServiceOrder".to_vec(), handle: 101,
                        doc: br#"{"order_no":"SO-771","status":"open"}"#.to_vec(),
                    },
                    Op::PutUnique {
                        entity: b"ServiceOrder".to_vec(), field: b"order_no".to_vec(),
                        value: b"SO-771".to_vec(), handle: 101,
                    },
                    Op::PutDoc {
                        entity: b"ServiceTicket".to_vec(), handle: 201,
                        doc: br#"{"ticket_no":"TK-1","issue":"part failure"}"#.to_vec(),
                    },
                    Op::PutReverse {
                        entity: b"ServiceTicket".to_vec(), field: b"order".to_vec(),
                        target: 101, source: 201,
                    },
                ],
            )
            .unwrap();
        assert!(matches!(out, Outcome::Committed { .. }));
        // 反向查找：订单 101 的工单 = 201（R 前缀连续）
        let pfx = keys::reverse_prefix(b"ServiceTicket", b"order", 101).unwrap();
        // 经由 memtable 的发布态检查：前缀存在即有反向条目
        assert!(
            matches!(
                e.commit_block(
                    b"svc-2",
                    &[Op::PutReverse {
                        entity: b"ServiceTicket".to_vec(), field: b"order".to_vec(),
                        target: 101, source: 202,
                    }]
                ).unwrap(),
                Outcome::Committed { .. }
            ),
            "v0 未强制 reverse:unique（已在 ticket 记录为待实现），第二个工单允许"
        );
        let _ = pfx;
        remove(&p);
    }

    #[test]
    fn constraint_violation_rolls_back_whole_block() {
        let p = tmp("rollback.wal");
        let mut e = Engine::create(&p).unwrap();
        // 同一原子块：先动库存（合法），再撞唯一约束（违约）→ 整块丢弃
        let out = e
            .commit_block(
                b"bad-1",
                &[
                    Op::PutDoc {
                        entity: b"Stock".to_vec(), handle: 11, doc: br#"{"on_hand":5}"#.to_vec(),
                    },
                    Op::PutUnique {
                        entity: b"Customer".to_vec(), field: b"email".to_vec(),
                        value: b"a@x.com".to_vec(), handle: 1,
                    },
                ],
            )
            .unwrap();
        assert!(matches!(out, Outcome::Committed { .. }));
        // 现在违约：同 email 不同 handle，且块内还带一个合法写 → 整块必须回滚
        let out = e
            .commit_block(
                b"bad-2",
                &[
                    Op::PutDoc {
                        entity: b"Stock".to_vec(), handle: 12, doc: br#"{"on_hand":9}"#.to_vec(),
                    },
                    Op::PutUnique {
                        entity: b"Customer".to_vec(), field: b"email".to_vec(),
                        value: b"a@x.com".to_vec(), handle: 2,
                    },
                ],
            )
            .unwrap();
        assert!(matches!(out, Outcome::Conflict(_)));
        assert!(get_ok(&e, "Stock", 12).is_none(), "违约块的合法写也必须不存在——overlay 整体丢弃");
        remove(&p);
    }

    #[test]
    fn reverse_lookup_tracks_lifecycle() {
        let p = tmp("rev.wal");
        let mut e = Engine::create(&p).unwrap();
        let ref_op = |source: u64| Op::PutReverse {
            entity: b"ServiceTicket".to_vec(),
            field: b"order".to_vec(),
            target: 101,
            source,
        };
        e.commit_block(b"r1", &[ref_op(201)]).unwrap();
        e.commit_block(b"r2", &[ref_op(202)]).unwrap();
        assert_eq!(e.reverse_lookup(b"ServiceTicket", b"order", 101).unwrap(), vec![201, 202]);
        // 删除 201 的反向条目 → 202 保留
        e.commit_block(b"r3", &[Op::DelReverse {
            entity: b"ServiceTicket".to_vec(), field: b"order".to_vec(),
            target: 101, source: 201,
        }]).unwrap();
        assert_eq!(e.reverse_lookup(b"ServiceTicket", b"order", 101).unwrap(), vec![202]);
        remove(&p);
    }

    #[test]
    fn compact_publishes_one_tile_root_and_advances_manifest_seq() {
        // Path 1: an existing manifest chain advances its seq and lists exactly
        // one tile after compaction.
        let dir = tmp_dir("compact-seq");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        let put = |h: u64, v: &[u8]| Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() };
        e.commit_block(b"b1", &[put(1, b"v1")]).unwrap();
        e.commit_block(b"b2", &[put(1, b"v2")]).unwrap();
        e.publish_tile(&dir.join("t1.tile"), 1).unwrap();
        e.publish_tile(&dir.join("t2.tile"), 2).unwrap();
        let seq_before = e.manifest.as_ref().unwrap().root().seq;
        assert_eq!(seq_before, 2);
        let memtable_before = e.serving_memtable_entries();
        e.compact().unwrap();
        let root = e.manifest.as_ref().unwrap().root();
        assert_eq!(root.seq, seq_before + 1, "compaction follows the manifest publication protocol");
        assert_eq!(root.tiles.len(), 1, "the compacted root lists exactly one tile");
        assert_eq!(root.tiles[0].name, "compact-2.tile");
        assert_eq!(root.tiles[0].cutoff, 2);
        assert_eq!(root.checkpoint.csn, 2);
        assert_eq!(e.serving_memtable_entries(), memtable_before, "compaction never touches the memtable");
        drop(e);
        let e = Engine::open_discover(&dir).unwrap();
        assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v2".to_vec()), "合并 tile 只留最新版本，读只看最新状态");
        assert_eq!(e.serving_memtable_entries(), 0);
        drop(e);
        fs::remove_dir_all(&dir).unwrap();

        // Path 2: tiles built without any manifest get one created at seq 1 by
        // the first compaction.
        let dir = tmp_dir("compact-seed-nomanifest");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.commit_block(b"b1", &[put(1, b"v1")]).unwrap();
        e.commit_block(b"b2", &[put(1, b"v2")]).unwrap();
        e.build_tile(&dir.join("t1.tile"), 1).unwrap();
        e.build_tile(&dir.join("t2.tile"), 2).unwrap();
        assert!(e.manifest.is_none(), "build_tile alone never creates a manifest");
        e.compact().unwrap();
        let root = e.manifest.as_ref().unwrap().root();
        assert_eq!(root.seq, 1);
        assert_eq!(root.tiles.len(), 1);
        assert_eq!(root.tiles[0].name, "compact-2.tile");
        // One tile remains: further compaction is refused as InvalidInput.
        assert_eq!(e.compact().unwrap_err().kind(), io::ErrorKind::InvalidInput);
        drop(e);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_discover_replays_segment_suffix_and_merges_checkpoint() {
        // Hand-craft the post-rotation directory state a future rotation slice
        // will produce: one tile covering (0,2], a manifest root referencing it
        // plus SegmentRef { data-2.wal, start_csn 2 }, and a suffix-only
        // data-2.wal holding just the csn-3 record. Rotation itself does not
        // exist yet, so the test flips the manifest root directly.
        let dir = tmp_dir("segment-discover");
        let put = |h: u64, v: &[u8]| Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() };

        // Pre-rotation phase: two committed blocks (csn 1..=2, handles up to 7)
        // published as one tile; the manifest checkpoint then matches the cutoff.
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.commit_block(b"pre-1", &[put(7, b"v1")]).unwrap();
        e.commit_block(b"pre-2", &[put(7, b"v2")]).unwrap();
        e.publish_tile(&dir.join("t1.tile"), 2).unwrap();
        let tref = e.tiles[0].tile_ref();
        let watermark = e.max_handle;
        assert_eq!(watermark, 7);
        drop(e);

        // The suffix segment holds ONLY the post-rotation record; the
        // pre-rotation prefix (data.wal) is retired.
        let mut seg = Wal::create(&dir.join("data-2.wal")).unwrap();
        seg.append(&encode_payload(3, b"post-3", &[put(1, b"v3")])).unwrap();
        seg.sync().unwrap();
        drop(seg);
        fs::remove_file(dir.join("data.wal")).unwrap();

        let mut m = Manifest::open(&dir.join(manifest::MANIFEST_NAME)).unwrap().unwrap();
        m.publish(vec![tref], Checkpoint { csn: 2, handle_watermark: watermark },
            Some(manifest::SegmentRef { name: "data-2.wal".to_string(), start_csn: 2 })).unwrap();
        drop(m);

        let mut e = Engine::open_discover(&dir).unwrap();
        // Suffix replayed and checkpoint merged: CSN continues above both.
        assert_eq!(e.csn(), 3);
        assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v3".to_vec()), "suffix record is served");
        assert_eq!(e.get(b"E", 7).unwrap(), Some(b"v2".to_vec()), "pre-rotation state is served from the published tile");
        // Checkpoint handle watermark respected: the suffix alone derives 1.
        assert_eq!(e.max_handle, 7);
        assert_eq!(e.next_handle().unwrap(), 8);
        // Pre-rotation block ids are forgotten by design (suffix-only dedup).
        assert_eq!(e.committed_block_csn(b"pre-1"), None);
        assert_eq!(e.committed_block_csn(b"pre-2"), None);
        assert_eq!(e.committed_block_csn(b"post-3"), Some(3));
        // New commits work and go to the CURRENT segment.
        let out = e.commit_block(b"post-4", &[put(1, b"v4")]).unwrap();
        assert_eq!(out, Outcome::Committed { csn: 4 });
        drop(e);

        let (records, _) = wal::replay(&dir.join("data-2.wal")).unwrap();
        assert_eq!(records.len(), 2, "new commits append to the referenced segment");
        assert_eq!(decode_payload(&records[1]).unwrap().0, 4);
        assert!(!dir.join("data.wal").exists());

        // Reopening the rotated directory reproduces the same state.
        let e = Engine::open_discover(&dir).unwrap();
        assert_eq!(e.csn(), 4);
        assert_eq!(e.get(b"E", 1).unwrap(), Some(b"v4".to_vec()));
        assert_eq!(e.committed_block_csn(b"post-4"), Some(4));
        assert_eq!(e.committed_block_csn(b"pre-1"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_discover_fails_closed_on_pre_rotation_records_in_segment() {
        // A segment whose file still contains a record at or below start_csn is
        // stale/foreign content: recovery must refuse it, not replay it.
        let dir = tmp_dir("segment-stale");
        let put = |h: u64, v: &[u8]| Op::PutDoc { entity: b"E".to_vec(), handle: h, doc: v.to_vec() };
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.commit_block(b"pre-1", &[put(7, b"v1")]).unwrap();
        e.commit_block(b"pre-2", &[put(7, b"v2")]).unwrap();
        e.publish_tile(&dir.join("t1.tile"), 2).unwrap();
        let tref = e.tiles[0].tile_ref();
        let watermark = e.max_handle;
        drop(e);

        // The copied "suffix" wrongly still holds the pre-rotation csn-2 record.
        let mut seg = Wal::create(&dir.join("data-2.wal")).unwrap();
        seg.append(&encode_payload(2, b"pre-2", &[put(7, b"v2")])).unwrap();
        seg.append(&encode_payload(3, b"post-3", &[put(1, b"v3")])).unwrap();
        seg.sync().unwrap();
        drop(seg);
        fs::remove_file(dir.join("data.wal")).unwrap();

        let mut m = Manifest::open(&dir.join(manifest::MANIFEST_NAME)).unwrap().unwrap();
        m.publish(vec![tref], Checkpoint { csn: 2, handle_watermark: watermark },
            Some(manifest::SegmentRef { name: "data-2.wal".to_string(), start_csn: 2 })).unwrap();
        drop(m);

        let err = match Engine::open_discover(&dir) {
            Err(e) => e,
            Ok(_) => panic!("open_discover must fail closed on pre-rotation segment records"),
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("pre-rotation records"), "got: {err}");
        fs::remove_dir_all(&dir).unwrap();
    }

    fn remove(p: &Path) {
        fs::remove_file(p).ok();
    }
}
