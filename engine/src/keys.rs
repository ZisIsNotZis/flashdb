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
//!
//! 两种排列（[`KeyEncoding`]）：默认 [`KeyEncoding::FamilyFirst`] 把族标记放在
//! 最前（`family | entity | 0 | rest`），同一实体的 `P`/`U`/`R` 键按族分居三处；
//! [`KeyEncoding::EntityFirst`] 把标记 [`ENTITY_FIRST`] 放最前、实体紧随其后
//! （`ENTITY_FIRST | entity | 0 | family | rest`），使同一实体的 `P`/`U`/`R` 键在
//! 字节序上相邻，从而共享同一个 [`region_prefix`]、可被分区写路径放进同一个
//! tile。两种排列的键首字节不同（`P`/`U`/`R` 对 [`ENTITY_FIRST`]），且 [`ENTITY_FIRST`]
//! 不在 `{P, U, R}` 内，故两种键互不为前缀；解析函数同时接受二者。

pub const P: u8 = b'P';
pub const U: u8 = b'U';
pub const R: u8 = b'R';

/// 实体优先（co-location）排布的首字节标记。取值不在 `{P, U, R}` 内，故两种排布
/// 的键可从首字节区分，且不会与族优先键互为前缀。
pub const ENTITY_FIRST: u8 = b'C';

/// 写路径产生的键排列。读路径同时接受两种排列，所以选择器只决定新键长什么样。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyEncoding {
    /// 今天的排布：`family | entity | 0 | rest`，区域 = `(family, entity)`，同一
    /// 实体的三族键分属三个区域。
    #[default]
    FamilyFirst,
    /// 实体优先排布：`ENTITY_FIRST | entity | 0 | family | rest`。同一实体的三族键
    /// 同属 `(encoding, entity)` 区域，分区写路径可将其放进同一个 tile。
    EntityFirst,
}

/// 名称为空或含 0x00。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidName;

impl std::fmt::Display for InvalidName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid keyspace name: empty or contains 0x00")
    }
}

impl std::error::Error for InvalidName {}

impl From<InvalidName> for std::io::Error {
    fn from(_: InvalidName) -> Self {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid keyspace name: empty or contains 0x00",
        )
    }
}

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

/// 写出键的 `(family, entity)` 前缀：`family | entity | 0`（[`KeyEncoding::FamilyFirst`]）
/// 或 `ENTITY_FIRST | entity | 0 | family`（[`KeyEncoding::EntityFirst`]）。调用方随后
/// 追加族特有的 rest。
fn push_family(buf: &mut Vec<u8>, enc: KeyEncoding, family: u8, entity: &[u8]) -> Result<(), InvalidName> {
    match enc {
        KeyEncoding::FamilyFirst => {
            buf.push(family);
            push_name(buf, entity)
        }
        KeyEncoding::EntityFirst => {
            buf.push(ENTITY_FIRST);
            push_name(buf, entity)?;
            buf.push(family);
            Ok(())
        }
    }
}

/// CSN 降序：`!csn` 大端。csn 越大，字节越小，排越前。
fn csn_desc(csn: u64) -> [u8; 8] {
    (!csn).to_be_bytes()
}

/// 主存储键（默认族优先排列）：`P | entity | 0 | handle(8 BE) | ~csn(8 BE)`
pub fn primary_key(entity: &[u8], handle: u64, csn: u64) -> Result<Vec<u8>, InvalidName> {
    primary_key_with(KeyEncoding::FamilyFirst, entity, handle, csn)
}

/// [`primary_key`] 的显式排列版本。
pub fn primary_key_with(enc: KeyEncoding, entity: &[u8], handle: u64, csn: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(3 + entity.len() + 16);
    push_family(&mut k, enc, P, entity)?;
    k.extend_from_slice(&handle.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 单文档最新版本扫描前缀（默认族优先排列）：`P | entity | 0 | handle(8 BE)`
pub fn primary_prefix(entity: &[u8], handle: u64) -> Result<Vec<u8>, InvalidName> {
    primary_prefix_with(KeyEncoding::FamilyFirst, entity, handle)
}

/// [`primary_prefix`] 的显式排列版本。
pub fn primary_prefix_with(enc: KeyEncoding, entity: &[u8], handle: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(3 + entity.len() + 8);
    push_family(&mut k, enc, P, entity)?;
    k.extend_from_slice(&handle.to_be_bytes());
    Ok(k)
}

/// 唯一索引键（默认族优先排列）：`U | entity | 0 | field | 0 | len(4 BE) | value | handle(8 BE) | ~csn(8 BE)`
///
/// 占用检查需遍历此前缀，**按 handle 分组取最新版本**并忽略 tombstone；
/// 不能对整个前缀只读第一条（较小 handle 的墓碑会遮蔽较大 handle 的活条目）。
pub fn unique_key(
    entity: &[u8],
    field: &[u8],
    value: &[u8],
    handle: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    unique_key_with(KeyEncoding::FamilyFirst, entity, field, value, handle, csn)
}

/// [`unique_key`] 的显式排列版本。
pub fn unique_key_with(
    enc: KeyEncoding,
    entity: &[u8],
    field: &[u8],
    value: &[u8],
    handle: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(7 + entity.len() + field.len() + value.len() + 16);
    push_family(&mut k, enc, U, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&(value.len() as u32).to_be_bytes());
    k.extend_from_slice(value);
    k.extend_from_slice(&handle.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 唯一索引存在性扫描前缀（默认族优先排列）：`U | entity | 0 | field | 0 | len(4 BE) | value`
pub fn unique_prefix(entity: &[u8], field: &[u8], value: &[u8]) -> Result<Vec<u8>, InvalidName> {
    unique_prefix_with(KeyEncoding::FamilyFirst, entity, field, value)
}

/// [`unique_prefix`] 的显式排列版本。
pub fn unique_prefix_with(enc: KeyEncoding, entity: &[u8], field: &[u8], value: &[u8]) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(7 + entity.len() + field.len() + value.len());
    push_family(&mut k, enc, U, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&(value.len() as u32).to_be_bytes());
    k.extend_from_slice(value);
    Ok(k)
}

/// 反向引用键（默认族优先排列）：`R | entity | 0 | field | 0 | target(8 BE) | source(8 BE) | ~csn(8 BE)`
///
/// 同一 target 的全部 source 在键序上连续 → 反向查找 = 一次前缀扫。
pub fn reverse_key(
    entity: &[u8],
    field: &[u8],
    target: u64,
    source: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    reverse_key_with(KeyEncoding::FamilyFirst, entity, field, target, source, csn)
}

/// [`reverse_key`] 的显式排列版本。
pub fn reverse_key_with(
    enc: KeyEncoding,
    entity: &[u8],
    field: &[u8],
    target: u64,
    source: u64,
    csn: u64,
) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(5 + entity.len() + field.len() + 24);
    push_family(&mut k, enc, R, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&target.to_be_bytes());
    k.extend_from_slice(&source.to_be_bytes());
    k.extend_from_slice(&csn_desc(csn));
    Ok(k)
}

/// 反向查找扫描前缀（默认族优先排列）：`R | entity | 0 | field | 0 | target(8 BE)`
pub fn reverse_prefix(entity: &[u8], field: &[u8], target: u64) -> Result<Vec<u8>, InvalidName> {
    reverse_prefix_with(KeyEncoding::FamilyFirst, entity, field, target)
}

/// [`reverse_prefix`] 的显式排列版本。
pub fn reverse_prefix_with(enc: KeyEncoding, entity: &[u8], field: &[u8], target: u64) -> Result<Vec<u8>, InvalidName> {
    let mut k = Vec::with_capacity(5 + entity.len() + field.len() + 8);
    push_family(&mut k, enc, R, entity)?;
    push_name(&mut k, field)?;
    k.extend_from_slice(&target.to_be_bytes());
    Ok(k)
}

/// 解析主存储键（两种排列都接受，用于 compaction 与调试）。
pub fn decode_primary(key: &[u8]) -> Option<(&[u8], u64, u64)> {
    let region = region_prefix(key)?;
    let marker = region[0];
    let entity = &region[1..region.len() - 1];
    let mut rest = &key[region.len()..];
    if marker == ENTITY_FIRST {
        if rest.first() != Some(&P) { return None; }
        rest = &rest[1..];
    } else if marker != P {
        return None;
    }
    if rest.len() != 16 {
        return None;
    }
    let handle = u64::from_be_bytes(rest[..8].try_into().ok()?);
    let csn = !u64::from_be_bytes(rest[8..].try_into().ok()?);
    Some((entity, handle, csn))
}

/// 从任意键尾解出 CSN（所有键都以 `~csn` 结尾，故与实体/字段名无关）。
pub fn key_csn(key: &[u8]) -> Option<u64> {
    if key.len() < 8 {
        return None;
    }
    Some(!u64::from_be_bytes(key[key.len() - 8..].try_into().ok()?))
}

/// 键的族标记 `P`/`U`/`R`（两种排列都接受），未知键返回 `None`。族优先键的首字节
/// 即族；实体优先键的首字节是 [`ENTITY_FIRST`]，族是实体名 0 之后的那个字节。
pub fn family_of(key: &[u8]) -> Option<u8> {
    let marker = *key.first()?;
    match marker {
        P | U | R => Some(marker),
        ENTITY_FIRST => {
            let rest = &key[1..];
            let z = rest.iter().position(|&b| b == 0)?;
            rest.get(z + 1).copied().filter(|&m| matches!(m, P | U | R))
        }
        _ => None,
    }
}

/// 解析反向引用键（两种排列都接受）：
/// `R | entity | 0 | field | 0 | target(8) | source(8) | ~csn(8)`（族优先）
/// 或 `ENTITY_FIRST | entity | 0 | R | field | 0 | target(8) | source(8) | ~csn(8)`。
/// → (target, source, csn)。非 R 键或长度不足返回 None。
pub fn decode_reverse(key: &[u8]) -> Option<(u64, u64, u64)> {
    if family_of(key)? != R {
        return None;
    }
    if key.len() < 26 {
        return None;
    }
    let body = &key[key.len() - 24..];
    let target = u64::from_be_bytes(body[..8].try_into().ok()?);
    let source = u64::from_be_bytes(body[8..16].try_into().ok()?);
    let csn = !u64::from_be_bytes(body[16..24].try_into().ok()?);
    Some((target, source, csn))
}

/// The key's **region prefix**: `MARKER | entity | 0` - the bytes that identify
/// the region of any `P`/`U`/`R` key. Under [`KeyEncoding::FamilyFirst`] the
/// marker is the key family, so the region is `(family, entity)`; under
/// [`KeyEncoding::EntityFirst`] the marker is [`ENTITY_FIRST`], so the region is
/// `(encoding, entity)` and an entity's three families share it. Both spellings
/// place the 0-terminated entity right after the marker, so this parse is
/// family-independent. Returns `None` for an unknown marker or a key with no
/// entity terminator.
///
/// This is the partitioning unit used by the region-partitioned write paths:
/// keys whose region prefix is equal belong to the same tile. Because the
/// prefix is a key prefix, byte order groups a region's keys contiguously and
/// orders regions by `(marker, entity)`, which is exactly the secondary sort of
/// the `(cutoff, region)` tile-list ordering.
pub fn region_prefix(key: &[u8]) -> Option<&[u8]> {
    let marker = *key.first()?;
    if marker != P && marker != U && marker != R && marker != ENTITY_FIRST {
        return None;
    }
    let rest = &key[1..];
    let entity_end = rest.iter().position(|&b| b == 0)?;
    Some(&key[..entity_end + 2])
}

/// The canonical `"<family>|<entity>"` region name for a [`region_prefix`] or a
/// `(marker, entity)` pair, as consumed by [`crate::engine::LayoutPolicy`]. The
/// entity is rendered lossily: the name is a policy-lookup key only, never an
/// identity, so two entities that sanitize alike merely share a policy. Under
/// [`KeyEncoding::EntityFirst`] the marker is [`ENTITY_FIRST`], so the name is
/// `C|entity` - a region no declared policy names, which simply falls through to
/// the default.
pub fn region_name(prefix: &[u8]) -> String {
    let marker = prefix.first().copied().unwrap_or(b'?');
    let end = prefix.len().saturating_sub(1);
    let entity = if prefix.len() >= 2 { &prefix[1..end] } else { &[][..] };
    let mut name = String::with_capacity(2 + entity.len());
    name.push(marker as char);
    name.push('|');
    name.push_str(&String::from_utf8_lossy(entity));
    name
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

    #[test]
    fn region_prefix_groups_by_family_and_entity_only() {
        // Same entity, same family, different field/handle/value/csn: one region.
        let p1 = primary_key(b"Stock", 1, 1).unwrap();
        let p2 = primary_key(b"Stock", 9, 7).unwrap();
        assert_eq!(region_prefix(&p1), region_prefix(&p2));
        let u1 = unique_key(b"Stock", b"sku", b"A", 1, 1).unwrap();
        let u2 = unique_key(b"Stock", b"loc", b"B", 2, 2).unwrap();
        assert_eq!(region_prefix(&u1), region_prefix(&u2), "fields share the (family, entity) region");
        // Different family or entity: different region, and the prefix is the
        // marker plus the 0-terminated entity.
        assert_ne!(region_prefix(&p1), region_prefix(&u1));
        assert_eq!(region_prefix(&p1), Some(&b"PStock\x00"[..]));
        assert_eq!(region_prefix(&u1), Some(&b"UStock\x00"[..]));
        let other = primary_key(b"Stockx", 1, 1).unwrap();
        assert_ne!(region_prefix(&p1), region_prefix(&other));
        // Region order is byte order of the prefix.
        assert!(region_prefix(&p1) < region_prefix(&u1), "P regions sort before U regions");
        assert!(region_prefix(&other) > region_prefix(&p1));
    }

    #[test]
    fn region_name_is_family_pipe_entity() {
        assert_eq!(region_name(b"PStock\x00"), "P|Stock");
        assert_eq!(region_name(b"UOrder\x00"), "U|Order");
        assert_eq!(region_name(b"R"), "R|");
        assert!(region_prefix(b"Xwhatever\x00").is_none(), "an unknown family is not a region");
        assert!(region_prefix(b"PNoTerminator").is_none());
    }

    // ---- entity-first encoding (KeyEncoding::EntityFirst) ----

    #[test]
    fn entity_first_colocates_one_entitys_three_families() {
        let enc = KeyEncoding::EntityFirst;
        let p = primary_key_with(enc, b"Stock", 7, 1).unwrap();
        let u = unique_key_with(enc, b"Stock", b"sku", b"A", 7, 1).unwrap();
        let r = reverse_key_with(enc, b"Stock", b"loc", 9, 7, 1).unwrap();
        // One entity's three families share a region prefix...
        let region = region_prefix(&p).unwrap();
        assert_eq!(region_prefix(&u), Some(region));
        assert_eq!(region_prefix(&r), Some(region));
        assert_eq!(region, &b"CStock\x00"[..]);
        // ...and sort adjacently, ahead of a later entity and behind an earlier one.
        let other = primary_key_with(enc, b"Stockx", 1, 1).unwrap();
        let earlier = primary_key_with(enc, b"Stoc", 1, 1).unwrap();
        // Family bytes order P < R < U within the entity; the earlier entity
        // sorts before all three and the later entity after all three.
        assert!(earlier < p && p < r && r < u && u < other, "entity-major byte order");
    }

    #[test]
    fn entity_first_prefixes_are_strict_prefixes_of_keys() {
        let enc = KeyEncoding::EntityFirst;
        let pfx = primary_prefix_with(enc, b"Stock", 7).unwrap();
        assert!(primary_key_with(enc, b"Stock", 7, 9).unwrap().starts_with(&pfx));
        let upfx = unique_prefix_with(enc, b"Stock", b"sku", b"A").unwrap();
        assert!(unique_key_with(enc, b"Stock", b"sku", b"A", 7, 9).unwrap().starts_with(&upfx));
        let rpfx = reverse_prefix_with(enc, b"Stock", b"loc", 9).unwrap();
        assert!(reverse_key_with(enc, b"Stock", b"loc", 9, 7, 9).unwrap().starts_with(&rpfx));
    }

    #[test]
    fn entity_first_decoders_accept_both_encodings() {
        let enc = KeyEncoding::EntityFirst;
        let p = primary_key_with(enc, b"Stock", 0xDEAD_BEEF, 42).unwrap();
        assert_eq!(decode_primary(&p).unwrap(), (&b"Stock"[..], 0xDEAD_BEEF, 42));
        let r = reverse_key_with(enc, b"Ticket", b"order", 7, 9, 5).unwrap();
        assert_eq!(decode_reverse(&r).unwrap(), (7, 9, 5));
        // The family helper resolves both spellings to the same family.
        assert_eq!(family_of(&p), Some(P));
        assert_eq!(family_of(&r), Some(R));
        assert_eq!(family_of(&primary_key(b"Stock", 1, 1).unwrap()), Some(P));
        assert_eq!(family_of(&reverse_key(b"Ticket", b"order", 7, 9, 5).unwrap()), Some(R));
        // A family-first key is not decoded as the wrong family.
        assert!(decode_primary(&unique_key(b"Stock", b"sku", b"A", 1, 1).unwrap()).is_none());
        assert!(decode_reverse(&primary_key(b"Stock", 1, 1).unwrap()).is_none());
    }

    #[test]
    fn entity_first_region_prefix_is_encoding_scoped() {
        // The same entity+family under the two encodings is two different regions:
        // the marker byte separates them, so keys of the two never share a tile.
        let f = primary_key(b"Stock", 1, 1).unwrap();
        let e = primary_key_with(KeyEncoding::EntityFirst, b"Stock", 1, 1).unwrap();
        assert_ne!(region_prefix(&f), region_prefix(&e));
        assert_eq!(region_name(region_prefix(&e).unwrap()), "C|Stock");
    }
}
