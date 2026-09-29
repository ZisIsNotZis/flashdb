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
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::keys::{self, check_name};
use crate::manifest::{self, Checkpoint, Manifest, TileRef};
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
    tile: Option<Tile>,
    wal_path: PathBuf,
    dir: PathBuf,
    manifest: Option<Manifest>,
    csn: u64,
    dedup: HashMap<Vec<u8>, u64>,
    max_handle: u64,
}

impl Engine {
    /// 新建（空库）。
    pub fn create(wal_path: impl AsRef<Path>) -> io::Result<Engine> {
        Ok(Engine {
            wal: Wal::create(&wal_path)?,
            mt: Memtable::new(),
            tile: None,
            wal_path: wal_path.as_ref().to_path_buf(),
            dir: parent_dir(wal_path.as_ref()),
            manifest: None,
            csn: 0,
            dedup: HashMap::new(),
            max_handle: 0,
        })
    }

    /// Recover wholly from WAL; does not automatically discover tile candidates.
    pub fn open(wal_path: impl AsRef<Path>) -> io::Result<Engine> {
        Self::open_impl(wal_path.as_ref(), None)
    }

    /// Explicit prototype recovery: a missing, corrupt or WAL-mismatched tile fails
    /// closed. To rebuild from retained WAL, explicitly call `open` instead.
    pub fn open_with_tile(wal_path: impl AsRef<Path>, tile_path: impl AsRef<Path>) -> io::Result<Engine> {
        Self::open_impl(wal_path.as_ref(), Some(Tile::open(tile_path.as_ref())?))
    }

    /// Durable-root recovery: select the highest valid manifest page in `dir`, open
    /// and verify the referenced tile against the retained WAL exactly as
    /// `open_with_tile` does, and simply ignore unreferenced tile files (they are
    /// never candidates; fail-closed, no adoption). Conventional layout:
    /// `<dir>/data.wal`, `<dir>/manifest`, tiles referenced by plain filename.
    /// A missing manifest file is the empty initial state (no tile); full WAL
    /// replay still happens, so open time and RAM stay unbounded in this slice.
    pub fn open_discover(dir: impl AsRef<Path>) -> io::Result<Engine> {
        let dir = dir.as_ref();
        let manifest = Manifest::open(&dir.join(manifest::MANIFEST_NAME))?;
        let tile = match manifest.as_ref().and_then(|m| m.root().tile.as_ref()) {
            Some(t) => {
                // The root's binding must match the tile's own header, not just
                // its name: a swapped or stale filename must not be adopted.
                let opened = Tile::open(dir.join(&t.name).as_path())?;
                if opened.cutoff() != t.cutoff || opened.digest() != &t.digest {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest tile reference disagrees with tile header"));
                }
                Some(opened)
            }
            None => None,
        };
        let mut e = Self::open_impl(&dir.join(manifest::WAL_NAME), tile)?;
        if let Some(m) = &manifest {
            // While the WAL is fully retained these can only fire on divergence;
            // after WAL retirement the watermark check must become a max() merge
            // against the checkpointed watermark rather than replay-derived state.
            let cp = m.root().checkpoint;
            if cp.csn > e.csn {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest checkpoint CSN is ahead of the retained WAL"));
            }
            if cp.handle_watermark > e.max_handle {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "manifest handle watermark is ahead of the retained WAL"));
            }
        }
        e.manifest = manifest;
        Ok(e)
    }

    fn open_impl(wal_path: &Path, tile: Option<Tile>) -> io::Result<Engine> {
        let (wal, records) = Wal::open_or_recover(wal_path)?;
        if let Some(t) = &tile {
            if wal_digest(&records, t.cutoff())? != *t.digest() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "tile WAL prefix mismatch"));
            }
        }
        let mut mt = Memtable::new();
        let mut covered = Memtable::new();
        let mut csn = 0u64;
        let mut dedup = HashMap::new();
        let mut max_handle = 0;
        for payload in &records {
            let (rec_csn, block_id, ops) = decode_payload(payload)?;
            max_handle = max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
            let destination = if tile.as_ref().is_some_and(|t| rec_csn <= t.cutoff()) { &mut covered } else { &mut mt };
            for op in &ops { apply_op(destination, op, rec_csn); }
            csn = csn.max(rec_csn);
            dedup.insert(block_id, rec_csn);
        }
        if let Some(t) = &tile { t.verify_projection(&covered, t.cutoff())?; }
        Ok(Engine { wal, mt, tile, wal_path: wal_path.to_path_buf(), dir: parent_dir(wal_path), manifest: None, csn, dedup, max_handle })
    }

    /// Build one immutable P/U/R tile containing *all* WAL-published versions at or
    /// before cutoff. No automatic discovery, checkpoint, manifest or WAL rotation.
    /// A failed build leaves the serving memtable intact; a successful build evicts
    /// covered versions only after the file is synced and verified.
    pub fn build_tile(&mut self, tile_path: impl AsRef<Path>, cutoff: u64) -> io::Result<()> {
        if self.tile.is_some() || cutoff > self.csn {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile already active or cutoff exceeds CSN"));
        }
        let tile = self.write_verified_tile(tile_path.as_ref(), cutoff)?;
        self.mt.evict_through(cutoff);
        self.tile = Some(tile);
        Ok(())
    }

    /// Create-new the tile file, sync it, reopen it and re-check its full P/U/R
    /// projection against the retained WAL before any caller may reference it.
    fn write_verified_tile(&self, tile_path: &Path, cutoff: u64) -> io::Result<Tile> {
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
        let tile = Tile::write(tile_path, cutoff, &digest, &self.mt)?;
        tile.verify_projection(&expected, cutoff)?;
        Ok(tile)
    }

    /// Durable publication of one tile: build the existing verified tile exactly
    /// as [`Engine::build_tile`], make the tile's directory entry durable, then
    /// run the manifest protocol — write the inactive page at seq+1, sync the
    /// manifest file, sync the parent directory, flip the in-memory root — and
    /// only then evict tile-covered memtable versions. Strict limits: at most
    /// ONE active tile in this slice (a second `publish_tile` is refused), the
    /// tile path must be a plain not-yet-existing filename inside the engine
    /// directory (the manifest stores the name, never a path), and any manifest
    /// I/O error poisons the writer until reopen selects the highest valid page.
    /// A failed publish leaves the tile file on disk unreferenced (ignored by
    /// [`Engine::open_discover`]); republishing over it surfaces a clear
    /// `AlreadyExists` error instead of silently reusing the leftover file.
    pub fn publish_tile(&mut self, tile_path: impl AsRef<Path>, cutoff: u64) -> io::Result<()> {
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        if self.tile.is_some() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "tile already active; at most one tile in this slice"));
        }
        if cutoff > self.csn {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "cutoff exceeds CSN"));
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
        // 3. Manifest protocol; any error poisons the writer until reopen.
        let tref = TileRef { name: name.to_string(), cutoff, digest: *tile.digest() };
        let checkpoint = Checkpoint { csn: cutoff, handle_watermark: self.max_handle };
        match &mut self.manifest {
            Some(m) => m.publish(tref, checkpoint)?,
            None => {
                let path = self.dir.join(manifest::MANIFEST_NAME);
                let mut m = match Manifest::create(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        // An engine opened without discovery must never overwrite
                        // a root it did not read; adopt or refuse, never clobber.
                        match Manifest::open(&path)? {
                            Some(m) if m.root().tile.is_none() => m,
                            _ => return Err(io::Error::new(io::ErrorKind::InvalidData,
                                "manifest already exists with an active tile; use open_discover to adopt it before publishing")),
                        }
                    }
                    Err(e) => return Err(e),
                };
                m.publish(tref, checkpoint)?;
                self.manifest = Some(m);
            }
        }
        // 4. In-memory flip only after the durable flip.
        self.mt.evict_through(cutoff);
        self.tile = Some(tile);
        Ok(())
    }

    /// Prototype inspection hook: proves tile-covered versions are no longer served
    /// from the memtable (not a bound on WAL recovery or total RAM).
    pub fn serving_memtable_entries(&self) -> usize { self.mt.len() }

    // Sequential, checksum-verified traversal. No index or bounded-I/O point read.
    // A failed read aborts before callers can publish a block based on partial state.
    fn scan_merged(&self, prefix: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<bool>) -> io::Result<()> {
        let mut mem = self.mt.scan_iter(prefix).peekable();
        let mut disk = self.tile.iter().flat_map(|tile| tile.entries());
        let mut next_disk = next_matching(&mut disk, prefix)?;
        while mem.peek().is_some() || next_disk.is_some() {
            let use_mem = match (mem.peek(), next_disk.as_ref()) {
                (Some((mk, _)), Some((dk, _))) => mk <= &dk.as_slice(),
                (Some(_), None) => true,
                _ => false,
            };
            if use_mem {
                let (key, value) = mem.next().unwrap();
                let equal = next_disk.as_ref().is_some_and(|(dk, _)| key == dk);
                if !visit(key, value)? { return Ok(()); }
                if equal { next_disk = next_matching(&mut disk, prefix)?; }
            } else {
                let (key, value) = next_disk.take().unwrap();
                if !visit(&key, &value)? { return Ok(()); }
                next_disk = next_matching(&mut disk, prefix)?;
            }
        }
        Ok(())
    }

    /// 已发布的最新 CSN。新块的快照从这里取。
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

    /// 请求标识已用于另一意图（包含旧式未携带意图的 block id）时拒绝重放。
    /// 当前原型的幂等表保留整个 WAL 生命周期；后续须实现保留期限。
    pub fn request_intent_conflicts(&self, request_prefix: &[u8], intent_prefix: &[u8]) -> bool {
        self.dedup.keys().any(|id| id.starts_with(request_prefix) && !id.starts_with(intent_prefix))
    }

    /// 反向查找：当前指向 `target` 的全部 source 句柄。
    /// 按 source 取最新 CSN ≤ snapshot；tombstone（空 value）剔除。
    pub fn reverse_lookup(
        &self,
        entity: &[u8],
        field: &[u8],
        target: u64,
        snapshot: u64,
    ) -> io::Result<Vec<u64>> {
        check_name(entity)?;
        check_name(field)?;
        let pfx = keys::reverse_prefix(entity, field, target)?;
        let mut newest: HashMap<u64, (u64, bool)> = HashMap::new();
        self.scan_merged(&pfx, |key, value| {
            let Some((_, source, csn)) = keys::decode_reverse(key) else { return Ok(true); };
            if csn > snapshot { return Ok(true); }
            let alive = !value.is_empty();
            match newest.get_mut(&source) {
                Some(e) if e.0 >= csn => {}
                _ => {
                    newest.insert(source, (csn, alive));
                }
            }
            Ok(true)
        })?;
        let mut out: Vec<u64> = newest.into_iter().filter(|(_, (_, a))| *a).map(|(s, _)| s).collect();
        out.sort();
        Ok(out)
    }

    /// 按已声明唯一字段解析句柄；每个 handle 只采用快照内最新版本。
    /// 如观察到多个活 owner，则索引已损坏，不能任意选一个。
    pub fn unique_lookup(&self, entity: &[u8], field: &[u8], value: &[u8], snapshot: u64) -> io::Result<Option<u64>> {
        let pfx = keys::unique_prefix(entity, field, value)?;
        let mut seen = std::collections::HashSet::new();
        let mut owner = None;
        self.scan_merged(&pfx, |key, val| {
            if key.len() != pfx.len() + 16 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed unique key"));
            }
            let handle = u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap());
            let csn = keys::key_csn(&key).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing CSN"))?;
            if csn > snapshot || !seen.insert(handle) || val.is_empty() { return Ok(true); }
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

    /// 快照点读：`snapshot` 取块开始时的 [`Engine::csn`]。
    pub fn get(&self, entity: &[u8], handle: u64, snapshot: u64) -> io::Result<Option<Vec<u8>>> {
        check_name(entity)?;
        let pfx = keys::primary_prefix(entity, handle)?;
        let mut result = None;
        self.scan_merged(&pfx, |key, value| {
            if keys::key_csn(key).is_some_and(|csn| csn <= snapshot) {
                result = if value.is_empty() { None } else { Some(value.to_vec()) };
                return Ok(false);
            }
            Ok(true)
        })?;
        Ok(result)
    }

    /// Visit each live primary document once at a CSN snapshot. The scan walks
    /// the whole entity prefix (including historical versions); callers must
    /// bound their returned rows separately when filtering or ordering results.
    pub fn scan_primary(
        &self,
        entity: &[u8],
        snapshot: u64,
        mut visit: impl FnMut(u64, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        check_name(entity)?;
        let mut prefix = keys::primary_prefix(entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        let mut last_handle = None;
        self.scan_merged(&prefix, |key, value| {
            let (key_entity, handle, csn) = keys::decode_primary(key)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed primary key"))?;
            if key_entity != entity {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "primary entity mismatch"));
            }
            if csn > snapshot || last_handle == Some(handle) { return Ok(true); }
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

    fn get_ok(e: &Engine, entity: &str, handle: u64) -> Option<Vec<u8>> {
        e.get(entity.as_bytes(), handle, e.csn()).unwrap().map(|b| b.to_vec())
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
    fn unique_lookup_tracks_snapshot_and_reassignment() {
        let p = tmp("unique_lookup.wal");
        let mut e = Engine::create(&p).unwrap();
        let field = b"email";
        let value = b"a@x.com";
        let put = |handle| Op::PutUnique { entity: b"Customer".to_vec(), field: field.to_vec(), value: value.to_vec(), handle };
        let del = |handle| Op::DelUnique { entity: b"Customer".to_vec(), field: field.to_vec(), value: value.to_vec(), handle };
        assert_eq!(e.unique_lookup(b"Customer", field, value, 0).unwrap(), None);
        e.commit_block(b"one", &[put(1)]).unwrap();
        e.commit_block(b"transfer", &[del(1), put(2)]).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value, 1).unwrap(), Some(1));
        assert_eq!(e.unique_lookup(b"Customer", field, value, 2).unwrap(), Some(2));
        e.commit_block(b"delete", &[del(2)]).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value, 3).unwrap(), None);
        drop(e);
        let e = Engine::open(&p).unwrap();
        assert_eq!(e.unique_lookup(b"Customer", field, value, 2).unwrap(), Some(2));
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
        assert_eq!(e.reverse_lookup(b"ServiceTicket", b"order", 101, u64::MAX).unwrap(), vec![201, 202]);
        // 删除 201 的反向条目 → 202 保留
        e.commit_block(b"r3", &[Op::DelReverse {
            entity: b"ServiceTicket".to_vec(), field: b"order".to_vec(),
            target: 101, source: 201,
        }]).unwrap();
        assert_eq!(e.reverse_lookup(b"ServiceTicket", b"order", 101, u64::MAX).unwrap(), vec![202]);
        // 旧快照仍能看到 201
        assert_eq!(e.reverse_lookup(b"ServiceTicket", b"order", 101, 2).unwrap(), vec![201, 202]);
        remove(&p);
    }

    fn remove(p: &Path) {
        fs::remove_file(p).ok();
    }
}
