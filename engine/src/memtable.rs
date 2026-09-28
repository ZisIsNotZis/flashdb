//! Memtable：已提交块的内存视图 + 当前块 overlay 的读入口。
//!
//! key = [`crate::keys`] 的完整键（CSN 降序嵌在键尾），value = 文档字节。
//! **空 value = tombstone**（文档在模型里是非空 JSON，因此空即删除标记）。
//!
//! 快照点读规则：`prefix` 下按键序迭代（= CSN 降序），第一个
//! CSN ≤ snapshot 的条目即该快照的最新版本；tombstone → 不存在。

use std::collections::BTreeMap;

use crate::keys::key_csn;

#[derive(Default)]
pub struct Memtable {
    entries: BTreeMap<Vec<u8>, Vec<u8>>,
}

impl Memtable {
    pub fn new() -> Self {
        Self { entries: BTreeMap::new() }
    }

    /// 应用一条变更。`value` 为空 = tombstone。
    pub fn apply(&mut self, key: Vec<u8>, value: Vec<u8>) {
        self.entries.insert(key, value);
    }

    /// 快照点读：返回 `Some(文档字节)` 或 `None`（不存在 / 已删除）。
    ///
    /// `snapshot = u64::MAX` 表示"当前最新"——publish 时的唯一性检查
    /// 就是用它对 `U` 前缀做的。
    pub fn get(&self, prefix: &[u8], snapshot: u64) -> Option<&[u8]> {
        for (key, value) in self.entries.range(prefix.to_vec()..) {
            if !key.starts_with(prefix) {
                break;
            }
            let Some(csn) = key_csn(key) else { continue };
            if csn > snapshot {
                continue; // 键序内 CSN 递减，越往后越旧
            }
            return if value.is_empty() { None } else { Some(value) };
        }
        None
    }

    /// 前缀下的全部条目（含版本历史；调用方自行按 CSN 过滤）。
    pub fn scan(&self, prefix: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.entries
            .range(prefix.to_vec()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// 已有条目数（测试与统计）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    fn mt_with_versions() -> Memtable {
        let mut m = Memtable::new();
        // 同一文档两个版本：csn 3 旧值，csn 5 新值
        m.apply(keys::primary_key(b"Order", 7, 3).unwrap(), b"v3".to_vec());
        m.apply(keys::primary_key(b"Order", 7, 5).unwrap(), b"v5".to_vec());
        m
    }

    #[test]
    fn snapshot_picks_newest_visible_version() {
        let m = mt_with_versions();
        let pfx = keys::primary_prefix(b"Order", 7).unwrap();
        assert_eq!(m.get(&pfx, 9).unwrap(), b"v5");
        assert_eq!(m.get(&pfx, 5).unwrap(), b"v5");
        assert_eq!(m.get(&pfx, 4).unwrap(), b"v3");
        assert_eq!(m.get(&pfx, 2), None, "快照早于任何版本 → 不存在");
    }

    #[test]
    fn tombstone_hides_document() {
        let mut m = mt_with_versions();
        // csn 7 删除
        m.apply(keys::primary_key(b"Order", 7, 7).unwrap(), Vec::new());
        let pfx = keys::primary_prefix(b"Order", 7).unwrap();
        assert_eq!(m.get(&pfx, 9), None, "删除后的快照看不到文档");
        assert_eq!(m.get(&pfx, 6).unwrap(), b"v5", "删除前的快照仍可见");
    }

    #[test]
    fn prefixes_are_isolated() {
        let mut m = Memtable::new();
        m.apply(keys::primary_key(b"Order", 1, 1).unwrap(), b"o".to_vec());
        let stock_pfx = keys::primary_prefix(b"Stock", 1).unwrap();
        assert_eq!(m.get(&stock_pfx, u64::MAX), None);
    }

    #[test]
    fn key_csn_roundtrip() {
        let key = keys::unique_key(b"E", b"f", b"v", 3, 99).unwrap();
        assert_eq!(keys::key_csn(&key), Some(99));
        assert_eq!(keys::key_csn(b"short"), None);
    }
}
