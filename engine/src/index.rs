//! Derived secondary access path (WP-M, WP-9): an off-request-path `order_no ->
//! StockMovement handle` index, built from the merged (memtable + tile) view of
//! the `P|StockMovement` rows and used by the authoritative read path only as a
//! HINT.
//!
//! Correctness is by construction: the index is never on the correctness path.
//! Every hit is cross-checked against the document's own `order_no` field, and a
//! miss, a mismatch, an absent or a stale index falls back to the existing
//! full-entity scan. The index only short-circuits a positive hit; it can never
//! turn a "not found" into a wrong answer. A base tile that cannot be READ returns
//! the error instead of falling back, matching the engine's fail-closed rule for a
//! corrupt tile: that is a device defect the run must see, not a hint to skip.
//!
//! It reuses the `U` (unique-index) key spelling — `unique_key(entity, field,
//! value, handle, csn)` / value = `handle.to_be_bytes()` — and the existing tile
//! writer/reader, so no new keyspace family and no change to recovery ordering.
//! The base tile lives OUTSIDE the engine's main `self.tiles` list (which is
//! WAL-projection-verified at recovery), as a sidecar optimization that is
//! rebuilt at every open by the driver when the policy asks for it.
//!
//! # Incremental maintenance: one durable base, one in-RAM delta
//!
//! Only the base is a tile. Everything the base does not cover yet lives in
//! [`DeltaMap`], an in-RAM map of the rows whose newest committed version is
//! above the base coverage:
//!
//! - a **rebuild** builds the delta of one interval — the rows newer than the
//!   coverage the adopted index already speaks for — and writes no device bytes;
//! - the **read path** probes the delta first and then the base tile, so a row
//!   written after the base coverage is answered without rewriting the base; a
//!   delta tombstone hides the base entry for that handle;
//! - a **merge** rewrites base + delta into a new base tile once the delta holds
//!   as many entries as the base (or [`INDEX_MERGE_MIN_DELTA_ENTRIES`]), so each
//!   entry is written to the device once, when it is merged, and the base is
//!   rewritten only once per base-sized batch of new rows.
//!
//! The delta is deliberately not durable. The index is a hint, rebuilt at every
//! open and never on the correctness path, so a lost delta costs the next build's
//! work and nothing else; writing it to a sidecar would be device bytes that
//! nobody reads back, which is the write amplification WP-9 exists to remove.
//!
//! Every decision about the index is a hint too: the driver logs one line per
//! build, refuses nothing on the read path, and keeps at most one job in flight
//! ([`IndexRebuildSlot`]), so a rejected or slow build leaves the authoritative
//! scan serving.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::Duration;

use crate::engine::Engine;
use crate::keys;
use crate::readahead::TileReadView;
use crate::tile::Tile;

/// The entity and field this secondary path serves. Only the movement scan
/// shape has a full-entity scan with a filterable predicate today.
pub const INDEX_ENTITY: &[u8] = b"StockMovement";
pub const INDEX_FIELD: &[u8] = b"order_no";

/// The sidecar filename the durable base tile is written under, in the engine's
/// directory. One spelling for every write, so a rebuild or a merge always
/// replaces the predecessor it is replacing.
pub const INDEX_TILE_NAME: &str = "order-no-index.tile";

/// The delta size at which a merge rewrites base + delta into a new base.
///
/// The rule is `delta_entries >= max(INDEX_MERGE_MIN_DELTA_ENTRIES,
/// base_entries)`. A merge writes the base's entries once more plus the delta's,
/// i.e. `base + delta` bytes, so the *amortised* index write per new row is
/// `1 + base/delta`: the base rewrite is at most as large as the delta that paid
/// for it, which is what keeps the total index bytes at or below ~2x the entry
/// bytes (each entry written once when merged, and at most once more by the next
/// merge). The floor exists because a small base would otherwise merge on every
/// interval and rewrite the whole base each time: at 1,024 movement entries the
/// merge's fixed cost is spread over at least 1,024 rows (~7 s of ingest at the
/// measured 200 rps rate, ~64 KiB of index), which is small enough that a tiny
/// region still converges quickly.
pub const INDEX_MERGE_MIN_DELTA_ENTRIES: u64 = 1_024;

/// How many bytes of the inline serving-memtable snapshot may be copied on the
/// thread that schedules a rebuild.
///
/// The snapshot exists because the engine's memtable cannot cross to the builder
/// thread, so its cost is paid by the thread that is also pacing requests. Two
/// things keep it far below this bound on the incremental path: the snapshot is
/// filtered to the rows newer than the coverage the adopted index already
/// speaks for, so it is ONE INTERVAL's ingest, not the region (a 1,000-rps
/// interval is ~150 KiB at the movement document shape, and even the
/// [`INDEX_REBUILD_STALE_CSN`] quiet-region interval is ~150 KiB); and the bound
/// is a refusal, not a truncation, so an over-bound snapshot copies nothing and
/// the rebuild is skipped with the reason logged.
///
/// The charge includes [`MEMTABLE_ROW_OVERHEAD_BYTES`] per row on top of the
/// document bytes, because the copy the scheduling thread pays for is the owned
/// `(handle, csn, Vec<u8>)` rows, not only their documents.
pub const INLINE_SNAPSHOT_MAX_BYTES: usize = 4 << 20;

/// Heap bytes one row of [`MemtableRows::rows`] costs beyond its document: the
/// `(handle, csn, Vec<u8>)` tuple plus the `Vec` header in the row buffer. The
/// inline bound charges this on top of the document bytes so a region of small
/// documents cannot sit inside the byte bound while its row buffer is far over
/// it.
pub const MEMTABLE_ROW_OVERHEAD_BYTES: usize = std::mem::size_of::<(u64, u64, Vec<u8>)>();

/// How many committed versions an adopted index may lag before a rebuild is
/// scheduled EVEN WITH NO scan demand.
///
/// Without this floor a region that is written far faster than it is scanned
/// would never be rebuilt, which is the ticket-83 state again with a log line
/// instead of silence. At the load's own commit rate this is a few seconds of
/// writes, so a quiet region costs at most one rebuild per that many versions
/// (a rebuild now writes no device bytes; its cost is one inline memtable
/// snapshot of that interval) and a region with demand is covered by the demand
/// arm long before.
pub const INDEX_REBUILD_STALE_CSN: u64 = 1_024;

/// How long the driver waits for the one in-flight index job when it closes the
/// run, so a job scheduled at the last boundary is still adopted and charged
/// instead of dying with the process. A delta build copies one interval and
/// allocates no device bytes; a merge reads and rewrites the base. Both are tens
/// of milliseconds at the measured region sizes, so two seconds is roughly two
/// orders of magnitude of headroom, and a job that outlives it is recorded as
/// skipped rather than holding the run open.
pub const INDEX_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The trigger labels [`rebuild_trigger`] and the driver log, so a run says which
/// arm of the rule fired: `scan_demand` (the region is being scanned and the
/// index lags what the scan would serve), `stale` (the lag passed
/// [`INDEX_REBUILD_STALE_CSN`] with no scan demand), `policy` (the policy wants the
/// index and none is adopted - the first off-path build of a run whose startup
/// build did not happen, e.g. the policy was reloaded mid-run), `merge` (the
/// delta reached the merge threshold) and `startup` (the eager build at open).
pub const TRIGGER_SCAN_DEMAND: &str = "scan_demand";
pub const TRIGGER_STALE: &str = "stale";
pub const TRIGGER_POLICY: &str = "policy";
pub const TRIGGER_MERGE: &str = "merge";
pub const TRIGGER_STARTUP: &str = "startup";

/// The rows changed after the base coverage: the derived index's incremental
/// half, held in RAM and never written to a sidecar (see the module docs).
///
/// One entry per handle the delta has seen since the base coverage, in the
/// handle's newest committed state: either `live` (`order_no -> (handle, csn)`) or
/// `dead` (the newest version is a tombstone). Each interval's build reduces its
/// own rows with the one newest-wins rule
/// ([`crate::engine::NewestPrimaryVersion`]) and therefore names a handle at most
/// once; absorbing a newer interval replaces the older information for the handles
/// it names.
#[derive(Clone, Debug, Default)]
pub struct DeltaMap {
    /// `order_no -> (handle, csn)` for the rows that are live after the coverage.
    live: HashMap<Vec<u8>, (u64, u64)>,
    /// Handles whose newest version after the coverage is a tombstone.
    dead: HashSet<u64>,
    /// The highest movement CSN the delta speaks for, 0 when it holds nothing.
    coverage_csn: u64,
}

/// What the delta alone can say about one `order_no`.
enum DeltaAnswer {
    /// The newest state of the handle under this `order_no` is live.
    Hit(u64),
    /// The delta knows a handle for this `order_no` and that handle is dead, so
    /// the base entry for it must be hidden too.
    Dead,
    /// The delta says nothing about this `order_no`; probe the base.
    Absent,
}

impl DeltaMap {
    fn with_coverage(coverage_csn: u64) -> Self {
        DeltaMap { live: HashMap::new(), dead: HashSet::new(), coverage_csn }
    }

    /// The highest movement CSN the delta speaks for (the base coverage when the
    /// delta is empty).
    pub fn coverage_csn(&self) -> u64 { self.coverage_csn }

    /// Handles the delta says anything about (live or tombstoned): the merge
    /// threshold and the RAM bound are charged against exactly this.
    pub fn entries(&self) -> u64 { (self.live.len() + self.dead.len()) as u64 }

    /// Live entries in the delta.
    pub fn live_entries(&self) -> u64 { self.live.len() as u64 }

    pub fn is_empty(&self) -> bool { self.live.is_empty() && self.dead.is_empty() }

    fn answer(&self, order_no: &[u8]) -> DeltaAnswer {
        match self.live.get(order_no) {
            // A stale live entry (an older order_no of a handle that was later
            // deleted) stays in the map; the tombstone set is what makes it a
            // miss, and the read path cross-checks anyway.
            Some(&(handle, _)) if !self.dead.contains(&handle) => DeltaAnswer::Hit(handle),
            Some(_) => DeltaAnswer::Dead,
            None => DeltaAnswer::Absent,
        }
    }

    /// Insert a newer interval: its rows replace the older information for the
    /// same handle, and a resurrection removes the handle from the dead set.
    fn absorb(&mut self, newer: DeltaMap) {
        for (order_no, (handle, csn)) in newer.live {
            self.dead.remove(&handle);
            self.live.insert(order_no, (handle, csn));
        }
        self.dead.extend(newer.dead);
        self.coverage_csn = self.coverage_csn.max(newer.coverage_csn);
    }

    /// The handles the delta names as dead, for a merge that drops their base
    /// entries.
    fn dead_handles(&self) -> &HashSet<u64> { &self.dead }
}

/// A built, durable base tile plus the in-RAM delta of the rows changed after
/// the base's coverage.
pub struct OrderNoIndex {
    /// The durable half: every row up to `base.cutoff()`, one entry per live row.
    base: Arc<Tile>,
    key_encoding: keys::KeyEncoding,
    /// The rows changed after the base coverage, up to `delta.coverage_csn()`.
    delta: DeltaMap,
}

/// The base half of an index. Cheap to clone (one `Arc`), which is what lets a
/// merge snapshot it off the same index the read path is serving.
#[derive(Clone)]
struct IndexBase {
    tile: Arc<Tile>,
    key_encoding: keys::KeyEncoding,
}

impl IndexBase {
    /// The highest CSN the base tile's entries cover.
    fn coverage_csn(&self) -> u64 { self.tile.cutoff() }
}

/// Everything a merge rewrites: the adopted base and a copy of the delta it is
/// merging. The copy is taken on the scheduling thread (a one-off memory copy of
/// at most the threshold's entries, ~1.8 MB at the 30 k-entry base) so the engine
/// keeps serving the same delta while the merge runs; that copy is the 2x half of
/// the RAM bound [`INDEX_MERGE_MIN_DELTA_ENTRIES`] states.
pub struct MergeSnapshot {
    base: IndexBase,
    delta: DeltaMap,
}

/// One interval's delta, ready to be inserted into the adopted index.
pub struct DeltaBuild {
    pub delta: DeltaMap,
    /// The base coverage the delta was built against. Adoption refuses a
    /// mismatch: the entries describe rows above THIS base, and attaching them
    /// to another one would claim coverage the index does not have.
    pub base_coverage_csn: u64,
}

/// The product of an off-path index job, ready to be adopted (or discarded).
pub struct OrderNoIndexBuild {
    pub index: OrderNoIndex,
    /// Durable output bytes the job wrote on disk (the base tile).
    pub output_bytes: u64,
}

/// What one off-path index job produced.
pub enum IndexProduct {
    /// A complete index: the first base build, or the merge of base + delta.
    Index(OrderNoIndexBuild),
    /// One interval's delta, to be inserted into the adopted index. It writes no
    /// device bytes.
    Delta(DeltaBuild),
}

/// One index job's product plus what the decision log line reports about it.
pub struct RebuildResult {
    pub product: IndexProduct,
    /// Rows the source dropped instead of indexing: superseded versions, and
    /// handles whose newest version is a tombstone. Never in the index.
    pub stale_rows: u64,
    /// Rows the inline serving-memtable snapshot contributed (one per handle).
    pub memtable_rows: u64,
    /// Bytes the inline snapshot was charged for (documents plus row overhead).
    pub inline_bytes: usize,
}

/// One finished job, taken from the slot: the trigger it was scheduled under is
/// kept even when the job failed, so the failure line names it.
pub struct FinishedJob {
    pub trigger: String,
    pub result: io::Result<RebuildResult>,
}

/// The single in-flight index job: the receiver of its one channel, the trigger
/// it was scheduled under, and the sidecar name it writes.
///
/// This is the only place the index is written, so a second caller cannot race a
/// builder for the same sidecar: [`schedule_base`](Self::schedule_base),
/// [`schedule_delta`](Self::schedule_delta) and
/// [`schedule_merge`](Self::schedule_merge) refuse while the slot is occupied
/// instead of starting a second writer, and the temp file every write goes
/// through is unique per write as well, so even a caller that ignores the slot
/// cannot tear another job's file.
pub struct IndexRebuildSlot {
    rx: Option<Receiver<io::Result<RebuildResult>>>,
    trigger: Option<String>,
}

impl IndexRebuildSlot {
    pub fn new() -> Self {
        IndexRebuildSlot { rx: None, trigger: None }
    }

    /// Whether a job is in flight.
    pub fn is_busy(&self) -> bool { self.rx.is_some() }

    /// The trigger the in-flight job was scheduled under, for the skipped line.
    pub fn trigger(&self) -> Option<&str> { self.trigger.as_deref() }

    /// Reserve the slot for one job, or refuse the second caller explicitly.
    fn reserve(&mut self, trigger: &str) -> io::Result<()> {
        if self.rx.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a derived index job is already in flight; the slot holds its only receiver",
            ));
        }
        self.trigger = Some(trigger.to_string());
        Ok(())
    }

    /// Take a finished job's result without blocking, or `None` while it is still
    /// running (or when nothing was scheduled).
    pub fn try_recv(&mut self) -> Option<FinishedJob> {
        let rx = self.rx.take()?;
        match rx.try_recv() {
            Ok(result) => Some(FinishedJob { trigger: self.trigger.take().unwrap_or_default(), result }),
            Err(std::sync::mpsc::TryRecvError::Empty) => { self.rx = Some(rx); None }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(FinishedJob {
                trigger: self.trigger.take().unwrap_or_default(),
                result: Err(io::Error::other("the derived index builder thread went away")),
            }),
        }
    }

    /// Wait at most `timeout` for the in-flight job. `None` means it is still
    /// running and the slot stays occupied; the caller may wait again or record
    /// the job as skipped.
    pub fn wait(&mut self, timeout: Duration) -> Option<FinishedJob> {
        let rx = self.rx.take()?;
        match rx.recv_timeout(timeout) {
            Ok(result) => Some(FinishedJob { trigger: self.trigger.take().unwrap_or_default(), result }),
            Err(RecvTimeoutError::Timeout) => { self.rx = Some(rx); None }
            Err(RecvTimeoutError::Disconnected) => Some(FinishedJob {
                trigger: self.trigger.take().unwrap_or_default(),
                result: Err(io::Error::other("the derived index builder thread went away")),
            }),
        }
    }

    /// Schedule the first build of an index: the whole merged region into one base
    /// tile, with the [`RegionRows`] witness guarding the empty-result case.
    pub fn schedule_base<F>(
        &mut self,
        dir: PathBuf,
        key_encoding: keys::KeyEncoding,
        view: TileReadView,
        tile_scan: F,
        memtable: MemtableRows,
        region_rows: RegionRows,
        trigger: &str,
    ) -> io::Result<()>
    where
        F: FnOnce(&TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>
            + Send
            + 'static,
    {
        self.reserve(trigger)?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(rebuild_base(&dir, key_encoding, view, tile_scan, memtable, region_rows));
        });
        self.rx = Some(rx);
        Ok(())
    }

    /// Schedule one interval's delta build. The caller has already filtered the
    /// source to the rows above the coverage the adopted index speaks for;
    /// `base_coverage_csn` is the base tile's coverage the delta will attach above,
    /// which adoption re-checks. A delta writes no device bytes, so it needs no
    /// directory.
    pub fn schedule_delta<F>(
        &mut self,
        base_coverage_csn: u64,
        view: TileReadView,
        tile_scan: F,
        memtable: MemtableRows,
        trigger: &str,
    ) -> io::Result<()>
    where
        F: FnOnce(&TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>
            + Send
            + 'static,
    {
        self.reserve(trigger)?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(rebuild_delta(base_coverage_csn, view, tile_scan, memtable));
        });
        self.rx = Some(rx);
        Ok(())
    }

    /// Schedule the merge of the adopted base and delta into a new base tile.
    pub fn schedule_merge(&mut self, dir: PathBuf, snapshot: MergeSnapshot, trigger: &str) -> io::Result<()> {
        self.reserve(trigger)?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(merge_index(&dir, snapshot));
        });
        self.rx = Some(rx);
        Ok(())
    }
}

/// Evidence, read from engine state outside the build's scan, that the index's
/// region holds at least one row.
///
/// The build cannot answer this itself: the scan reports how many rows it
/// visited, but a source that misses part of the region reports zero and sees
/// zero, so "zero rows and zero entries" looks exactly like an empty region
/// while the region is in fact full. That is ticket 83: a tiles-only source saw
/// no rows for a region that was entirely in the serving memtable, and the build
/// wrote an empty sidecar. Only a structure the scan does not walk can testify
/// that the region is non-empty.
///
/// This witness guards the BASE build. A delta build is narrowed to the rows
/// above the coverage on purpose, so an empty delta is a legitimate "nothing
/// changed in this region" interval and the base's refusal cannot apply to it;
/// the delta build instead refuses when the inline snapshot copied rows and the
/// reduce indexed none of them (see [`rebuild_delta`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionRows {
    /// At least one active tile's first key names the region.
    InTiles,
    /// The serving memtable holds live rows of this region: one per handle whose
    /// newest committed version is not a tombstone. Exact for the region (the
    /// memtable is shared by every region, so counting it whole would refuse a
    /// build on a genuinely empty movement region; see
    /// [`Engine::primary_region_live_rows`]).
    InMemtable { rows: usize },
    /// No engine structure holds a live row in the region, so an empty build is
    /// legitimate.
    Empty,
}

impl RegionRows {
    fn holds_rows(self) -> bool { !matches!(self, RegionRows::Empty) }
}

impl fmt::Display for RegionRows {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RegionRows::InTiles => write!(f, "an active tile names this region"),
            RegionRows::InMemtable { rows } => {
                write!(f, "the serving memtable holds {rows} live rows of this region")
            }
            RegionRows::Empty => write!(f, "no engine structure holds a live row of this region"),
        }
    }
}

/// The loud empty-build refusal: the source produced no index entry for a region
/// the [`RegionRows`] witness says holds rows.
///
/// It is its own error type so a caller can tell a refusal (log one decision and
/// keep serving from the full scan) from a genuine scan error (abort the run):
/// both are `InvalidData`, and only the message would distinguish them
/// otherwise. See [`is_index_build_refused`].
#[derive(Debug)]
pub struct IndexBuildRefused(String);

impl fmt::Display for IndexBuildRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl std::error::Error for IndexBuildRefused {}

/// Whether `error` is a [`IndexBuildRefused`] rather than a scan or I/O failure.
pub fn is_index_build_refused(error: &io::Error) -> bool {
    error.get_ref().and_then(|inner| inner.downcast_ref::<IndexBuildRefused>()).is_some()
}

fn refused(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, IndexBuildRefused(message))
}

/// The `family|entity` name of the region the index covers, spelled from the
/// same primary prefix the authoritative scan walks, so it matches what
/// [`Engine::active_tile_regions`] reports for a tile of that region under the
/// given key encoding (family-first gives `P|StockMovement`, the entity-first
/// encoding `C|StockMovement`). `None` only for an entity the key codec rejects,
/// which cannot happen for the literal [`INDEX_ENTITY`].
fn region_name(encoding: keys::KeyEncoding) -> Option<String> {
    let mut prefix = keys::primary_prefix_with(encoding, INDEX_ENTITY, 0).ok()?;
    // Drop the 8-byte handle a prefix carries to leave `marker | entity | 0`.
    prefix.truncate(prefix.len() - 8);
    Some(keys::region_name(&prefix))
}

/// Whether the index's region holds live rows, read from the engine structures
/// the build's scan does not consume - the independent witness [`RegionRows`]
/// describes. The caller reads it at the same boundary as the scan, so the build
/// can fail on an empty result instead of writing a sidecar that would leave
/// every scan on the full-entity fallback.
///
/// The memtable arm is exact for the region ([`Engine::primary_region_live_rows`]
/// counts the region's live rows, not the whole shared memtable), so a target whose
/// memtable is busy with other regions while the movement region is genuinely empty
/// builds a legitimate empty index instead of aborting the run.
///
/// The tile arm is only as exact as a tile's first key. Under
/// [`crate::engine::RegionMode::PerRegion`] a tile holds one region, so its first
/// key names exactly that region. Under [`crate::engine::RegionMode::Global`] a tile
/// holds the whole keyspace and `active_tile_regions` reports the region of its
/// LOWEST key, so the arm is wrong in either direction there: it can report this
/// region for a tile whose movements are all tombstones or which holds no movement
/// at all (a false positive refuses an empty build), and it can stay silent while
/// live movement rows sit in a tile whose lowest key is another region (a false
/// negative leaves the ticket-83 state unguarded). Reads are unaffected either way:
/// the build's source walks the tiles themselves, so a false negative costs a loud
/// guard, never a wrong answer, and the authoritative scan is merged in both modes.
pub fn movement_region_rows(engine: &Engine) -> RegionRows {
    if let Some(region) = region_name(engine.key_encoding()) {
        if engine.active_tile_regions().iter().flatten().any(|r| *r == region) {
            return RegionRows::InTiles;
        }
    }
    match engine.primary_region_live_rows(INDEX_ENTITY).expect("index entity is a valid name") {
        0 => RegionRows::Empty,
        rows => RegionRows::InMemtable { rows },
    }
}

/// The `order_no` string a movement document indexes under. A document whose
/// `order_no` is not a string is an error, never a skipped row: skipping it
/// silently would leave the index permanently incomplete for a row the scan
/// serves.
fn index_order_no(doc: &[u8]) -> io::Result<String> {
    let row: serde_json::Value = serde_json::from_slice(doc)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    row[std::str::from_utf8(INDEX_FIELD).expect("ascii field name")].as_str()
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "StockMovement.order_no is not a string"))
}

/// The handle an index entry key names: every `unique_key` spelling ends with
/// handle (8 BE) then `~csn` (8 BE).
fn entry_handle(key: &[u8]) -> Option<u64> {
    key.len().checked_sub(16).map(|at| u64::from_be_bytes(key[at..at + 8].try_into().expect("8 bytes")))
}

/// Write `entries` (sorted by key) as a tile named `name` in `dir`, and return
/// the opened tile plus its on-disk size.
///
/// The write goes through a temp name unique per call and is renamed over `name`,
/// which is atomic: a crash leaves either the old sidecar or the new one, and two
/// writers cannot tear each other's file (the slot already refuses a second
/// concurrent job; this is the second, independent guard).
fn write_index_tile(
    dir: &Path,
    name: &str,
    cutoff: u64,
    entries: &[(Vec<u8>, Vec<u8>)],
) -> io::Result<(Tile, u64)> {
    static NEXT_TMP: AtomicU64 = AtomicU64::new(0);
    let path = dir.join(name);
    let tmp_path = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT_TMP.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| -> io::Result<(Tile, u64)> {
        // 4 KiB pages: a point probe reads one 4 KiB block, and the one-time write
        // amplification stays at the data's own size (no 64 KiB block padding).
        let mut writer = crate::tile::TileWriter::new(&tmp_path, cutoff, &[0u8; 32], 4_096)?;
        for (key, value) in entries {
            writer.push(key, value)?;
        }
        let tile = writer.finish()?;
        std::fs::rename(&tmp_path, &path)?;
        let bytes = std::fs::metadata(&path)?.len();
        Ok((tile, bytes))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    written
}

/// Build the `order_no -> handle` index tile from the live `P|StockMovement`
/// rows `scan_primary` visits. Runs off the request path (the caller owns the
/// thread): it streams the whole entity prefix once, JSON-parses each document
/// once, sorts the `(order_no, handle, csn)` entries, and writes ONE 4 KiB-page,
/// no-bloom tile with the existing [`crate::tile::TileWriter`] (which syncs the
/// file). The tile is written to a unique scratch name and atomically renamed
/// over the final `name`, so a rebuild after a retire replaces the previous
/// sidecar file rather than failing on `create_new`. The caller must sync the
/// parent directory before adopting the tile, matching the compaction apply
/// ordering.
///
/// Memory: this build holds one sort buffer of index entries (each a `U` unique
/// key plus its 8-byte handle), not one per document. What the source costs is
/// the source's business: the offline caller passes a scan closure that streams
/// the merged view, while the driver's rebuild ([`IndexRebuildSlot`]) hands over
/// an inline memtable snapshot plus a tile walk, so neither side keeps a second
/// copy of a document.
///
/// `scan_primary` walks the rows at latest committed state - one row per LIVE
/// handle, at the CSN of the version that won - and returns how many it visited.
/// It is the caller's job to have reduced the source that way (`scan_primary_with_csn`
/// does it for the merged view, and [`IndexRebuildSlot`]'s merge applies the same
/// rule to the tile walk and the memtable snapshot). `region_rows` is the
/// independent non-emptiness witness: an empty result for a region that holds
/// rows fails with a [`IndexBuildRefused`] rather than writing an empty sidecar.
/// The scan's own count is quoted in that failure but never compared against the
/// entry count, because both come from the same iteration and can only agree.
///
/// `coverage_floor_csn` is the CSN the caller's source snapshot speaks for, and
/// the index covers up to `max(its newest entry's CSN, the floor)`. The floor is
/// what makes the coverage a claim about the engine state instead of a claim
/// about the survivors: a tombstone or a superseded version produces no entry, so
/// without the floor the coverage would sit at the newest LIVE row and a rebuild
/// trigger reading it would re-index the region forever. The driver passes
/// `engine.csn()` at the instant it snapshotted the source; passing 0 claims only
/// what the entries themselves prove.
pub fn build_order_no_index(
    dir: &Path,
    key_encoding: keys::KeyEncoding,
    scan_primary: impl FnOnce(&mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
    region_rows: RegionRows,
    coverage_floor_csn: u64,
    name: &str,
) -> io::Result<OrderNoIndexBuild> {
    // (encoded key, value). Keys must be strictly increasing, so collect and
    // sort before pushing to the writer.
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut newest_entry_csn: u64 = 0;
    let rows = scan_primary(&mut |handle, csn, doc| {
        let order_no = index_order_no(doc)?;
        let key = keys::unique_key_with(key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no.as_bytes(), handle, csn)?;
        entries.push((key, handle.to_be_bytes().to_vec()));
        newest_entry_csn = newest_entry_csn.max(csn);
        Ok(())
    })?;
    let coverage_csn = newest_entry_csn.max(coverage_floor_csn);
    if entries.is_empty() && region_rows.holds_rows() {
        return Err(refused(format!(
            "order_no index build produced 0 entries for a region that holds rows ({region_rows}): \
             the scan visited {rows} live StockMovement rows, so its source does not cover the \
             region and the index would leave every scan on the full-entity fallback")));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let (tile, output_bytes) = write_index_tile(dir, name, coverage_csn, &entries)?;
    Ok(OrderNoIndexBuild {
        index: OrderNoIndex {
            base: Arc::new(tile),
            key_encoding,
            delta: DeltaMap::default(),
        },
        output_bytes,
    })
}

impl OrderNoIndex {
    /// The base tile (an `Arc<Tile>`) for the read-ahead worker's warm view.
    /// Sharing it is safe: the tile is immutable after `TileWriter::finish`.
    pub(crate) fn tile(&self) -> Arc<Tile> {
        Arc::clone(&self.base)
    }

    /// The highest movement CSN the index covers: the delta's coverage when the
    /// delta holds anything, the base tile's cutoff otherwise. This is the value
    /// [`rebuild_trigger`] compares against the engine CSN.
    pub fn coverage_csn(&self) -> u64 { self.delta.coverage_csn.max(self.base.cutoff()) }

    /// The highest CSN the base tile alone covers: the coverage a delta is built
    /// above.
    pub fn base_coverage_csn(&self) -> u64 { self.base.cutoff() }

    /// Live entries in the index: the base's entries plus the delta's.
    pub fn entries(&self) -> u64 { self.base.count() + self.delta.live_entries() }

    /// Handles the delta says anything about (the merge threshold's numerator).
    pub fn delta_entries(&self) -> u64 { self.delta.entries() }

    /// Whether the delta has grown enough to pay for rewriting the base:
    /// `delta_entries >= max(INDEX_MERGE_MIN_DELTA_ENTRIES, base_entries)`.
    pub fn needs_merge(&self) -> bool {
        self.delta.entries() >= INDEX_MERGE_MIN_DELTA_ENTRIES.max(self.base.count())
    }

    /// The base + a copy of the delta, for an off-path merge. `None` until the
    /// delta is big enough to be worth merging.
    pub fn merge_snapshot(&self) -> Option<MergeSnapshot> {
        self.needs_merge().then(|| MergeSnapshot {
            base: IndexBase { tile: Arc::clone(&self.base), key_encoding: self.key_encoding },
            delta: self.delta.clone(),
        })
    }

    /// Insert one newer interval into the delta. The caller has already checked
    /// that the interval was built against this base
    /// ([`Engine::adopt_order_no_delta`]).
    pub(crate) fn extend_delta(&mut self, delta: DeltaMap) {
        self.delta.absorb(delta);
    }

    /// Probe the index for `order_no`. `Some(handle)` when the index holds a live
    /// entry, `None` otherwise (absent entry, a tombstone, or no index). Never
    /// authoritative: the caller cross-checks the returned document and falls back
    /// to the scan on a miss or mismatch.
    pub fn lookup(&self, order_no: &[u8]) -> io::Result<Option<u64>> {
        // The delta is the newest state of every row above the base coverage, so
        // it answers first; an empty delta costs one length check.
        if !self.delta.is_empty() {
            match self.delta.answer(order_no) {
                DeltaAnswer::Hit(handle) => return Ok(Some(handle)),
                // The handle this `order_no` named is deleted in the delta, which
                // is newer than the base: the base entry must stay hidden.
                DeltaAnswer::Dead => return Ok(None),
                DeltaAnswer::Absent => {}
            }
        }
        let pfx = keys::unique_prefix_with(self.key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no)?;
        if !self.base.could_contain_prefix(&pfx) { return Ok(None); }
        let mut cursor = self.base.prefix_cursor(&pfx)?;
        if let Some((key, _)) = cursor.next()? {
            if let Some(handle) = entry_handle(key).filter(|_| key.len() == pfx.len() + 16) {
                if !self.delta.is_empty() && self.delta.dead_handles().contains(&handle) {
                    return Ok(None);
                }
                return Ok(Some(handle));
            }
        }
        Ok(None)
    }
}

/// Whether an adopted index must be rebuilt, and if so why.
///
/// The rule, in full: a rebuild is scheduled only when the committed state has
/// moved past what the index covers (`engine_csn > coverage_csn`) AND it can pay
/// for itself, which is one of
///
/// - the region had scan demand since the index was last made current
///   ([`Engine::order_no_index_lookups`] advanced): the scans are what the index
///   makes cheap, so keeping it current is the point of having it; or
/// - the coverage lags by at least `stale_bound` committed versions
///   ([`INDEX_REBUILD_STALE_CSN`] in production): a region written much faster
///   than it is scanned still gets covered instead of waiting for a scan that may
///   never come.
///
/// A lag of zero schedules nothing: the same snapshot would be rebuilt and the
/// same bytes rewritten. A caller that has a rebuild in flight must not ask again
/// (one at a time), which is what keeps two rebuilds from racing. The merge arm
/// is not part of this rule: [`OrderNoIndex::needs_merge`] fires on the delta's
/// size alone, because at that point the write is already owed.
pub fn rebuild_trigger(coverage_csn: u64, engine_csn: u64, scan_demand: bool, stale_bound: u64) -> Option<&'static str> {
    if engine_csn <= coverage_csn { return None; }
    if scan_demand { return Some(TRIGGER_SCAN_DEMAND); }
    if engine_csn - coverage_csn >= stale_bound { return Some(TRIGGER_STALE); }
    None
}

/// The serving-memtable half of a hybrid source: the region's rows above a
/// coverage CSN, one per handle at its newest committed version, copied inline on
/// the caller's thread and owned by the builder thread from then on.
///
/// One row per handle is the whole point of the copy being small: the memtable
/// may hold several versions of the same handle and the index only ever needs the
/// winning one. An empty `doc` is a tombstone and is kept, because the merge must
/// be able to tell "this handle is deleted" from "this handle is absent from the
/// memtable".
#[derive(Debug, Default)]
pub struct MemtableRows {
    /// `(handle, csn, doc)` ascending by handle; empty `doc` = tombstone.
    pub rows: Vec<(u64, u64, Vec<u8>)>,
    /// Bytes the bound was charged for: the document bytes held plus
    /// [`MEMTABLE_ROW_OVERHEAD_BYTES`] per row.
    pub bytes: usize,
    /// Live rows in the snapshot (tombstone rows excluded), i.e. the memtable arm
    /// of the non-emptiness witness.
    pub live_rows: usize,
    /// The engine CSN the snapshot speaks for: with the tile snapshot taken at the
    /// same instant, the state up to this CSN is represented, so it is the
    /// rebuild's coverage floor. A tombstone or a superseded version raises it
    /// without producing an entry, which is why coverage cannot be derived from
    /// the entries alone.
    pub csn: u64,
}

/// Why an inline memtable snapshot was refused: the region holds more charged
/// bytes (documents plus row overhead) than the caller's bound allows to be
/// copied on its thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemtableSnapshotOver {
    /// Handles in the region's memtable rows (tombstones included).
    pub rows: usize,
    /// Live rows among them.
    pub live_rows: usize,
    /// Bytes the bound was charged for.
    pub bytes: usize,
    /// The bound the caller set.
    pub bound: usize,
}

impl fmt::Display for MemtableSnapshotOver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "inline memtable snapshot would exceed the {bound}-byte bound \
             ({bytes} bytes over {rows} handles, {live_rows} live)",
            bound = self.bound,
            bytes = self.bytes,
            rows = self.rows,
            live_rows = self.live_rows,
        )
    }
}

/// The outcome of the bounded inline memtable snapshot
/// ([`Engine::snapshot_primary_memtable`]): either the region's rows above the
/// coverage were copied and the rebuild can proceed, or the region is over the
/// caller's bound and nothing was copied, so the rebuild is skipped instead of
/// stalling the thread that would have paid for it.
#[derive(Debug)]
pub enum MemtableSnapshot {
    /// The region fits the bound; its rows are owned by the caller from here.
    Copied(MemtableRows),
    /// The region does not fit: no document was copied.
    OverBound(MemtableSnapshotOver),
}

/// The hybrid source in key order: every row the two halves hold, `visit`ed
/// unreduced, so the caller applies the one newest-wins rule
/// ([`crate::engine::NewestPrimaryVersion`]) itself and the base build and the
/// delta build cannot disagree about which version of a handle is current. The
/// order is the one that rule reads - handle ascending, CSN descending - so each
/// handle's first row is the version the authoritative scan would serve.
///
/// A tie cannot arise: a publish evicts exactly the versions a tile holds, so the
/// memtable's version of a handle is always newer than any tile's, and the union
/// skips a tile row for a handle the snapshot already has.
/// Returns `(rows visited, rows the tile dedupe dropped)`.
fn scan_hybrid_source<F>(
    tile_scan: F,
    view: &TileReadView,
    memtable: &MemtableRows,
    mut visit: impl FnMut(u64, u64, &[u8]) -> io::Result<()>,
) -> io::Result<(u64, u64)>
where
    F: FnOnce(&TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
{
    let memtable_handles: HashSet<u64> = memtable.rows.iter().map(|(handle, _, _)| *handle).collect();
    // One document per handle in the tiles: the newest version seen. A tile walk
    // can yield a handle once per tile that holds a version of it, and only the
    // newest can win, so holding the older ones would only grow the buffer.
    let mut tile_rows: HashMap<u64, (u64, Vec<u8>)> = HashMap::new();
    let mut stale_rows: u64 = 0;
    tile_scan(view, &mut |handle, csn, doc| {
        if memtable_handles.contains(&handle) {
            // The memtable's version is newer and is the one the scan serves.
            stale_rows += 1;
            return Ok(());
        }
        match tile_rows.get_mut(&handle) {
            Some(current) if current.0 < csn => {
                *current = (csn, doc.to_vec());
                stale_rows += 1;
            }
            Some(_) => stale_rows += 1,
            None => {
                tile_rows.insert(handle, (csn, doc.to_vec()));
            }
        }
        Ok(())
    })?;

    let mut union: Vec<(u64, u64, &[u8])> = memtable
        .rows
        .iter()
        .map(|(handle, csn, doc)| (*handle, *csn, doc.as_slice()))
        .collect();
    union.extend(tile_rows.iter().map(|(handle, (csn, doc))| (*handle, *csn, doc.as_slice())));
    union.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));

    let mut visited: u64 = 0;
    for &(handle, csn, doc) in &union {
        visited += 1;
        visit(handle, csn, doc)?;
    }
    Ok((visited, stale_rows))
}

/// The base half of one off-path build: the whole merged region into a base tile,
/// reduced by the newest-wins rule that only serves LIVE rows (`accept`): a
/// tombstone or a superseded version contributes no entry, and the coverage floor
/// is what records that the state was covered.
fn rebuild_base<F>(
    dir: &Path,
    key_encoding: keys::KeyEncoding,
    view: TileReadView,
    tile_scan: F,
    memtable: MemtableRows,
    region_rows: RegionRows,
) -> io::Result<RebuildResult>
where
    F: FnOnce(&TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
{
    let memtable_rows = memtable.rows.len() as u64;
    let inline_bytes = memtable.bytes;
    let mut stale_rows = 0u64;
    let mut newest = crate::engine::NewestPrimaryVersion::new();
    let build = build_order_no_index(
        dir,
        key_encoding,
        |visit| {
            let (rows, tile_stale) = scan_hybrid_source(tile_scan, &view, &memtable, |handle, csn, doc| {
                match newest.accept(handle, csn, doc) {
                    Some((handle, csn, doc)) => visit(handle, csn, doc),
                    None => {
                        stale_rows += 1;
                        Ok(())
                    }
                }
            })?;
            stale_rows += tile_stale;
            Ok(rows)
        },
        region_rows,
        memtable.csn,
        INDEX_TILE_NAME,
    )?;
    Ok(RebuildResult {
        product: IndexProduct::Index(build),
        stale_rows,
        memtable_rows,
        inline_bytes,
    })
}

/// The delta half of one off-path build: the rows above the base coverage, in
/// the newest committed state, into an in-RAM [`DeltaMap`]. Writes nothing.
///
/// The source is two halves, as the base build's is: the inline memtable
/// snapshot (already filtered to the rows above the previous coverage) and the
/// tile walk (filtered by [`TileReadView::scan_primary_after`], which touches no
/// tile when every published tile's cutoff is at or below that coverage). The
/// reduce here is `accept_version`, not `accept`: a tombstone at the handle's
/// newest version is the fact that hides its base entry, so the delta must see it.
///
/// An empty delta is legitimate when the interval changed nothing in this region
/// (another region's commits raised the engine CSN). The base build's
/// [`RegionRows`] refusal cannot apply here for that reason; this build instead
/// refuses when the inline snapshot copied rows of this region and the reduce
/// indexed none of them, which is a defect in the source or the merge rather
/// than an empty interval.
fn rebuild_delta<F>(
    base_coverage_csn: u64,
    view: TileReadView,
    tile_scan: F,
    memtable: MemtableRows,
) -> io::Result<RebuildResult>
where
    F: FnOnce(&TileReadView, &mut dyn FnMut(u64, u64, &[u8]) -> io::Result<()>) -> io::Result<u64>,
{
    let snapshot_rows = memtable.rows.len() as u64;
    let mut delta = DeltaMap::with_coverage(memtable.csn);
    let mut stale_rows = 0u64;
    let mut newest = crate::engine::NewestPrimaryVersion::new();
    let (rows, tile_stale) = scan_hybrid_source(tile_scan, &view, &memtable, |handle, csn, doc| {
        match newest.accept_version(handle, csn, doc) {
            Some((handle, csn, doc)) => {
                if doc.is_empty() {
                    delta.dead.insert(handle);
                } else {
                    let order_no = index_order_no(doc)?;
                    delta.live.insert(order_no.into_bytes(), (handle, csn));
                }
            }
            None => stale_rows += 1,
        }
        Ok(())
    })?;
    stale_rows += tile_stale;
    if snapshot_rows > 0 && delta.is_empty() {
        return Err(refused(format!(
            "order_no index delta produced 0 entries for an interval whose memtable snapshot \
             copied {snapshot_rows} rows above the coverage (the source visited {rows} rows): \
             the newest-wins merge dropped them, so the delta would silently leave those rows \
             on the full-entity fallback")));
    }
    Ok(RebuildResult {
        product: IndexProduct::Delta(DeltaBuild { delta, base_coverage_csn }),
        stale_rows,
        memtable_rows: snapshot_rows,
        inline_bytes: 0,
    })
}

/// Merge the adopted base and its delta into a new base tile and drop the delta.
///
/// The delta holds the newest version of every handle above the base coverage,
/// already resolved by the one newest-wins rule, so the merge is a set operation
/// and not a second version rule: a delta live entry replaces the base entry for
/// its handle, and a delta tombstone removes it. The merged tile's cutoff is the
/// delta's coverage, so the new base covers exactly the state the pair covered.
fn merge_index(dir: &Path, snapshot: MergeSnapshot) -> io::Result<RebuildResult> {
    let MergeSnapshot { base, delta } = snapshot;
    if delta.coverage_csn() <= base.coverage_csn() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "delta coverage {} does not advance the base coverage {}",
                delta.coverage_csn(),
                base.coverage_csn()
            ),
        ));
    }
    let live: HashSet<u64> = delta.live.values().map(|(handle, _)| *handle).collect();
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity((base.tile.count() + delta.live_entries()) as usize);
    for entry in base.tile.entries() {
        let (key, value) = entry?;
        let handle = entry_handle(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "derived index entry key is too short"))?;
        // The delta is newer than the base for every handle it names, whether the
        // newer state is live (its entry replaces the base's) or a tombstone.
        if live.contains(&handle) || delta.dead_handles().contains(&handle) { continue; }
        entries.push((key, value));
    }
    // Each handle's NEWEST delta entry, by the same rule the scan uses: the delta
    // can hold an older order_no of a handle it has since seen again (an update
    // inside the delta's range), and writing that superseded entry into the new base
    // would leave it there for every later lookup to cross-check. Sorting into the
    // rule's key order (handle ascending, CSN descending) and taking the first row
    // per handle is that rule, not a second one.
    let mut rows: Vec<(&[u8], u64, u64)> =
        delta.live.iter().map(|(order_no, (handle, csn))| (order_no.as_slice(), *handle, *csn)).collect();
    rows.sort_unstable_by(|a, b| a.1.cmp(&b.1).then_with(|| b.2.cmp(&a.2)));
    let mut newest = crate::engine::NewestPrimaryVersion::new();
    for (order_no, handle, csn) in rows {
        if csn <= base.coverage_csn() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("delta entry for handle {handle} is at csn {csn}, not above the base coverage {}", base.coverage_csn()),
            ));
        }
        if newest.accept_version(handle, csn, order_no).is_none() { continue; }
        entries.push((
            keys::unique_key_with(base.key_encoding, INDEX_ENTITY, INDEX_FIELD, order_no, handle, csn)?,
            handle.to_be_bytes().to_vec(),
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let (tile, output_bytes) = write_index_tile(dir, INDEX_TILE_NAME, delta.coverage_csn(), &entries)?;
    Ok(RebuildResult {
        product: IndexProduct::Index(OrderNoIndexBuild {
            index: OrderNoIndex {
                base: Arc::new(tile),
                key_encoding: base.key_encoding,
                delta: DeltaMap::default(),
            },
            output_bytes,
        }),
        stale_rows: 0,
        memtable_rows: 0,
        inline_bytes: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Engine, Op};

    fn tmp_dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-index-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The negative control for the loud empty-build refusal: a region that really
    /// holds no live row is NOT a defect. The memtable is non-empty (another region
    /// is being written), which under the whole-memtable witness refused the build;
    /// the exact witness says Empty, the build returns Ok with no entries, and the
    /// sidecar it writes opens as a valid 0-entry tile.
    #[test]
    fn an_empty_region_builds_a_zero_entry_sidecar() {
        let dir = tmp_dir("emptyregion");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"c0", &[Op::PutDoc {
            entity: b"Customer".to_vec(),
            handle: 1,
            doc: br#"{"email":"a@b"}"#.to_vec(),
        }])
        .unwrap();
        assert!(e.serving_memtable_entries() > 0, "the memtable is busy with another region");
        assert_eq!(movement_region_rows(&e), RegionRows::Empty, "the movement region itself holds no live row");

        let build = build_order_no_index(
            &dir,
            e.key_encoding(),
            |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
            movement_region_rows(&e),
            e.csn(),
            "empty-index.tile",
        )
        .expect("an empty region is a legitimate empty index, not a defect");
        assert_eq!(build.index.entries(), 0);
        assert_eq!(build.index.coverage_csn(), e.csn(), "the floor records the state the empty index covers");

        let path = dir.join("empty-index.tile");
        let tile = Tile::open(&path).expect("a zero-entry sidecar still opens as a tile");
        assert_eq!(tile.first_key(), None, "the sidecar holds no key");
        assert_eq!(tile.last_key(), None);
        assert_eq!(tile.cutoff(), e.csn());
        assert_eq!(build.index.lookup(b"O1").unwrap(), None, "and it answers no lookup");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The merge compacts the delta with the one newest-wins rule: a handle the
    /// delta saw twice keeps only its newest order_no in the new base, and the base
    /// entry that update superseded is dropped, so neither survives as a stale entry
    /// every later lookup would cross-check.
    #[test]
    fn a_merge_keeps_only_each_handles_newest_delta_entry() {
        let dir = tmp_dir("mergenewest");
        let mut e = Engine::create(dir.join("data.wal")).unwrap();
        e.commit_block(b"v0", &[Op::PutDoc {
            entity: INDEX_ENTITY.to_vec(),
            handle: 5,
            doc: br#"{"order_no":"A"}"#.to_vec(),
        }])
        .unwrap();
        let base = build_order_no_index(
            &dir,
            e.key_encoding(),
            |visit| e.scan_primary_with_csn(INDEX_ENTITY, visit),
            movement_region_rows(&e),
            e.csn(),
            "base-only.tile",
        )
        .unwrap();
        let base_coverage = base.index.base_coverage_csn();
        // The delta saw handle 5 twice: order B, then order C.
        let mut delta = DeltaMap::with_coverage(base_coverage + 2);
        delta.live.insert(b"B".to_vec(), (5, base_coverage + 1));
        delta.live.insert(b"C".to_vec(), (5, base_coverage + 2));
        let merged = merge_index(
            &dir,
            MergeSnapshot {
                base: IndexBase { tile: base.index.tile(), key_encoding: e.key_encoding() },
                delta,
            },
        )
        .unwrap();
        let IndexProduct::Index(build) = merged.product else { panic!("the merge produces a base index") };
        let index = build.index;
        assert_eq!(index.entries(), 1, "one live handle");
        assert_eq!(index.lookup(b"C").unwrap(), Some(5), "the newest order_no is kept");
        assert_eq!(index.lookup(b"B").unwrap(), None, "the superseded delta entry is not carried into the base");
        assert_eq!(index.lookup(b"A").unwrap(), None, "and neither is the base entry the update replaced");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
