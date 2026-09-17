//! 键空间编码：`P`（主存储）/ `U`（唯一查找）/ `R`（反向引用）。
//!
//! 字节序约定：
//!
//! - 同一文档的多个版本按 **CSN 降序**排列（键内存 `!csn`，位取反），
//!   因此前缀扫描的第一个条目永远是最新版本。
//! - 实体名与字段名 0 结尾；**名不得为空、不得含 0x00**（构造时拒绝），
//!   这同时保证 `"Or"` 的键不会成为 `"Order"` 键的前缀。
//! - 引用值按 `u32 BE` 长度前缀编码（值是任意字节，不能按 0 截断）。
//!
//! 删除（tombstone）编码在 value 里，不在 key 里；本模块只负责 key。

pub const P: u8 = b'P';
pub const U: u8 = b'U';
pub const R: u8 = b'R';

/// 名称为空或含 0x00。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidName;

/// 拒绝空名称与含 0x00 的名称。
pub fn check_name(name: &[u8]) -> Result<(), InvalidName> {
    if name.is_empty() || name.contains(&0) {
        Err(InvalidName)
    } else {
        Ok(())
    }
}

fn push_name(buf: &mut Vec<u8>, name: &[u8]) -> Result<(), InvalidName> {
    check_name(name)?;
    buf.extend_from_slice(name);
    buf.push(0);
    Ok(())
}

/// CSN 降序：`!csn` 大端。csn 越大，字节越小，排越前。
fn csn_desc(csn: u64) -> [u8; 8] {
    (!csn).to_be_bytes()
}

/// 主存储键：`P | entity | 0 | handle(8 BE) | ~csn(8 BE)`
pub fn primary_key(entity: &[u8], handle: u64, csn: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(2 + entity.len() + 16);
    k.push(P);
    push_name(&mut k, entity)?;
    k.extend_from_slice(&handle.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 单文档最新版本扫描前缀：`P | entity | 0 | handle(8 BE)`
pub fn primary_prefix(entity: &[u8], handle: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(2 + entity.len() + 8);
    k.push(P);
    push_name(&mut k, entity)?;
    k.extend_from_slice(&handle.to_be_bytes());
    Ok(k)
}

/// 唯一索引键：`U | entity | 0 | field | 0 | len(4 BE) | value | handle(8 BE) | ~csn(8 BE)`
///
/// 存在性检查 = 对 `unique_prefix` 做前缀扫（至多一个当前条目，由引擎在
/// publish 时强制）；值非唯一字段进不了这个前缀。
pub fn unique_key(
    entity: &[u8],
    field: &[u8],
    value: &[u8],
    handle: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(6 + entity.len() + field.len() + value.len() + 16);
    k.push(U);
    push_name(&mut k, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&(value.len() as u32).to_be_bytes());
    k.extend_from_slice(value);
    k.extend_from_slice(&handle.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 唯一索引存在性扫描前缀：`U | entity | 0 | field | 0 | len(4 BE) | value`
pub fn unique_prefix(entity: &[u8], field: &[u8], value: &[u8]) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(6 + entity.len() + field.len() + value.len());
    k.push(U);
    push_name(&mut k, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&(value.len() as u32).to_be_bytes());
    k.extend_from_slice(value);
    Ok(k)
}

/// 反向引用键：`R | entity | 0 | field | 0 | target(8 BE) | source(8 BE) | ~csn(8 BE)`
///
/// 同一 target 的全部 source 在键序上连续 → 反向查找 = 一次前缀扫。
pub fn reverse_key(
    entity: &[u8],
    field: &[u8],
    target: u64,
    source: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(4 + entity.len() + field.len() + 24);
    k.push(R);
    push_name(&mut k, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&target.to_be_bytes());
    k.extend_from_slice(&source.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 反向查找扫描前缀：`R | entity | 0 | field | 0 | target(8 BE)`
pub fn reverse_prefix(entity: &[u8], field: &[u8], target: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(4 + entity.len() + field.len() + 8);
    k.push(R);
    push_name(&mut k, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&target.to_be_bytes());
    Ok(k)
}

/// 解析主存储键（用于 compaction 与调试）。
pub fn decode_primary(key: &[u8]) -> Option<(&[u8], u64, u64)> {
    debug_assert_eq!(key[0], P);
    let rest = &key[1..];
    let z = rest.iter().position(|&b| b == 0)?;
    let entity = &rest[..z];
    let after = &rest[z + 1..];
    if after.len() != 16 {
        return None;
    }
    let handle = u64::from_be_bytes(after[..8].try_into().ok()?);
    let csn = !u64::from_be_bytes(after[8..].try_into().ok()?);
    Some((entity, handle, csn))
}

/// 从任意键尾解出 CSN（所有键都以 `~csn` 结尾，故与实体/字段名无关）。
pub fn key_csn(key: &[u8]) -> Option<u64> {
    if key.len() < 8 {
        return None;
    }
    Some(!u64::from_be_bytes(key[key.len() - 8..].try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_version_sorts_first() {
        let old = primary_key(b"Order", 1, 3).unwrap();
        let new = primary_key(b"Order", 1, 5).unwrap();
        assert!(new < old, "csn=5 (newer) must sort before csn=3");
    }

    #[test]
    fn handles_do_not_interleave() {
        let a = primary_key(b"Order", 1, 9).unwrap();
        let b = primary_key(b"Order", 2, 1).unwrap();
        assert!(a < b);
    }

    #[test]
    fn zero_termination_prevents_prefix_collision() {
        // "Or" 的扫描前缀不得命中 "Order" 的键。
        let short = primary_prefix(b"Or", 1).unwrap();
        let long = primary_key(b"Order", 7, 1).unwrap();
        assert!(!long.starts_with(&short));
    }

    #[test]
    fn unique_prefix_groups_by_value() {
        let k1 = unique_key(b"Stock", b"sku_loc", b"\x01A1L1", 1, 1).unwrap();
        let k2 = unique_key(b"Stock", b"sku_loc", b"\x01A1L1", 2, 1).unwrap();
        let other = unique_key(b"Stock", b"sku_loc", b"\x01A2L1", 3, 1).unwrap();
        let pfx = unique_prefix(b"Stock", b"sku_loc", b"\x01A1L1").unwrap();
        assert!(k1.starts_with(&pfx) && k2.starts_with(&pfx));
        assert!(!other.starts_with(&pfx));
        // 长度前缀保证 "A1" 不会命中 "A1_extra"
        let k3 = unique_key(b"Stock", b"sku_loc", b"\x01A1L1x", 4, 1).unwrap();
        assert!(!k3.starts_with(&pfx));
    }

    #[test]
    fn reverse_prefix_groups_targets() {
        let t7_a = reverse_key(b"Ticket", b"order", 7, 1, 1).unwrap();
        let t7_b = reverse_key(b"Ticket", b"order", 7, 2, 1).unwrap();
        let t9 = reverse_key(b"Ticket", b"order", 9, 1, 1).unwrap();
        let pfx = reverse_prefix(b"Ticket", b"order", 7).unwrap();
        assert!(t7_a.starts_with(&pfx) && t7_b.starts_with(&pfx));
        assert!(!t9.starts_with(&pfx));
    }

    #[test]
    fn decode_primary_roundtrip() {
        let key = primary_key(b"Stock", 0xDEAD_BEEF, 42).unwrap();
        let (entity, handle, csn) = decode_primary(&key).unwrap();
        assert_eq!(entity, b"Stock");
        assert_eq!(handle, 0xDEAD_BEEF);
        assert_eq!(csn, 42);
    }

    #[test]
    fn names_are_validated() {
        assert!(check_name(b"").is_err());
        assert!(check_name(b"a\x00b").is_err());
        assert!(primary_key(b"Bad\x00Name", 1, 1).is_err());
        assert!(unique_key(b"E", b"f\x00", b"v", 1, 1).is_err());
    }
}
