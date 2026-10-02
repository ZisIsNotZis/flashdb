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

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::bloom::BloomBuilder;
use crate::keys::{self, check_name};
use crate::manifest::{self, Checkpoint, Manifest, SegmentRef, TileRef};
use crate::memtable::Memtable;
use crate::metrics::{self, Metrics};
use crate::tile::{Tile, TileCursor, TileWriter, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, MIN_PAGE_SIZE};
use crate::wal::{self, Wal};

const MAGIC: &[u8; 4] = b"FDB1";

/// Bits per *expected* entry for a tile's unique-prefix bloom under
/// [`LayoutPolicy::declared_default`]. The tile stores this value in its
/// superblock and bloom header; 10 implies 7 probes. The effective bits per
/// *inserted* prefix is higher because the filter is sized from an upper bound
/// on the tile's entry count (which also counts P/R keys) while only U-key
/// prefixes are inserted, so the filter is over-provisioned and its
/// false-positive rate is below the nominal one. The rate is measured in
/// `bloom.rs`, never asserted. A policy of 0 disables the bloom entirely (see
/// [`crate::tile::Tile::might_contain_unique_prefix`]).
const UNIQUE_BLOOM_BITS_PER_KEY: u32 = 10;

/// Region name under which a v0 tile is written. A v0 tile is a CSN-window
/// projection of the whole keyspace (every key family interleaved), so it spans
/// every prefix-region; the engine therefore has exactly one write region today.
/// Family partitioning (per-region tiles) is a later slice and will supply finer
/// region keys.
const TILE_LAYOUT_REGION: &str = "all";

/// Whether a tile write path emits one tile per `(entity, key family)` region
/// (slice 24) or the historical single whole-keyspace tile.
///
/// [`RegionMode::Global`] is the shipped default and reproduces the pre-slice-24
/// behaviour exactly: one tile holding every region's entries interleaved by
/// encoded key, ordered by strictly increasing cutoff. [`RegionMode::PerRegion`]
/// splits each publish/compaction output one tile per region; those tiles share
/// the publish cutoff, so the active list is ordered by `(cutoff, region)` and
/// each region's tiles partition that region's CSN range. The mode is a
/// measurement/rollout switch, not durable state: recovery derives each tile's
/// region from the tile itself (its first key), so it never needs the mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionMode {
    /// One whole-keyspace tile per publish (pre-slice-24 behaviour).
    Global,
    /// One tile per `(entity, key family)` region per publish/compaction output.
    PerRegion,
}

impl RegionMode {
    /// Parse the `FLASHDB_REGION_MODE` spelling; `None` for an unknown name so
    /// the caller can warn and keep the shipped default.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "global" => Some(Self::Global),
            "per_region" | "per-region" => Some(Self::PerRegion),
            _ => None,
        }
    }

    /// The canonical `FLASHDB_REGION_MODE` spelling, for driver reporting.
    pub fn name(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::PerRegion => "per_region",
        }
    }
}

/// Provisional per-prefix-region layout declaration (slice 17).
///
/// A policy maps a prefix-region name to a tile's two already-safe physical
/// parameters - page size and bloom bits per key - and returns a default for any
/// region it does not name. It is deliberately **not persisted**: there is no
/// policy table and no manifest field, and the reader never consults it. The
/// only durable declaration is what each tile's superblock already records
/// (`page_size`, `bits_per_key`), which is exactly what makes this safe to land
/// now: a policy change affects only *new* tiles, and a reader honors the stored
/// per-tile value, so a stale or bad layout is slow, never wrong (I2).
///
/// [`LayoutPolicy::declared_default`] reproduces today's behaviour exactly
/// (4096-byte pages, 10 bloom bits/key everywhere). A durable declaration
/// surface mapping regions to layouts - and the family partitioning that needs
/// it - is a later slice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPolicy {
    default: (u32, u8),
    regions: Vec<(String, u32, u8)>,
}

impl Default for LayoutPolicy {
    fn default() -> Self { Self::declared_default() }
}

impl LayoutPolicy {
    /// Today's values exactly: 4096-byte pages and 10 bloom bits/key for every
    /// region.
    pub fn declared_default() -> Self {
        LayoutPolicy { default: (DEFAULT_PAGE_SIZE, UNIQUE_BLOOM_BITS_PER_KEY as u8), regions: Vec::new() }
    }

    /// A policy with a different default and no per-region overrides. `page_size`
    /// must be in `4096..=65536`; the tile writer rejects anything else at build
    /// time rather than writing a tile it cannot read back.
    pub fn with_default(page_size: u32, bloom_bits_per_key: u8) -> Self {
        LayoutPolicy { default: (page_size, bloom_bits_per_key), regions: Vec::new() }
    }

    /// The `(page_size, bloom_bits_per_key)` a tile in `region` is written with.
    /// An unknown region returns the policy default, never an error: a missing
    /// declaration is today's behavior (I2 - a bad layout is slow, never wrong).
    pub fn for_region(&self, region: &str) -> (u32, u8) {
        self.regions
            .iter()
            .find(|(name, _, _)| name == region)
            .map(|(_, page_size, bits)| (*page_size, *bits))
            .unwrap_or(self.default)
    }

    /// The policy-wide default, returned for every region without an override.
    pub fn default_layout(&self) -> (u32, u8) { self.default }

    /// Override one named prefix-region; returns the previous value when the
    /// region was already declared. `page_size` is validated by the tile writer,
    /// not here, so a bad override surfaces as a failed tile build (`InvalidData`).
    pub fn set_region(&mut self, region: &str, page_size: u32, bloom_bits_per_key: u8) -> Option<(u32, u8)> {
        if let Some(entry) = self.regions.iter_mut().find(|(name, _, _)| name == region) {
            let previous = (entry.1, entry.2);
            entry.1 = page_size;
            entry.2 = bloom_bits_per_key;
            return Some(previous);
        }
        self.regions.push((region.to_string(), page_size, bloom_bits_per_key));
        None
    }

    /// Provisional benchmark override: read `FLASHDB_PAGE_SIZE` and
    /// `FLASHDB_BLOOM_BITS`, falling back to [`LayoutPolicy::declared_default`]
    /// for an unset or invalid value. Measurement scaffolding only: nothing here
    /// is persisted and the reader never consults it. `FLASHDB_BLOOM_BITS=0`
    /// disables the bloom; values above 64 are clamped by the bloom builder.
    pub fn from_env() -> Self {
        let mut policy = Self::declared_default();
        if let Ok(raw) = std::env::var("FLASHDB_PAGE_SIZE") {
            match raw.parse::<u32>() {
                Ok(v) if (MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&v) => policy.default.0 = v,
                _ => eprintln!(
                    "FLASHDB_PAGE_SIZE={raw:?} is not in {MIN_PAGE_SIZE}..={MAX_PAGE_SIZE}; using {}",
                    policy.default.0
                ),
            }
        }
        if let Ok(raw) = std::env::var("FLASHDB_BLOOM_BITS") {
            match raw.parse::<u8>() {
                Ok(v) => policy.default.1 = v,
                Err(_) => eprintln!(
                    "FLASHDB_BLOOM_BITS={raw:?} is not a small integer; using {}",
                    policy.default.1
                ),
            }
        }
        policy
    }
}

/// The largest active-tile list the engine will hold, re-exported from
/// [`crate::manifest::MAX_TILES`]. It is a **policy constant**, not a page
/// artefact: the manifest chains continuation pages, so this is the caller's
/// compaction-trigger knob rather than the size of one 4 KiB root.
pub const MAX_ACTIVE_TILES: usize = manifest::MAX_TILES;

/// Which contiguous run [`Engine::maybe_compact_shape`] merges. All three
/// policies live entirely in `select_run`; the rest of compaction is unchanged.
/// The scale driver picks one at runtime from `FLASHDB_COMPACT_SHAPE` so a shape
/// sweep needs no rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactShape {
    /// Today's tiered policy: the contiguous run of length >= 2 with the
    /// smallest combined on-disk size (ties: the oldest), so a tile larger than
    /// the budget is skipped while a cheaper run exists. The minimum is always
    /// attained by an adjacent pair, so this is a Huffman-style pairwise tier
    /// merge; per-merge work is bounded by the budget and rewrites per byte are
    /// O(log N). When no run fits the budget, the two smallest adjacent tiles
    /// merge (progress guarantee).
    CheapestPair,
    /// Universal-compaction shape: seed on the globally smallest tile (ties:
    /// oldest) and repeatedly absorb the smaller eligible adjacent tile while
    /// every input stays within 2x of the run's smallest size (the seed's size,
    /// since the seed is the global minimum) and the run fits the budget and
    /// the tile cap. Falls back to `CheapestPair` when the run is shorter than
    /// two tiles.
    RatioGroup,
    /// Budget-maximal interval around the smallest tile: seed on the globally
    /// smallest tile (ties: oldest) and repeatedly absorb the smaller eligible
    /// adjacent tile while the run fits the budget and the tile cap, with no
    /// ratio limit. Falls back to `CheapestPair` when the run is shorter than
    /// two tiles (no neighbour fits the budget).
    BudgetPrefix,
}

impl CompactShape {
    /// Parse the `FLASHDB_COMPACT_SHAPE` spelling; `None` for an unknown name so
    /// the caller can warn and keep its shipped default.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "cheapest_pair" => Some(Self::CheapestPair),
            "ratio_group" => Some(Self::RatioGroup),
            "budget_prefix" => Some(Self::BudgetPrefix),
            _ => None,
        }
    }

    /// The canonical `FLASHDB_COMPACT_SHAPE` spelling, for driver reporting.
    pub fn name(self) -> &'static str {
        match self {
            Self::CheapestPair => "cheapest_pair",
            Self::RatioGroup => "ratio_group",
            Self::BudgetPrefix => "budget_prefix",
        }
    }
}

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

/// Result of [`Engine::commit_batch`].
///
/// A batch is all-or-nothing: a unique conflict anywhere rejects the *whole*
/// batch before a record is appended, so a `Conflict` here means the WAL did
/// not grow and no CSN was consumed. `Committed` carries one [`Outcome`] per
/// input block, in input order (each `Committed { csn }` is that block's own
/// CSN; an idempotent retry is `AlreadyCommitted { csn }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BatchOutcome {
    Committed(Vec<Outcome>),
    /// `index` is the highest-index input block that contributed a unique op
    /// under the conflicted prefix, i.e. the block whose op leaves more than one
    /// live owner in the batch's projected post-state.
    Conflict { index: usize, conflict: Conflict },
}

/// One touched unique prefix inside a batch: the constraint identity, the
/// batch's per-handle net effect (later ops win, exactly as [`Memtable::apply`]
/// leaves them), and the highest-index input block that contributed to it.
struct BatchUnique {
    entity: Vec<u8>,
    field: Vec<u8>,
    value: Vec<u8>,
    owners: HashMap<u64, bool>,
    last_block: usize,
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
    /// Active tiles, ordered by `(cutoff, region)` strictly increasing. Cutoffs
    /// alone are NON-decreasing: a region-partitioned publish emits one tile per
    /// region at the same cutoff. The pair ordering is what keeps regions disjoint.
    tiles: Vec<TileHandle>,
    wal_path: PathBuf,
    dir: PathBuf,
    manifest: Option<Manifest>,
    /// Current WAL segment per the manifest root; `None` while the whole
    /// history lives in `data.wal`. Republishing carries it verbatim into every
    /// new root so a rotated engine's root never stops naming the live suffix.
    segment: Option<SegmentRef>,
    /// Max CSN present in the active WAL FILE (0 when it holds no records): the
    /// replay-derived suffix maximum in segment mode, where the engine CSN is
    /// merged with the manifest checkpoint below the segment floor and may
    /// legitimately exceed it. The republish guard compares the file against
    /// this, not against the merged CSN.
    segment_suffix_csn: u64,
    /// Set when the live WAL binding could not follow the durable segment flip
    /// during rotation: the manifest root already names the new segment, so any
    /// further append through the old binding would be unrecoverable. Every
    /// mutating operation fails closed until reopen, which recovers from the
    /// durable root (the never-unlinked old segment is an ignored orphan).
    wal_detached: bool,
    csn: u64,
    dedup: HashMap<Vec<u8>, u64>,
    max_handle: u64,
    /// True while a deferred-barrier commit group is open (slice 23). Inside a
    /// group every commit is WAL-appended and applied to the memtable
    /// IMMEDIATELY, so a later probe/guard in the group sees it exactly as a
    /// sequential commit would, but it is NOT `fdatasync`ed; the single barrier
    /// is taken by [`Engine::end_group`]. Every other mutating entry point
    /// refuses while a group is open, so a caller that forgets to end one fails
    /// closed instead of silently committing through a path the group's barrier
    /// never covered.
    group_open: bool,
    /// True once a grouped commit has run in the open group, so
    /// [`Engine::end_group`] knows a barrier is owed. An empty group takes no
    /// `sync` (nothing was appended). Reset by `begin_group`/`end_group`.
    group_pending: bool,
    /// Provisional per-region layout the two tile write paths consult. Never
    /// persisted; the per-tile parameters live in each tile's superblock.
    layout: LayoutPolicy,
    /// Whether a write path partitions its output one tile per `(entity, key
    /// family)` region ([`RegionMode::PerRegion`]) or the historical single
    /// whole-keyspace tile ([`RegionMode::Global`], the default). Not persisted:
    /// recovery derives each tile's region from the tile's first key.
    region_mode: RegionMode,
    /// Per-pattern latency telemetry (slice 16). Observational only: never
    /// persisted, never read on the correctness path. `RefCell` is required
    /// because read paths take `&self`; it is the only interior mutability here.
    /// Disabled via [`Engine::set_metrics_enabled`].
    metrics: RefCell<Metrics>,
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

/// RAII latency sample: records on drop, so every exit path (including `?`) is
/// observed. A no-op with no clock read when telemetry is disabled.
struct MetricsSample<'a> {
    metrics: &'a RefCell<Metrics>,
    id: usize,
    started: Option<Instant>,
}

impl Drop for MetricsSample<'_> {
    fn drop(&mut self) {
        if let Some(t0) = self.started {
            self.metrics.borrow_mut().record(self.id, t0.elapsed());
        }
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
            segment_suffix_csn: 0,
            wal_detached: false,
            csn: 0,
            dedup: HashMap::new(),
            max_handle: 0,
            group_open: false,
            group_pending: false,
            layout: LayoutPolicy::declared_default(),
            region_mode: RegionMode::Global,
            metrics: RefCell::new(Metrics::new()),
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
        // The active list is ordered by `(cutoff, region)` strictly increasing.
        // A whole-keyspace tile spans every region, so for a legacy list the
        // region is constant per tile and this reduces to strictly increasing
        // cutoffs; a region-partitioned publish leaves several tiles sharing the
        // cutoff, ordered by their region prefix. The secondary order also makes
        // a same-cutoff group's regions distinct (no region is covered twice).
        if !tiles.windows(2).all(|w| {
            (w[0].cutoff(), tile_first_region(&w[0])) < (w[1].cutoff(), tile_first_region(&w[1]))
        }) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "tile list is not strictly increasing by (cutoff, region)"));
        }
        let (wal, records) = match Wal::open_or_recover(wal_path) {
            Ok(opened) => opened,
            // A rotated directory no longer has `data.wal`; only `open_discover`
            // (which follows the manifest's segment reference) can open it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(io::ErrorKind::NotFound, format!(
                    "WAL segment {} is missing: the durable manifest names it as the live segment, so the directory is incomplete (deleted or never fully rotated)", wal_path.display())));
            }
            Err(e) => return Err(e),
        };
        for t in &tiles {
            if floor.is_some_and(|f| t.cutoff() <= f) { continue; }
            if wal_digest(&records, t.cutoff(), floor.unwrap_or(0))? != *t.tile.digest() {
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
        let floor_csn = floor.unwrap_or(0);
        // `lower` is per region: a tile covers exactly its region's `(lower,
        // cutoff]`, and its region's previous tile (or the rotation floor) sets
        // that lower. A whole-keyspace tile covers every region in the range, so
        // it uses the global previous cutoff and is still checked exactly.
        let mut region_last: HashMap<Vec<u8>, u64> = HashMap::new();
        let mut prev_cutoff = floor_csn;
        for t in &tiles {
            if floor.is_some_and(|f| t.cutoff() <= f) { continue; }
            // Tiles hold exactly the newest version per logical key within their
            // CSN range (same reduction the tile writers apply), so the WAL
            // projection is reduced the same way before the 1:1 comparison.
            let lower = match tile_single_region(t) {
                Some(region) => region_last.get(region).copied().unwrap_or(floor_csn),
                None => prev_cutoff,
            };
            t.tile.verify_projection(&newest_projection(&covered, lower, t.cutoff()), lower, t.cutoff())?;
            if let Some(region) = tile_single_region(t) {
                region_last.insert(region.to_vec(), t.cutoff());
            }
            prev_cutoff = t.cutoff();
        }
        Ok(Engine { wal, mt, tiles, wal_path: wal_path.to_path_buf(), dir: parent_dir(wal_path), manifest: None, segment: None, segment_suffix_csn: csn, wal_detached: false, csn, dedup, max_handle, group_open: false, group_pending: false, layout: LayoutPolicy::declared_default(), region_mode: RegionMode::Global, metrics: RefCell::new(Metrics::new()) })
    }

    /// The CSN floor below which the active WAL file holds no records: the
    /// segment start CSN once rotation exists, 0 while the full history is
    /// retained in `data.wal`.
    fn wal_floor(&self) -> u64 {
        self.segment.as_ref().map(|s| s.start_csn).unwrap_or(0)
    }

    /// Fail closed once a rotation's live-WAL rebinding failed after the
    /// durable flip: the manifest root already names the new segment, so an
    /// append through the stale binding would land in a file recovery ignores.
    /// Reopen recovers from the durable root (the never-unlinked old segment is
    /// an ignored orphan).
    fn ensure_attached(&self) -> io::Result<()> {
        if self.wal_detached {
            return Err(io::Error::new(io::ErrorKind::Other, "engine WAL binding detached from the durable segment by a failed rotation; reopen to recover"));
        }
        Ok(())
    }

    /// Refuse a mutating operation that must not race an open deferred-barrier
    /// group. While a group is open its records are appended but not yet
    /// synced, so republishing a tile, rotating, compacting, or taking the
    /// single-block path could bind durable state to bytes the group's barrier
    /// has not yet covered. Refusing keeps a forgotten `end_group` fail-closed.
    fn ensure_no_group(&self, op: &str) -> io::Result<()> {
        if self.group_open {
            return Err(io::Error::new(io::ErrorKind::Other, format!(
                "cannot {op} while a commit group is open; end_group first")));
        }
        Ok(())
    }

    /// The republish guard: the WAL file being published from must still hold
    /// exactly the records the serving state was recovered from. Segment mode
    /// merges the checkpoint frontier below the segment floor into the engine
    /// CSN, so there the comparison target is the suffix maximum derived from
    /// the active segment at open (and advanced by each commit), not the merged
    /// CSN. Any divergence fails closed before a tile can be bound to content
    /// the engine never served.
    fn verify_wal_matches_state(&self, records: &[Vec<u8>]) -> io::Result<()> {
        let wal_max = records.last().map(|r| decode_payload(r).map(|v| v.0)).transpose()?.unwrap_or(0);
        self.verify_wal_max(wal_max)
    }

    /// The republish guard's comparison, shared by the collected-record callers
    /// and the streaming publish path so both reject the same divergences.
    fn verify_wal_max(&self, wal_max: u64) -> io::Result<()> {
        let expected = if self.segment.is_some() { self.segment_suffix_csn } else { self.csn };
        if wal_max != expected && self.csn != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL differs from serving state"));
        }
        Ok(())
    }

    /// Build one immutable P/U/R tile holding, for the WAL-published versions
    /// in `(previous cutoff, cutoff]`, only the NEWEST version per logical key.
    /// No manifest, checkpoint, or WAL rotation. A failed build leaves the
    /// serving memtable intact; a successful build evicts covered versions only
    /// after the file is synced and verified.
    pub fn build_tile(&mut self, tile_path: impl AsRef<Path>, cutoff: u64) -> io::Result<()> {
        self.ensure_no_group("build_tile")?;
        self.ensure_attached()?;
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
        let emitted = self.write_verified_tiles(tile_path, cutoff)?;
        self.mt.evict_through(cutoff);
        for EmittedTile { name, tile } in emitted {
            self.tiles.push(TileHandle { tile, name });
        }
        Ok(())
    }

    /// Create-new the tile file(s), sync them, reopen each and re-check its
    /// exact newest-version-per-logical-key `(previous cutoff, cutoff]` P/U/R
    /// projection against the active WAL file — the retained WAL before
    /// rotation, the live segment's `(segment start, cutoff]` suffix after it
    /// (the prefix below the segment floor is already verified state carried by
    /// the published tiles and the manifest checkpoint) — before any caller may
    /// reference them.
    ///
    /// Under [`RegionMode::Global`] this is the historical single tile over the
    /// whole keyspace. Under [`RegionMode::PerRegion`] it is one tile per
    /// `(entity, key family)` region present in the window: the memtable is
    /// already key-ordered, so its keys of one region are contiguous and the
    /// split is a single streaming pass, one [`TileWriter`] open at a time. Each
    /// region tile is verified only against that region's WAL projection (its
    /// sibling tiles carry the other regions); every emitted tile shares the
    /// publish digest, which binds records by `(floor, cutoff]` and is
    /// region-independent.
    ///
    /// One bounded WAL-side copy: the suffix in `(previous cutoff, cutoff]` is
    /// decoded once, in a single streaming pass, into a key-ordered
    /// [`BTreeMap`]. The tile(s) themselves are written by streaming the serving
    /// memtable through [`TileWriter`]; no projection of either side is
    /// materialized, so the publish peak is one memtable plus one WAL window.
    fn write_verified_tiles(&self, tile_path: &Path, cutoff: u64) -> io::Result<Vec<EmittedTile>> {
        let lower = self.tiles.last().map(|t| t.cutoff()).unwrap_or(0);
        // Remaining manifest capacity. A partitioned publish emits one tile per region, so the
        // cap must be checked BEFORE each region's file is created: otherwise a publish near the
        // cap wrote its files, failed inside the manifest, and POISONED the writer (requiring a
        // reopen) for a condition that is simply "not enough room". Global mode still costs one.
        let capacity = manifest::MAX_TILES.saturating_sub(self.tiles.len());
        if capacity == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                "active tile list is full; compact or rotate before publishing"));
        }
        // Stream the retained WAL suffix once. Alongside the `(lower, cutoff]`
        // expectation map this reproduces exactly the two former preconditions:
        // the last record's CSN must match the serving state
        // (`verify_wal_matches_state`) and the digest binds `(floor, cutoff]`
        // (`wal_digest`). `op_kv` is the single op -> key/value spelling shared
        // with `apply_op` and compaction, so the map matches `Memtable::apply`.
        let mut wal_entries: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        let mut hash = Sha256::new();
        let mut last_csn = 0u64;
        wal::replay_each(&self.wal_path, |payload| {
            let (csn, _, ops) = decode_payload(payload)?;
            wal_digest_record(&mut hash, &mut last_csn, payload, csn, cutoff)?;
            if csn <= cutoff && csn > lower {
                for op in &ops {
                    let (key, value) = op_kv(op, csn);
                    wal_entries.insert(key, value);
                }
            }
            Ok(())
        })?;
        self.verify_wal_max(last_csn)?;
        let digest = wal_digest_finish(hash, last_csn, cutoff, self.wal_floor())?;
        if self.region_mode == RegionMode::Global {
            let tile = self.write_one_tile(tile_path, cutoff, &digest, lower, TILE_LAYOUT_REGION)?;
            // Source (b): the WAL-derived expectation map. Independent of the
            // memtable the tile was written from, so a divergence between the two
            // fails closed here - unlike compaction's self-referential re-merge.
            verify_tile_against_wal(&tile, &wal_entries, lower, cutoff, None)?;
            return Ok(vec![EmittedTile { name: tile_name(tile_path), tile }]);
        }
        // Partitioned: the memtable stream is key-ordered, so a region's keys
        // are contiguous. Buffer nothing; keep exactly one writer open and
        // finish it when the region prefix changes.
        let mut emitted: Vec<EmittedTile> = Vec::new();
        let mut writer: Option<TileWriter> = None;
        let mut bloom: Option<BloomBuilder> = None;
        let mut current: Option<Vec<u8>> = None;
        let mut last_logical: Option<Vec<u8>> = None;
        for (key, value) in self.mt.entries() {
            let Some(csn) = keys::key_csn(key) else { continue };
            if csn <= lower || csn > cutoff { continue; }
            let logical = &key[..key.len() - 8];
            if last_logical.as_deref() == Some(logical) { continue; }
            last_logical = Some(logical.to_vec());
            let region = keys::region_prefix(key).map(<[u8]>::to_vec);
            if current.as_deref() != region.as_deref() {
                if let Some(mut w) = writer.take() {
                    if let Some(b) = bloom.take() { w.set_bloom(b.finish())?; }
                    let tile = w.finish()?;
                    let region = current.take();
                    verify_tile_against_wal(&tile, &wal_entries, lower, cutoff, region.as_deref())?;
                    emitted.push(EmittedTile { name: tile_name(&region_tile_path(tile_path, emitted.len())), tile });
                }
                if emitted.len() >= capacity {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput,
                        "active tile list is full for this publish: a partitioned publish emits one tile per region and the remaining capacity is smaller"));
                }
                current = region;
                let path = region_tile_path(tile_path, emitted.len());
                let (page_size, bits) = self.layout.for_region(&current.as_deref().map(keys::region_name).unwrap_or_default());
                writer = Some(TileWriter::new(&path, cutoff, &digest, page_size)?);
                bloom = (bits > 0).then(|| BloomBuilder::new(self.mt.len() as u64, bits as u32));
            }
            let w = writer.as_mut().expect("a writer exists once a region is open");
            w.push(key, value)?;
            if let Some(b) = bloom.as_mut() {
                if let Some(prefix) = unique_prefix_of(key) { b.insert(prefix); }
            }
        }
        if let Some(mut w) = writer.take() {
            if let Some(b) = bloom.take() { w.set_bloom(b.finish())?; }
            let tile = w.finish()?;
            let region = current.take();
            verify_tile_against_wal(&tile, &wal_entries, lower, cutoff, region.as_deref())?;
            emitted.push(EmittedTile { name: tile_name(&region_tile_path(tile_path, emitted.len())), tile });
        }
        Ok(emitted)
    }

    /// Write one tile covering the whole `(lower, cutoff]` window from the
    /// serving memtable, with the given layout region's `(page_size, bits)`.
    fn write_one_tile(&self, tile_path: &Path, cutoff: u64, digest: &[u8; 32], lower: u64, layout_region: &str) -> io::Result<Tile> {
        let (page_size, bloom_bits_per_key) = self.layout.for_region(layout_region);
        let mut writer = TileWriter::new(tile_path, cutoff, digest, page_size)?;
        // Bloom over exactly the unique prefixes this pass stores, sized from a
        // known upper bound (the serving memtable's entry count) so the publish
        // pass stays single and streaming: no second read to count keys and no
        // entry retained. Only U keys are inserted, so the filter is
        // over-provisioned relative to its stored `bits_per_key`. A policy of 0
        // bits means "no bloom": the tile carries no bloom section and
        // `might_contain_unique_prefix` answers `true` for every prefix.
        let mut bloom = (bloom_bits_per_key > 0)
            .then(|| BloomBuilder::new(self.mt.len() as u64, bloom_bits_per_key as u32));
        let mut last_logical: Option<Vec<u8>> = None;
        for (key, value) in self.mt.entries() {
            let Some(csn) = keys::key_csn(key) else { continue };
            if csn <= lower || csn > cutoff { continue; }
            let logical = &key[..key.len() - 8];
            if last_logical.as_deref() == Some(logical) { continue; }
            last_logical = Some(logical.to_vec());
            writer.push(key, value)?;
            if let Some(bloom) = bloom.as_mut() {
                if let Some(prefix) = unique_prefix_of(key) { bloom.insert(prefix); }
            }
        }
        if let Some(bloom) = bloom { writer.set_bloom(bloom.finish())?; }
        writer.finish()
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
        self.ensure_no_group("publish_tile")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
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
        // 1. Build + verify the tile file(s) without touching serving state.
        let emitted = match self.write_verified_tiles(tile_path, cutoff) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "tile candidate already exists and is unreferenced by the manifest (leftover from a failed publish or crash after tile sync); remove {} or publish to another name", tile_path.display())));
            }
            Err(e) => return Err(e),
        };
        // 2. The tiles' own directory entries must be durable before the manifest
        //    can reference them (checkpoint publication ordering).
        File::open(&self.dir)?.sync_all()?;
        // 3. Manifest protocol; any error poisons the writer until reopen. The
        //    page always carries the full ordered active list plus the new tiles,
        //    which append in region order at the new (strictly greater) cutoff.
        let mut refs: Vec<TileRef> = self.tiles.iter().map(|t| t.tile_ref()).collect();
        for e in &emitted {
            refs.push(TileRef { name: e.name.clone(), cutoff, digest: *e.tile.digest() });
        }
        let checkpoint = Checkpoint { csn: cutoff, handle_watermark: self.max_handle };
        self.manifest_publish(refs, checkpoint, self.segment.clone())?;
        // 4. In-memory flip only after the durable flip.
        self.mt.evict_through(cutoff);
        for EmittedTile { name, tile } in emitted {
            self.tiles.push(TileHandle { tile, name });
        }
        Ok(())
    }

    // Shared manifest step of the publication protocol ([`Engine::publish_tile`],
    // [`Engine::compact`] and [`Engine::rotate_wal`]): write the inactive page at
    // `seq + 1` carrying the FULL active tile list, sync the manifest file, sync
    // the parent directory, then flip the in-memory root. `segment` is the root's
    // new segment reference: callers carry the engine's current one verbatim
    // (`None` until rotation exists — a rotated engine's republished root must
    // keep naming the live suffix, a legacy engine stays None-encoded), while
    // rotation installs the fresh one. A missing manifest is created, or an
    // existing empty-root manifest adopted — an engine opened without discovery
    // must never overwrite a root it did not read. Any error poisons the writer
    // until reopen.
    fn manifest_publish(&mut self, refs: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<()> {
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

    /// Rotate the WAL onto a fresh, empty segment once the live segment exceeds
    /// `max_wal_bytes`. Preconditions: every committed block must already be
    /// published as a tile (`newest cutoff == csn`) and at least one tile must
    /// exist — otherwise `Ok(false)`. Protocol mirrors publish: create-new the
    /// new segment file + dir sync, manifest-publish the same tile list with
    /// `SegmentRef{new name, start_csn: cutoff}` via the seq+1 flip, and only
    /// after the durable flip rebind the engine's WAL to the empty segment and
    /// best-effort unlink the old one (open fds keep it alive; a crash before
    /// the flip leaves the new file an orphan that discovery ignores).
    /// Fail-closed: a target-name collision, or any post-flip rebinding failure
    /// (detached WAL), requires reopen; commits are refused while detached.
    pub fn rotate_wal(&mut self, max_wal_bytes: u64) -> io::Result<bool> {
        self.ensure_no_group("rotate_wal")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
        let Some(newest) = self.tiles.last() else { return Ok(false) };
        if self.wal.valid_len() <= max_wal_bytes {
            return Ok(false);
        }
        let cutoff = newest.cutoff();
        if cutoff != self.csn || self.mt.len() != 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput,
                "rotation requires every committed block published (newest tile cutoff == csn, empty memtable)"));
        }
        let name = format!("data-{cutoff}.wal");
        let path = self.dir.join(&name);
        let f = OpenOptions::new().read(true).write(true).create_new(true).open(&path)
            .map_err(|e| if e.kind() == io::ErrorKind::AlreadyExists {
                io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "rotation target {name} already exists (leftover from a failed rotation); remove it or recover via open_discover"))
            } else { e })?;
        f.sync_data()?;
        drop(f);
        File::open(&self.dir)?.sync_all()?;
        // The checkpoint is unchanged (same tiles, same csn/watermark); only the
        // live-segment reference moves to the born-empty file.
        let refs: Vec<TileRef> = self.tiles.iter().map(|t| t.tile_ref()).collect();
        let checkpoint = Checkpoint { csn: cutoff, handle_watermark: self.max_handle };
        let seg = SegmentRef { name: name.clone(), start_csn: cutoff };
        self.manifest_publish(refs, checkpoint, Some(seg))?;
        // Durable flip done: rebind to the empty segment, then unlink the old
        // segment best-effort.
        let old_path = self.wal_path.clone();
        // The durable root already names the new segment; a rebinding failure must
        // not let later commits append to the old binding that recovery would
        // ignore, so it detaches the writer instead of only propagating.
        //
        // (The earlier version of this match bound the Ok case with an irrefutable
        // `ok` pattern, which made the Err arm dead code: the `?` still returned the
        // error but `wal_detached` was never set. Caught by a rustc unreachable-pattern
        // warning, not by a test - hence the warnings gate.)
        let (wal, records) = match Wal::open_or_recover(&path) {
            Ok(recovered) => recovered,
            Err(e) => {
                self.wal_detached = true;
                return Err(e);
            }
        };
        if !records.is_empty() {
            self.wal_detached = true;
            return Err(io::Error::new(io::ErrorKind::InvalidData, "fresh rotation segment is not empty"));
        }
        self.wal = wal;
        self.wal_path = path;
        self.segment = Some(SegmentRef { name, start_csn: cutoff });
        self.segment_suffix_csn = 0;
        // Pre-rotation block ids die with the old segment, in memory as well:
        // keeping them would promise a dedup the next reopen will not honor.
        self.dedup.retain(|_, csn| *csn > cutoff);
        let _ = fs::remove_file(&old_path);
        Ok(true)
    }

    /// Merge every active tile into ONE new whole tile file covering the full
    /// `(0, newest tile cutoff]` CSN range, publish it through the manifest
    /// protocol, and only after the new root is durable unlink the superseded
    /// tile files. This is the whole-prefix case of the bounded merge; use
    /// [`Engine::maybe_compact`] for a tiered policy that merges only as much as
    /// a byte budget allows.
    ///
    /// **Only the newest version per logical key survives.** Reads never see
    /// historical state (2026-09-29, `docs/contracts.md` L-02 resolution), so
    /// superseded versions are unreadable garbage: the merged tile keeps the
    /// newest version of every logical key (everything before the trailing
    /// `~csn`), where a tombstone is itself the newest state and is retained.
    /// The WAL is never rotated or truncated, and the serving memtable (exactly
    /// the versions above the newest cutoff) is untouched. Compaction's purpose
    /// is reducing the per-read file count / rewrite bound.
    ///
    /// Refusals: fewer than two active tiles (`InvalidInput` — merging one tile
    /// into a fresh copy buys nothing) or a leftover candidate file with the
    /// engine-chosen name `compact-<cutoff>.tile` inside the engine directory
    /// (`AlreadyExists`, mirroring `publish_tile`'s leftover handling).
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
        self.ensure_no_group("compact")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
        if self.tiles.len() < 2 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "compaction requires at least two active tiles"));
        }
        if self.region_mode == RegionMode::PerRegion {
            // One output per region: each region's full cutoff-ordered sequence
            // collapses to a single tile, and no region merges with another.
            let mut groups: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
            for (i, t) in self.tiles.iter().enumerate() {
                groups.entry(tile_first_region(t).unwrap_or_default().to_vec()).or_default().push(i);
            }
            let mut runs: Vec<Vec<usize>> = groups.into_values().filter(|v| v.len() >= 2).collect();
            runs.sort_by_key(|v| v[0]);
            for indices in runs {
                self.merge_region_run(&indices)?;
            }
            return Ok(());
        }
        let len = self.tiles.len();
        self.merge_run(0, len)?;
        Ok(())
    }

    /// Bounded, tiered compaction: merge only a size-budgeted run instead of
    /// rewriting the whole dataset. Does nothing (`Ok(None)`) while fewer than
    /// `max_tiles` tiles are active. Otherwise it selects a CONTIGUOUS RUN of at
    /// least two tiles whose combined on-disk size is at most `max_merge_bytes`,
    /// replaces exactly that run with one tile covering its union range, and
    /// returns the merged output's size in bytes.
    ///
    /// This is [`Engine::maybe_compact_shape`] with the shipped default, [`CompactShape::CheapestPair`]:
    /// the contiguous run of length >= 2 with the SMALLEST combined size (ties:
    /// the oldest). Because sizes are positive the minimum is always attained by
    /// an adjacent pair, so a tile larger than the budget is a finished tier:
    /// any run containing it costs more than a cheaper eligible run of smaller
    /// tiles, and is never selected while that cheaper run exists. When no run
    /// fits the budget at all (every adjacent pair exceeds it), the fallback
    /// merges the two adjacent tiles with the smallest combined size, so every
    /// call still makes progress (the active count strictly drops).
    ///
    /// "Size" is the sum of the selected tiles' on-disk file lengths, stat'ed
    /// once per call. The replacement's cutoff is the run's newest cutoff and its
    /// content covers exactly the run's union range, so the surviving active list
    /// stays strictly increasing and disjoint — the invariant the read path and
    /// recovery verification depend on. Memory stays bounded: the merge reuses
    /// the streaming reduction that buffers one block per input plus the
    /// WAL-window map.
    pub fn maybe_compact(&mut self, max_merge_bytes: u64, max_tiles: usize) -> io::Result<Option<u64>> {
        self.maybe_compact_shape(max_merge_bytes, max_tiles, CompactShape::CheapestPair)
    }

    /// [`Engine::maybe_compact`] with an explicit run-selection policy, so the
    /// scale driver can measure a shape sweep at runtime without a rebuild. The
    /// invariants (contiguous run, length >= 2, union range, disjoint ranges,
    /// streaming merge) are shared by every [`CompactShape`]; only `select_run`
    /// differs.
    pub fn maybe_compact_shape(&mut self, max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> io::Result<Option<u64>> {
        if self.tiles.len() < max_tiles || self.tiles.len() < 2 {
            return Ok(None);
        }
        self.ensure_no_group("maybe_compact_shape")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
        if self.region_mode == RegionMode::PerRegion {
            let Some(indices) = self.select_region_run(max_merge_bytes, max_tiles, shape)? else {
                return Ok(None);
            };
            return self.merge_region_run(&indices).map(Some);
        }
        let (start, len) = self.select_run(max_merge_bytes, max_tiles, shape)?;
        self.merge_run(start, len).map(Some)
    }

    /// Start and length of the contiguous run to merge under `shape`. The
    /// cheapest-pair policy is the run of length >= 2 with the smallest combined
    /// on-disk size within `max_merge_bytes`, ties in favour of the oldest
    /// start; `RatioGroup` and `BudgetPrefix` seed on the smallest tile and grow
    /// an interval around it (see below), falling back to the cheapest-pair rule
    /// when they cannot reach two tiles. Every policy ends at the two adjacent
    /// tiles with the smallest combined size when nothing fits the budget, so
    /// every call makes progress. All tile sizes are read once here and every
    /// candidate run reuses them. Requires >= 2 active tiles.
    fn select_run(&self, max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> io::Result<(usize, usize)> {
        debug_assert!(self.tiles.len() >= 2);
        let sizes: Vec<u64> = self.tiles
            .iter()
            .map(|t| fs::metadata(self.dir.join(&t.name)).map(|m| m.len()))
            .collect::<io::Result<Vec<_>>>()?;
        Ok(Self::choose_run(&sizes, max_merge_bytes, max_tiles, shape))
    }

    /// The list indices of a run to merge WITHIN one region, or `None` when no
    /// region has at least two tiles. Region-partitioned compaction never merges
    /// across regions: each region's tiers are its own, so a per-region trigger
    /// is meaningful. The run is selected by the same [`CompactShape`] policy
    /// applied to that region's own cutoff-ordered sizes, and among regions the
    /// smallest combined run wins (ties: the oldest list index).
    fn select_region_run(&self, max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> io::Result<Option<Vec<usize>>> {
        let sizes: Vec<u64> = self.tiles
            .iter()
            .map(|t| fs::metadata(self.dir.join(&t.name)).map(|m| m.len()))
            .collect::<io::Result<Vec<_>>>()?;
        // A region's tiles are spread across the `(cutoff, region)`-ordered list
        // (one per cutoff group), so group by region rather than adjacency; each
        // group's indices stay in increasing-cutoff order.
        let mut groups: HashMap<Vec<u8>, Vec<usize>> = HashMap::new();
        for (i, t) in self.tiles.iter().enumerate() {
            let region = tile_first_region(t).unwrap_or_default().to_vec();
            groups.entry(region).or_default().push(i);
        }
        let mut best: Option<(u64, usize, Vec<usize>)> = None;
        for idxs in groups.values() {
            if idxs.len() < 2 { continue; }
            let region_sizes: Vec<u64> = idxs.iter().map(|&i| sizes[i]).collect();
            let (start, len) = Self::choose_run(&region_sizes, max_merge_bytes, max_tiles, shape);
            let combined: u64 = region_sizes[start..start + len].iter().sum();
            let first = idxs[start];
            let indices: Vec<usize> = idxs[start..start + len].to_vec();
            if best.as_ref().map_or(true, |(c0, f0, _)| (combined, first) < (*c0, *f0)) {
                best = Some((combined, first, indices));
            }
        }
        Ok(best.map(|(_, _, indices)| indices))
    }

    /// The run-selection core shared by [`Engine::select_run`] (whole-list) and
    /// [`Engine::select_region_run`] (one region's sizes). Returns a local start
    /// and length. Requires `sizes.len() >= 2`.
    fn choose_run(sizes: &[u64], max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> (usize, usize) {
        let n = sizes.len();
        if shape != CompactShape::CheapestPair {
            let ratio = (shape == CompactShape::RatioGroup).then_some(2u64);
            if let Some(run) = Self::grow_around_smallest(sizes, max_merge_bytes, max_tiles, ratio) {
                return run;
            }
        }
        let mut best: Option<(u64, usize, usize)> = None; // (combined size, start, len)
        for start in 0..n {
            let mut sum = 0u64;
            for len in 1..=(n - start) {
                sum = sum.saturating_add(sizes[start + len - 1]);
                if len < 2 { continue; }
                // Longer runs from this start only grow, so stop at the budget.
                if sum > max_merge_bytes { break; }
                if best.map_or(true, |(sum0, start0, _)| (sum, start) < (sum0, start0)) {
                    best = Some((sum, start, len));
                }
            }
        }
        if let Some((_, start, len)) = best {
            return (start, len);
        }
        // Nothing fits the budget; merge the two adjacent tiles with the
        // smallest combined size so the active count still drops.
        let start = (0..n - 1)
            .min_by_key(|&i| (sizes[i].saturating_add(sizes[i + 1]), i))
            .expect("choose_run requires at least two tiles");
        (start, 2)
    }

    /// Greedy interval around the globally smallest tile, the seed policy shared
    /// by `RatioGroup` and `BudgetPrefix`. Start with the seed (smallest size,
    /// ties: oldest) and repeatedly absorb the smaller eligible adjacent tile;
    /// on equal sizes prefer the older. A candidate is eligible while the running
    /// total stays within `max_merge_bytes` and, when `max_ratio` is `Some(f)`,
    /// while its size is within `f` of the seed's size (the run's smallest, since
    /// the seed is the global minimum). Growth stops when neither neighbour is
    /// eligible or the run reaches `max_tiles` inputs, so at most `max_tiles`
    /// tiles merge. `None` when the run holds fewer than two tiles, i.e. no
    /// neighbour fits, so the caller can fall back to the cheapest pair.
    fn grow_around_smallest(sizes: &[u64], max_merge_bytes: u64, max_tiles: usize, max_ratio: Option<u64>) -> Option<(usize, usize)> {
        let n = sizes.len();
        let seed = (0..n).min_by_key(|&i| (sizes[i], i))?;
        let limit = max_ratio.map_or(u64::MAX, |f| sizes[seed].saturating_mul(f));
        let mut lo = seed;
        let mut hi = seed;
        let mut total = sizes[seed];
        while hi - lo + 1 < max_tiles {
            let left = lo.checked_sub(1).map(|i| (i, sizes[i]));
            let right = (hi + 1 < n).then(|| (hi + 1, sizes[hi + 1]));
            let eligible = |c: (usize, u64)| c.1 <= limit && total.saturating_add(c.1) <= max_merge_bytes;
            let pick = match (left, right) {
                (Some(l), Some(r)) if eligible(l) && eligible(r) => {
                    Some(if (l.1, l.0) <= (r.1, r.0) { l } else { r })
                }
                (Some(l), _) if eligible(l) => Some(l),
                (_, Some(r)) if eligible(r) => Some(r),
                _ => None,
            };
            let Some((idx, size)) = pick else { break };
            total = total.saturating_add(size);
            lo = lo.min(idx);
            hi = hi.max(idx);
        }
        (hi - lo + 1 >= 2).then_some((lo, hi - lo + 1))
    }

    /// Merge the active run `tiles[start..start + len]` (`2 <= len`, with at
    /// least two tiles in the run) into one tile covering exactly the run's
    /// union range `(tiles[start - 1].cutoff, tiles[start + len - 1].cutoff]`
    /// (lower bound 0 when the run starts at the oldest tile) via the streaming
    /// merge, replace that run through the range-preserving manifest protocol,
    /// and only after the durable flip unlink the retired inputs. The run need
    /// not start at the oldest tile: a middle run leaves every tile outside it
    /// untouched, so its lower bound is the previous tile's cutoff and its
    /// coverage is exactly the newest version per logical key in that range —
    /// the same projection `open_impl` verifies each tile against. Returns the
    /// merged output size. The WAL digest binding is
    /// [`Engine::write_compacted_tile`]'s, i.e. exactly the digest an ordinary
    /// publish at the run's newest cutoff would carry.
    fn merge_run(&mut self, start: usize, len: usize) -> io::Result<u64> {
        debug_assert!(len >= 2 && start + len <= self.tiles.len());
        let end = start + len;
        let cutoff = self.tiles[end - 1].cutoff();
        let lower = if start == 0 { 0 } else { self.tiles[start - 1].cutoff() };
        // The run's newest input can itself be a prior compaction output named
        // `compact-<cutoff>.tile` (a middle run may end on a compacted tier), so
        // the deterministic base name would collide with a tile this merge
        // retires. In that case pick a fresh suffixed name; a hand-published
        // live tile under the base name is still refused below.
        let base = format!("compact-{cutoff}.tile");
        let name = if self.tiles[end - 1].name == base {
            self.unique_compaction_name(cutoff)?
        } else {
            base
        };
        let path = self.dir.join(&name);
        if self.tiles.iter().any(|t| t.name == name) {
            // A LIVE published tile owns this name; overwriting it while it is
            // also a merge input (or deleting it) would brick discovery after
            // restart. This can only arise from a hand-published colliding name.
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                format!("compaction candidate {name} is a live published tile; compaction cannot proceed under this name")));
        }
        // 1. Build + verify the merged tile for the run's union range against the
        //    retained WAL projection, exactly as publish verifies one tile.
        let inputs: Vec<&TileHandle> = self.tiles[start..end].iter().collect();
        let tile = match self.write_compacted_tile(&path, &inputs, lower, cutoff) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "compaction candidate already exists and is unreferenced by the manifest (leftover from a failed compaction or crash after tile sync); remove {} and retry", path.display())));
            }
            Err(e) => return Err(e),
        };
        let out_size = fs::metadata(&path)?.len();
        // 2. The tile's own directory entry must be durable before the manifest
        //    can reference it (checkpoint publication ordering).
        File::open(&self.dir)?.sync_all()?;
        // 3. Manifest protocol: the retired run is replaced by the one new tile.
        //    The global newest cutoff (and therefore the checkpoint) is unchanged:
        //    tiles after the run keep the frontier, or the replacement itself
        //    carries it when the run reached the end. `publish_replacing` accepts
        //    any contiguous window, not just a prefix.
        let replacement = TileRef { name: name.clone(), cutoff, digest: *tile.digest() };
        let checkpoint = Checkpoint { csn: self.tiles.last().unwrap().cutoff(), handle_watermark: self.max_handle };
        let segment = self.segment.clone();
        if self.manifest.is_some() {
            let retire: Vec<String> = self.tiles[start..end].iter().map(|t| t.name.clone()).collect();
            let retire: Vec<&str> = retire.iter().map(String::as_str).collect();
            self.manifest.as_mut().unwrap().publish_replacing(&retire, replacement, checkpoint, segment)?;
        } else {
            // No manifest yet (tiles built by `build_tile`, which never creates
            // one): create it listing the replacement interleaved in cutoff
            // order with every surviving tile, none of which is retired.
            let mut refs: Vec<TileRef> = Vec::with_capacity(self.tiles.len() - len + 1);
            refs.extend(self.tiles[..start].iter().map(|t| t.tile_ref()));
            refs.push(replacement);
            refs.extend(self.tiles[end..].iter().map(|t| t.tile_ref()));
            self.manifest_publish(refs, checkpoint, segment)?;
        }
        // 4. In-memory flip: the replacement takes the run's place in the ordered
        //    list; the memtable is untouched (it holds only versions above the
        //    global newest cutoff).
        let retired: Vec<TileHandle> = self.tiles.drain(start..end).collect();
        self.tiles.insert(start, TileHandle { tile, name });
        // 5. Only now drop the retired input files. Unlink after the durable flip
        //    is safe: open fds keep working (POSIX) and the new root no longer
        //    references them. Failures are best-effort ignored — the compaction
        //    is already durable and a leftover file is a harmless orphan that
        //    discovery ignores and never adopts.
        for old in &retired {
            let _ = fs::remove_file(self.dir.join(&old.name));
        }
        Ok(out_size)
    }

    /// Merge a run of tiles that all belong to one `(entity, key family)` region
    /// into a single replacement tile covering that region's `(lower, cutoff]`,
    /// where `lower` is the region's previous tile cutoff (0 when the run reaches
    /// the region's oldest tile) and `cutoff` is the run's newest cutoff. The
    /// replacement keeps the region's per-region invariant while leaving every
    /// other region's tiles untouched, so compaction never merges across regions.
    /// The manifest is rewritten as the full `(cutoff, region)`-ordered list with
    /// the selected tiles retired and the replacement inserted at its
    /// `(cutoff, region)` position; the durable protocol and cleanup mirror
    /// [`Engine::merge_run`].
    fn merge_region_run(&mut self, indices: &[usize]) -> io::Result<u64> {
        debug_assert!(indices.len() >= 2 && indices.windows(2).all(|w| w[0] < w[1]));
        let newest = *indices.last().unwrap();
        let cutoff = self.tiles[newest].cutoff();
        let region: Option<Vec<u8>> = tile_first_region(&self.tiles[indices[0]]).map(<[u8]>::to_vec);
        let lower = (0..indices[0])
            .rev()
            .find(|&i| tile_first_region(&self.tiles[i]).map(<[u8]>::to_vec) == region)
            .map(|i| self.tiles[i].cutoff())
            .unwrap_or(0);
        let name = self.unique_compaction_name(cutoff)?;
        let path = self.dir.join(&name);
        let inputs: Vec<&TileHandle> = indices.iter().map(|&i| &self.tiles[i]).collect();
        let tile = match self.write_compacted_tile(&path, &inputs, lower, cutoff) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "compaction candidate already exists and is unreferenced by the manifest (leftover from a failed compaction or crash after tile sync); remove {} and retry", path.display())));
            }
            Err(e) => return Err(e),
        };
        let out_size = fs::metadata(&path)?.len();
        File::open(&self.dir)?.sync_all()?;
        let retired: HashSet<usize> = indices.iter().copied().collect();
        let replacement = TileRef { name: name.clone(), cutoff, digest: *tile.digest() };
        let checkpoint = Checkpoint { csn: self.tiles.last().unwrap().cutoff(), handle_watermark: self.max_handle };
        let segment = self.segment.clone();
        // The replacement's position in the `(cutoff, region)`-ordered list: the
        // count of surviving tiles that sort before it.
        let pos = self.tiles.iter().enumerate()
            .filter(|(i, t)| !retired.contains(i)
                && (t.cutoff(), tile_first_region(t).map(<[u8]>::to_vec)) < (cutoff, region.clone()))
            .count();
        let mut refs: Vec<TileRef> = Vec::with_capacity(self.tiles.len() - indices.len() + 1);
        for (i, t) in self.tiles.iter().enumerate() {
            if !retired.contains(&i) { refs.push(t.tile_ref()); }
        }
        refs.insert(pos, replacement);
        self.manifest_publish(refs, checkpoint, segment)?;
        // In-memory flip: rebuild the ordered list with the replacement inserted.
        let mut new_tile = Some(tile);
        let mut kept: Vec<TileHandle> = Vec::with_capacity(self.tiles.len() - indices.len() + 1);
        for (i, t) in self.tiles.drain(..).enumerate() {
            if retired.contains(&i) { continue; }
            if kept.len() == pos {
                kept.push(TileHandle { tile: new_tile.take().expect("replacement inserted once"), name: name.clone() });
            }
            kept.push(t);
        }
        if let Some(t) = new_tile { kept.push(TileHandle { tile: t, name }); }
        self.tiles = kept;
        Ok(out_size)
    }

    /// A free compaction output name: `compact-<cutoff>.tile` when no live tile
    /// or on-disk file claims it, otherwise the first free
    /// `compact-<cutoff>-<k>.tile`. Needed because a middle run may end on a
    /// previous compaction output whose name already is the base name.
    fn unique_compaction_name(&self, cutoff: u64) -> io::Result<String> {
        let mut candidate = format!("compact-{cutoff}.tile");
        let mut k = 0u64;
        loop {
            let live = self.tiles.iter().any(|t| t.name == candidate);
            if !live && !self.dir.join(&candidate).try_exists()? {
                return Ok(candidate);
            }
            k += 1;
            candidate = format!("compact-{cutoff}-{k}.tile");
        }
    }

    /// Active tile cutoffs in order (inspection/benchmark hook). Non-decreasing
    /// (a partitioned publish shares one cutoff across its region tiles); the
    /// `(cutoff, region)` pair is strictly increasing, which is what makes the
    /// regions disjoint.
    pub fn active_tile_cutoffs(&self) -> Vec<u64> {
        self.tiles.iter().map(|t| t.cutoff()).collect()
    }

    /// Create-new the compacted tile for the `(lower, cutoff]` union of the
    /// given active tile run, sync it, reopen it and re-check its exact
    /// newest-version-per-logical-key projection against the active WAL file's
    /// `(lower, cutoff]` range combined with the already-verified tile-carried
    /// prefix, before any caller may reference it. Each input tile was itself
    /// verified against its disjoint WAL range at open/publish, so merging the
    /// run's tiles with the retained WAL suffix and reducing to the newest
    /// version per logical key IS the multi-tile merge (same codec, same key
    /// order, tombstones retained as newest state). The run covers exactly
    /// `(lower, cutoff]`: `lower` is the previous active tile's cutoff (0 for a
    /// run starting at the oldest tile) and `cutoff` is the run's newest cutoff,
    /// so the digest is exactly what a publish at `cutoff` would bind. The WAL
    /// suffix map holds only records above `lower`, so a version outside the
    /// run's range is never pulled into the output.
    ///
    /// Nothing here materializes the union: the tiles are streamed by one
    /// [`TileCursor`] each (borrowing a single 4 KiB block apiece) and only the
    /// WAL suffix - already bounded by the rotation window - is copied into a
    /// key-ordered map. [`Engine::stream_merged_reduction`] merges both sources
    /// and pushes the reduced stream straight into the streaming writer, so peak
    /// compaction RAM tracks the WAL window, not the total tile bytes.
    fn write_compacted_tile(&self, tile_path: &Path, tiles: &[&TileHandle], lower: u64, cutoff: u64) -> io::Result<Tile> {
        let (records, _) = wal::replay(&self.wal_path)?;
        self.verify_wal_matches_state(&records)?;
        let digest = wal_digest(&records, cutoff, self.wal_floor())?;
        // Source (b): the retained WAL suffix inside `(lower, cutoff]`, key-ordered
        // by construction. A later op overwrites an earlier one for the exact same
        // key, matching `Memtable::apply`. Records at or below `lower` are carried
        // by the run's own tiles (or an earlier, still-active tile) and must not be
        // merged here, or they would escape the run's range.
        let mut wal_entries: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for payload in &records {
            let (csn, _, ops) = decode_payload(payload)?;
            if csn > lower && csn <= cutoff {
                for op in &ops {
                    let (key, value) = op_kv(op, csn);
                    wal_entries.insert(key, value);
                }
            }
        }
        // Source (a): the run's tiles, streamed. Merge + reduce runs straight
        // into the encoder (one 4 KiB block buffered): no projection of the
        // union ever exists.
        // Bloom over exactly the unique prefixes the merge stores, sized from a
        // known upper bound on the reduced stream: the input tiles' stored-entry
        // counts plus the WAL suffix map length. No second pass and no retained
        // projection, so the merge stays single and streaming; only U keys are
        // inserted, so the filter is over-provisioned w.r.t. its `bits_per_key`.
        let expected: u64 = tiles
            .iter()
            .fold(wal_entries.len() as u64, |acc, t| acc.saturating_add(t.tile.count()));
        let layout_region = match (self.region_mode, tiles.first().and_then(|t| t.tile.first_key()).and_then(keys::region_prefix)) {
            (RegionMode::PerRegion, Some(region)) => keys::region_name(region),
            _ => TILE_LAYOUT_REGION.to_string(),
        };
        let (page_size, bloom_bits_per_key) = self.layout.for_region(&layout_region);
        let mut bloom = (bloom_bits_per_key > 0)
            .then(|| BloomBuilder::new(expected, bloom_bits_per_key as u32));
        let mut writer = TileWriter::new(tile_path, cutoff, &digest, page_size)?;
        self.stream_merged_reduction(tiles, lower, cutoff, &wal_entries, |key, value| {
            writer.push(key, value)?;
            if let Some(bloom) = bloom.as_mut() {
                if let Some(prefix) = unique_prefix_of(key) { bloom.insert(prefix); }
            }
            Ok(())
        })?;
        if let Some(bloom) = bloom { writer.set_bloom(bloom.finish())?; }
        let tile = writer.finish()?;
        // Fail closed: re-run the identical merge and compare it 1:1, streaming,
        // against the written tile. That catches encoder/streaming defects but is
        // self-referential (same reduction, same inputs), so it is paired with an
        // INDEPENDENT check against the WAL-derived expectation below.
        self.verify_compacted_tile(&tile, tiles, lower, &wal_entries, cutoff)?;
        // Independent verification of everything above the rotation floor: the
        // retained WAL suffix is the authority for versions in (floor, cutoff], so
        // compare the written tile against it. This restores the check that the
        // old materialized projection provided, at the cost of one bounded
        // (window-sized) expectation instead of an all-tiles one.
        //
        // Entries at or below the floor have no retained WAL behind them - they
        // were verified when the floor-spanning tile was first published and can
        // never be re-derived - so the comparison starts at the floor. That is the
        // documented trust boundary (see FDBTILE2.md/FDBTILE3.md), stated here so
        // the gap is visible at the call site rather than implied.
        self.verify_compacted_against_wal(&tile, &wal_entries, lower.max(self.wal_floor()), cutoff)?;
        Ok(tile)
    }

    /// K-way merge of the given active tile run (one [`TileCursor`] each) and the
    /// key-ordered WAL suffix map, in ascending encoded-key order, reduced to the
    /// newest version per logical key within `(lower, cutoff]` - the same
    /// reduction [`newest_projection`] performs. Keys within one logical key sort
    /// by CSN descending, so the first in-range entry of each logical-key group
    /// wins and the rest are skipped; a tombstone (empty value) is itself the
    /// newest state. Entries at or below `lower` cannot belong to the run's
    /// range and are dropped (a run tile may carry floor-spanning entries below
    /// its lower bound). Only one logical key is retained as state, so nothing
    /// here is proportional to the tile bytes.
    fn stream_merged_reduction(
        &self,
        tiles: &[&TileHandle],
        lower: u64,
        cutoff: u64,
        wal_entries: &BTreeMap<Vec<u8>, Vec<u8>>,
        mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut cursors: Vec<TileCursor<'_>> = Vec::with_capacity(tiles.len());
        for t in tiles {
            // Empty prefix = the whole tile, still borrowed and block-streamed.
            cursors.push(t.tile.prefix_cursor(b"")?);
        }
        let mut wal_iter = wal_entries.iter().peekable();
        let mut disk_key: Vec<u8> = Vec::new();
        let mut have_logical = false;
        let mut last_logical: Vec<u8> = Vec::new();
        loop {
            let disk = smallest_disk_head(&mut cursors, &mut disk_key)?;
            let use_wal = match (disk, wal_iter.peek()) {
                (None, None) => break,
                (None, Some(_)) => true,
                (Some(_), None) => false,
                // Equal keys are the same logical key+CSN; prefer the WAL, the
                // recovery authority (a verified tile entry carries the same value).
                (Some(_), Some((wk, _))) => wk.as_slice() <= disk_key.as_slice(),
            };
            let (key, value): (&[u8], &[u8]) = if use_wal {
                let (k, v) = wal_iter.next().unwrap();
                (k.as_slice(), v.as_slice())
            } else {
                let i = disk.unwrap();
                match cursors[i].next()? {
                    Some(entry) => entry,
                    None => return Err(io::Error::new(io::ErrorKind::InvalidData, "tile cursor lost its head")),
                }
            };
            let Some(csn) = keys::key_csn(key) else { continue };
            if csn <= lower || csn > cutoff { continue; }
            let logical = &key[..key.len() - 8];
            if have_logical && last_logical.as_slice() == logical { continue; }
            have_logical = true;
            last_logical.clear();
            last_logical.extend_from_slice(logical);
            visit(key, value)?;
        }
        Ok(())
    }

    /// Stream the written tile against the merged reduction and require an exact
    /// 1:1 match: every reduced entry must appear in key order with the same
    /// value, and any extra tile entry is an error.
    fn verify_compacted_tile(
        &self,
        tile: &Tile,
        tiles: &[&TileHandle],
        lower: u64,
        wal_entries: &BTreeMap<Vec<u8>, Vec<u8>>,
        cutoff: u64,
    ) -> io::Result<()> {
        let mut actual = tile.entries();
        let mut pending = actual.next();
        let mut compare = |key: &[u8], value: &[u8]| -> io::Result<()> {
            let Some(entry) = pending.take() else {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile is missing an entry"));
            };
            let (akey, avalue) = entry?;
            if akey.as_slice() != key {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile entry key disagrees with the merged projection"));
            }
            if avalue.as_slice() != value {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile entry value disagrees with the merged projection"));
            }
            pending = actual.next();
            Ok(())
        };
        self.stream_merged_reduction(tiles, lower, cutoff, wal_entries, &mut compare)?;
        if let Some(entry) = pending {
            entry?;
            return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile contains an extra entry"));
        }
        Ok(())
    }

    /// Independent, streaming check of a compacted tile against the WAL-derived
    /// expectation for `(lower, cutoff]`: the retained WAL suffix is the authority
    /// for versions above the rotation floor, so this does NOT re-run the merge
    /// that wrote the tile (the self-referential check `verify_compacted_tile`
    /// does that) and it holds no extra materialization - it walks the tile's
    /// cursor positionally against the already-built `wal_entries` map, skipping
    /// tile entries at or below `lower` (below-floor content cannot be re-derived
    /// and was verified when the floor-spanning tile was first published; see the
    /// trust boundary in FDBTILE2.md/FDBTILE3.md).
    fn verify_compacted_against_wal(
        &self,
        tile: &Tile,
        wal_entries: &BTreeMap<Vec<u8>, Vec<u8>>,
        lower: u64,
        cutoff: u64,
    ) -> io::Result<()> {
        let mut actual = tile.entries();
        // The map holds raw ops, so several versions of one logical key can be
        // present while the tile keeps only the newest; keys sort CSN-descending
        // within a logical key, so the first in-range row of a group is the one the
        // tile must carry and later rows are skipped.
        let mut last_logical: Option<Vec<u8>> = None;
        for (key, value) in wal_entries {
            let Some(csn) = keys::key_csn(key) else { continue };
            if csn <= lower || csn > cutoff { continue; }
            let logical = &key[..key.len() - 8];
            if last_logical.as_deref() == Some(logical) { continue; }
            last_logical = Some(logical.to_vec());
            // Advance past tile entries at or below `lower`: below-floor content
            // cannot be re-derived from the retained WAL and was verified when the
            // floor-spanning tile was first published (see the trust boundary).
            let mut found = None;
            while let Some(entry) = actual.next() {
                let (akey, avalue) = entry?;
                if keys::key_csn(&akey).is_some_and(|acsn| acsn <= lower) { continue; }
                found = Some((akey, avalue));
                break;
            }
            let (akey, avalue) = found.ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "compacted tile is missing a WAL entry")
            })?;
            if akey.as_slice() != key.as_slice() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile entry key disagrees with the WAL projection"));
            }
            if avalue.as_slice() != value.as_slice() {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile entry value disagrees with the WAL projection"));
            }
        }
        while let Some(entry) = actual.next() {
            let (akey, _) = entry?;
            if keys::key_csn(&akey).is_none_or(|csn| csn > lower) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "compacted tile contains an entry absent from the WAL projection"));
            }
        }
        Ok(())
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

    /// Install the provisional layout policy the two tile write paths (publish and
    /// compaction) consult. The default is [`LayoutPolicy::declared_default`].
    /// Because the policy is not persisted, setting it affects only tiles written
    /// after this call; every existing tile keeps the parameters in its own
    /// superblock and is read on those terms.
    pub fn set_layout_policy(&mut self, policy: LayoutPolicy) { self.layout = policy; }

    /// The engine's current layout policy (inspection/measurement hook).
    pub fn layout_policy(&self) -> &LayoutPolicy { &self.layout }

    /// Select the tile write mode. [`RegionMode::Global`] (the default) writes
    /// one whole-keyspace tile per publish/compaction output exactly as before
    /// slice 24; [`RegionMode::PerRegion`] writes one tile per `(entity, key
    /// family)` region so a per-region [`LayoutPolicy`] has something to act on.
    /// The choice is not persisted: recovery derives each tile's region from the
    /// tile itself, so both layouts reopen under either mode.
    pub fn set_region_mode(&mut self, mode: RegionMode) { self.region_mode = mode; }

    /// The engine's current tile write mode (inspection/measurement hook).
    pub fn region_mode(&self) -> RegionMode { self.region_mode }

    // K-way merge of the serving memtable and every published tile, all
    // traversed in global key order. Keys encode CSN descending, so each logical
    // key's versions arrive newest-first and the caller's first visible entry
    // wins — the only state reads observe (2026-09-29: no historical reads).
    // Tile CSN ranges are disjoint, so cross-tier equal keys are impossible;
    // the memtable-wins skip below is defensive only.
    fn scan_merged(&self, prefix: &[u8], mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<bool>) -> io::Result<()> {
        let mut mem = self.mt.scan_iter(prefix).peekable();
        // The bloom reject below is only sound for a full unique prefix, so it is
        // gated once per scan on the prefix shape; every other probe (primary,
        // reverse, partial, whole-range) stays on the exact path.
        let unique_prefix = is_unique_prefix(prefix);
        let mut cursors: Vec<TileCursor<'_>> = Vec::with_capacity(self.tiles.len());
        for t in &self.tiles {
            // Conservative fast reject: a tile whose cached key range cannot hold
            // `prefix` is skipped without even a directory read, let alone the
            // cursor and its 4 KiB block buffer. Append-mostly keys (ascending
            // order numbers) reject every older tile here, which is the common
            // case and the measured bottleneck this filter targets.
            if !t.tile.could_contain_prefix(prefix) { continue; }
            // In-range membership reject: the tile's persisted bloom is keyed by
            // the unique prefixes it stored, so `false` is a proof this tile
            // holds no key with this prefix, answered from RAM. A tile with no
            // bloom answers `true`.
            if unique_prefix && !t.tile.might_contain_unique_prefix(prefix) { continue; }
            cursors.push(t.tile.prefix_cursor(prefix)?);
        }
        let mut disk_key: Vec<u8> = Vec::new();
        loop {
            let disk = smallest_disk_head(&mut cursors, &mut disk_key)?;
            let mem_head = mem.peek();
            if mem_head.is_none() && disk.is_none() { break; }
            let use_mem = match (mem_head, disk) {
                (Some((mk, _)), Some(_)) => *mk <= disk_key.as_slice(),
                (Some(_), None) => true,
                (None, _) => false,
            };
            if use_mem {
                let (key, value) = mem.next().unwrap();
                if let Some(i) = disk {
                    // Tile CSN ranges are disjoint, so an equal key is defensive only.
                    if disk_key.as_slice() == key {
                        cursors[i].next()?;
                    }
                }
                if !visit(key, value)? { return Ok(()); }
            } else {
                let i = disk.unwrap();
                let (key, value) = match cursors[i].next()? {
                    Some(entry) => entry,
                    None => return Err(io::Error::new(io::ErrorKind::InvalidData, "tile cursor lost its head")),
                };
                if !visit(key, value)? { return Ok(()); }
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

    /// Enable or disable per-pattern latency telemetry. Disabled is a true no-op
    /// on every read path and on `commit_block`, so it is the baseline for
    /// measuring the telemetry's overhead; nothing in the engine depends on
    /// metrics either way.
    pub fn set_metrics_enabled(&mut self, on: bool) {
        self.metrics.borrow_mut().set_enabled(on);
    }

    /// Whether per-pattern latency telemetry is currently recording.
    pub fn metrics_enabled(&self) -> bool {
        self.metrics.borrow().enabled()
    }

    /// Plain, printable summary of the current observation window.
    pub fn metrics_snapshot(&self) -> metrics::WindowSnapshot {
        self.metrics.borrow().snapshot()
    }

    /// Roll the observation window: the finished window becomes the previous
    /// window (at most one is retained) and every counter clears. Returns the
    /// finished window. Off the request path; allocates only the snapshot.
    pub fn metrics_rotate_window(&self) -> metrics::WindowSnapshot {
        self.metrics.borrow_mut().rotate_window().clone()
    }

    /// The most recently rotated window, if any.
    pub fn metrics_previous_window(&self) -> Option<metrics::WindowSnapshot> {
        self.metrics.borrow().previous_window().cloned()
    }

    /// Start a latency sample only when telemetry is on, so the disabled path
    /// pays no clock read.
    #[inline]
    fn timing(&self) -> Option<Instant> {
        self.metrics.borrow().enabled().then(Instant::now)
    }

    /// Whether a block was published (including after WAL recovery).
    pub fn committed_block_csn(&self, block_id: &[u8]) -> Option<u64> {
        self.dedup.get(block_id).copied()
    }

    /// 反向查找：当前指向 `target` 的全部 source 句柄（最新提交状态，无历史读）。
    /// tombstone（空 value）剔除。
    pub fn reverse_lookup(&self, entity: &[u8], field: &[u8], target: u64) -> io::Result<Vec<u64>> {
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_reverse(entity, field);
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
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
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_unique(entity, field);
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
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
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_primary(entity);
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
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
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_scan(entity);
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
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

    /// Full merged key-order pass over every serving key (primary, unique and
    /// reverse). Inspection/benchmark hook for read throughput, not part of the
    /// product read surface; `visit` returns `false` to stop early.
    pub fn scan_all(
        &self,
        mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.scan_merged(&[], |key, value| {
            visit(key, value)?;
            Ok(true)
        })
    }

    /// 原子提交一个块。唯一约束冲突 → `Outcome::Conflict`（未写 WAL，调用方直接报业务错误）。
    pub fn commit_block(&mut self, block_id: &[u8], ops: &[Op]) -> io::Result<Outcome> {
        self.ensure_no_group("commit_block")?;
        self.commit_block_inner(block_id, ops, true)
    }

    /// Whether a deferred-barrier commit group is currently open (slice 23).
    /// The request executor routes a block through
    /// [`Engine::commit_block_grouped`] when this is true, so a request executed
    /// inside a group shares the group's single barrier; every other caller of
    /// [`Engine::commit_block`] still fails closed during a group.
    pub fn group_open(&self) -> bool { self.group_open }

    /// Open a deferred-barrier commit group (slice 23).
    ///
    /// Inside the group [`Engine::commit_block_grouped`] validates, WAL-appends
    /// and applies each block in order, so every later probe/guard sees every
    /// earlier member exactly as a sequence of single commits would; the single
    /// `fdatasync` for the whole group is taken by [`Engine::end_group`].
    ///
    /// Durability rule, stated plainly: the barrier in `end_group` is the
    /// group's ONLY durability point. Before it returns `Ok` nothing in the
    /// group is acknowledged, and a crash before the barrier can lose the whole
    /// group - the group is one durability unit, coarser than the per-block
    /// durability of [`Engine::commit_block`]. Because no member was acked, no
    /// acknowledged work is lost. A conflict inside the group is per-block (like
    /// a single commit), never a whole-group rejection: the conflicting block
    /// returns [`Outcome::Conflict`], is not appended, and later grouped commits
    /// continue.
    ///
    /// While a group is open every other mutating entry point (`commit_block`,
    /// `commit_batch`, `build_tile`, `publish_tile`, `rotate_wal`, `compact`,
    /// `maybe_compact_shape`) refuses; `begin_group` twice also refuses. A
    /// caller that forgets `end_group` therefore fails closed instead of
    /// silently committing through a path its barrier never covered.
    pub fn begin_group(&mut self) -> io::Result<()> {
        self.ensure_attached()?;
        if self.group_open {
            return Err(io::Error::new(io::ErrorKind::Other, "a commit group is already open"));
        }
        self.group_open = true;
        self.group_pending = false;
        Ok(())
    }

    /// Commit one block inside an open group: identical to [`Engine::commit_block`]
    /// except the WAL `fdatasync` is deferred to [`Engine::end_group`]. Uniqueness
    /// is still evaluated against the current (already group-updated) state, so a
    /// conflict is a per-block [`Outcome::Conflict`] and nothing was appended.
    /// Fails closed when no group is open.
    pub fn commit_block_grouped(&mut self, block_id: &[u8], ops: &[Op]) -> io::Result<Outcome> {
        if !self.group_open {
            return Err(io::Error::new(io::ErrorKind::Other, "no commit group is open; begin_group first"));
        }
        // Any attempt owes a barrier: a failed append poisons the WAL and must
        // surface in `end_group`'s sync, not be silently skipped as empty.
        self.group_pending = true;
        self.commit_block_inner(block_id, ops, false)
    }

    /// Take the group's single WAL barrier.
    ///
    /// **Durability semantics, stated precisely** (the earlier "the group is one durability unit"
    /// wording was wrong): each grouped commit is appended and applied immediately, so a crash
    /// before this barrier can leave a durable PREFIX of the group - recovery replays every
    /// complete, CRC-valid frame. That is exactly the sequential reference semantics: a sequence
    /// of single commits would also have made the earlier blocks durable and lost the later ones,
    /// and nothing in the group was acknowledged, so no acknowledged work is lost and a client
    /// retry is idempotent through its business unique key. What is NOT claimed: whole-group
    /// atomicity across a crash. A true atomic multi-block group would need an explicit group-end
    /// marker that recovery requires before admitting the group (a future feature; the request
    /// path does not need it and would be *less* faithful to the sequential reference with it).
    ///
    /// On a partial append failure inside the group the WAL writer is poisoned exactly as in the
    /// single-block path, so this `sync` fails and the group is never acknowledged; the caller
    /// must reopen to recover. Fails closed when no group is open (so `end_group` twice fails).
    /// Clears the group even when the barrier fails, because the group is over and the poisoned
    /// writer already refuses further commits until reopen.
    pub fn end_group(&mut self) -> io::Result<()> {
        if !self.group_open {
            return Err(io::Error::new(io::ErrorKind::Other, "no commit group is open; begin_group first"));
        }
        let result = if self.group_pending { self.wal.sync() } else { Ok(()) };
        self.group_open = false;
        self.group_pending = false;
        result
    }

    /// Shared single-block commit core. `barrier` selects whether the WAL
    /// `fdatasync` is taken before the block is published: the single-block path
    /// passes `true` (unchanged observable behaviour), the deferred-barrier group
    /// path passes `false` and relies on [`Engine::end_group`] for the one sync.
    fn commit_block_inner(&mut self, block_id: &[u8], ops: &[Op], barrier: bool) -> io::Result<Outcome> {
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_commit();
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
        self.ensure_attached()?;
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
                // Fail closed: a key under the unique prefix that cannot be
                // decoded cannot be proven to have no other live owner, so
                // silently skipping it would fail *open* in a constraint check.
                if key.len() != pfx.len() + 16 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed unique key in merged view"));
                }
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
        if barrier {
            self.wal.sync()?;
        }

        for op in ops {
            apply_op(&mut self.mt, op, csn);
        }
        self.csn = csn;
        // The synced record is now the newest content of the active WAL file
        // (in segment mode too), keeping the republish guard's comparison
        // target in lockstep with the file.
        self.segment_suffix_csn = csn;
        self.max_handle = self.max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
        self.dedup.insert(block_id.to_vec(), csn);
        Ok(Outcome::Committed { csn })
    }

    /// Atomically commit many blocks with ONE WAL barrier.
    ///
    /// Uniqueness is validated against the batch's **projected post-state**: one
    /// overlay collects the net unique effect of every new block in input order
    /// (later ops win, exactly as [`Memtable::apply`] leaves them), and is merged
    /// once per touched unique prefix over the published owners. At most one live
    /// owner per prefix is required, so "block A releases value v and block B
    /// takes it" inside one batch is legal, exactly as inside one block today;
    /// two blocks leaving two live owners for one prefix is a conflict.
    ///
    /// **Conflict rule: whole-batch rejection.** Any conflict rejects the entire
    /// batch before a record is appended, so [`BatchOutcome::Conflict`] means
    /// nothing durable (the WAL did not grow and no CSN advanced). This is chosen
    /// over "skip the conflicting block while the rest commit" because a unique
    /// violation is a terminal business error for its block, not an `else`: the
    /// batch is a single durability unit, and silently absorbing one member would
    /// make the durable set depend on which block lost, i.e. on evaluation order.
    ///
    /// The durable bytes are byte-for-byte what committing the same new blocks
    /// one at a time would leave: one record per block in input order, each with
    /// its own CSN and the same framing, taking ONE `fdatasync` instead of one per
    /// block.
    ///
    /// The promise is bounded on purpose: a `Conflict` returns with nothing appended,
    /// but like every append path, if the single `fdatasync` itself fails then complete
    /// frames may already be in the filesystem, and recovery replays what is complete.
    /// The caller gets `Err`, so nothing is acknowledged and a retry is idempotent
    /// through the business unique key - the same rule the deferred-barrier group path
    /// states. A block already in `dedup` (an idempotent retry) returns its existing
    /// CSN, is not re-appended, and contributes no overlay entry - its effect is
    /// already in the published state, so re-including it could only reject a batch
    /// the sequential order accepts. CSN, `segment_suffix_csn`, `dedup` and the
    /// handle watermark advance exactly as a sequence of single commits would. An
    /// empty batch is a no-op.
    pub fn commit_batch(&mut self, blocks: &[(Vec<u8>, Vec<Op>)]) -> io::Result<BatchOutcome> {
        self.ensure_no_group("commit_batch")?;
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_commit();
        let _sample = MetricsSample { metrics: &self.metrics, id, started: t0 };
        self.ensure_attached()?;

        let mut overlay: HashMap<Vec<u8>, BatchUnique> = HashMap::new();
        let mut outcomes: Vec<Outcome> = Vec::with_capacity(blocks.len());
        // block_id -> CSN for blocks newly committed within this batch, so a
        // block_id repeated inside the batch replays its first occurrence.
        let mut seen: HashMap<&[u8], u64> = HashMap::new();
        // (csn, block_id, ops) of the blocks that will be appended, in order.
        let mut pending: Vec<(u64, &[u8], &[Op])> = Vec::new();
        let mut last_csn = self.csn;
        for (index, (block_id, ops)) in blocks.iter().enumerate() {
            if block_id.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty block_id"));
            }
            if let Some(&csn) = self.dedup.get(block_id).or_else(|| seen.get(block_id.as_slice())) {
                outcomes.push(Outcome::AlreadyCommitted { csn });
                continue;
            }
            // Validate every key name before the barrier (so `apply_op`'s unwraps
            // can never fire after a successful sync) and build the batch overlay.
            for op in ops {
                match op {
                    Op::PutDoc { entity, doc, .. } => {
                        check_name(entity)?;
                        if doc.is_empty() {
                            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty document"));
                        }
                    }
                    Op::DelDoc { entity, .. } => { check_name(entity)?; }
                    Op::PutReverse { entity, field, .. } | Op::DelReverse { entity, field, .. } => {
                        check_name(entity)?;
                        check_name(field)?;
                    }
                    Op::PutUnique { entity, field, value, handle }
                    | Op::DelUnique { entity, field, value, handle } => {
                        let pfx = keys::unique_prefix(entity, field, value)?;
                        let entry = overlay.entry(pfx).or_insert_with(|| BatchUnique {
                            entity: entity.clone(),
                            field: field.clone(),
                            value: value.clone(),
                            owners: HashMap::new(),
                            last_block: index,
                        });
                        entry.owners.insert(*handle, matches!(op, Op::PutUnique { .. }));
                        entry.last_block = index;
                    }
                }
            }
            let csn = last_csn.checked_add(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CSN exhausted"))?;
            last_csn = csn;
            seen.insert(block_id.as_slice(), csn);
            pending.push((csn, block_id.as_slice(), ops.as_slice()));
            outcomes.push(Outcome::Committed { csn });
        }

        // Uniqueness against the projected post-state: the published owners of
        // each touched prefix, overlaid with the batch's own net effect, must
        // leave at most one live owner.
        for (pfx, entry) in overlay {
            let mut live: HashMap<u64, bool> = HashMap::new();
            self.scan_merged(&pfx, |key, val| {
                if key.len() != pfx.len() + 16 {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed unique key in merged view"));
                }
                let handle = u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap());
                live.entry(handle).or_insert(!val.is_empty());
                Ok(true)
            })?;
            live.extend(entry.owners);
            if live.values().filter(|&&alive| alive).take(2).count() > 1 {
                return Ok(BatchOutcome::Conflict {
                    index: entry.last_block,
                    conflict: Conflict { kind: "x_unique", entity: entry.entity, field: entry.field, value: entry.value },
                });
            }
        }

        if pending.is_empty() {
            // Every block was an idempotent retry: nothing to append or sync.
            return Ok(BatchOutcome::Committed(outcomes));
        }
        // Append every record, take the single barrier, then publish.
        for &(csn, block_id, ops) in &pending {
            let payload = encode_payload(csn, block_id, ops);
            self.wal.append(&payload)?;
        }
        self.wal.sync()?;
        for &(csn, _block_id, ops) in &pending {
            for op in ops {
                apply_op(&mut self.mt, op, csn);
            }
            self.max_handle = self.max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
        }
        self.csn = last_csn;
        // The last synced record is the active WAL file's newest content (segment
        // mode too), keeping the republish guard's comparison target in lockstep.
        self.segment_suffix_csn = last_csn;
        for &(csn, block_id, _) in &pending {
            self.dedup.insert(block_id.to_vec(), csn);
        }
        Ok(BatchOutcome::Committed(outcomes))
    }
}

// Index of the smallest pending tile head, if any; linear over at most
// manifest::MAX_TILES cursors. The winning key is copied into `best_key` (one
// reusable buffer) so no cursor's borrowed peek is held across another cursor's.
fn smallest_disk_head(cursors: &mut [TileCursor<'_>], best_key: &mut Vec<u8>) -> io::Result<Option<usize>> {
    best_key.clear();
    let mut best: Option<usize> = None;
    for i in 0..cursors.len() {
        if let Some((key, _)) = cursors[i].peek()? {
            if best.is_none() || key < best_key.as_slice() {
                best_key.clear();
                best_key.extend_from_slice(key);
                best = Some(i);
            }
        }
    }
    Ok(best)
}

// Bind a tile to the exact WAL range which produced it; the WAL remains the only
// recovery authority. Length delimiters prevent ambiguous concatenation of records.
// `floor` is the segment start CSN in segment mode: records below it are retired
// (their state lives in tiles + manifest checkpoint), so the digest covers only
// `(floor, cutoff]` and the coverage requirement is relaxed to `cutoff >
// max(last, floor)` — with floor 0 this is exactly the full-WAL check.
fn wal_digest(records: &[Vec<u8>], cutoff: u64, floor: u64) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut last = 0u64;
    for payload in records {
        let (csn, _, _) = decode_payload(payload)?;
        wal_digest_record(&mut hash, &mut last, payload, csn, cutoff)?;
    }
    wal_digest_finish(hash, last, cutoff, floor)
}

/// Fold one record into the running digest and CSN-ordering check. Shared by the
/// collected-record callers and the streaming publish path, so both bind exactly
/// the same bytes and reject reordering identically.
fn wal_digest_record(hash: &mut Sha256, last: &mut u64, payload: &[u8], csn: u64, cutoff: u64) -> io::Result<()> {
    if csn == 0 || csn <= *last {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL CSN reordering"));
    }
    if csn <= cutoff {
        hash.update((payload.len() as u64).to_le_bytes());
        hash.update(payload);
    }
    *last = csn;
    Ok(())
}

fn wal_digest_finish(hash: Sha256, last: u64, cutoff: u64, floor: u64) -> io::Result<[u8; 32]> {
    if cutoff > last.max(floor) { return Err(io::Error::new(io::ErrorKind::InvalidData, "tile cutoff exceeds WAL")); }
    Ok(hash.finalize().into())
}

// Directory a manifest or tile name resolves against; empty parents mean ".".
fn parent_dir(path: &Path) -> PathBuf {
    path.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// One tile emitted by a write path: the plain filename the manifest will
/// reference it under, and the opened, verified tile.
struct EmittedTile {
    name: String,
    tile: Tile,
}

/// The plain filename a tile path resolves to.
fn tile_name(path: &Path) -> String {
    path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string()
}

/// The filename for the `index`-th region tile of a partitioned write, derived
/// from the caller's base name (`tile-9.tile` -> `tile-9.r0.tile`). Region
/// identity is derived from each tile's own first key at recovery, so the name
/// only needs to be unique and a valid plain filename, never reversible.
fn region_tile_path(base: &Path, index: usize) -> PathBuf {
    let name = tile_name(base);
    match name.rsplit_once('.') {
        Some((stem, ext)) => base.with_file_name(format!("{stem}.r{index}.{ext}")),
        None => base.with_file_name(format!("{name}.r{index}")),
    }
}

/// The `(entity, key family)` region a tile belongs to, taken from its first
/// stored key. `None` for an empty tile.
fn tile_first_region(t: &TileHandle) -> Option<&[u8]> {
    t.tile.first_key().and_then(keys::region_prefix)
}

/// `Some(region)` when the tile covers exactly one region (its first and last
/// keys share a region prefix, i.e. a region-partitioned tile), `None` for a
/// whole-keyspace tile or an empty one. Recovery uses this to pick a per-region
/// `lower` bound; a multi-region tile keeps the global one.
fn tile_single_region(t: &TileHandle) -> Option<&[u8]> {
    let first = tile_first_region(t)?;
    (t.tile.last_key().and_then(keys::region_prefix) == Some(first)).then_some(first)
}

/// The unique-index prefix a stored key belongs to, or `None` for a non-`U`
/// key. The prefix is the key up to and including its value terminator - exactly
/// the bytes `keys::unique_prefix(entity, field, value)` produces, i.e. without
/// the trailing 8-byte handle and 8-byte `~csn`. A bloom built from these strings
/// is safe to probe for a full unique prefix: a `false` can never hide a stored
/// key that a probe for that prefix would match.
fn unique_prefix_of(key: &[u8]) -> Option<&[u8]> {
    if key.first() != Some(&keys::U) || key.len() < 17 { return None; }
    Some(&key[..key.len() - 16])
}

/// Whether `prefix` is exactly a full unique-index prefix as
/// [`keys::unique_prefix`] spells it (`U | entity | 0 | field | 0 | len(4 BE) |
/// value`, nonempty entity/field, `len` matching the value bytes). The
/// probe-path bloom reject is gated on this: a primary/reverse prefix or a
/// partial unique prefix was never inserted into a tile's bloom, so testing it
/// there could answer `false` for a tile that holds a matching key - a false
/// negative.
fn is_unique_prefix(prefix: &[u8]) -> bool {
    if prefix.first() != Some(&keys::U) { return false; }
    let rest = &prefix[1..];
    let Some(entity_end) = rest.iter().position(|&b| b == 0) else { return false };
    if entity_end == 0 { return false; }
    let after_entity = &rest[entity_end + 1..];
    let Some(field_end) = after_entity.iter().position(|&b| b == 0) else { return false };
    if field_end == 0 { return false; }
    let value = &after_entity[field_end + 1..];
    if value.len() < 4 { return false; }
    let value_len = u32::from_be_bytes(value[..4].try_into().unwrap()) as usize;
    value.len() == 4 + value_len
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
    let (key, value) = op_kv(op, csn);
    mt.apply(key, value);
}

/// The encoded key/value an op applies at `csn` - the single spelling shared by
/// [`apply_op`] and compaction's WAL-suffix map.
fn op_kv(op: &Op, csn: u64) -> (Vec<u8>, Vec<u8>) {
    match op {
        Op::PutDoc { entity, handle, doc } => (keys::primary_key(entity, *handle, csn).unwrap(), doc.clone()),
        Op::DelDoc { entity, handle } => (keys::primary_key(entity, *handle, csn).unwrap(), Vec::new()),
        Op::PutUnique { entity, field, value, handle } => {
            (keys::unique_key(entity, field, value, *handle, csn).unwrap(), handle.to_be_bytes().to_vec())
        }
        Op::DelUnique { entity, field, value, handle } => {
            (keys::unique_key(entity, field, value, *handle, csn).unwrap(), Vec::new())
        }
        Op::PutReverse { entity, field, target, source } => {
            (keys::reverse_key(entity, field, *target, *source, csn).unwrap(), vec![1])
        }
        Op::DelReverse { entity, field, target, source } => {
            (keys::reverse_key(entity, field, *target, *source, csn).unwrap(), Vec::new())
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

/// Publish verification against the WAL-derived expectation map: stream the map
/// in key order with the same newest-version-per-logical-key / `(lower, cutoff]`
/// reduction as the tile write, and require an exact 1:1 match with the written
/// tile - missing, extra, key or value mismatches all fail closed. The tile was
/// written from the *serving memtable*, so this comparison is genuinely
/// independent (two different sources), unlike a re-derivation of the writer's
/// own input.
///
/// `region` restricts the expectation to one `(family, entity)` region: a
/// region-partitioned tile is only obligated for its own region's keys (its
/// siblings cover the rest), so the map is filtered to that region before the
/// alignment. `None` is the whole-keyspace tile.
fn verify_tile_against_wal(
    tile: &Tile,
    wal_entries: &BTreeMap<Vec<u8>, Vec<u8>>,
    lower: u64,
    cutoff: u64,
    region: Option<&[u8]>,
) -> io::Result<()> {
    let mut actual = tile.entries();
    let mut pending = actual.next();
    let mut last_logical: Option<Vec<u8>> = None;
    for (key, value) in wal_entries {
        if let Some(region) = region {
            if keys::region_prefix(key) != Some(region) { continue; }
        }
        let Some(csn) = keys::key_csn(key) else { continue };
        if csn <= lower || csn > cutoff { continue; }
        let logical = &key[..key.len() - 8];
        if last_logical.as_deref() == Some(logical) { continue; }
        last_logical = Some(logical.to_vec());
        let Some(entry) = pending.take() else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "tile is missing a WAL entry"));
        };
        let (akey, avalue) = entry?;
        if akey.as_slice() != key.as_slice() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "tile entry key disagrees with WAL"));
        }
        if avalue.as_slice() != value.as_slice() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "tile entry value disagrees with WAL"));
        }
        pending = actual.next();
    }
    if let Some(entry) = pending {
        entry?;
        return Err(io::Error::new(io::ErrorKind::InvalidData, "tile contains an extra entry"));
    }
    Ok(())
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

    /// A partial append inside a group poisons the WAL writer exactly as it does
    /// on the single-block path, so the group's one barrier (`end_group`) fails
    /// instead of acking a torn frame, and recovery truncates it.
    #[test]
    fn end_group_after_a_failed_append_poisons_like_the_single_block_path() {
        let p = tmp("group_poison.wal");
        let mut e = Engine::create(&p).unwrap();
        e.commit_block(b"base", &[Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"doc1".to_vec() }]).unwrap();
        e.begin_group().unwrap();
        e.wal.arm_partial_append_fault();
        assert!(e.commit_block_grouped(b"bad", &[Op::PutDoc { entity: b"E".to_vec(), handle: 2, doc: b"doc2".to_vec() }]).is_err(),
            "the injected partial append fails");
        assert!(e.end_group().is_err(), "the group's barrier surfaces the poisoned writer");
        assert!(e.commit_block(b"after", &[Op::PutDoc { entity: b"E".to_vec(), handle: 3, doc: b"doc3".to_vec() }]).is_err(),
            "the writer stays poisoned until reopen");
        drop(e);
        let e = Engine::open(&p).unwrap();
        assert_eq!(e.csn(), 1, "recovery truncates the torn frame; the acknowledged base survives");
        assert_eq!(get_ok(&e, "E", 1), Some(b"doc1".to_vec()));
        assert_eq!(get_ok(&e, "E", 2), None);
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

    /// White-box check that BOTH engine write paths attach a generated unique
    /// bloom and that it never rejects a stored prefix. The reject is gated on a
    /// full unique prefix (`is_unique_prefix`), and a tile with no bloom answers
    /// `true` for everything, so "some absent prefix is rejected" is exactly the
    /// evidence that a filter is present. The false-positive RATE is measured in
    /// `probe_cost`; only the no-false-negative direction is asserted here.
    #[test]
    fn published_and_compacted_tiles_carry_a_working_unique_bloom() {
        let dir = tmp_dir("bloom_wiring");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        let uval = |value: &[u8], handle: u64| Op::PutUnique {
            entity: b"E".to_vec(), field: b"u".to_vec(), value: value.to_vec(), handle,
        };
        e.commit_block(b"t1", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"d1".to_vec() },
            uval(b"a1", 1), uval(b"a3", 3), uval(b"a5", 5),
        ]).unwrap();
        e.publish_tile(dir.join("tile-1.tile"), 1).unwrap();
        e.commit_block(b"t2", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 11, doc: b"d11".to_vec() },
            uval(b"b1", 11), uval(b"b3", 13),
        ]).unwrap();
        e.publish_tile(dir.join("tile-2.tile"), 2).unwrap();

        let no_false_negatives = |tile: &Tile, values: &[&[u8]]| {
            for v in values {
                let pfx = keys::unique_prefix(b"E", b"u", v).unwrap();
                assert!(tile.might_contain_unique_prefix(&pfx), "a stored prefix must never be rejected: {v:?}");
            }
        };
        let rejects_absent = |tile: &Tile, values: &[&[u8]]| values.iter().any(|v| {
            let pfx = keys::unique_prefix(b"E", b"u", v).unwrap();
            !tile.might_contain_unique_prefix(&pfx)
        });
        let absent = [b"a2".as_slice(), b"a4".as_slice(), b"c9".as_slice(), b"zzz".as_slice(), b"a2x".as_slice()];

        no_false_negatives(&e.tiles[0].tile, &[b"a1", b"a3", b"a5"]);
        no_false_negatives(&e.tiles[1].tile, &[b"b1", b"b3"]);
        // The range filter accepts an in-range absent prefix on tile one, so
        // only the bloom can skip it; a filter-less tile answers `true` always,
        // so a rejection proves the filter is attached.
        let in_range_absent = keys::unique_prefix(b"E", b"u", b"a2").unwrap();
        assert!(e.tiles[0].tile.could_contain_prefix(&in_range_absent), "a2 sorts inside tile one's range");
        assert!(rejects_absent(&e.tiles[0].tile, &absent), "the published tile must carry a bloom");

        // The compaction output carries one too.
        e.compact().unwrap();
        assert_eq!(e.tiles.len(), 1);
        no_false_negatives(&e.tiles[0].tile, &[b"a1", b"a3", b"a5", b"b1", b"b3"]);
        assert!(e.tiles[0].tile.could_contain_prefix(&keys::unique_prefix(b"E", b"u", b"b2").unwrap()));
        assert!(rejects_absent(&e.tiles[0].tile, &absent), "the compacted tile must carry a bloom");

        // The persisted filter survives reopen.
        drop(e);
        let e = Engine::open_discover(&dir).unwrap();
        no_false_negatives(&e.tiles[0].tile, &[b"a1", b"a3", b"a5", b"b1", b"b3"]);
        assert!(rejects_absent(&e.tiles[0].tile, &absent), "the persisted bloom survives reopen");
        let _ = fs::remove_dir_all(&dir);
    }

    fn superblock_u32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// The provisional layout policy: an unknown region gets the declared default,
    /// a named region gets its override, and `declared_default` is today's pair.
    #[test]
    fn layout_policy_defaults_and_region_lookup() {
        let policy = LayoutPolicy::declared_default();
        assert_eq!(policy.default_layout(), (4096, 10));
        assert_eq!(policy.for_region("anything"), (4096, 10), "an unknown region is today's behaviour");

        let mut policy = LayoutPolicy::with_default(16384, 0);
        assert_eq!(policy.for_region("still-unknown"), (16384, 0));
        assert_eq!(policy.set_region("U|Order|no", 65536, 14), None);
        assert_eq!(policy.for_region("U|Order|no"), (65536, 14));
        assert_eq!(policy.set_region("U|Order|no", 8192, 7), Some((65536, 14)));
        assert_eq!(policy.for_region("U|Order|no"), (8192, 7));
        assert_eq!(policy.for_region("U|Order|other"), (16384, 0));
    }

    /// White-box proof that both write paths take their per-tile parameters from
    /// the layout policy: a non-default page size lands in the tile superblock, a
    /// 0-bit bloom means "no section", and `might_contain_unique_prefix` then
    /// answers `true` (never a false negative) instead of rejecting an absent
    /// prefix; a later compaction re-chooses the layout rather than inheriting it.
    #[test]
    fn write_paths_honor_the_layout_policy() {
        let dir = tmp_dir("layout_policy");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.set_layout_policy(LayoutPolicy::with_default(16384, 0));
        e.commit_block(b"t1", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"d1".to_vec() },
            Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"a1".to_vec(), handle: 1 },
        ]).unwrap();
        e.publish_tile(dir.join("tile-1.tile"), 1).unwrap();
        let bytes = fs::read(dir.join("tile-1.tile")).unwrap();
        assert_eq!(superblock_u32(&bytes, 72), 16384, "the publish path stored the policy page size");
        assert_eq!(superblock_u32(&bytes, 84), 0, "0 bloom bits writes no bloom length");
        assert_eq!(superblock_u32(&bytes, 96), 0, "0 bloom bits stores a zero bits/key");
        let stored = keys::unique_prefix(b"E", b"u", b"a1").unwrap();
        assert!(e.tiles[0].tile.might_contain_unique_prefix(&stored),
            "a tile with no bloom must answer true, never a false negative");

        e.commit_block(b"t2", &[
            Op::PutDoc { entity: b"E".to_vec(), handle: 11, doc: b"d11".to_vec() },
            Op::PutUnique { entity: b"E".to_vec(), field: b"u".to_vec(), value: b"b1".to_vec(), handle: 11 },
        ]).unwrap();
        e.publish_tile(dir.join("tile-2.tile"), 2).unwrap();
        e.set_layout_policy(LayoutPolicy::with_default(65536, 12));
        e.compact().unwrap();
        assert_eq!(e.tiles.len(), 1);
        let name = e.tiles[0].name.clone();
        let bytes = fs::read(dir.join(&name)).unwrap();
        assert_eq!(superblock_u32(&bytes, 72), 65536, "the compaction path stored the policy page size");
        assert!(superblock_u32(&bytes, 84) > 0, "a nonzero bloom policy attaches a section");
        assert_eq!(superblock_u32(&bytes, 96), 12, "the bloom bits/key is the policy value");

        // The mixed-layout directory reopens and answers every read path.
        let reopened = Engine::open_discover(&dir).unwrap();
        assert_eq!(get_ok(&reopened, "E", 1), Some(b"d1".to_vec()));
        assert_eq!(get_ok(&reopened, "E", 11), Some(b"d11".to_vec()));
        assert_eq!(reopened.unique_lookup(b"E", b"u", b"a1").unwrap(), Some(1));
        assert_eq!(reopened.unique_lookup(b"E", b"u", b"b1").unwrap(), Some(11));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn region_partitioned_publish_fault_leaves_the_previous_region_set_intact() {
        let dir = tmp_dir("region-publish-fault");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        let put = |entity: &[u8], h: u64, v: &[u8]| Op::PutDoc { entity: entity.to_vec(), handle: h, doc: v.to_vec() };
        let uniq = |v: &[u8], h: u64| Op::PutUnique { entity: b"Stock".to_vec(), field: b"u".to_vec(), value: v.to_vec(), handle: h };
        e.commit_block(b"b1", &[put(b"Order", 1, b"o1"), uniq(b"A", 1)]).unwrap();
        e.publish_tile(dir.join("p1.tile"), 1).unwrap();
        assert_eq!(e.tiles.len(), 2, "P|Order and U|Stock");
        let before = e.active_tile_cutoffs();
        let root_tiles_before = e.manifest.as_ref().unwrap().root().tiles.len();
        // The next publish writes both region files, then the manifest page write
        // fails: no region of the set may become durable while another does not.
        e.commit_block(b"b2", &[put(b"Order", 1, b"o2"), uniq(b"A", 1)]).unwrap();
        e.manifest.as_mut().unwrap().inject_page_write_error = true;
        assert!(e.publish_tile(dir.join("p2.tile"), 2).is_err(), "the injected manifest write fails the publish");
        assert_eq!(e.active_tile_cutoffs(), before, "the in-memory region set is unchanged by the failed publish");
        assert_eq!(e.manifest.as_ref().unwrap().root().tiles.len(), root_tiles_before, "the durable root still lists the previous set");
        drop(e);
        let e = Engine::open_discover(&dir).unwrap();
        assert_eq!(e.active_tile_cutoffs(), before, "no partially published region set survived the crash");
        assert_eq!(get_ok(&e, "Order", 1), Some(b"o2".to_vec()), "the WAL still serves the failed publish's state");
        assert_eq!(e.unique_lookup(b"Stock", b"u", b"A").unwrap(), Some(1));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn region_partitioned_compaction_merges_only_within_one_region() {
        let dir = tmp_dir("region-compact");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        for (i, v) in [b"a".as_slice(), b"b", b"c"].iter().enumerate() {
            let csn = (i + 1) as u64;
            e.commit_block(format!("b{csn}").as_bytes(), &[
                Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: v.to_vec() },
                Op::PutUnique { entity: b"Stock".to_vec(), field: b"u".to_vec(), value: b"A".to_vec(), handle: 1 },
            ]).unwrap();
            e.publish_tile(dir.join(format!("p{csn}.tile")), csn).unwrap();
        }
        assert_eq!(e.tiles.len(), 6);
        e.maybe_compact(u64::MAX, 2).unwrap().expect("a region run merges");
        assert_eq!(e.tiles.len(), 5, "one run of ONE region merged; the other region is untouched");
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for t in &e.tiles {
            *counts.entry(keys::region_name(tile_first_region(t).unwrap())).or_default() += 1;
        }
        let mut per_region: Vec<usize> = counts.values().copied().collect();
        per_region.sort();
        assert_eq!(per_region, vec![2, 3], "no tile spans two regions and none merged across them");
        assert_eq!(get_ok(&e, "Order", 1), Some(b"c".to_vec()));
        assert_eq!(e.unique_lookup(b"Stock", b"u", b"A").unwrap(), Some(1));
        drop(e);
        let e = Engine::open_discover(&dir).unwrap();
        assert_eq!(e.tiles.len(), 5, "the region-local merge reopens");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn region_partitioned_tiles_are_single_region_and_ordered_by_region() {
        let dir = tmp_dir("region-helpers");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        e.commit_block(b"b1", &[
            Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: b"o".to_vec() },
            Op::PutUnique { entity: b"Stock".to_vec(), field: b"sku".to_vec(), value: b"A".to_vec(), handle: 1 },
            Op::PutUnique { entity: b"Stock".to_vec(), field: b"loc".to_vec(), value: b"L".to_vec(), handle: 1 },
        ]).unwrap();
        e.publish_tile(dir.join("t.tile"), 1).unwrap();
        // Both U fields of Stock are one region; the list is region-ordered.
        let names: Vec<String> = e.tiles.iter().map(|t| keys::region_name(tile_first_region(t).unwrap())).collect();
        assert_eq!(names, vec!["P|Order".to_string(), "U|Stock".to_string()]);
        assert!(e.tiles.iter().all(|t| tile_single_region(t).is_some()), "every partitioned tile is single-region");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reopen_rejects_a_region_out_of_order_within_a_cutoff_group() {
        let dir = tmp_dir("region-order");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        e.commit_block(b"b1", &[
            Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: b"o".to_vec() },
            Op::PutUnique { entity: b"Stock".to_vec(), field: b"u".to_vec(), value: b"A".to_vec(), handle: 1 },
        ]).unwrap();
        e.publish_tile(dir.join("t.tile"), 1).unwrap();
        // A hand-built root lists the same-cutoff tiles in region-DECREASING
        // order. The manifest only requires non-decreasing cutoffs, so it accepts
        // it; `open_impl` must reject it on the `(cutoff, region)` invariant.
        let mut refs: Vec<TileRef> = e.tiles.iter().map(|t| t.tile_ref()).collect();
        refs.reverse();
        e.manifest.as_mut().unwrap().publish(refs, Checkpoint { csn: 1, handle_watermark: e.max_handle }, None).unwrap();
        drop(e);
        assert!(Engine::open_discover(&dir).is_err(), "reopen enforces (cutoff, region)");
        let _ = fs::remove_dir_all(&dir);
    }

    fn remove(p: &Path) {
        fs::remove_file(p).ok();
    }
}
