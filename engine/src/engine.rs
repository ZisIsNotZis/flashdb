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
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::bloom::BloomBuilder;
use crate::keys::{self, check_name};
use crate::manifest::{self, Checkpoint, Manifest, SegmentRef, TileRef};
use crate::memtable::Memtable;
use crate::metrics::{self, Metrics};
use crate::tile::{Tile, TileCursor, TileWriter, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, MIN_PAGE_SIZE};
use crate::wal::{self, Wal};

const MAGIC: &[u8; 4] = b"FDB1";

/// Bounded overlapped-barrier cap (slice 56): an engine refuses to submit more
/// than this many WAL `fdatasync` barriers in flight at once, which bounds the
/// pending-group memory the load driver may retain while group N+1 is appended
/// ahead of group N's barrier. Ordering is preserved regardless: the barrier
/// worker drains its queue FIFO and completions are observed in submission order.
pub const MAX_PENDING_BARRIERS: usize = 64;

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

/// Default per-region **merge threshold** (in that region's own tile count): a
/// region's automatic merges are only considered once the region holds this
/// many tiles. `2` reproduces today's effective behaviour exactly - selection
/// already required a run of at least two tiles, so the only precondition a
/// region had to satisfy was that it held two tiles. Raising a region's
/// threshold *defers* its merges; that trade of write amplification against
/// read latency is the per-region compaction-trigger actuator a learner sets.
///
/// The threshold gates only the automatic [`Engine::maybe_compact_shape`] path,
/// never the explicit [`Engine::compact`] (which a caller invokes to collapse a
/// region on request). A threshold above the active-tile cap can therefore defer
/// a region's merges until publishes start refusing once the manifest is full;
/// that is the documented cost of the lazy end of the range.
pub const DEFAULT_MERGE_MIN_TILES: u8 = 2;

/// One region's declared physical parameters: page size and bloom bits per key
/// (per-tile values, written into each tile's superblock) plus the region's
/// merge threshold (`merge_min_tiles`).
#[derive(Clone, Debug, PartialEq, Eq)]
struct RegionLayout {
    name: String,
    page_size: u32,
    bloom_bits_per_key: u8,
    merge_min_tiles: u8,
}

/// Provisional per-prefix-region layout declaration (slice 17; per-region merge
/// threshold added by slice 27).
///
/// A policy maps a prefix-region name to a tile's two already-safe physical
/// parameters - page size and bloom bits per key - and to the region's
/// **compaction trigger** (`merge_min_tiles`), and returns defaults for any
/// region it does not name. It is deliberately **not persisted**: there is no
/// policy table and no manifest field, and the reader never consults it. The
/// only durable declaration is what each tile's superblock already records
/// (`page_size`, `bits_per_key`), which is exactly what makes this safe to land
/// now: a policy change affects only *new* tiles and the *future* merge schedule,
/// and a reader honors the stored per-tile value, so a stale or bad layout is
/// slow, never wrong (I2).
///
/// [`LayoutPolicy::declared_default`] reproduces today's behaviour exactly
/// (4096-byte pages, 10 bloom bits/key everywhere, `merge_min_tiles = 2`).
///
/// ## External control: JSON policy file
///
/// A controller outside the process declares the policy by writing a JSON file
/// and pointing the driver at it (`FLASHDB_POLICY_FILE`, or `--policy-file` for
/// `flashdb-load`). The frozen schema is:
///
/// ```json
/// {
///   "default": { "page_size": 4096, "bloom_bits_per_key": 10, "merge_min_tiles": 2 },
///   "regions": {
///     "P|Stock":  { "merge_min_tiles": 2 },
///     "P|Order":  { "merge_min_tiles": 64 },
///     "P|Movement": { "page_size": 65536, "merge_min_tiles": 256 }
///   }
/// }
/// ```
///
/// `default` and `regions` are both optional; every field inside an entry is
/// optional and falls back to the policy default. `page_size` must be in
/// `4096..=65536`, `bloom_bits_per_key` in `0..=64`, and `merge_min_tiles`
/// `>= 2`. A region key must be a real region spelling (`P|`, `U|` or `R|`
/// followed by a non-empty entity).
///
/// **Error policy (frozen):** any malformed or invalid entry is a hard error -
/// [`LayoutPolicy::from_json_slice`] rejects the whole file rather than silently
/// ignoring part of a controller's declaration, so a controller can never
/// believe it set a threshold it did not. A *well-formed* region name that names
/// no tile in the database is not an error: the policy is a lookup table, and an
/// unused row simply has no effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutPolicy {
    default: (u32, u8),
    default_merge_min_tiles: u8,
    regions: Vec<RegionLayout>,
}

impl Default for LayoutPolicy {
    fn default() -> Self { Self::declared_default() }
}

impl LayoutPolicy {
    /// Today's values exactly: 4096-byte pages, 10 bloom bits/key and a merge
    /// threshold of 2 everywhere.
    pub fn declared_default() -> Self {
        LayoutPolicy {
            default: (DEFAULT_PAGE_SIZE, UNIQUE_BLOOM_BITS_PER_KEY as u8),
            default_merge_min_tiles: DEFAULT_MERGE_MIN_TILES,
            regions: Vec::new(),
        }
    }

    /// A policy with a different default and no per-region overrides. `page_size`
    /// must be in `4096..=65536`; the tile writer rejects anything else at build
    /// time rather than writing a tile it cannot read back.
    pub fn with_default(page_size: u32, bloom_bits_per_key: u8) -> Self {
        LayoutPolicy {
            default: (page_size, bloom_bits_per_key),
            default_merge_min_tiles: DEFAULT_MERGE_MIN_TILES,
            regions: Vec::new(),
        }
    }

    /// The `(page_size, bloom_bits_per_key)` a tile in `region` is written with.
    /// An unknown region returns the policy default, never an error: a missing
    /// declaration is today's behavior (I2 - a bad layout is slow, never wrong).
    pub fn for_region(&self, region: &str) -> (u32, u8) {
        self.regions
            .iter()
            .find(|r| r.name == region)
            .map(|r| (r.page_size, r.bloom_bits_per_key))
            .unwrap_or(self.default)
    }

    /// The issue-27 actuator: the tile count at which `region`'s automatic
    /// merges become eligible. An undeclared region returns the policy default
    /// (today: 2).
    pub fn merge_min_tiles_for_region(&self, region: &str) -> u8 {
        self.regions
            .iter()
            .find(|r| r.name == region)
            .map(|r| r.merge_min_tiles)
            .unwrap_or(self.default_merge_min_tiles)
    }

    /// The policy-wide default, returned for every region without an override.
    pub fn default_layout(&self) -> (u32, u8) { self.default }

    /// The policy-wide merge threshold, returned for every region without an
    /// override (today: [`DEFAULT_MERGE_MIN_TILES`]).
    pub fn default_merge_min_tiles(&self) -> u8 { self.default_merge_min_tiles }

    /// Override one named prefix-region's page size and bloom bits; the region's
    /// merge threshold keeps its existing value (or the policy default). Returns
    /// the previous `(page_size, bits)` when the region was already declared.
    /// `page_size` is validated by the tile writer, not here, so a bad override
    /// surfaces as a failed tile build (`InvalidData`).
    pub fn set_region(&mut self, region: &str, page_size: u32, bloom_bits_per_key: u8) -> Option<(u32, u8)> {
        if let Some(entry) = self.regions.iter_mut().find(|r| r.name == region) {
            let previous = (entry.page_size, entry.bloom_bits_per_key);
            entry.page_size = page_size;
            entry.bloom_bits_per_key = bloom_bits_per_key;
            return Some(previous);
        }
        let merge_min_tiles = self.default_merge_min_tiles;
        self.regions.push(RegionLayout { name: region.to_string(), page_size, bloom_bits_per_key, merge_min_tiles });
        None
    }

    /// Override one named prefix-region's merge threshold; the region's page
    /// size and bloom bits keep their existing values (or the policy default).
    /// Returns the previous threshold when the region was already declared.
    pub fn set_region_merge(&mut self, region: &str, merge_min_tiles: u8) -> Option<u8> {
        if let Some(entry) = self.regions.iter_mut().find(|r| r.name == region) {
            return Some(std::mem::replace(&mut entry.merge_min_tiles, merge_min_tiles));
        }
        let (page_size, bloom_bits_per_key) = self.default;
        self.regions.push(RegionLayout { name: region.to_string(), page_size, bloom_bits_per_key, merge_min_tiles });
        None
    }

    /// Set the policy-wide merge threshold returned for every region without an
    /// override.
    pub fn set_default_merge_min_tiles(&mut self, merge_min_tiles: u8) { self.default_merge_min_tiles = merge_min_tiles; }

    /// The frozen controller-facing JSON form of this policy (sorted object
    /// keys, so a dump is byte-deterministic). Mirrors [`LayoutPolicy::from_json_slice`].
    pub fn to_json(&self) -> serde_json::Value {
        let mut regions = serde_json::Map::new();
        for r in &self.regions {
            regions.insert(r.name.clone(), serde_json::json!({
                "page_size": r.page_size,
                "bloom_bits_per_key": r.bloom_bits_per_key,
                "merge_min_tiles": r.merge_min_tiles,
            }));
        }
        serde_json::json!({
            "default": {
                "page_size": self.default.0,
                "bloom_bits_per_key": self.default.1,
                "merge_min_tiles": self.default_merge_min_tiles,
            },
            "regions": serde_json::Value::Object(regions),
        })
    }

    /// Parse the frozen JSON policy-file schema (see the type docs). Rejects the
    /// whole document - not individual entries - on any malformed field, invalid
    /// value, or region key that is not a real region spelling.
    pub fn from_json_slice(bytes: &[u8]) -> io::Result<Self> {
        let file: PolicyFile = serde_json::from_slice(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("policy file is not valid JSON: {e}")))?;
        let mut policy = Self::declared_default();
        if let Some(entry) = &file.default {
            if let Some(page) = entry.page_size {
                validate_page_size(page)?;
                policy.default.0 = page;
            }
            if let Some(bits) = entry.bloom_bits_per_key {
                validate_bloom_bits(bits)?;
                policy.default.1 = bits;
            }
            if let Some(min) = entry.merge_min_tiles {
                validate_merge_min_tiles(min)?;
                policy.default_merge_min_tiles = min;
            }
        }
        for (name, entry) in &file.regions {
            if !is_region_spelling(name) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!(
                    "policy region {name:?} is not a region spelling (expected P|<entity>, U|<entity> or R|<entity> with a non-empty entity)")));
            }
            let mut layout = RegionLayout {
                name: name.clone(),
                page_size: policy.default.0,
                bloom_bits_per_key: policy.default.1,
                merge_min_tiles: policy.default_merge_min_tiles,
            };
            if let Some(page) = entry.page_size {
                validate_page_size(page)?;
                layout.page_size = page;
            }
            if let Some(bits) = entry.bloom_bits_per_key {
                validate_bloom_bits(bits)?;
                layout.bloom_bits_per_key = bits;
            }
            if let Some(min) = entry.merge_min_tiles {
                validate_merge_min_tiles(min)?;
                layout.merge_min_tiles = min;
            }
            if let Some(existing) = policy.regions.iter_mut().find(|r| r.name == *name) {
                *existing = layout;
            } else {
                policy.regions.push(layout);
            }
        }
        Ok(policy)
    }

    /// Read and parse the JSON policy file at `path`. See
    /// [`LayoutPolicy::from_json_slice`] for the schema and error policy.
    pub fn from_json_file(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path).map_err(|e| io::Error::new(e.kind(), format!("policy file {} unreadable: {e}", path.display())))?;
        Self::from_json_slice(&bytes)
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

/// One entry of the JSON policy file: every field is optional and falls back to
/// the policy default. `deny_unknown_fields` makes a typo a hard error rather
/// than a silently ignored setting.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyEntry {
    #[serde(default)]
    page_size: Option<u32>,
    #[serde(default)]
    bloom_bits_per_key: Option<u8>,
    #[serde(default)]
    merge_min_tiles: Option<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    default: Option<PolicyEntry>,
    #[serde(default)]
    regions: BTreeMap<String, PolicyEntry>,
}

fn validate_page_size(page: u32) -> io::Result<()> {
    if (MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page) { Ok(()) } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, format!(
            "policy page_size={page} is not in {MIN_PAGE_SIZE}..={MAX_PAGE_SIZE}")))
    }
}

fn validate_bloom_bits(bits: u8) -> io::Result<()> {
    if bits <= 64 { Ok(()) } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, format!(
            "policy bloom_bits_per_key={bits} is above the supported maximum 64")))
    }
}

fn validate_merge_min_tiles(min: u8) -> io::Result<()> {
    if min >= DEFAULT_MERGE_MIN_TILES { Ok(()) } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, format!(
            "policy merge_min_tiles={min} must be >= {DEFAULT_MERGE_MIN_TILES} (a merge needs two tiles)")))
    }
}

/// Whether `name` is a well-formed region spelling: `P`/`U`/`R`, a `|`, and a
/// non-empty entity (the only names [`keys::region_name`] can produce).
fn is_region_spelling(name: &str) -> bool {
    matches!(name.split_once('|'), Some((family, entity))
        if matches!(family, "P" | "U" | "R") && !entity.is_empty())
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

/// Wall-clock phase split of ONE compaction actuation's tile build (the
/// [`Engine::write_compacted_tile`] half), in microseconds. Observability for
/// the latency-tail diagnosis (slice 53): never persisted, never consulted by
/// any read/write/durability decision, and recorded only on a successful merge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompactionBuildStats {
    /// Replay the WAL suffix, reduce it into the run's `(lower, cutoff]` window,
    /// and stream the merged reduction into the tile writer (data blocks only;
    /// no fsync). This is the merge CPU + tile data write.
    pub stream_us: u64,
    /// `TileWriter::finish`: directory + superblock, the tile file `sync_data`,
    /// and the reopen/verification of the written superblock.
    pub finish_us: u64,
    /// The two fail-closed verification passes over the written tile.
    pub verify_us: u64,
}

/// Wall-clock phase split of ONE successful compaction actuation, in
/// microseconds. `input_bytes`/`output_bytes` are the merged run's on-disk
/// input sizes and the replacement tile's on-disk size; `elapsed_us` is the
/// whole [`Engine::merge_run`]/[`Engine::merge_region_run`] wall time.
/// Observability for the latency-tail diagnosis (slice 53): never persisted and
/// never consulted by any decision.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompactionStats {
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub elapsed_us: u64,
    pub build: CompactionBuildStats,
    /// The standalone parent-directory `sync_all` that makes the replacement
    /// tile's directory entry durable before the manifest references it.
    pub dir_sync_us: u64,
    /// Manifest inactive-page write + file `sync_data` + parent-directory
    /// `sync_all` (the durable flip).
    pub manifest_us: u64,
    /// Best-effort unlink of the retired input tiles.
    pub unlink_us: u64,
}

/// A self-contained compaction build: every input a worker needs to produce ONE
/// replacement tile without touching the live engine. The worker opens the
/// named input tiles read-only and reads the WAL suffix through `wal_file`
/// bounded by `wal_valid_len` (the length captured when the job was created), so
/// it can run off the request path while the engine keeps committing.
#[derive(Debug)]
pub struct CompactionJob {
    /// Retired input tile filenames, in cutoff order.
    pub input_names: Vec<String>,
    pub lower: u64,
    pub cutoff: u64,
    /// `None` for a whole-keyspace run; `Some(prefix)` for a per-region run.
    pub region: Option<Vec<u8>>,
    pub output_name: String,
    pub page_size: u32,
    pub bloom_bits_per_key: u8,
    pub key_encoding: keys::KeyEncoding,
    /// An already-open handle to the WAL segment the job reads from. Bounded by
    /// `wal_valid_len` so concurrent appends are never observed.
    pub wal_file: std::fs::File,
    pub wal_valid_len: u64,
    /// The engine's expected WAL max CSN at job creation (the republish guard).
    pub wal_max_expected: u64,
    /// The engine's CSN at job creation; the republish guard is skipped only when
    /// it is 0 (an engine that has never committed).
    pub wal_csn: u64,
    /// The retained-WAL floor below which records are not re-derivable.
    pub wal_floor: u64,
}

/// A finished off-request-path compaction build: the replacement tile's
/// identity plus its sizes and phase split, ready for
/// [`Engine::apply_compaction_job`].
#[derive(Debug)]
pub struct CompactionOutcome {
    pub name: String,
    pub cutoff: u64,
    pub digest: [u8; 32],
    /// The retired input tile filenames (the job's run), needed to apply the
    /// flip against the live tile list.
    pub input_names: Vec<String>,
    /// The region for a per-region run; `None` for a whole-keyspace run.
    pub region: Option<Vec<u8>>,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub stats: CompactionStats,
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

/// Result of one request inside a [`Engine::commit_batch`] group.
///
/// The unit of a uniqueness verdict is the request, not the group: a request
/// whose projected post-state leaves more than one live owner for a touched
/// unique prefix is dropped alone ([`RequestOutcome::Conflict`]) while the rest
/// of the group commits. `Committed` carries one [`Outcome`] per block of that
/// request, in input order (each `Committed { csn }` is that block's own CSN; an
/// idempotent retry is `AlreadyCommitted { csn }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Committed(Vec<Outcome>),
    /// The request was dropped entirely: no block was appended and no CSN was
    /// consumed. The conflict names the unique prefix that could not be proven
    /// to have at most one live owner.
    Conflict(Conflict),
}

/// One touched unique prefix inside a commit group: the constraint identity and
/// the request's per-handle net effect (later ops win, exactly as
/// [`Memtable::apply`] leaves them).
struct PrefixEffect {
    entity: Vec<u8>,
    field: Vec<u8>,
    value: Vec<u8>,
    owners: HashMap<u64, bool>,
}

/// A unique prefix's live-owner set: the published owners, updated with the net
/// effect of every earlier ACCEPTED request in the group (a dropped request
/// leaves no trace, so a later request is validated against exactly the prefix
/// of the group that actually commits).
struct LivePrefix {
    owners: HashMap<u64, bool>,
}

/// Whether the published owners overlaid with a request's net effect leave more
/// than one live owner for one unique prefix. The effect wins per handle (as
/// [`Memtable::apply`] would leave it); a handle only in the published set keeps
/// its published state.
fn leaves_two_owners(published: &HashMap<u64, bool>, effect: &HashMap<u64, bool>) -> bool {
    let mut alive = 0usize;
    for (&handle, &is_alive) in published {
        if is_alive && !effect.contains_key(&handle) {
            alive += 1;
        }
    }
    for &is_alive in effect.values() {
        if is_alive {
            alive += 1;
        }
    }
    alive > 1
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
    /// Sum of the per-group WAL `append` elapsed time (us) since the last
    /// [`Engine::take_group_phase_us`]. Observational only (slice 55): never
    /// persisted and never read on any correctness path; it attributes the
    /// request-path WAL append separately from the memtable apply and the
    /// durability barrier.
    group_append_us: u64,
    /// Sum of the per-group memtable `apply` elapsed time (us) since the last
    /// [`Engine::take_group_phase_us`]. Observational only (slice 55): never
    /// persisted and never read on any correctness path.
    group_apply_us: u64,
    /// Sum of the per-group WAL `fdatasync` elapsed time (us) since the last
    /// [`Engine::take_group_phase_us`]. Observational only (slice 54/55): never
    /// persisted and never read on any correctness path; it attributes the
    /// request-path durability barrier separately from the read/allocator stalls.
    group_fsync_us: u64,
    /// Overlapped WAL-barrier worker (slice 56). A single background thread that
    /// runs one `fdatasync` at a time on cloned WAL handles, so the request path
    /// can append group N+1 while group N's barrier is in flight. Lazily spawned
    /// on the first overlapped submit; `None` until then. Observational only:
    /// never persisted.
    barrier_worker: Option<BarrierWorker>,
    /// Number of submitted overlapped barriers whose completion has not yet been
    /// drained. Bounded by [`MAX_PENDING_BARRIERS`] (enforced in
    /// [`Engine::submit_group`]); the drain preserves submission order.
    barrier_in_flight: usize,
    /// Provisional per-region layout the two tile write paths consult. Never
    /// persisted; the per-tile parameters live in each tile's superblock.
    layout: LayoutPolicy,
    /// Whether a write path partitions its output one tile per `(entity, key
    /// family)` region ([`RegionMode::PerRegion`]) or the historical single
    /// whole-keyspace tile ([`RegionMode::Global`], the default). Not persisted:
    /// recovery derives each tile's region from the tile's first key.
    region_mode: RegionMode,
    /// The key spelling new keys are written with. [`keys::KeyEncoding::FamilyFirst`]
    /// (the default) reproduces today's keyspace exactly; [`keys::KeyEncoding::EntityFirst`]
    /// writes one entity's `P`/`U`/`R` keys adjacently so a region-partitioned
    /// publish can co-locate them in one tile. Not persisted (like `region_mode`
    /// and `LayoutPolicy`): recovery must be told the encoding used to write
    /// (slice 50), and reads accept both spellings regardless.
    key_encoding: keys::KeyEncoding,
    /// Per-pattern latency telemetry (slice 16). Observational only: never
    /// persisted, never read on the correctness path. `RefCell` is required
    /// because read paths take `&self`; it is the only interior mutability here.
    /// Disabled via [`Engine::set_metrics_enabled`].
    metrics: RefCell<Metrics>,
    /// Phase split of the most recent successful compaction actuation (slice
    /// 53). Observational only: never persisted and never read on any
    /// correctness path.
    last_compaction: Option<CompactionStats>,
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
/// observed. A no-op with no clock read when telemetry is disabled. The region
/// is set after the call resolves its prefix ([`metrics::DISABLED`] for a call
/// with no single region, e.g. a commit), so the same clock read feeds both the
/// pattern and the region histogram.
struct MetricsSample<'a> {
    metrics: &'a RefCell<Metrics>,
    id: usize,
    region: usize,
    started: Option<Instant>,
}

impl Drop for MetricsSample<'_> {
    fn drop(&mut self) {
        if let Some(t0) = self.started {
            let elapsed = t0.elapsed();
            let mut metrics = self.metrics.borrow_mut();
            metrics.record(self.id, elapsed);
            metrics.record_region_latency(self.region, elapsed);
        }
    }
}

/// Per-group WAL `append` / memtable `apply` / WAL `fdatasync` elapsed time
/// (us), drained by [`Engine::take_group_phase_us`] once per dispatched group.
/// Observational only (slice 55): never persisted and never read on any
/// correctness path; it attributes a single-request stall to the append, the
/// apply, or the durability barrier (or, when all three are small, to time spent
/// off-CPU before dispatch).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GroupPhase {
    pub append_us: u64,
    pub apply_us: u64,
    pub fsync_us: u64,
}

/// One overlapped WAL-barrier job: a duplicated handle to the live WAL file the
/// worker must `fdatasync`. The handle shares the open file description with the
/// main writer, so flushing it covers every append the main writer made before
/// the job was submitted.
struct WalSyncJob {
    handle: File,
    /// Test-only: when armed, the worker reports an `fdatasync` failure instead
    /// of calling the OS, so the fail-closed barrier path is exercisable
    /// deterministically. Compiled out of the shipping API.
    #[cfg(test)]
    fail: bool,
}

impl WalSyncJob {
    fn sync(self) -> io::Result<()> {
        #[cfg(test)]
        {
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::Other, "injected overlapped WAL sync failure"));
            }
        }
        self.handle.sync_data()
    }
}

/// The worker's completion report for one barrier.
struct BarrierResult {
    result: io::Result<()>,
    /// The `fdatasync` elapsed time (us) measured on the worker thread.
    fsync_us: u64,
    /// The wall-clock instant the `fdatasync` returned on the worker thread.
    completed_at: Instant,
}

/// Observed completion of one overlapped WAL barrier (slice 56): the
/// worker-measured `fdatasync` elapsed time and the instant the sync returned.
/// The load driver uses `completed_at` (not its own drain time) as the group's
/// completion, so service latency includes the barrier but not the later
/// window-boundary drain.
#[derive(Clone, Copy, Debug)]
pub struct BarrierCompletion {
    pub fsync_us: u64,
    pub completed_at: Instant,
}

/// A single background thread that runs overlapped WAL barriers in FIFO order.
/// The FIFO queue is what preserves barrier ordering: the main writer submits
/// jobs in group order, the worker processes them one at a time, and completions
/// are reported back in the same order.
struct BarrierWorker {
    job_tx: mpsc::Sender<WalSyncJob>,
    result_rx: mpsc::Receiver<BarrierResult>,
}

impl BarrierWorker {
    fn spawn() -> Self {
        let (job_tx, job_rx) = mpsc::channel::<WalSyncJob>();
        let (result_tx, result_rx) = mpsc::channel::<BarrierResult>();
        std::thread::spawn(move || {
            for job in job_rx {
                let t0 = Instant::now();
                let result = job.sync();
                let fsync_us = t0.elapsed().as_micros() as u64;
                let completed_at = Instant::now();
                if result_tx.send(BarrierResult { result, fsync_us, completed_at }).is_err() {
                    break;
                }
            }
        });
        BarrierWorker { job_tx, result_rx }
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
            group_append_us: 0,
            group_apply_us: 0,
            group_fsync_us: 0,
            barrier_worker: None,
            barrier_in_flight: 0,
            layout: LayoutPolicy::declared_default(),
            region_mode: RegionMode::Global,
            key_encoding: keys::KeyEncoding::FamilyFirst,
            metrics: RefCell::new(Metrics::new()),
            last_compaction: None,
        })
    }

    /// Recover wholly from WAL under the default [`keys::KeyEncoding::FamilyFirst`];
    /// does not automatically discover tile candidates.
    pub fn open(wal_path: impl AsRef<Path>) -> io::Result<Engine> {
        Self::open_impl(wal_path.as_ref(), Vec::new(), None, keys::KeyEncoding::FamilyFirst)
    }

    /// [`Engine::open`] with an explicit key encoding. The WAL holds ops, not
    /// keys, so replay re-encodes them and must use the same spelling the data
    /// was written with; the encoding is not persisted.
    pub fn open_with_encoding(wal_path: impl AsRef<Path>, encoding: keys::KeyEncoding) -> io::Result<Engine> {
        Self::open_impl(wal_path.as_ref(), Vec::new(), None, encoding)
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
        Self::open_impl(wal_path.as_ref(), vec![TileHandle { tile, name }], None, keys::KeyEncoding::FamilyFirst)
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
        Self::open_discover_with_encoding(dir, keys::KeyEncoding::FamilyFirst)
    }

    /// [`Engine::open_discover`] with an explicit key encoding. Required to
    /// reopen a database whose WAL records were written under
    /// [`keys::KeyEncoding::EntityFirst`]: replay re-encodes the ops, so the
    /// encoding is a recovery input, not a persisted fact (slice 50).
    pub fn open_discover_with_encoding(dir: impl AsRef<Path>, encoding: keys::KeyEncoding) -> io::Result<Engine> {
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
        let mut e = Self::open_impl(&dir.join(wal_name), tiles, segment.as_ref().map(|s| s.start_csn), encoding)?;
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
    fn open_impl(wal_path: &Path, tiles: Vec<TileHandle>, floor: Option<u64>, encoding: keys::KeyEncoding) -> io::Result<Engine> {
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
            for op in &ops { apply_op(destination, op, rec_csn, encoding); }
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
        // Every active tile is now verified against the WAL projection. That
        // verification built each tile's expected-version map and streamed the
        // tile's entries, so the peak live set during open is bounded by the
        // retained WAL suffix (the replayed `records`, the `covered` projection
        // and the per-tile `newest_projection`/`expect` copies), not by total
        // data. Release the suffix temporaries, then return the freed arena pages
        // to the OS: glibc keeps them, so without this the open-time peak stays
        // resident afterwards - and a cgroup `MemoryMax` counts it. Measured on a
        // rotated 6-tile / 118 MB instance (16 MiB checkpoint+rotate): open-time
        // peak (VmHWM) 88 MB, live RSS 5.3 MB with this trim (82 MB resident with
        // the trim omitted). Allocator hygiene only: no format, read, write or
        // durability change.
        drop(records);
        drop(covered);
        Self::trim_retained_heap();
        Ok(Engine { wal, mt, tiles, wal_path: wal_path.to_path_buf(), dir: parent_dir(wal_path), manifest: None, segment: None, segment_suffix_csn: csn, wal_detached: false, csn, dedup, max_handle, group_open: false, group_pending: false, group_append_us: 0, group_apply_us: 0, group_fsync_us: 0, barrier_worker: None, barrier_in_flight: 0, layout: LayoutPolicy::declared_default(), region_mode: RegionMode::Global, key_encoding: encoding, metrics: RefCell::new(Metrics::new()), last_compaction: None })
    }

    /// The CSN floor below which the active WAL file holds no records: the
    /// segment start CSN once rotation exists, 0 while the full history is
    /// retained in `data.wal`.
    fn wal_floor(&self) -> u64 {
        self.segment.as_ref().map(|s| s.start_csn).unwrap_or(0)
    }

    /// Return freed-but-retained heap back to the OS after open-time verification.
    ///
    /// `open_impl` streams every active tile's entries and builds each tile's
    /// projected `expect` map to check it against the WAL, so the peak allocated
    /// during open can exceed the live engine's own footprint. glibc keeps freed
    /// small/medium allocations in per-thread arenas and does not return those
    /// pages to the kernel, so that transient peak stays resident (RSS) after
    /// open returns - and a cgroup `MemoryMax` counts it. `malloc_trim(0)` returns
    /// the retained free pages; it is pure allocator hygiene and changes neither
    /// the on-disk format nor any read, write, or durability semantics, only RSS.
    /// It is a no-op where the symbol is absent (non-glibc targets).
    fn trim_retained_heap() {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            extern "C" {
                fn malloc_trim(pad: usize) -> std::ffi::c_int;
            }
            malloc_trim(0);
        }
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

    /// Refuse a mutating operation that must not race an overlapped WAL barrier.
    /// While a barrier is in flight the WAL has appended but not-yet-synced
    /// bytes, so a synchronous commit, republish, rotation or compaction could
    /// bind durable state to bytes the barrier has not yet covered (or, for
    /// rotation, rebind the WAL away from a live barrier). The overlapped path
    /// itself (`begin_group`/`commit_block_grouped`/`submit_group`/
    /// `wait_group_barrier`) is the only one allowed with barriers in flight.
    fn ensure_no_pending_barrier(&self, op: &str) -> io::Result<()> {
        if self.barrier_in_flight > 0 {
            return Err(io::Error::new(io::ErrorKind::Other, format!(
                "cannot {op} while a WAL barrier is in flight; drain barriers first")));
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
        self.ensure_no_pending_barrier("build_tile")?;
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
                    let (key, value) = op_kv(op, csn, self.key_encoding);
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
        self.ensure_no_pending_barrier("publish_tile")?;
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

    /// [`Engine::manifest_publish`] without the manifest file and parent-
    /// directory syncs, for a lazy compaction flip (see
    /// [`Engine::apply_compaction_job`]). The in-memory root still flips and the
    /// next durable publish carries it. Callers must have already made any tile
    /// the page references durable (file and directory entry), so OS writeback of
    /// the page cannot name a missing tile; a crash before a durable publish can
    /// still revert to the previous durable root whose tiles are all still
    /// present.
    fn manifest_publish_lazy(&mut self, refs: Vec<TileRef>, checkpoint: Checkpoint, segment: Option<SegmentRef>) -> io::Result<()> {
        match &mut self.manifest {
            Some(m) => m.publish_lazy(refs, checkpoint, segment)?,
            None => {
                let path = self.dir.join(manifest::MANIFEST_NAME);
                let mut m = match Manifest::create(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        match Manifest::open(&path)? {
                            Some(m) if m.root().tiles.is_empty() => m,
                            _ => return Err(io::Error::new(io::ErrorKind::InvalidData,
                                "manifest already exists with active tiles; use open_discover to adopt them before publishing")),
                        }
                    }
                    Err(e) => return Err(e),
                };
                m.publish_lazy(refs, checkpoint, segment)?;
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
        // A no-op rotation (no tiles, or below the threshold) must not refuse an
        // in-flight barrier: the overlapped pacer calls this on every group while
        // a barrier may legitimately be in flight. The refusal is only owed when
        // this call will actually rebind the WAL away from a live barrier.
        self.ensure_no_pending_barrier("rotate_wal")?;
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
        self.ensure_no_pending_barrier("compact")?;
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
    ///
    /// Under [`RegionMode::PerRegion`] the run is selected per region (never
    /// across regions) and a region is eligible only once its own tile count
    /// reaches its [`LayoutPolicy`] merge threshold - the slice-27 actuator. The
    /// `max_tiles` gate below still decides whether compaction runs at all; a
    /// region whose threshold is not yet reached is simply skipped, so a high
    /// threshold defers that region's merges while an eager neighbour keeps
    /// compacting.
    pub fn maybe_compact_shape(&mut self, max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> io::Result<Option<u64>> {
        if self.tiles.len() < max_tiles || self.tiles.len() < 2 {
            return Ok(None);
        }
        self.ensure_no_group("maybe_compact_shape")?;
        self.ensure_no_pending_barrier("maybe_compact_shape")?;
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

    /// Select the same run [`Engine::maybe_compact_shape`] would merge and
    /// return it as a self-contained [`CompactionJob`] WITHOUT doing any merge.
    /// `Ok(None)` when no region/run is eligible (the same gate as
    /// [`Engine::maybe_compact_shape`]). The job carries a duplicate handle to
    /// the live WAL file bounded by the current valid length, so a worker can
    /// build the replacement off the request path while the engine keeps
    /// committing.
    pub fn compaction_job(&self, max_merge_bytes: u64, max_tiles: usize, shape: CompactShape) -> io::Result<Option<CompactionJob>> {
        if self.tiles.len() < max_tiles || self.tiles.len() < 2 {
            return Ok(None);
        }
        self.ensure_no_group("compaction_job")?;
        self.ensure_no_pending_barrier("compaction_job")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
        let (indices, region) = if self.region_mode == RegionMode::PerRegion {
            let Some(indices) = self.select_region_run(max_merge_bytes, max_tiles, shape)? else {
                return Ok(None);
            };
            let region = tile_first_region(&self.tiles[indices[0]]).map(<[u8]>::to_vec);
            (indices, region)
        } else {
            let (start, len) = self.select_run(max_merge_bytes, max_tiles, shape)?;
            ((start..start + len).collect::<Vec<usize>>(), None)
        };
        let newest = *indices.last().unwrap();
        let cutoff = self.tiles[newest].cutoff();
        let lower = if region.is_some() {
            (0..indices[0])
                .rev()
                .find(|&i| tile_first_region(&self.tiles[i]).map(<[u8]>::to_vec) == region)
                .map(|i| self.tiles[i].cutoff())
                .unwrap_or(0)
        } else if indices[0] == 0 {
            0
        } else {
            self.tiles[indices[0] - 1].cutoff()
        };
        // The replacement name follows the synchronous merge's collision rule.
        let base = format!("compact-{cutoff}.tile");
        let output_name = if self.tiles[newest].name == base || self.dir.join(&base).try_exists()? {
            self.unique_compaction_name(cutoff)?
        } else {
            base
        };
        let layout_region = match (self.region_mode, &region, self.tiles[indices[0]].tile.first_key().and_then(keys::region_prefix)) {
            (RegionMode::PerRegion, Some(_), Some(prefix)) => keys::region_name(prefix),
            _ => TILE_LAYOUT_REGION.to_string(),
        };
        let (page_size, bloom_bits_per_key) = self.layout.for_region(&layout_region);
        Ok(Some(CompactionJob {
            input_names: indices.iter().map(|&i| self.tiles[i].name.clone()).collect(),
            lower,
            cutoff,
            region,
            output_name,
            page_size,
            bloom_bits_per_key,
            key_encoding: self.key_encoding,
            wal_file: self.wal.file_clone()?,
            wal_valid_len: self.wal.valid_len(),
            wal_max_expected: if self.segment.is_some() { self.segment_suffix_csn } else { self.csn },
            wal_csn: self.csn,
            wal_floor: self.wal_floor(),
        }))
    }

    /// Apply a worker-built [`CompactionOutcome`] produced from this engine's
    /// [`CompactionJob`]: flip the manifest root and the in-memory tile list to
    /// install the replacement, and record the per-merge observability. The flip
    /// is LAZY (no manifest file/dir sync) and the retired inputs are NOT
    /// unlinked: the build side already made the replacement tile and its
    /// directory entry durable, and the WAL stays the durability authority, so
    /// even an OS writeback of the lazy manifest page can only reference a
    /// durable tile. A crash before the next durable manifest publish
    /// (checkpoint/rotation) simply reverts to the previous durable root whose
    /// tiles are still on disk. Returns the replacement's output size.
    pub fn apply_compaction_job(&mut self, outcome: &CompactionOutcome) -> io::Result<u64> {
        self.ensure_no_group("apply_compaction_job")?;
        self.ensure_no_pending_barrier("apply_compaction_job")?;
        if self.manifest.as_ref().is_some_and(|m| m.poisoned()) {
            return Err(io::Error::new(io::ErrorKind::Other, "manifest writer poisoned; reopen to recover"));
        }
        self.ensure_attached()?;
        // Resolve the retired run against the CURRENT list. Tiles are only ever
        // appended between job creation and apply, so the named run is still
        // present; an absent name is a caller error and fails closed.
        let mut indices: Vec<usize> = Vec::with_capacity(outcome.input_names.len());
        for name in &outcome.input_names {
            let i = self.tiles.iter().position(|t| &t.name == name)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "compaction job retired a tile no longer active"))?;
            indices.push(i);
        }
        let replacement_tile = Tile::open(&self.dir.join(&outcome.name))?;
        // Defense in depth: the worker already verified the tile against the WAL
        // and the inputs, but re-bind the opened tile's own header to the outcome
        // before it is installed, so a swapped or stale file cannot be flipped in.
        if replacement_tile.cutoff() != outcome.cutoff || replacement_tile.digest() != &outcome.digest {
            return Err(io::Error::new(io::ErrorKind::InvalidData,
                "compaction replacement tile disagrees with the built outcome"));
        }
        let replacement = TileRef { name: outcome.name.clone(), cutoff: outcome.cutoff, digest: outcome.digest };
        let checkpoint = Checkpoint { csn: self.tiles.last().unwrap().cutoff(), handle_watermark: self.max_handle };
        let segment = self.segment.clone();
        if outcome.region.is_some() {
            // Per-region: rebuild the full `(cutoff, region)`-ordered list with
            // the replacement inserted at its sort position (same as
            // `merge_region_run`).
            let region = outcome.region.clone();
            let retired: HashSet<usize> = indices.iter().copied().collect();
            let pos = self.tiles.iter().enumerate()
                .filter(|(i, t)| !retired.contains(i)
                    && (t.cutoff(), tile_first_region(t).map(<[u8]>::to_vec)) < (outcome.cutoff, region.clone()))
                .count();
            let mut refs: Vec<TileRef> = Vec::with_capacity(self.tiles.len() - indices.len() + 1);
            for (i, t) in self.tiles.iter().enumerate() {
                if !retired.contains(&i) { refs.push(t.tile_ref()); }
            }
            refs.insert(pos, replacement.clone());
            self.manifest_publish_lazy(refs, checkpoint, segment)?;
            let mut new_tile = Some(replacement_tile);
            let mut kept: Vec<TileHandle> = Vec::with_capacity(self.tiles.len() - indices.len() + 1);
            for (i, t) in self.tiles.drain(..).enumerate() {
                if retired.contains(&i) { continue; }
                if kept.len() == pos {
                    kept.push(TileHandle { tile: new_tile.take().expect("replacement inserted once"), name: outcome.name.clone() });
                }
                kept.push(t);
            }
            if let Some(t) = new_tile { kept.push(TileHandle { tile: t, name: outcome.name.clone() }); }
            self.tiles = kept;
            if let Some(region) = &region {
                let id = self.metrics.borrow_mut().resolve_region(region);
                self.metrics.borrow_mut().region_compaction(id, outcome.output_bytes);
            }
        } else {
            // Whole-list: the retired names are one contiguous window; replace it.
            let start = indices[0];
            let len = indices.len();
            let retire: Vec<&str> = outcome.input_names.iter().map(String::as_str).collect();
            self.manifest.as_mut().unwrap().publish_replacing_lazy(&retire, replacement, checkpoint, segment)?;
            let retired: Vec<TileHandle> = self.tiles.drain(start..start + len).collect();
            self.tiles.insert(start, TileHandle { tile: replacement_tile, name: outcome.name.clone() });
            debug_assert!(retired.len() == len);
        }
        self.last_compaction = Some(outcome.stats);
        Ok(outcome.output_bytes)
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
    /// region is eligible. Region-partitioned compaction never merges across
    /// regions: each region's tiers are its own, so a per-region trigger is
    /// meaningful. A region is eligible only when its OWN tile count reaches its
    /// `LayoutPolicy` merge threshold ([`LayoutPolicy::merge_min_tiles_for_region`],
    /// today 2); among eligible regions the smallest combined run wins (ties: the
    /// oldest list index). The run is selected by the same [`CompactShape`] policy
    /// applied to that region's own cutoff-ordered sizes.
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
        for (region, idxs) in &groups {
            let threshold = self.layout.merge_min_tiles_for_region(&keys::region_name(region));
            if idxs.len() < 2 || idxs.len() < threshold as usize { continue; }
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
        let t0 = Instant::now();
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
        let mut input_bytes: u64 = 0;
        for t in &inputs {
            input_bytes = input_bytes.saturating_add(fs::metadata(self.dir.join(&t.name)).map(|m| m.len())?);
        }
        let (tile, build) = match self.write_compacted_tile(&path, &inputs, lower, cutoff, None) {
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
        let dir_sync_t0 = Instant::now();
        File::open(&self.dir)?.sync_all()?;
        let dir_sync_us = dir_sync_t0.elapsed().as_micros() as u64;
        // 3. Manifest protocol: the retired run is replaced by the one new tile.
        //    The global newest cutoff (and therefore the checkpoint) is unchanged:
        //    tiles after the run keep the frontier, or the replacement itself
        //    carries it when the run reached the end. `publish_replacing` accepts
        //    any contiguous window, not just a prefix.
        let manifest_t0 = Instant::now();
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
        let manifest_us = manifest_t0.elapsed().as_micros() as u64;
        // 5. Only now drop the retired input files. Unlink after the durable flip
        //    is safe: open fds keep working (POSIX) and the new root no longer
        //    references them. Failures are best-effort ignored — the compaction
        //    is already durable and a leftover file is a harmless orphan that
        //    discovery ignores and never adopts.
        let unlink_t0 = Instant::now();
        for old in &retired {
            let _ = fs::remove_file(self.dir.join(&old.name));
        }
        let unlink_us = unlink_t0.elapsed().as_micros() as u64;
        self.last_compaction = Some(CompactionStats {
            input_bytes,
            output_bytes: out_size,
            elapsed_us: t0.elapsed().as_micros() as u64,
            build,
            dir_sync_us,
            manifest_us,
            unlink_us,
        });
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
    /// `(cutoff, region)` position; the durable protocol mirrors
    /// [`Engine::merge_run`]. The WAL suffix is restricted to this run's region (a
    /// region run must not adopt another region's keys), and the retired input
    /// files are unlinked only after the manifest flip, exactly as `merge_run`
    /// does. An unlink failure is reported, not returned: the manifest is already
    /// durable and correct, so the only residue is an on-disk orphan that
    /// discovery ignores and never adopts (the directory then holds the correct
    /// active set plus that unreferenced file).
    fn merge_region_run(&mut self, indices: &[usize]) -> io::Result<u64> {
        debug_assert!(indices.len() >= 2 && indices.windows(2).all(|w| w[0] < w[1]));
        let t0 = Instant::now();
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
        let mut input_bytes: u64 = 0;
        for t in &inputs {
            input_bytes = input_bytes.saturating_add(fs::metadata(self.dir.join(&t.name)).map(|m| m.len())?);
        }
        let (tile, build) = match self.write_compacted_tile(&path, &inputs, lower, cutoff, region.as_deref()) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(io::Error::new(io::ErrorKind::AlreadyExists, format!(
                    "compaction candidate already exists and is unreferenced by the manifest (leftover from a failed compaction or crash after tile sync); remove {} and retry", path.display())));
            }
            Err(e) => return Err(e),
        };
        let out_size = fs::metadata(&path)?.len();
        let dir_sync_t0 = Instant::now();
        File::open(&self.dir)?.sync_all()?;
        let dir_sync_us = dir_sync_t0.elapsed().as_micros() as u64;
        let manifest_t0 = Instant::now();
        let retired: HashSet<usize> = indices.iter().copied().collect();
        let retired_names: Vec<String> = indices.iter().map(|&i| self.tiles[i].name.clone()).collect();
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
        let manifest_us = manifest_t0.elapsed().as_micros() as u64;
        // Observability only (slice 34): attribute this region merge's output tile
        // bytes and the merge itself to the region that was merged, so the learner
        // can derive the per-region rewrite factor (`metrics.rs` module docs).
        // Nothing reads these counters to make a decision; disabled mode and a
        // failed merge record nothing (this runs after the durable flip).
        if let Some(region) = &region {
            let id = self.metrics.borrow_mut().resolve_region(region);
            self.metrics.borrow_mut().region_compaction(id, out_size);
        }
        // Only now drop the retired input files: the durable flip already removed
        // them from the root, so a leftover can never be adopted, only ignored.
        // Report a failure rather than swallow it - `merge_run`'s silent best-effort
        // is fine for a whole-dataset merge, but a per-region compaction should say
        // when it leaves an orphan and what the directory then is.
        let unlink_t0 = Instant::now();
        for old in &retired_names {
            if let Err(e) = fs::remove_file(self.dir.join(old)) {
                if e.kind() != io::ErrorKind::NotFound {
                    eprintln!("flashdb: region compaction retired {old} from the manifest but could not unlink it: {e}; the file is now an orphan discovery ignores");
                }
            }
        }
        let unlink_us = unlink_t0.elapsed().as_micros() as u64;
        self.last_compaction = Some(CompactionStats {
            input_bytes,
            output_bytes: out_size,
            elapsed_us: t0.elapsed().as_micros() as u64,
            build,
            dir_sync_us,
            manifest_us,
            unlink_us,
        });
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

    /// Phase split of the most recent successful compaction actuation, if any
    /// (observability hook for the slice-53 latency-tail diagnosis). `None`
    /// until the first successful [`Engine::maybe_compact_shape`] merge.
    pub fn last_compaction_stats(&self) -> Option<CompactionStats> {
        self.last_compaction
    }

    /// Each active tile's region name in list order (inspection/benchmark hook),
    /// derived from the tile's own first key, so it is correct for both layouts.
    /// `None` for a whole-keyspace (Global) tile or an empty one.
    pub fn active_tile_regions(&self) -> Vec<Option<String>> {
        self.tiles.iter().map(|t| tile_first_region(t).map(keys::region_name)).collect()
    }

    /// Each active tile's stored-key fence range `(first_key, last_key)` in list
    /// order (inspection/measurement hook). `None` for an empty tile or an
    /// untrusted final block. Lets a caller place a key in the tile that owns it
    /// without reading the tile body.
    pub fn active_tile_key_ranges(&self) -> Vec<(Option<Vec<u8>>, Option<Vec<u8>>)> {
        self.tiles.iter().map(|t| (t.tile.first_key().map(<[u8]>::to_vec), t.tile.last_key().map(<[u8]>::to_vec))).collect()
    }

    /// Sum of the active tiles' on-disk file lengths (inspection/benchmark hook).
    /// Unlike a directory glob this counts only manifest-referenced tiles, so it
    /// is not inflated by orphaned files a crash or failed compaction left behind.
    pub fn active_tile_bytes(&self) -> io::Result<u64> {
        self.tiles.iter().try_fold(0u64, |acc, t| Ok(acc.saturating_add(fs::metadata(self.dir.join(&t.name))?.len())))
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
    /// `region` restricts the WAL-suffix map to one `(family, entity)` region and
    /// is `Some` exactly when the run is region-partitioned. Without it the map is
    /// the GLOBAL `(lower, cutoff]` window, which is correct for a whole-keyspace
    /// run but would inject every other region's ops into a region run whose
    /// `lower` reaches that region's oldest tile (`lower = 0` or the floor). The
    /// running tiles are not region-filtered: they are the run's own input and a
    /// floor-spanning one legitimately carries below-floor entries. The filter is
    /// applied only to the WAL suffix (which the rotation window keeps above the
    /// floor), so the below-floor part of the projection is preserved unchanged; it
    /// merely must belong to `region`, as the tiles do. The digest stays global: it
    /// binds `(floor, cutoff]` regardless of region, matching a publish at `cutoff`.
    ///
    /// Nothing here materializes the union: the tiles are streamed by one
    /// [`TileCursor`] each (borrowing a single 4 KiB block apiece) and only the
    /// WAL suffix - already bounded by the rotation window - is copied into a
    /// key-ordered map. [`Engine::stream_merged_reduction`] merges both sources
    /// and pushes the reduced stream straight into the streaming writer, so peak
    /// compaction RAM tracks the WAL window, not the total tile bytes.
    fn write_compacted_tile(&self, tile_path: &Path, tiles: &[&TileHandle], lower: u64, cutoff: u64, region: Option<&[u8]>) -> io::Result<(Tile, CompactionBuildStats)> {
        let stream_t0 = Instant::now();
        let (records, _) = wal::replay(&self.wal_path)?;
        self.verify_wal_matches_state(&records)?;
        let digest = wal_digest(&records, cutoff, self.wal_floor())?;
        // Source (b): the retained WAL suffix inside `(lower, cutoff]`, key-ordered
        // by construction. A later op overwrites an earlier one for the exact same
        // key, matching `Memtable::apply`. Records at or below `lower` are carried
        // by the run's own tiles (or an earlier, still-active tile) and must not be
        // merged here, or they would escape the run's range. A region run keeps only
        // its own region's records so the output is EXACTLY that region's
        // projection: another region's key has no tile in this run to carry it and
        // must not be adopted here.
        let mut wal_entries: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
        for payload in &records {
            let (csn, _, ops) = decode_payload(payload)?;
            if csn > lower && csn <= cutoff {
                for op in &ops {
                    let (key, value) = op_kv(op, csn, self.key_encoding);
                    if region.is_some_and(|r| keys::region_prefix(&key) != Some(r)) { continue; }
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
        Self::stream_merged_reduction(tiles, lower, cutoff, &wal_entries, |key, value| {
            writer.push(key, value)?;
            if let Some(bloom) = bloom.as_mut() {
                if let Some(prefix) = unique_prefix_of(key) { bloom.insert(prefix); }
            }
            Ok(())
        })?;
        if let Some(bloom) = bloom { writer.set_bloom(bloom.finish())?; }
        let stream_us = stream_t0.elapsed().as_micros() as u64;
        let finish_t0 = Instant::now();
        let tile = writer.finish()?;
        let finish_us = finish_t0.elapsed().as_micros() as u64;
        // Fail closed: re-run the identical merge and compare it 1:1, streaming,
        // against the written tile. That catches encoder/streaming defects but is
        // self-referential (same reduction, same inputs), so it is paired with an
        // INDEPENDENT check against the WAL-derived expectation below.
        let verify_t0 = Instant::now();
        Self::verify_compacted_tile(&tile, tiles, lower, &wal_entries, cutoff)?;
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
        Self::verify_compacted_against_wal(&tile, &wal_entries, lower.max(self.wal_floor()), cutoff)?;
        let verify_us = verify_t0.elapsed().as_micros() as u64;
        Ok((tile, CompactionBuildStats { stream_us, finish_us, verify_us }))
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
        Self::stream_merged_reduction(tiles, lower, cutoff, wal_entries, &mut compare)?;
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
    /// compaction) consult, including the per-region merge threshold the
    /// automatic compaction trigger reads. The default is
    /// [`LayoutPolicy::declared_default`].
    /// Because the policy is not persisted, setting it affects only tiles written
    /// after this call and the future merge schedule; every existing tile keeps
    /// the parameters in its own
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

    /// Select the key spelling new keys are written with (slice 50). The default
    /// [`keys::KeyEncoding::FamilyFirst`] reproduces today's keyspace byte for
    /// byte. [`keys::KeyEncoding::EntityFirst`] writes one entity's `P`/`U`/`R`
    /// keys adjacently, so under [`RegionMode::PerRegion`] the three land in one
    /// tile. Not persisted: reads accept both spellings, but WAL replay
    /// re-encodes ops, so a later reopen must pass the same encoding to
    /// [`Engine::open_with_encoding`]/[`Engine::open_discover_with_encoding`].
    pub fn set_key_encoding(&mut self, encoding: keys::KeyEncoding) { self.key_encoding = encoding; }

    /// The engine's current key encoding (inspection/measurement hook).
    pub fn key_encoding(&self) -> keys::KeyEncoding { self.key_encoding }

    // K-way merge of the serving memtable and every published tile, all
    // traversed in global key order. Keys encode CSN descending, so each logical
    // key's versions arrive newest-first and the caller's first visible entry
    // wins — the only state reads observe (2026-09-29: no historical reads).
    // Tile CSN ranges are disjoint, so cross-tier equal keys are impossible;
    // the memtable-wins skip below is defensive only.
    //
    // `region` is the metrics region id this call serves (or [`metrics::DISABLED`]
    // for a whole-keyspace pass). Every data-block byte the tile cursors read is
    // attributed to it once the merge ends, on both the success and the error
    // path, so a bloom miss that still reads a block is visible as `bytes_read`.
    fn scan_merged(&self, prefix: &[u8], region: usize, mut visit: impl FnMut(&[u8], &[u8]) -> io::Result<bool>) -> io::Result<()> {
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
        let result = merge_streams(&mut mem, &mut cursors, &mut visit);
        if region != metrics::DISABLED {
            let bytes: u64 = cursors.iter().map(|c| c.bytes_read()).sum();
            self.metrics.borrow_mut().region_bytes_read(region, bytes);
        }
        result
    }

    /// 已发布的最新 CSN。读只看最新提交状态（不存在历史读），发布检查亦然。
    pub fn csn(&self) -> u64 {
        self.csn
    }

    /// The live WAL's valid byte length (excluding any torn tail). Observation
    /// only: the overlapped load driver uses it to decide when a bounded-WAL
    /// maintenance call will actually publish/rotate, so it can drain in-flight
    /// barriers first instead of refusing below the threshold.
    pub fn wal_valid_len(&self) -> u64 {
        self.wal.valid_len()
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

    /// The metrics region id of an operation's prefix, or [`metrics::DISABLED`]
    /// when observation is off or the prefix names no `P`/`U`/`R` region. The
    /// region is taken from the prefix bytes, so a probe and a commit op on the
    /// same `(family, entity)` resolve to the same row.
    fn region_of_prefix(&self, prefix: &[u8]) -> usize {
        match keys::region_prefix(prefix) {
            Some(region) => self.metrics.borrow_mut().resolve_region(region),
            None => metrics::DISABLED,
        }
    }

    /// Attribute one appended WAL record's ops to their regions. `bytes_written`
    /// is each op's exact encoded size in `encode_payload` (the shared record
    /// header is not attributed); a region touched by the block gets one
    /// `commit_blocks` and its op count. Allocates one `Vec` proportional to the
    /// block's op count and nothing after region warm-up; the commit path already
    /// allocates per op, and this is off the read path entirely.
    fn record_commit_regions(&self, ops: &[Op]) {
        if !self.metrics.borrow().enabled() {
            return;
        }
        let mut touched: Vec<usize> = Vec::with_capacity(ops.len());
        for op in ops {
            let (marker, entity) = op_region(op, self.key_encoding);
            let id = self.metrics.borrow_mut().resolve_region_parts(marker, entity);
            let bytes = op_encoded_len(op);
            self.metrics.borrow_mut().region_bytes_written(id, bytes);
            self.metrics.borrow_mut().region_commit_op(id);
            touched.push(id);
        }
        touched.sort_unstable();
        touched.dedup();
        for id in touched {
            self.metrics.borrow_mut().region_commit_block(id);
        }
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
        let mut sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
        check_name(entity)?;
        check_name(field)?;
        let pfx = keys::reverse_prefix_with(self.key_encoding, entity, field, target)?;
        let region = self.region_of_prefix(&pfx);
        sample.region = region;
        let mut newest: HashMap<u64, bool> = HashMap::new();
        self.scan_merged(&pfx, region, |key, value| {
            let Some((_, source, _)) = keys::decode_reverse(key) else { return Ok(true); };
            // 键序内同一 source 按 CSN 降序排列，首个条目即最新状态。
            newest.entry(source).or_insert(!value.is_empty());
            Ok(true)
        })?;
        let mut out: Vec<u64> = newest.into_iter().filter(|(_, alive)| *alive).map(|(s, _)| s).collect();
        out.sort();
        self.metrics.borrow_mut().region_probe(region, !out.is_empty());
        Ok(out)
    }

    /// 按已声明唯一字段解析句柄（最新提交状态，无历史读）。
    /// 如观察到多个活 owner，则索引已损坏，不能任意选一个。
    pub fn unique_lookup(&self, entity: &[u8], field: &[u8], value: &[u8]) -> io::Result<Option<u64>> {
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_unique(entity, field);
        let mut sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
        let pfx = keys::unique_prefix_with(self.key_encoding, entity, field, value)?;
        let region = self.region_of_prefix(&pfx);
        sample.region = region;
        let mut seen = std::collections::HashSet::new();
        let mut owner = None;
        self.scan_merged(&pfx, region, |key, val| {
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
        self.metrics.borrow_mut().region_probe(region, owner.is_some());
        Ok(owner)
    }

    /// 当前状态点读：只看最新提交版本（2026-09-29：不存在历史读），
    /// tombstone → 不存在。
    pub fn get(&self, entity: &[u8], handle: u64) -> io::Result<Option<Vec<u8>>> {
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_primary(entity);
        let mut sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
        check_name(entity)?;
        let pfx = keys::primary_prefix_with(self.key_encoding, entity, handle)?;
        let region = self.region_of_prefix(&pfx);
        sample.region = region;
        let mut result = None;
        self.scan_merged(&pfx, region, |key, value| {
            if keys::key_csn(key).is_some() {
                result = if value.is_empty() { None } else { Some(value.to_vec()) };
                return Ok(false);
            }
            Ok(true)
        })?;
        self.metrics.borrow_mut().region_probe(region, result.is_some());
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
        let mut sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
        check_name(entity)?;
        let mut prefix = keys::primary_prefix_with(self.key_encoding, entity, 0)?;
        prefix.truncate(prefix.len() - 8);
        let region = self.region_of_prefix(&prefix);
        sample.region = region;
        let mut last_handle = None;
        self.scan_merged(&prefix, region, |key, value| {
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
        self.scan_merged(&[], metrics::DISABLED, |key, value| {
            visit(key, value)?;
            Ok(true)
        })
    }

    /// 原子提交一个块。唯一约束冲突 → `Outcome::Conflict`（未写 WAL，调用方直接报业务错误）。
    pub fn commit_block(&mut self, block_id: &[u8], ops: &[Op]) -> io::Result<Outcome> {
        self.ensure_no_group("commit_block")?;
        self.ensure_no_pending_barrier("commit_block")?;
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
        // The synchronous barrier path must not race an overlapped barrier still
        // in flight: a synchronous `wal.sync()` would cover another group's
        // appended-but-unacknowledged bytes out of order (and the overlapped path
        // is the only one allowed to run with barriers in flight).
        self.ensure_no_pending_barrier("end_group")?;
        let result = if self.group_pending {
            let t0 = Instant::now();
            let r = self.wal.sync();
            self.group_fsync_us = self.group_fsync_us.saturating_add(t0.elapsed().as_micros() as u64);
            r
        } else { Ok(()) };
        self.group_open = false;
        self.group_pending = false;
        result
    }

    /// Per-group WAL `append` / memtable `apply` / WAL `fdatasync` elapsed time
    /// (us) accumulated since the previous call, then reset. Observational only
    /// (slice 55): the load driver drains this once per dispatched group so a
    /// single-request stall can be attributed to the WAL append, the memtable
    /// apply, or the durability barrier — or, when all three are small, to time
    /// spent off-CPU before the group was dispatched.
    pub fn take_group_phase_us(&mut self) -> GroupPhase {
        let phase = GroupPhase {
            append_us: self.group_append_us,
            apply_us: self.group_apply_us,
            fsync_us: self.group_fsync_us,
        };
        self.group_append_us = 0;
        self.group_apply_us = 0;
        self.group_fsync_us = 0;
        phase
    }

    /// Per-group WAL `append` + memtable `apply` elapsed time (us) since the
    /// previous call, then reset — the overlapped-barrier path drains this at
    /// SUBMIT time, before group N+1's appends mix into the counters. The
    /// barrier's own `fdatasync` elapsed is returned separately by
    /// [`Engine::wait_group_barrier`] / [`Engine::try_take_group_barrier`], so
    /// the two are never confused across groups.
    pub fn take_group_append_apply_us(&mut self) -> (u64, u64) {
        let phase = (self.group_append_us, self.group_apply_us);
        self.group_append_us = 0;
        self.group_apply_us = 0;
        phase
    }

    /// Submit the open group's WAL barrier to the overlapped barrier worker and
    /// close the group (slice 56).
    ///
    /// Returns `Ok(true)` when a barrier was submitted: the group performed at
    /// least one grouped commit, so an `fdatasync` covering its appends is owed.
    /// Returns `Ok(false)` for an empty/read-only group: nothing was appended,
    /// so no barrier is owed and the group is already complete. In both cases
    /// the group is closed (so `submit_group` twice fails, exactly like
    /// `end_group` twice).
    ///
    /// The barrier runs on a single background worker, FIFO in submission
    /// order, so the request path may begin and append group N+1 while group N's
    /// `fdatasync` is in flight — but a request is NEVER durable until its own
    /// covering barrier completes and is drained
    /// ([`Engine::wait_group_barrier`] / [`Engine::try_take_group_barrier`]).
    /// At most [`MAX_PENDING_BARRIERS`] barriers may be in flight; when the cap
    /// is reached this call fails closed WITHOUT closing the group, so the
    /// caller can drain barriers and retry. A failed submission (poisoned WAL,
    /// detached WAL, or a dead worker) poisons the WAL exactly like a failed
    /// synchronous barrier.
    pub fn submit_group(&mut self) -> io::Result<bool> {
        if !self.group_open {
            return Err(io::Error::new(io::ErrorKind::Other, "no commit group is open; begin_group first"));
        }
        let owed = self.group_pending;
        if owed {
            if self.barrier_in_flight >= MAX_PENDING_BARRIERS {
                // Leave the group OPEN: the caller must drain barriers and retry.
                // The group's commits are already appended to the WAL and applied
                // to the memtable, so closing it here would strand them with no
                // covering barrier owed and no way to retry. This is the one
                // failure that does NOT close the group.
                return Err(io::Error::new(io::ErrorKind::Other,
                    "WAL barrier in-flight cap reached; drain barriers before submitting more"));
            }
            let submitted = self.submit_overlapped_barrier();
            self.group_open = false;
            self.group_pending = false;
            submitted?;
            Ok(true)
        } else {
            self.group_open = false;
            self.group_pending = false;
            Ok(false)
        }
    }

    /// Enqueue the open group's WAL barrier on the FIFO worker and increment
    /// `barrier_in_flight`. Any failure poisons the WAL (or leaves it detached)
    /// so the caller fails closed exactly like a failed synchronous barrier; the
    /// caller still closes the group, because the poisoned writer refuses every
    /// further commit until reopen.
    fn submit_overlapped_barrier(&mut self) -> io::Result<()> {
        self.ensure_attached()?;
        let handle = self.wal.sync_handle()?;
        let worker = self.barrier_worker.get_or_insert_with(BarrierWorker::spawn);
        let job = WalSyncJob {
            handle,
            #[cfg(test)]
            fail: self.wal.take_barrier_error(),
        };
        match worker.job_tx.send(job) {
            Ok(()) => {
                self.barrier_in_flight += 1;
                Ok(())
            }
            Err(_) => {
                self.wal.poison();
                Err(io::Error::new(io::ErrorKind::Other, "WAL barrier worker disconnected"))
            }
        }
    }

    /// Number of submitted overlapped barriers whose completion has not yet been
    /// drained. The overlapped driver caps this below
    /// [`MAX_PENDING_BARRIERS`] by draining before submitting more.
    pub fn pending_barriers(&self) -> usize {
        self.barrier_in_flight
    }

    /// Block until the OLDEST in-flight overlapped barrier completes, drain it,
    /// and return its [`BarrierCompletion`]. A failed `fdatasync`
    /// poisons the WAL writer exactly like the synchronous path and is returned
    /// as `Err`; nothing is acknowledged, and every later append fails until
    /// reopen. Fails closed when no barrier is in flight.
    pub fn wait_group_barrier(&mut self) -> io::Result<BarrierCompletion> {
        let received = match self.barrier_worker.as_mut() {
            Some(worker) => worker.result_rx.recv(),
            None => return Err(io::Error::new(io::ErrorKind::Other, "no WAL barrier in flight")),
        };
        match received {
            Ok(result) => {
                self.barrier_in_flight = self.barrier_in_flight.saturating_sub(1);
                self.observe_barrier_result(result)
            }
            Err(_) => {
                self.barrier_in_flight = 0;
                self.wal.poison();
                Err(io::Error::new(io::ErrorKind::Other, "WAL barrier worker disconnected"))
            }
        }
    }

    /// Non-blocking variant of [`Engine::wait_group_barrier`]: drain the oldest
    /// completed barrier (if one is ready) and return its completion, or
    /// `Ok(None)` when no barrier has completed yet. A disconnected worker or a
    /// failed `fdatasync` fails closed exactly as the blocking form.
    pub fn try_take_group_barrier(&mut self) -> io::Result<Option<BarrierCompletion>> {
        let Some(worker) = self.barrier_worker.as_mut() else {
            return Ok(None);
        };
        match worker.result_rx.try_recv() {
            Ok(result) => {
                self.barrier_in_flight = self.barrier_in_flight.saturating_sub(1);
                self.observe_barrier_result(result).map(Some)
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.barrier_in_flight = 0;
                self.wal.poison();
                Err(io::Error::new(io::ErrorKind::Other, "WAL barrier worker disconnected"))
            }
        }
    }

    /// Fold one drained barrier completion into the engine: return the observed
    /// completion, and on failure poison the WAL writer so every later append
    /// fails closed (recovery required), exactly like a failed synchronous
    /// [`Engine::end_group`].
    fn observe_barrier_result(&mut self, result: BarrierResult) -> io::Result<BarrierCompletion> {
        match result.result {
            Ok(()) => Ok(BarrierCompletion { fsync_us: result.fsync_us, completed_at: result.completed_at }),
            Err(e) => {
                self.wal.poison();
                Err(e)
            }
        }
    }

    /// Shared single-block commit core. `barrier` selects whether the WAL
    /// `fdatasync` is taken before the block is published: the single-block path
    /// passes `true` (unchanged observable behaviour), the deferred-barrier group
    /// path passes `false` and relies on [`Engine::end_group`] for the one sync.
    fn commit_block_inner(&mut self, block_id: &[u8], ops: &[Op], barrier: bool) -> io::Result<Outcome> {
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_commit();
        let _sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
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
                    let pfx = keys::unique_prefix_with(self.key_encoding, entity, field, value)?;
                    let entry = touched.entry(pfx).or_insert_with(||
                        (entity.clone(), field.clone(), value.clone(), HashMap::new()));
                    entry.3.insert(*handle, matches!(op, Op::PutUnique { .. }));
                }
            }
        }
        for (pfx, (entity, field, value, overlay)) in touched {
            let mut live: HashMap<u64, bool> = HashMap::new();
            let region = self.region_of_prefix(&pfx);
            self.scan_merged(&pfx, region, |key, val| {
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
        let append_t0 = Instant::now();
        self.wal.append(&payload)?;
        self.group_append_us = self.group_append_us.saturating_add(append_t0.elapsed().as_micros() as u64);
        self.record_commit_regions(ops);
        if barrier {
            let sync_t0 = Instant::now();
            self.wal.sync()?;
            self.group_fsync_us = self.group_fsync_us.saturating_add(sync_t0.elapsed().as_micros() as u64);
        }

        let apply_t0 = Instant::now();
        for op in ops {
            apply_op(&mut self.mt, op, csn, self.key_encoding);
        }
        self.group_apply_us = self.group_apply_us.saturating_add(apply_t0.elapsed().as_micros() as u64);
        self.csn = csn;
        // The synced record is now the newest content of the active WAL file
        // (in segment mode too), keeping the republish guard's comparison
        // target in lockstep with the file.
        self.segment_suffix_csn = csn;
        self.max_handle = self.max_handle.max(ops.iter().map(op_handle).max().unwrap_or(0));
        self.dedup.insert(block_id.to_vec(), csn);
        Ok(Outcome::Committed { csn })
    }

    /// Commit a group of requests with ONE WAL barrier.
    ///
    /// `requests` is a slice of requests, each request a slice of its blocks
    /// `(block_id, ops)`. The unit of a uniqueness verdict is the REQUEST, not
    /// the group and not the block. Requests are resolved in input order: a
    /// request is validated against the published state plus the net effect of
    /// every earlier ACCEPTED request, merged with the request's own projected
    /// post-state (one overlay over all its blocks, later ops win exactly as
    /// [`Memtable::apply`] leaves them). At most one live owner per touched
    /// unique prefix is required, so "the request deletes owner A and adds owner
    /// B for the same prefix" is legal; a request that leaves two live owners is
    /// **dropped entirely** ([`RequestOutcome::Conflict`]): none of its blocks is
    /// appended, none consumes a CSN, and the other requests commit normally,
    /// each block consuming its own CSN in input order. By induction the durable
    /// state keeps at most one live owner per unique prefix.
    ///
    /// Because a dropped request leaves no trace, a later request sees only the
    /// effects of the requests that actually commit - the durable set does not
    /// depend on an evaluation order the caller cannot see, only on input order.
    ///
    /// The durable bytes are byte-for-byte what committing the accepted blocks
    /// one at a time would leave: one record per block in input order, each with
    /// its own CSN and the same framing, taking ONE `fdatasync` instead of one per
    /// block. Durability is a group property only at the barrier: a crash before
    /// it can leave a valid prefix of the group's records, exactly as the
    /// deferred-barrier path states. The at-most-one-live-owner-per-prefix
    /// invariant holds at REQUEST granularity after a successful call; a prefix
    /// torn inside a request was never itself validated as a state and follows
    /// the pre-existing block-atomicity rule (docs/design.md I6: a block is
    /// all-or-nothing, a request is not).
    ///
    /// The promise is bounded on purpose: a `Conflict` drops one request with
    /// nothing of it appended, but like every append path, if the single
    /// `fdatasync` itself fails then complete frames may already be in the
    /// filesystem, and recovery replays what is complete. The caller gets `Err`,
    /// so nothing is acknowledged and a retry is idempotent through the business
    /// unique key. A block already in `dedup` (an idempotent retry) returns its
    /// existing CSN, is not re-appended, and contributes no overlay entry - its
    /// effect is already in the published state, so re-including it could only
    /// reject a request the sequential order accepts. CSN, `segment_suffix_csn`,
    /// `dedup` and the handle watermark advance exactly as a sequence of single
    /// commits would. An empty group, or a group whose requests are all retries
    /// or empty, is a no-op (no append, no sync).
    pub fn commit_batch(&mut self, requests: &[Vec<(Vec<u8>, Vec<Op>)>]) -> io::Result<Vec<RequestOutcome>> {
        self.ensure_no_group("commit_batch")?;
        self.ensure_no_pending_barrier("commit_batch")?;
        let t0 = self.timing();
        let id = self.metrics.borrow_mut().resolve_commit();
        let _sample = MetricsSample { metrics: &self.metrics, id, region: metrics::DISABLED, started: t0 };
        self.ensure_attached()?;

        // One request block's slot before CSN assignment: a retry keeps its
        // recorded CSN, a new block gets one only if the request is accepted.
        enum Slot<'a> {
            Retry(u64),
            New(&'a [u8], &'a [Op]),
        }

        // Published owners plus the net effect of every earlier accepted request,
        // per touched unique prefix. A dropped request never enters this map.
        let mut live: HashMap<Vec<u8>, LivePrefix> = HashMap::new();
        // (csn, block_id, ops) of the accepted blocks, in input order.
        let mut pending: Vec<(u64, &[u8], &[Op])> = Vec::new();
        // block_id -> CSN for blocks newly committed within this group, so a
        // block_id repeated ACROSS the group's requests replays its first
        // occurrence. A repeat WITHIN one request is refused up front.
        let mut seen: HashMap<&[u8], u64> = HashMap::new();
        let mut last_csn = self.csn;
        let mut results: Vec<RequestOutcome> = Vec::with_capacity(requests.len());

        for request in requests {
            // The request's net unique effect (over all its blocks) and its
            // per-block plan in input order. Validate every key name before the
            // barrier so `apply_op`'s unwraps can never fire after a successful sync.
            let mut overlay: HashMap<Vec<u8>, PrefixEffect> = HashMap::new();
            let mut slots: Vec<Slot<'_>> = Vec::with_capacity(request.len());
            // Block ids are names in the request's `blocks` object, so a request
            // that reached the engine through the request surface has them unique
            // by construction. A duplicate inside one request can only come from
            // a programmatic caller; the engine refuses what it cannot order, so
            // reject it here - before any overlay fold or append, consuming no
            // CSN and leaving no state. Retry dedup is unchanged for a block id
            // repeated ACROSS the group's requests (`seen`) or across calls
            // (`self.dedup`).
            let mut request_ids: HashSet<&[u8]> = HashSet::with_capacity(request.len());
            for (block_id, ops) in request {
                if block_id.is_empty() {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty block_id"));
                }
                if !request_ids.insert(block_id.as_slice()) {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "duplicate block_id within one request"));
                }
                if let Some(&csn) = self.dedup.get(block_id).or_else(|| seen.get(block_id.as_slice())) {
                    slots.push(Slot::Retry(csn));
                    continue;
                }
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
                            let pfx = keys::unique_prefix_with(self.key_encoding, entity, field, value)?;
                            let entry = overlay.entry(pfx).or_insert_with(|| PrefixEffect {
                                entity: entity.clone(),
                                field: field.clone(),
                                value: value.clone(),
                                owners: HashMap::new(),
                            });
                            entry.owners.insert(*handle, matches!(op, Op::PutUnique { .. }));
                        }
                    }
                }
                slots.push(Slot::New(block_id.as_slice(), ops.as_slice()));
            }

            // INVARIANT: `overlay` is the net unique effect of exactly the blocks
            // this request will append (its `Slot::New` blocks). A retry folds
            // nothing, and a duplicate block_id inside the request was refused
            // above, so overlay and the append plan cannot disagree by
            // construction. The `debug_assert_eq!` after CSN assignment pins it; a
            // pre-fix defect folded EVERY non-retry block into the overlay while
            // the same-request duplicate check later dropped one from `pending`,
            // so the request could be accepted after only half-applying.
            let folded = slots.iter().filter(|s| matches!(s, Slot::New(..))).count();

            // Uniqueness against the request's projected post-state: each touched
            // prefix's live set (published + earlier accepted) overlaid with this
            // request's net effect must leave at most one live owner. Any prefix
            // that does not drops the whole request.
            let mut conflict: Option<Conflict> = None;
            for (pfx, effect) in &overlay {
                let published = match live.entry(pfx.clone()) {
                    Entry::Occupied(e) => e.into_mut(),
                    Entry::Vacant(v) => {
                        let mut owners: HashMap<u64, bool> = HashMap::new();
                        let region = self.region_of_prefix(pfx);
                        self.scan_merged(pfx, region, |key, val| {
                            if key.len() != pfx.len() + 16 {
                                return Err(io::Error::new(io::ErrorKind::InvalidData, "malformed unique key in merged view"));
                            }
                            let handle = u64::from_be_bytes(key[pfx.len()..pfx.len() + 8].try_into().unwrap());
                            owners.entry(handle).or_insert(!val.is_empty());
                            Ok(true)
                        })?;
                        v.insert(LivePrefix { owners })
                    }
                };
                if leaves_two_owners(&published.owners, &effect.owners) {
                    conflict = Some(Conflict {
                        kind: "x_unique",
                        entity: effect.entity.clone(),
                        field: effect.field.clone(),
                        value: effect.value.clone(),
                    });
                    break;
                }
            }
            if let Some(conflict) = conflict {
                results.push(RequestOutcome::Conflict(conflict));
                continue;
            }

            // Accepted: fold the request's effect into the running live view, then
            // assign each new block its own CSN in input order.
            for (pfx, effect) in overlay {
                live.get_mut(&pfx).expect("touched prefix was initialized during validation")
                    .owners.extend(effect.owners);
            }
            // Every `Slot::New` block is appended: a duplicate block_id inside
            // the request was refused before the overlay fold, and a block id
            // already committed (this group or a prior call) is a `Slot::Retry`.
            let pending_before = pending.len();
            let mut block_outcomes = Vec::with_capacity(slots.len());
            for slot in slots {
                match slot {
                    Slot::Retry(csn) => block_outcomes.push(Outcome::AlreadyCommitted { csn }),
                    Slot::New(block_id, ops) => {
                        let csn = last_csn.checked_add(1)
                            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CSN exhausted"))?;
                        last_csn = csn;
                        seen.insert(block_id, csn);
                        pending.push((csn, block_id, ops));
                        block_outcomes.push(Outcome::Committed { csn });
                    }
                }
            }
            debug_assert_eq!(
                folded, pending.len() - pending_before,
                "overlay must be built from exactly the blocks this request appends",
            );
            results.push(RequestOutcome::Committed(block_outcomes));
        }

        if pending.is_empty() {
            // Every block was an idempotent retry (or every request was empty):
            // nothing to append or sync.
            return Ok(results);
        }
        // Append every accepted record, take the single barrier, then publish.
        for &(csn, block_id, ops) in &pending {
            let payload = encode_payload(csn, block_id, ops);
            self.wal.append(&payload)?;
            self.record_commit_regions(ops);
        }
        self.wal.sync()?;
        for &(csn, _block_id, ops) in &pending {
            for op in ops {
                apply_op(&mut self.mt, op, csn, self.key_encoding);
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
        Ok(results)
    }
}

// Merge the serving-memtable cursor and the tile cursors in ascending encoded-key
// order, reduced to the newest version per logical key (the memtable wins a
// defensive equal key; tile ranges are disjoint). `visit` returns `false` to stop
// early. Split out of `Engine::scan_merged` so the caller can attribute the tile
// cursors' read bytes on every exit path. Generic over the memtable iterator type.
fn merge_streams<'a, I>(
    mem: &mut std::iter::Peekable<I>,
    cursors: &mut [TileCursor<'a>],
    visit: &mut impl FnMut(&[u8], &[u8]) -> io::Result<bool>,
) -> io::Result<()>
where
    I: Iterator<Item = (&'a [u8], &'a [u8])>,
{
    let mut disk_key: Vec<u8> = Vec::new();
    loop {
        let disk = smallest_disk_head(cursors, &mut disk_key)?;
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

/// Build ONE compaction replacement tile from a self-contained [`CompactionJob`],
/// entirely off the live engine: open the named input tiles read-only, replay the
/// WAL suffix bounded by the job's captured valid length, stream the same
/// reduction [`Engine::write_compacted_tile`] performs, sync the output tile and
/// its parent directory entry, and run both fail-closed verification passes.
/// Returns the replacement's identity and phase split. Nothing here touches the
/// manifest or the engine's in-memory tile list; the caller applies the result
/// with [`Engine::apply_compaction_job`].
///
/// The tile file is synced by [`TileWriter::finish`] and the parent directory is
/// synced here, off the request path, BEFORE the outcome can be applied. That is
/// the crash-consistency ordering the lazy apply relies on: a manifest page that
/// references this tile can never become durable (even by OS writeback of the
/// lazy apply page) before both the tile data and its directory entry are
/// durable. The apply side stays lazy (no manifest file/dir sync, no unlink), so
/// every fsync remains off the request path; a crash before the next durable
/// publish simply reverts to the previous durable root, whose tiles are all
/// still present, so compaction is only ever lost, never corrupt.
pub fn build_compaction_job(dir: &Path, job: &CompactionJob) -> io::Result<CompactionOutcome> {
    let t0 = Instant::now();
    let (records, _) = wal::replay_file(&job.wal_file, job.wal_valid_len)?;
    // The republish guard, identical to the synchronous path: the WAL suffix the
    // worker reads must end at the CSN the engine had served when the job was
    // created. `wal_csn == 0` (never committed) skips the comparison.
    let wal_max = records.last().map(|r| decode_payload(r).map(|v| v.0)).transpose()?.unwrap_or(0);
    if wal_max != job.wal_max_expected && job.wal_csn != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL differs from serving state"));
    }
    let digest = wal_digest(&records, job.cutoff, job.wal_floor)?;
    // Source (b): the retained WAL suffix inside `(lower, cutoff]`, key-ordered.
    let mut wal_entries: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for payload in &records {
        let (csn, _, ops) = decode_payload(payload)?;
        if csn > job.lower && csn <= job.cutoff {
            for op in &ops {
                let (key, value) = op_kv(op, csn, job.key_encoding);
                if job.region.as_deref().is_some_and(|r| keys::region_prefix(&key) != Some(r)) { continue; }
                wal_entries.insert(key, value);
            }
        }
    }
    // Source (a): the run's input tiles, opened read-only by name.
    let mut tiles: Vec<TileHandle> = Vec::with_capacity(job.input_names.len());
    for name in &job.input_names {
        let tile = Tile::open(&dir.join(name))?;
        tiles.push(TileHandle { tile, name: name.clone() });
    }
    let refs: Vec<&TileHandle> = tiles.iter().collect();
    let expected: u64 = refs.iter().fold(wal_entries.len() as u64, |acc, t| acc.saturating_add(t.tile.count()));
    let mut bloom = (job.bloom_bits_per_key > 0)
        .then(|| BloomBuilder::new(expected, job.bloom_bits_per_key as u32));
    let path = dir.join(&job.output_name);
    let mut writer = TileWriter::new(&path, job.cutoff, &digest, job.page_size)?;
    Engine::stream_merged_reduction(&refs, job.lower, job.cutoff, &wal_entries, |key, value| {
        writer.push(key, value)?;
        if let Some(bloom) = bloom.as_mut() {
            if let Some(prefix) = unique_prefix_of(key) { bloom.insert(prefix); }
        }
        Ok(())
    })?;
    if let Some(bloom) = bloom { writer.set_bloom(bloom.finish())?; }
    let stream_us = t0.elapsed().as_micros() as u64;
    let finish_t0 = Instant::now();
    let tile = writer.finish()?;
    let finish_us = finish_t0.elapsed().as_micros() as u64;
    let verify_t0 = Instant::now();
    Engine::verify_compacted_tile(&tile, &refs, job.lower, &wal_entries, job.cutoff)?;
    Engine::verify_compacted_against_wal(&tile, &wal_entries, job.lower.max(job.wal_floor), job.cutoff)?;
    let verify_us = verify_t0.elapsed().as_micros() as u64;
    // Make the tile's directory entry durable now, before the outcome can be
    // applied. `TileWriter::finish` synced the file itself but not its dirent;
    // without this ordering a lazy manifest page could be written back by the OS
    // and name a tile whose dirent was lost by a crash. Placed after the two
    // verification passes so a tile that fails validation is never made durable,
    // matching the synchronous merge path's verify-then-dir-sync order.
    let dir_sync_t0 = Instant::now();
    File::open(dir)?.sync_all()?;
    let dir_sync_us = dir_sync_t0.elapsed().as_micros() as u64;
    let output_bytes = fs::metadata(&path)?.len();
    let input_bytes = refs.iter().fold(0u64, |acc, t| {
        acc.saturating_add(fs::metadata(dir.join(&t.name)).map(|m| m.len()).unwrap_or(0))
    });
    let elapsed_us = t0.elapsed().as_micros() as u64;
    Ok(CompactionOutcome {
        name: job.output_name.clone(),
        cutoff: job.cutoff,
        digest: *tile.digest(),
        input_names: job.input_names.clone(),
        region: job.region.clone(),
        input_bytes,
        output_bytes,
        stats: CompactionStats {
            input_bytes,
            output_bytes,
            elapsed_us,
            build: CompactionBuildStats { stream_us, finish_us, verify_us },
            dir_sync_us,
            manifest_us: 0,
            unlink_us: 0,
        },
    })
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
    if key.len() < 17 || keys::family_of(key) != Some(keys::U) { return None; }
    Some(&key[..key.len() - 16])
}

/// Whether `prefix` is exactly a full unique-index prefix as
/// [`keys::unique_prefix`] spells it under either encoding (`U | entity | 0 |
/// field | 0 | len(4 BE) | value` family-first, or `ENTITY_FIRST | entity | 0 |
/// U | field | 0 | len(4 BE) | value` entity-first; nonempty entity/field, `len`
/// matching the value bytes). The probe-path bloom reject is gated on this: a
/// primary/reverse prefix or a partial unique prefix was never inserted into a
/// tile's bloom, so testing it there could answer `false` for a tile that holds
/// a matching key - a false negative.
fn is_unique_prefix(prefix: &[u8]) -> bool {
    // `rest` starts at the first still-unconsumed 0-terminated name and `names`
    // is how many name/0 pairs precede the length-prefixed value: two under
    // family-first (entity, field), one under entity-first (the entity is
    // already behind the marker + family byte).
    let (rest, names) = match prefix.first() {
        Some(&keys::U) => (&prefix[1..], 2usize),
        Some(&keys::ENTITY_FIRST) => {
            let after = &prefix[1..];
            let Some(z) = after.iter().position(|&b| b == 0) else { return false };
            if z == 0 { return false; }
            match after.get(z + 1) {
                Some(&keys::U) => (&after[z + 2..], 1),
                _ => return false,
            }
        }
        _ => return false,
    };
    let mut value = rest;
    for _ in 0..names {
        let Some(name_end) = value.iter().position(|&b| b == 0) else { return false };
        if name_end == 0 { return false; }
        value = &value[name_end + 1..];
    }
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

/// The `(region marker, entity)` region an op writes to under encoding `enc`. The
/// marker is the region prefix's first byte (the key's family under
/// [`keys::KeyEncoding::FamilyFirst`], [`keys::ENTITY_FIRST`] under
/// [`keys::KeyEncoding::EntityFirst`]), so the metrics region this resolves to is
/// exactly the region a probe on the same key serves.
fn op_region(op: &Op, enc: keys::KeyEncoding) -> (u8, &[u8]) {
    let family = match op {
        Op::PutDoc { .. } | Op::DelDoc { .. } => keys::P,
        Op::PutUnique { .. } | Op::DelUnique { .. } => keys::U,
        Op::PutReverse { .. } | Op::DelReverse { .. } => keys::R,
    };
    let marker = match enc {
        keys::KeyEncoding::FamilyFirst => family,
        keys::KeyEncoding::EntityFirst => keys::ENTITY_FIRST,
    };
    let entity = match op {
        Op::PutDoc { entity, .. } | Op::DelDoc { entity, .. }
        | Op::PutUnique { entity, .. } | Op::DelUnique { entity, .. }
        | Op::PutReverse { entity, .. } | Op::DelReverse { entity, .. } => entity,
    };
    (marker, entity)
}

/// The exact number of bytes one op contributes to `encode_payload`: its type tag
/// plus each length-prefixed field and fixed-width integer, in the same order the
/// encoder writes them. Kept beside `encode_payload` so a field-size change there
/// is a visible diff here (a unit test also pins their sum to a real payload).
fn op_encoded_len(op: &Op) -> u64 {
    match op {
        Op::PutDoc { entity, doc, .. } => 1 + 4 + entity.len() as u64 + 8 + 4 + doc.len() as u64,
        Op::DelDoc { entity, .. } => 1 + 4 + entity.len() as u64 + 8,
        Op::PutUnique { entity, field, value, .. } | Op::DelUnique { entity, field, value, .. } =>
            1 + 4 + entity.len() as u64 + 4 + field.len() as u64 + 4 + value.len() as u64 + 8,
        Op::PutReverse { entity, field, .. } | Op::DelReverse { entity, field, .. } =>
            1 + 4 + entity.len() as u64 + 4 + field.len() as u64 + 8 + 8,
    }
}

fn apply_op(mt: &mut Memtable, op: &Op, csn: u64, enc: keys::KeyEncoding) {
    let (key, value) = op_kv(op, csn, enc);
    mt.apply(key, value);
}

/// The encoded key/value an op applies at `csn` under encoding `enc` - the
/// single spelling shared by [`apply_op`] and compaction's WAL-suffix map.
fn op_kv(op: &Op, csn: u64, enc: keys::KeyEncoding) -> (Vec<u8>, Vec<u8>) {
    match op {
        Op::PutDoc { entity, handle, doc } => (keys::primary_key_with(enc, entity, *handle, csn).unwrap(), doc.clone()),
        Op::DelDoc { entity, handle } => (keys::primary_key_with(enc, entity, *handle, csn).unwrap(), Vec::new()),
        Op::PutUnique { entity, field, value, handle } => {
            (keys::unique_key_with(enc, entity, field, value, *handle, csn).unwrap(), handle.to_be_bytes().to_vec())
        }
        Op::DelUnique { entity, field, value, handle } => {
            (keys::unique_key_with(enc, entity, field, value, *handle, csn).unwrap(), Vec::new())
        }
        Op::PutReverse { entity, field, target, source } => {
            (keys::reverse_key_with(enc, entity, field, *target, *source, csn).unwrap(), vec![1])
        }
        Op::DelReverse { entity, field, target, source } => {
            (keys::reverse_key_with(enc, entity, field, *target, *source, csn).unwrap(), Vec::new())
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

    /// An overlapped barrier's `fdatasync` failure must fail closed exactly like
    /// the synchronous barrier: the completion is observed as `Err` (never a
    /// silent ack), the WAL writer is poisoned so the next group cannot append,
    /// and only a reopen can recover. The recovered prefix may include the
    /// complete-but-unacknowledged frame (as with the synchronous path), so the
    /// asserted invariant is the poison, not the on-disk truncation.
    #[test]
    fn overlapped_barrier_sync_error_fails_closed_and_poisons_the_wal() {
        let p = tmp("overlapped_poison.wal");
        let mut e = Engine::create(&p).unwrap();
        e.commit_block(b"base", &[Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"doc1".to_vec() }]).unwrap();

        e.begin_group().unwrap();
        e.commit_block_grouped(b"b1", &[Op::PutDoc { entity: b"E".to_vec(), handle: 2, doc: b"doc2".to_vec() }]).unwrap();
        e.wal.arm_barrier_error();
        assert!(e.submit_group().unwrap(), "a write group submits a barrier");

        let err = e.wait_group_barrier().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other, "the injected fdatasync failure is observed");
        assert_eq!(e.pending_barriers(), 0);

        e.begin_group().unwrap();
        assert!(e.commit_block_grouped(b"b2", &[Op::PutDoc { entity: b"E".to_vec(), handle: 3, doc: b"doc3".to_vec() }]).is_err(),
            "the poisoned writer refuses the next grouped append");
        assert!(e.submit_group().is_err(), "submit fails closed on the poisoned writer");
        drop(e);

        let e = Engine::open(&p).unwrap();
        assert_eq!(get_ok(&e, "E", 1), Some(b"doc1".to_vec()), "the acknowledged base survives recovery");
        remove(&p);
    }

    /// Barriers are drained in submission order, and a failing barrier only
    /// poisons the writer once its own turn arrives: an earlier in-flight barrier
    /// still completes and is acknowledged, while the failing one and everything
    /// after it fail closed. This is the ordering guarantee the overlapped pacer
    /// relies on.
    #[test]
    fn overlapped_barrier_failure_is_observed_in_submission_order() {
        let p = tmp("overlapped_order.wal");
        let mut e = Engine::create(&p).unwrap();
        e.commit_block(b"base", &[Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"doc1".to_vec() }]).unwrap();

        e.begin_group().unwrap();
        e.commit_block_grouped(b"b1", &[Op::PutDoc { entity: b"E".to_vec(), handle: 2, doc: b"doc2".to_vec() }]).unwrap();
        assert!(e.submit_group().unwrap());

        e.begin_group().unwrap();
        e.commit_block_grouped(b"b2", &[Op::PutDoc { entity: b"E".to_vec(), handle: 3, doc: b"doc3".to_vec() }]).unwrap();
        e.wal.arm_barrier_error();
        assert!(e.submit_group().unwrap());
        assert_eq!(e.pending_barriers(), 2);

        assert!(e.wait_group_barrier().is_ok(), "the first (healthy) barrier completes and is acknowledged");
        assert_eq!(e.pending_barriers(), 1);
        let err = e.wait_group_barrier().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Other, "the second (injected) barrier fails in its own turn");
        assert_eq!(e.pending_barriers(), 0);

        e.begin_group().unwrap();
        assert!(e.commit_block_grouped(b"b3", &[Op::PutDoc { entity: b"E".to_vec(), handle: 4, doc: b"doc4".to_vec() }]).is_err(),
            "the writer is poisoned after the failing barrier");
        remove(&p);
    }

    /// `submit_group` at the in-flight cap must fail closed WITHOUT closing the
    /// group, so the caller can drain barriers and retry the SAME group: the
    /// group's commits are already appended to the WAL and applied, and closing
    /// it would strand them with no covering barrier owed and no way to retry.
    #[test]
    fn submit_group_at_the_in_flight_cap_leaves_the_group_open_for_retry() {
        let p = tmp("overlapped_cap.wal");
        let mut e = Engine::create(&p).unwrap();
        for i in 0..MAX_PENDING_BARRIERS {
            e.begin_group().unwrap();
            e.commit_block_grouped(format!("cap{i}").as_bytes(),
                &[Op::PutDoc { entity: b"E".to_vec(), handle: (i as u64) + 1, doc: b"d".to_vec() }]).unwrap();
            assert!(e.submit_group().unwrap(), "barrier {i} submits below the cap");
        }
        assert_eq!(e.pending_barriers(), MAX_PENDING_BARRIERS);

        e.begin_group().unwrap();
        e.commit_block_grouped(b"blocked",
            &[Op::PutDoc { entity: b"E".to_vec(), handle: 65, doc: b"d".to_vec() }]).unwrap();
        assert!(e.submit_group().is_err(), "the cap reached is a fail-closed error");
        assert!(e.group_open(), "the group stays open so the caller can drain and retry");

        for _ in 0..MAX_PENDING_BARRIERS {
            assert!(e.wait_group_barrier().is_ok(), "each earlier barrier completes in order");
        }
        assert_eq!(e.pending_barriers(), 0);
        assert!(e.submit_group().unwrap(), "the retained group submits after the drain");
        assert!(e.wait_group_barrier().is_ok());
        assert_eq!(e.pending_barriers(), 0);
        remove(&p);
    }

    /// The synchronous `end_group` barrier must refuse while an overlapped
    /// barrier is still in flight: a synchronous `wal.sync()` would cover
    /// another group's appended-but-unacknowledged bytes out of order.
    #[test]
    fn end_group_refuses_while_an_overlapped_barrier_is_in_flight() {
        let p = tmp("end_group_guard.wal");
        let mut e = Engine::create(&p).unwrap();
        e.begin_group().unwrap();
        e.commit_block_grouped(b"o", &[Op::PutDoc { entity: b"E".to_vec(), handle: 1, doc: b"d".to_vec() }]).unwrap();
        assert!(e.submit_group().unwrap());
        assert_eq!(e.pending_barriers(), 1);

        e.begin_group().unwrap();
        e.commit_block_grouped(b"s", &[Op::PutDoc { entity: b"E".to_vec(), handle: 2, doc: b"d".to_vec() }]).unwrap();
        assert!(e.end_group().is_err(), "end_group refuses a barrier in flight");

        assert!(e.wait_group_barrier().is_ok(), "the overlapped barrier still completes");
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

        // Slice 27: the merge threshold defaults to today's effective value and
        // is set/looked up per region independently of the page/bloom layout.
        let policy = LayoutPolicy::declared_default();
        assert_eq!(policy.default_merge_min_tiles(), DEFAULT_MERGE_MIN_TILES);
        assert_eq!(DEFAULT_MERGE_MIN_TILES, 2, "today's effective per-region threshold");
        assert_eq!(policy.merge_min_tiles_for_region("anything"), 2);
        let mut policy = LayoutPolicy::declared_default();
        assert_eq!(policy.set_region_merge("P|Order", 64), None);
        assert_eq!(policy.merge_min_tiles_for_region("P|Order"), 64);
        assert_eq!(policy.merge_min_tiles_for_region("U|Stock"), 2, "another region keeps the default");
        assert_eq!(policy.set_region_merge("P|Order", 8), Some(64));
        assert_eq!(policy.for_region("P|Order"), (4096, 10), "a merge-only row keeps the layout default");
        // A layout override on an existing merge-only row keeps its threshold.
        policy.set_region("P|Order", 65536, 4);
        assert_eq!(policy.merge_min_tiles_for_region("P|Order"), 8);
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

    /// Count the `*.tile` files a directory holds. Only tiles are counted:
    /// `data.wal`/`data-N.wal` and `manifest` are expected non-tile files, and a
    /// failed rotation's leftover segment is unrelated to a region merge.
    fn tile_file_count(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap()
            .filter_map(|d| d.ok())
            .filter(|d| d.file_name().to_string_lossy().ends_with(".tile"))
            .count()
    }

    /// DEFECT 1, white-box: a region merge output must hold EXACTLY the run
    /// region's keys. `P|Order` is written first and again at cutoff 3, so its
    /// run starts at its oldest tile (`lower = 0`) and the global WAL suffix
    /// `(0, 3]` also holds the `P|Stock` op from cutoff 2; before the fix that op
    /// landed in the `P|Order` output.
    #[test]
    fn region_compaction_output_holds_only_its_region() {
        let dir = tmp_dir("region-only-keys");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        e.commit_block(b"b1", &[Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: b"o1".to_vec() }]).unwrap();
        e.publish_tile(dir.join("t1.tile"), 1).unwrap();
        e.commit_block(b"b2", &[Op::PutDoc { entity: b"Stock".to_vec(), handle: 1, doc: b"s1".to_vec() }]).unwrap();
        e.publish_tile(dir.join("t2.tile"), 2).unwrap();
        e.commit_block(b"b3", &[Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: b"o3".to_vec() }]).unwrap();
        e.publish_tile(dir.join("t3.tile"), 3).unwrap();
        assert_eq!(e.tiles.len(), 3, "P|Order at 1 and 3, P|Stock at 2");

        assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "P|Order is eligible at two tiles");
        // Scan the emitted tile entry by entry, not just its first/last key: the
        // newest version of Order/1 (cutoff 3) is the only entry it may hold.
        let newest_order_key = keys::primary_key(b"Order", 1, 3).unwrap();
        let order_region = keys::region_prefix(&newest_order_key).unwrap();
        let order = e.tiles.iter()
            .find(|t| tile_single_region(t) == Some(order_region))
            .expect("the merged P|Order tile exists");
        let keys_in_tile: Vec<Vec<u8>> = order.tile.entries()
            .map(|entry| entry.unwrap().0)
            .collect();
        assert_eq!(keys_in_tile.len(), 1, "one newest version per logical key");
        for key in &keys_in_tile {
            assert_eq!(keys::region_prefix(key), Some(order_region),
                "a P|Order merge output holds only P|Order keys");
        }
        assert_eq!(keys_in_tile[0], keys::primary_key(b"Order", 1, 3).unwrap());
        // Both regions still read correctly, and the directory reopens.
        assert_eq!(get_ok(&e, "Order", 1), Some(b"o3".to_vec()));
        assert_eq!(get_ok(&e, "Stock", 1), Some(b"s1".to_vec()));
        drop(e);
        let e = Engine::open_discover(&dir).unwrap();
        assert_eq!(get_ok(&e, "Order", 1), Some(b"o3".to_vec()));
        assert_eq!(get_ok(&e, "Stock", 1), Some(b"s1".to_vec()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// DEFECT 2: a region merge retires its inputs from the manifest and then
    /// unlinks their files, exactly like `merge_run`, so no orphan accumulates.
    #[test]
    fn region_compaction_unlinks_retired_inputs_after_the_flip() {
        let dir = tmp_dir("region-unlink");
        let mut e = Engine::create(&dir.join("data.wal")).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        for csn in 1..=3u64 {
            e.commit_block(format!("b{csn}").as_bytes(), &[
                Op::PutDoc { entity: b"Order".to_vec(), handle: 1, doc: format!("v{csn}").into_bytes() },
            ]).unwrap();
            e.publish_tile(dir.join(format!("t{csn}.tile")), csn).unwrap();
        }
        assert_eq!(e.tiles.len(), 3);
        assert_eq!(tile_file_count(&dir), 3);

        assert!(e.maybe_compact(u64::MAX, 2).unwrap().is_some(), "two P|Order tiles merge");
        assert_eq!(e.tiles.len(), 2, "three tiles became two");
        // No other path ran, so every `.tile` on disk is manifest-referenced: the
        // two retired inputs are gone, not merely unreferenced.
        assert_eq!(tile_file_count(&dir), e.tiles.len(), "retired inputs are unlinked, no orphan");
        assert!(!dir.join("t1.tile").exists() && !dir.join("t2.tile").exists(),
            "the merged run's input files are removed");
        assert_eq!(get_ok(&e, "Order", 1), Some(b"v3".to_vec()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Slice 24 recorded "a crafted tile whose middle keys belong to another region
    /// would pass" as unreachable; slice 27 reached it through the WAL suffix. The
    /// region path no longer writes such a tile, and this fault-injects one directly
    /// to prove reopen rejects it rather than treating it as whole-keyspace.
    #[test]
    fn reopen_rejects_a_single_region_tile_holding_a_foreign_key() {
        let dir = tmp_dir("region-foreign-key");
        let wal = dir.join("data.wal");
        let mut e = Engine::create(&wal).unwrap();
        e.set_region_mode(RegionMode::PerRegion);
        // Enough P|Order docs to span TWO data blocks, plus one real P|Stock doc.
        // The splice lands in block 0 while the cached last key (from block 1)
        // stays a trusted P|Order key, so `tile_single_region` still calls the tile
        // single-region and only the region check can reject it.
        let mut ops: Vec<Op> = (1..=200u64).map(|h| Op::PutDoc {
            entity: b"Order".to_vec(), handle: h, doc: b"o".to_vec(),
        }).collect();
        ops.push(Op::PutDoc { entity: b"Stock".to_vec(), handle: 1, doc: b"s".to_vec() });
        e.commit_block(b"b1", &ops).unwrap();
        e.publish_tile(dir.join("t.tile"), 1).unwrap();
        let order_key = keys::primary_key(b"Order", 1, 1).unwrap();
        let order_region = keys::region_prefix(&order_key).unwrap();
        let order_name = e.tiles.iter()
            .find(|t| tile_single_region(t) == Some(order_region))
            .expect("the P|Order tile exists").name.clone();
        drop(e);

        // Splice the P|Stock key over block 0's SECOND entry. Both keys are 23
        // bytes, so the block layout is unchanged; repairing block 0's CRC leaves
        // only the foreign key as the defect. Block 1 (the last block, whose key is
        // the tile's cached last key) is untouched, so the tile still starts and
        // ends in P|Order.
        let stock_key = keys::primary_key(b"Stock", 1, 1).unwrap();
        let mut bytes = fs::read(dir.join(&order_name)).unwrap();
        let page = DEFAULT_PAGE_SIZE as usize;
        let block_len = u32::from_le_bytes(bytes[page..page + 4].try_into().unwrap()) as usize;
        let first_key_len = u32::from_le_bytes(bytes[page + 8..page + 12].try_into().unwrap()) as usize;
        let mut at = page + 14 + first_key_len;
        let klen0 = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let vlen0 = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        at += 8 + klen0 + vlen0; // skip entry 0 (Order/1), landing on entry 1
        let klen1 = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        assert_eq!(klen1, stock_key.len(), "the spliced key keeps the entry size");
        bytes[at + 8..at + 8 + klen1].copy_from_slice(&stock_key);
        let crc = crate::crc::crc32c(&bytes[page + 8..page + block_len]);
        bytes[page + 4..page + 8].copy_from_slice(&crc.to_le_bytes());
        fs::write(dir.join(&order_name), &bytes).unwrap();

        let err = match Engine::open_with_tile(&wal, dir.join(&order_name)) {
            Ok(_) => panic!("a single-region tile carrying another region's key must fail closed"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("outside its region"),
            "a single-region tile carrying another region's key must fail closed: {err}");
        let _ = fs::remove_dir_all(&dir);
    }

    fn remove(p: &Path) {
        fs::remove_file(p).ok();
    }
}
