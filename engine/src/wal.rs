//! WAL：追加日志。
//!
//! 记录分帧：`[len u32 LE][crc32c(payload) u32 LE][payload]`。
//!
//! 恢复规则（SS-01 的机制落点）：
//!
//! - 顺序扫描；记录长度非法 / 载荷校验失败 / 文件截断 → 停在最后一条完整记录，
//!   其后视为撕裂尾；
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
use std::sync::LazyLock;

const HEADER: usize = 8; // len u32 LE + crc u32 LE
const MAX_RECORD: usize = 1 << 26; // 64 MiB，防御性上界

fn table() -> &'static [u32; 256] {
    static T: LazyLock<[u32; 256]> = LazyLock::new(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F63B78 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    });
    &*T
}

/// CRC-32C（Castagnoli，反射多项式 0x82F63B78）。
/// 校验值：`crc32c(b"123456789") == 0xE3069283`。
pub fn crc32c(data: &[u8]) -> u32 {
    let t = table();
    let mut c = !0u32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

/// 追加日志句柄。单写者；`sync` 调 `fdatasync`。
pub struct Wal {
    file: File,
    #[allow(dead_code)]
    path: PathBuf,
    valid_len: u64,
}

impl Wal {
    /// 新建（截断已有文件）。
    pub fn create(path: impl AsRef<Path>) -> io::Result<Wal> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path.as_ref())?;
        Ok(Wal { file, path: path.as_ref().to_path_buf(), valid_len: 0 })
    }

    /// 打开已存在的 WAL：扫描到最后的完整记录，截断撕裂尾，定位到文件尾。
    /// 返回句柄与恢复出的记录（按序）。
    pub fn open_or_recover(path: impl AsRef<Path>) -> io::Result<(Wal, Vec<Vec<u8>>)> {
        let (records, valid_len) = replay(path.as_ref())?;
        let mut file = OpenOptions::new().write(true).open(path.as_ref())?;
        file.set_len(valid_len)?;
        file.seek(SeekFrom::Start(valid_len))?;
        Ok((Wal { file, path: path.as_ref().to_path_buf(), valid_len }, records))
    }

    /// 追加一条记录（未持久化；调用方随后 `sync`）。拒绝空载荷和恢复器无法读取的超长载荷。
    pub fn append(&mut self, payload: &[u8]) -> io::Result<()> {
        if payload.is_empty() || payload.len() > MAX_RECORD {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "WAL payload length out of bounds"));
        }
        let mut frame = Vec::with_capacity(HEADER + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc32c(payload).to_le_bytes());
        frame.extend_from_slice(payload);
        self.file.write_all(&frame)?;
        self.valid_len += frame.len() as u64;
        Ok(())
    }

    /// `fdatasync`：调用返回后，此前所有 append 的记录对崩溃持久。
    pub fn sync(&self) -> io::Result<()> {
        self.file.sync_data()
    }

    /// 有效字节数（不含撕裂尾）。
    pub fn valid_len(&self) -> u64 {
        self.valid_len
    }
}

/// 顺序扫描 WAL，返回（完整记录, 有效字节数）。
/// 撕裂尾、校验失败、全零区域：静默停止——它们之后的字节从不是已承认的提交。
pub fn replay(path: impl AsRef<Path>) -> io::Result<(Vec<Vec<u8>>, u64)> {
    let file = File::open(path.as_ref())?;
    let mut r = BufReader::with_capacity(1 << 16, file);
    let mut records = Vec::new();
    let mut valid: u64 = 0;
    loop {
        let mut header = [0u8; HEADER];
        match r.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        let len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        if len == 0 || len > MAX_RECORD {
            break; // 全零（预分配未写）或垃圾长度 → 日志尾
        }
        let mut payload = vec![0u8; len];
        match r.read_exact(&mut payload) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
        if crc32c(&payload) != u32::from_le_bytes(header[4..].try_into().unwrap()) {
            break; // 撕裂或损坏 → 尾
        }
        records.push(payload);
        valid += (HEADER + len) as u64;
    }
    Ok((records, valid))
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
    fn corrupted_payload_stops_replay() {
        let p = tmp("corrupt.wal");
        let mut w = Wal::create(&p).unwrap();
        w.append(b"one").unwrap();
        w.append(b"two").unwrap();
        w.append(b"three").unwrap();
        drop(w);
        // 翻转第三条记录载荷的一个字节
        let mut bytes = fs::read(&p).unwrap();
        let off = bytes.len() - 2;
        bytes[off] ^= 0xFF;
        fs::write(&p, &bytes).unwrap();
        let (records, _) = replay(&p).unwrap();
        assert_eq!(records.len(), 2, "crc 损坏必须停在最后一条完整记录");
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
