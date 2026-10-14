//! WAL：追加日志。
//!
//! 记录分帧：`[len u32 LE][crc32c(payload) u32 LE][payload]`。
//!
//! 恢复规则（SS-01 的机制落点）：
//!
//! - 顺序扫描；不完整头/载荷视为撕裂尾；**完整**坏长度或 CRC 错误则
//!   拒绝恢复，避免把可能已确认的损坏记录当作可丢弃尾部；
//! - 全零头部（预分配但从未写过的区域）视作日志尾；
//! - 空载荷不合法（一条提交记录最少含块标识），写入即拒绝——否则全零区域会被
//!   解析成合法的空记录；
//! - [`Wal::open_or_recover`] 把文件截断到最后一条完整记录并定位到文件尾，
//!   之后的追加覆盖撕裂尾。
//!
//! 持久化时序由调用方保证（SS-01）：`append` → `sync` → 之后才允许发布 CSN。

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const HEADER: usize = 8; // len u32 LE + crc u32 LE
const MAX_RECORD: usize = 1 << 26; // 64 MiB，防御性上界

/// CRC-32C（Castagnoli，反射多项式 0x82F63B78）。
/// 校验值：`crc32c(b"123456789") == 0xE3069283`。
///
/// 共享实现见 [`crate::crc`]；此处保留再导出，使既有调用方不变。
pub use crate::crc::crc32c;

/// 追加日志句柄。单写者；`sync` 调 `fdatasync`。
pub struct Wal {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
    valid_len: u64,
    poisoned: bool,
    #[cfg(test)]
    inject_partial_append: bool,
    #[cfg(test)]
    inject_sync_error: bool,
    #[cfg(test)]
    inject_barrier_error: bool,
}

impl Wal {
    /// 仅新建：绝不截断已有 WAL；同步父目录使新文件名先于提交可持久。
    pub fn create(path: impl AsRef<Path>) -> io::Result<Wal> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()?;
        Ok(Wal { file, path: path.to_path_buf(), valid_len: 0, poisoned: false,
            #[cfg(test)] inject_partial_append: false, #[cfg(test)] inject_sync_error: false,
            #[cfg(test)] inject_barrier_error: false })
    }

    /// 打开已存在的 WAL：扫描到最后的完整记录，截断撕裂尾，定位到文件尾。
    /// 返回句柄与恢复出的记录（按序）。
    pub fn open_or_recover(path: impl AsRef<Path>) -> io::Result<(Wal, Vec<Vec<u8>>)> {
        let (records, valid_len) = replay(path.as_ref())?;
        let mut file = OpenOptions::new().read(true).write(true).open(path.as_ref())?;
        file.set_len(valid_len)?;
        file.seek(SeekFrom::Start(valid_len))?;
        Ok((Wal { file, path: path.as_ref().to_path_buf(), valid_len, poisoned: false,
            #[cfg(test)] inject_partial_append: false, #[cfg(test)] inject_sync_error: false,
            #[cfg(test)] inject_barrier_error: false }, records))
    }

    /// 追加一条记录（未持久化；调用方随后 `sync`）。拒绝空载荷和恢复器无法读取的超长载荷。
    pub fn append(&mut self, payload: &[u8]) -> io::Result<()> {
        let frame = self.encode_frame(payload)?;
        #[cfg(test)]
        if self.inject_partial_append {
            self.inject_partial_append = false;
            // Reproduce a short frame, then an I/O error. Do not let a later
            // append go past the invalid tail before recovery truncates it.
            if let Err(e) = self.file.write_all(&frame[..HEADER + 1]) {
                self.poisoned = true;
                return Err(e);
            }
            self.poisoned = true;
            return Err(io::Error::new(io::ErrorKind::Other, "injected partial WAL append"));
        }
        if let Err(e) = self.file.write_all(&frame) {
            self.poisoned = true;
            return Err(e);
        }
        self.valid_len += frame.len() as u64;
        Ok(())
    }

    /// Build the framed bytes for one record WITHOUT writing them
    /// (`[len u32 LE][crc32c(payload) u32 LE][payload]`). Used by the grouped
    /// commit path (slice 67) to defer the device-bound `write_all` off the
    /// request thread onto the WAL append/barrier worker: the frame is built
    /// cheaply on the pacer, and the worker later writes these exact bytes and
    /// `fdatasync`s them. The bounds and poison checks are identical to
    /// [`Wal::append`], so a buffered frame that would fail closed on the pacer
    /// also refuses to buffer.
    pub(crate) fn encode_frame(&self, payload: &[u8]) -> io::Result<Vec<u8>> {
        self.ensure_writable()?;
        if payload.is_empty() || payload.len() > MAX_RECORD {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "WAL payload length out of bounds"));
        }
        let mut frame = Vec::with_capacity(HEADER + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32c(payload).to_le_bytes());
        frame.extend_from_slice(payload);
        Ok(frame)
    }

    /// Advance the valid-length watermark after a worker has appended `len`
    /// bytes (slice 67). The grouped path appends off the request thread, so
    /// the writer only learns an append succeeded when the barrier completes;
    /// every `valid_len` consumer already runs with no barrier in flight, so
    /// the watermark never needs to see an in-flight group's bytes.
    pub(crate) fn record_appended(&mut self, len: u64) {
        self.valid_len += len;
    }

    /// `fdatasync`：失败后写者中毒，必须重新打开并重放 WAL 才能继续。
    pub fn sync(&mut self) -> io::Result<()> {
        self.ensure_writable()?;
        #[cfg(test)]
        if self.inject_sync_error {
            self.inject_sync_error = false;
            self.poisoned = true;
            return Err(io::Error::new(io::ErrorKind::Other, "injected WAL sync failure"));
        }
        if let Err(e) = self.file.sync_data() {
            self.poisoned = true;
            return Err(e);
        }
        Ok(())
    }

    fn ensure_writable(&self) -> io::Result<()> {
        if self.poisoned {
            Err(io::Error::new(io::ErrorKind::Other, "WAL writer poisoned; reopen for recovery"))
        } else { Ok(()) }
    }

    /// 有效字节数（不含撕裂尾）。
    pub fn valid_len(&self) -> u64 {
        self.valid_len
    }

    /// A duplicated handle to the live WAL file (for off-request-path workers
    /// that must read a bounded suffix without reopening the path, which a
    /// concurrent rotation may have renamed or unlinked).
    pub(crate) fn file_clone(&self) -> io::Result<File> {
        self.file.try_clone()
    }

    /// A duplicated handle to the live WAL file for an overlapped append+barrier
    /// worker (slice 56/67). The clone shares the open file description, so the
    /// worker's `write_all` appends at the same offset the main writer owns and
    /// `sync_data` flushes the same inode; the main writer is NOT borrowed while
    /// the worker runs. A failed append or sync is observed on the clone, so the
    /// caller must propagate it by [`Wal::poison`]ing the writer.
    /// Refuses (and therefore fails closed) once the writer is poisoned, exactly
    /// as [`Wal::sync`] does.
    pub(crate) fn sync_handle(&mut self) -> io::Result<File> {
        self.ensure_writable()?;
        self.file.try_clone()
    }

    /// Mark the writer poisoned so every later append/sync fails closed until
    /// reopen (recovery). Used when an overlapped barrier's `fdatasync` fails on
    /// a cloned handle, where the main writer cannot observe the error itself.
    pub(crate) fn poison(&mut self) {
        self.poisoned = true;
    }

    /// Test-only: arm the injected partial-append fault, so an engine-level test
    /// can exercise a failed append. Compiled only under `cfg(test)` and
    /// `pub(crate)`, so it is unreachable from the shipping API.
    #[cfg(test)]
    pub(crate) fn arm_partial_append_fault(&mut self) {
        self.inject_partial_append = true;
    }

    /// Test-only: arm the next overlapped barrier to fail its `fdatasync`, so an
    /// engine-level test can exercise the fail-closed barrier path. The flag is
    /// consumed by [`crate::engine::Engine::submit_group`] into the barrier job,
    /// keeping the clone untouched.
    #[cfg(test)]
    pub(crate) fn arm_barrier_error(&mut self) {
        self.inject_barrier_error = true;
    }

    /// Test-only: consume the armed overlapped-barrier fault.
    #[cfg(test)]
    pub(crate) fn take_barrier_error(&mut self) -> bool {
        let armed = self.inject_barrier_error;
        self.inject_barrier_error = false;
        armed
    }
}

/// 顺序扫描 WAL，返回（完整记录, 有效字节数）。
/// 不完整尾和全零区域停止；完整帧 CRC/长度损坏时失败并保留原文件。
/// 不完整尾仍无法与外部截断的已确认数据区分；不要据此声称抗任意介质损坏。
pub fn replay(path: impl AsRef<Path>) -> io::Result<(Vec<Vec<u8>>, u64)> {
    let mut records = Vec::new();
    let valid = replay_each(path, |payload| { records.push(payload.to_vec()); Ok(()) })?;
    Ok((records, valid))
}

/// 流式读取 WAL：逐条把完整载荷交给 `visit`，不收集整段后缀，因此一次遍历的
/// 调用方不必再持有一份 replay 记录向量。分帧、CRC、全零尾与失败即拒绝的规则
/// 与 [`replay`] 完全一致；返回有效字节数。
pub fn replay_each(path: impl AsRef<Path>, visit: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<u64> {
    let file = File::open(path.as_ref())?;
    let mut r = BufReader::with_capacity(1 << 16, file);
    replay_reader(&mut r, visit)
}

/// 从已经打开的 WAL 句柄流式读取前 `limit` 个有效字节，规则与 [`replay`] 完全
/// 一致。供离请求路径的压缩工作线程使用：把压缩任务创建时的 `valid_len` 作为
/// 上界，工作线程就绝不会看到快照之后才追加的记录（也绝不会撞上并发追加的
/// 撕裂尾之外的、父线程尚未确认的数据）。
///
/// 用 `pread`（`FileExt::read_at`）按显式偏移读，绝不动用共享的文件偏移：
/// `file` 通常来自 `Wal::file_clone`（dup），它与写路径共享同一个文件描述，
/// 若用带偏移的 `Read` 会把写游标一起搅乱。
pub fn replay_file_each(file: &File, limit: u64, mut visit: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<u64> {
    let mut offset: u64 = 0;
    let mut valid: u64 = 0;
    loop {
        if offset >= limit { break; }
        if limit - offset < HEADER as u64 { break; }
        let mut header = [0u8; HEADER];
        read_at_exact(file, &mut header, offset)?;
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if len == 0 {
            // Only an entirely zero remainder is a preallocated tail. A
            // zeroed acknowledged frame header followed by its old payload
            // must fail closed instead of truncating that committed record.
            if header[4..].iter().any(|&b| b != 0) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL zero-length header has nonzero CRC"));
            }
            let mut pos = offset + HEADER as u64;
            while pos < limit {
                let n = (limit - pos).min(8192) as usize;
                let mut buf = vec![0u8; n];
                read_at_exact(file, &mut buf, pos)?;
                if buf.iter().any(|&b| b != 0) {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL zero tail contains data"));
                }
                pos += n as u64;
            }
            break;
        }
        if len > MAX_RECORD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL record length exceeds limit"));
        }
        offset = offset.checked_add(HEADER as u64).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "WAL offset overflow"))?;
        if limit - offset < len as u64 { break; }
        let mut payload = vec![0u8; len];
        read_at_exact(file, &mut payload, offset)?;
        if crc32c(&payload) != u32::from_le_bytes(header[4..].try_into().unwrap()) {
            // A complete bad frame might be an acknowledged record corrupted
            // after sync. Never silently truncate it as an unacknowledged tail.
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL record CRC mismatch"));
        }
        visit(&payload)?;
        offset = offset.checked_add(len as u64).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "WAL offset overflow"))?;
        valid += (HEADER + len) as u64;
    }
    Ok(valid)
}

/// `pread` 精确读：显式偏移读满 `bytes`，不依赖也不改动共享文件偏移。
fn read_at_exact(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !bytes.is_empty() {
        match file.read_at(bytes, offset) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "WAL truncated during read")),
            Ok(n) => {
                offset = offset.checked_add(n as u64).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "WAL offset overflow"))?;
                bytes = &mut bytes[n..];
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// [`replay_file_each`] 的收集版本，返回（完整记录, 有效字节数）。
pub fn replay_file(file: &File, limit: u64) -> io::Result<(Vec<Vec<u8>>, u64)> {
    let mut records = Vec::new();
    let valid = replay_file_each(file, limit, |payload| { records.push(payload.to_vec()); Ok(()) })?;
    Ok((records, valid))
}

fn replay_reader(mut r: impl Read, mut visit: impl FnMut(&[u8]) -> io::Result<()>) -> io::Result<u64> {
    let mut valid: u64 = 0;
    loop {
        let mut header = [0u8; HEADER];
        match r.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if len == 0 {
            // Only an entirely zero remainder is a preallocated tail. A
            // zeroed acknowledged frame header followed by its old payload
            // must fail closed instead of truncating that committed record.
            if header[4..].iter().any(|&b| b != 0) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL zero-length header has nonzero CRC"));
            }
            let mut remainder = [0u8; 8192];
            loop {
                let n = r.read(&mut remainder)?;
                if n == 0 { break; }
                if remainder[..n].iter().any(|&b| b != 0) {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL zero tail contains data"));
                }
            }
            break;
        }
        if len > MAX_RECORD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL record length exceeds limit"));
        }
        let mut payload = vec![0u8; len];
        match r.read_exact(&mut payload) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        if crc32c(&payload) != u32::from_le_bytes(header[4..].try_into().unwrap()) {
            // A complete bad frame might be an acknowledged record corrupted
            // after sync. Never silently truncate it as an unacknowledged tail.
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WAL record CRC mismatch"));
        }
        visit(&payload)?;
        valid += (HEADER + len) as u64;
    }
    Ok(valid)
}

/// 仅删除文件（测试与工具用）。
#[cfg(test)]
pub(crate) fn remove(path: impl AsRef<Path>) {
    std::fs::remove_file(path).ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("flashdb-wal-{}-{name}", process::id()));
        fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn crc32c_matches_known_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn create_never_truncates_existing_wal() {
        let p = tmp("create_new.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"acknowledged").unwrap();
        w.sync().unwrap();
        drop(w);
        assert_eq!(Wal::create(&p).err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(replay(&p).unwrap().0, vec![b"acknowledged".to_vec()]);
        remove(&p);
    }

    #[test]
    fn append_replay_roundtrip() {
        let p = tmp("roundtrip.wal");
        let mut w = Wal::create(&p).unwrap();
        for rec in [b"alpha".as_slice(), b"beta", b"gamma"] {
            w.append(rec).unwrap();
        }
        w.sync().unwrap();
        let (records, len) = replay(&p).unwrap();
        assert_eq!(records, vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()]);
        assert_eq!(len, w.valid_len());
        remove(&p);
    }

    #[test]
    fn replay_each_matches_replay_and_propagates_errors() {
        let p = tmp("replay_each.wal");
        let mut w = Wal::create(&p).unwrap();
        for rec in [b"alpha".as_slice(), b"beta", b"gamma"] {
            w.append(rec).unwrap();
        }
        w.sync().unwrap();
        drop(w);
        let mut seen: Vec<Vec<u8>> = Vec::new();
        let valid = replay_each(&p, |payload| { seen.push(payload.to_vec()); Ok(()) }).unwrap();
        let (records, replay_valid) = replay(&p).unwrap();
        assert_eq!(seen, records, "streaming reader must yield the same records as replay");
        assert_eq!(valid, replay_valid);
        // A visitor error aborts the scan and is returned unchanged.
        let err = replay_each(&p, |_| Err(io::Error::new(io::ErrorKind::Other, "stop"))).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        remove(&p);
    }

    #[test]
    fn torn_tail_is_ignored() {
        let p = tmp("torn.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"one").unwrap();
        w.append(b"two").unwrap();
        w.sync().unwrap();
        let good = p.metadata().unwrap().len();
        w.append(b"three-partial").unwrap();
        // 截到第三条记录中间：撕裂尾
        let f = OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(good + 6).unwrap();
        drop(f);
        let (records, len) = replay(&p).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(len, good);
        remove(&p);
    }

    #[test]
    fn complete_corrupt_frame_fails_closed_without_truncation() {
        let p = tmp("corrupt.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"one").unwrap();
        w.append(b"two").unwrap();
        w.append(b"three").unwrap();
        w.sync().unwrap();
        drop(w);
        // 模拟已确认记录的介质损坏：翻转第三条记录载荷的一个字节
        let mut bytes = fs::read(&p).unwrap();
        let off = bytes.len() - 2;
        bytes[off] ^= 0xFF;
        fs::write(&p, &bytes).unwrap();
        assert_eq!(replay(&p).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(Wal::open_or_recover(&p).err().unwrap().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&p).unwrap(), bytes, "失败时不得截断原始证据");
        remove(&p);
    }

    #[test]
    fn zeroed_acknowledged_header_does_not_truncate_record() {
        for zero_crc in [false, true] {
            let p = tmp(if zero_crc { "zero_full_header.wal" } else { "zero_length.wal" });
            let mut w = Wal::create(&p).unwrap();
            w.append(b"first").unwrap();
            w.append(b"acknowledged-second").unwrap();
            w.sync().unwrap();
            drop(w);
            let mut bytes = fs::read(&p).unwrap();
            let second = HEADER + b"first".len();
            bytes[second..second + if zero_crc { HEADER } else { 4 }].fill(0);
            fs::write(&p, &bytes).unwrap();
            assert_eq!(Wal::open_or_recover(&p).err().unwrap().kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&p).unwrap(), bytes, "failure must preserve WAL evidence");
            remove(&p);
        }
    }

    #[test]
    fn impossible_nonzero_length_fails_closed() {
        let p = tmp("bad_length.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"acknowledged").unwrap();
        w.sync().unwrap();
        drop(w);
        let mut f = OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(&u32::MAX.to_le_bytes()).unwrap();
        f.write_all(&[0u8; 4]).unwrap();
        drop(f);
        let before = fs::read(&p).unwrap();
        assert_eq!(Wal::open_or_recover(&p).err().unwrap().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&p).unwrap(), before);
        remove(&p);
    }

    #[test]
    fn zeroed_region_is_log_tail() {
        let p = tmp("zeros.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"only").unwrap();
        w.sync().unwrap();
        // 模拟预分配：文件向后扩 4KB 全零
        let f = OpenOptions::new().write(true).open(&p).unwrap();
        f.set_len(p.metadata().unwrap().len() + 4096).unwrap();
        drop(f);
        let (records, _) = replay(&p).unwrap();
        assert_eq!(records, vec![b"only".to_vec()]);
        remove(&p);
    }

    #[test]
    fn recover_truncates_then_continues() {
        let p = tmp("recover.wal");
        {
            let mut w = Wal::create(&p).unwrap();
            w.append(b"one").unwrap();
            w.append(b"two").unwrap();
            w.sync().unwrap();
            let good = p.metadata().unwrap().len();
            w.append(b"tail-start").unwrap();
            drop(w);
            // 制造撕裂尾：第三条帧只剩 len 字段
            let f = OpenOptions::new().write(true).open(&p).unwrap();
            f.set_len(good + 4).unwrap();
            drop(f);
        }
        {
            let (mut w, records) = Wal::open_or_recover(&p).unwrap();
            assert_eq!(records.len(), 2, "撕裂尾之前的完整记录必须全部恢复");
            w.append(b"three").unwrap();
            w.sync().unwrap();
        }
        let (records, _) = replay(&p).unwrap();
        assert_eq!(records, vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()]);
        remove(&p);
    }

    #[test]
    fn empty_payload_rejected() {
        let p = tmp("empty.wal");
        let mut w = Wal::create(&p).unwrap();
        assert_eq!(w.append(b"").unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(w.valid_len(), 0);
        remove(&p);
    }

    #[test]
    fn partial_append_poison_prevents_later_ack_and_reopen_truncates() {
        let p = tmp("partial_poison.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"good").unwrap();
        w.sync().unwrap();
        let good_end = w.valid_len();
        w.inject_partial_append = true;
        assert!(w.append(b"bad").is_err());
        assert_eq!(w.valid_len(), good_end);
        assert!(w.append(b"later").unwrap_err().to_string().contains("poisoned"));
        assert!(w.sync().is_err());
        drop(w);
        let (mut w, records) = Wal::open_or_recover(&p).unwrap();
        assert_eq!(records, vec![b"good".to_vec()]);
        assert_eq!(w.valid_len(), good_end);
        w.append(b"later").unwrap();
        w.sync().unwrap();
        assert_eq!(replay(&p).unwrap().0, vec![b"good".to_vec(), b"later".to_vec()]);
        remove(&p);
    }

    #[test]
    fn sync_failure_poison_requires_recovery_before_more_writes() {
        let p = tmp("sync_poison.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"complete-but-unacked").unwrap();
        w.inject_sync_error = true;
        assert!(w.sync().is_err());
        assert!(w.append(b"must-not-follow").is_err());
        drop(w);
        // A full frame may be recovered even if its sync/ack failed; retry
        // dedup in Engine owns the uncertainty after reopening.
        let (mut w, records) = Wal::open_or_recover(&p).unwrap();
        assert_eq!(records, vec![b"complete-but-unacked".to_vec()]);
        w.append(b"after-recovery").unwrap();
        w.sync().unwrap();
        remove(&p);
    }

    #[test]
    fn oversized_payload_cannot_be_acknowledged_then_lost_on_replay() {
        let p = tmp("oversized.wal");
        let mut w = Wal::create(&p).unwrap();
        let oversized = vec![1u8; MAX_RECORD + 1];
        assert_eq!(w.append(&oversized).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert_eq!(w.valid_len(), 0);
        assert_eq!(p.metadata().unwrap().len(), 0);
        remove(&p);
    }
}
