//! Deterministic, persisted bloom filter over byte-string keys.
//!
//! This is a standard split-block-free bloom filter with a compact fixed header
//! so the whole filter is one self-describing byte string that a tile can store
//! and validate. It is dependency-free and its hash function is frozen: the
//! persisted bytes depend on it, so changing [`fnv1a64`] (or the double-hashing
//! derivation) is a format change.
//!
//! # The one invariant: no false negatives
//!
//! A bloom filter answers set membership approximately. `might_contain` returning
//! `false` is a *proof* that the key was never inserted; returning `true` means
//! "possibly present". **A false negative is a correctness bug** (it would make
//! the caller hide a live key), while **a false positive is only wasted work**
//! (the caller falls back to the exact, slower path). Callers must treat `false`
//! as definitive and `true` as a hint. The tests below assert the no-false-negative
//! direction exhaustively; the false-positive rate is only measured, never
//! asserted.
//!
//! # Hash function
//!
//! One 64-bit FNV-1a hash of the key seeds a double-hashing scheme: `h1 = h`,
//! `h2 = rotate_left(h, 32) | 1`, and probe `i` tests bit
//! `(h1 + i * h2) mod bit_count` for `i in 0..hash_count`. Forcing `h2` odd
//! keeps the probe sequence from collapsing for keys with structured hashes.
//! `hash_count` is derived from `bits_per_key` as `round(bits_per_key * ln 2)`
//! clamped to `1..=32`.
//!
//! # Persisted layout (little-endian)
//!
//! ```text
//! [bit_count u64][entry_count u64][bits_per_key u32][hash_count u8][reserved 3 = 0]
//! [bit array: ceil(bit_count / 8) bytes]
//! ```
//!
//! The bit array length is implied by `bit_count`, so a byte string is valid
//! only when its length is exactly `24 + ceil(bit_count / 8)`; trailing bytes
//! are rejected. A zero-entry filter has an all-zero bit array.

// This module is the format-only slice 08d-2a: the builder and the parts of the
// read API not yet exercised by `tile.rs` become live when the engine wires the
// bloom into the probe path in 08d-2b. Allow the not-yet-wired surface so the
// workspace build stays warning-free in the meantime.
#![allow(dead_code)]

use std::io::{self, ErrorKind};

/// Bytes of the fixed bloom header that precede the bit array.
const HEADER: usize = 24;
/// FNV-1a 64 offset basis.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64 prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn invalid(message: &'static str) -> io::Error { io::Error::new(ErrorKind::InvalidData, message) }

/// FNV-1a, 64-bit. Frozen: the persisted filter bytes depend on this value.
fn fnv1a64(key: &[u8]) -> u64 {
    let mut h = FNV_OFFSET;
    for &byte in key {
        h ^= byte as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Double-hashing seeds `(h1, h2)` for one key. `h2` is forced odd so the probe
/// sequence `h1 + i*h2` walks distinct residues.
fn hashes(key: &[u8]) -> (u64, u64) {
    let h = fnv1a64(key);
    (h, h.rotate_left(32) | 1)
}

/// Optimal probe count for `bits_per_key` bits of filter per key, clamped to the
/// range [`Bloom::from_bytes`] accepts.
fn hash_count_for(bits_per_key: u32) -> u8 {
    let k = ((bits_per_key as f64) * std::f64::consts::LN_2).round();
    k.clamp(1.0, 32.0) as u8
}

/// Builds a bloom filter over a known key count. `expected_entries` sizes the bit
/// array up front (`bits_per_key` bits per expected key); inserting more keys than
/// expected only raises the fill ratio, it never changes the layout.
pub(crate) struct BloomBuilder {
    bits: Vec<u8>,
    bit_count: u64,
    entries: u64,
    bits_per_key: u32,
    hash_count: u8,
}

impl BloomBuilder {
    /// `bits_per_key` is clamped to `1..=64` so a nonsense knob cannot produce a
    /// filter the persisted form could not round-trip.
    pub(crate) fn new(expected_entries: u64, bits_per_key: u32) -> Self {
        let bits_per_key = bits_per_key.clamp(1, 64);
        let bit_count = expected_entries.saturating_mul(bits_per_key as u64).max(1);
        let bytes = bit_count.div_ceil(8) as usize;
        BloomBuilder {
            bits: vec![0u8; bytes],
            bit_count,
            entries: 0,
            bits_per_key,
            hash_count: hash_count_for(bits_per_key),
        }
    }

    /// Add one key. Every byte string is a valid key, including the empty one.
    pub(crate) fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = hashes(key);
        for i in 0..self.hash_count as u64 {
            let bit = h1.wrapping_add(h2.wrapping_mul(i)) % self.bit_count;
            self.bits[(bit >> 3) as usize] |= 1 << (bit & 7);
        }
        self.entries += 1;
    }

    pub(crate) fn finish(self) -> Bloom {
        Bloom {
            bits: self.bits,
            bit_count: self.bit_count,
            entries: self.entries,
            bits_per_key: self.bits_per_key,
            hash_count: self.hash_count,
        }
    }
}

/// A finished bloom filter; see the module docs for the persisted layout and the
/// false-negative contract.
#[derive(Clone, Debug)]
pub(crate) struct Bloom {
    bits: Vec<u8>,
    bit_count: u64,
    entries: u64,
    bits_per_key: u32,
    hash_count: u8,
}

impl Bloom {
    /// `false` is a proof the key was never inserted; `true` is "maybe". See the
    /// module docs: a false negative is a bug, a false positive only costs work.
    pub(crate) fn might_contain(&self, key: &[u8]) -> bool {
        let (h1, h2) = hashes(key);
        for i in 0..self.hash_count as u64 {
            let bit = h1.wrapping_add(h2.wrapping_mul(i)) % self.bit_count;
            if self.bits[(bit >> 3) as usize] & (1 << (bit & 7)) == 0 {
                return false;
            }
        }
        true
    }

    pub(crate) fn bits_per_key(&self) -> u32 { self.bits_per_key }
    pub(crate) fn hash_count(&self) -> u8 { self.hash_count }
    pub(crate) fn entry_count(&self) -> u64 { self.entries }

    /// Serialize to the self-describing form documented on the module.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER + self.bits.len());
        out.extend_from_slice(&self.bit_count.to_le_bytes());
        out.extend_from_slice(&self.entries.to_le_bytes());
        out.extend_from_slice(&self.bits_per_key.to_le_bytes());
        out.push(self.hash_count);
        out.extend_from_slice(&[0u8; 3]);
        out.extend_from_slice(&self.bits);
        out
    }

    /// Parse and structurally validate a persisted filter, failing closed
    /// (`InvalidData`) on anything a builder could not have produced: short
    /// header, non-zero reserved bytes, out-of-range `bits_per_key` or
    /// `hash_count`, a bit-array length inconsistent with `bit_count`, trailing
    /// bytes, or a zero-entry filter with set bits.
    pub(crate) fn from_bytes(bytes: &[u8]) -> io::Result<Bloom> {
        if bytes.len() < HEADER { return Err(invalid("bloom header truncated")); }
        let bit_count = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let entries = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let bits_per_key = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let hash_count = bytes[20];
        if bytes[21..24] != [0, 0, 0] { return Err(invalid("bloom reserved bytes nonzero")); }
        if !(1..=64).contains(&bits_per_key) { return Err(invalid("bloom bits_per_key out of range")); }
        if !(1..=32).contains(&hash_count) { return Err(invalid("bloom hash_count out of range")); }
        if bit_count == 0 { return Err(invalid("bloom bit count is zero")); }
        let array_len = usize::try_from(bit_count.div_ceil(8)).map_err(|_| invalid("bloom bit count too large"))?;
        let expected = HEADER.checked_add(array_len).ok_or_else(|| invalid("bloom length overflow"))?;
        if bytes.len() != expected { return Err(invalid("bloom length inconsistent with header")); }
        let bits = bytes[HEADER..].to_vec();
        if entries == 0 && bits.iter().any(|&b| b != 0) {
            return Err(invalid("bloom with zero entries has set bits"));
        }
        Ok(Bloom { bits, bit_count, entries, bits_per_key, hash_count })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic 8-byte key for probe `i`.
    fn key(i: u64) -> Vec<u8> {
        let mut h = i.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let mut v = Vec::with_capacity(8);
        for _ in 0..8 {
            v.push((h & 0xff) as u8);
            h >>= 8;
        }
        v
    }

    /// The filter's core contract, exhaustively: every inserted key reports
    /// present. The set is prefix-closed (every non-empty prefix of every key is
    /// also a member), which is exactly the situation a caller creates when the
    /// query key is a prefix of the stored key and it blooms the query key set.
    #[test]
    fn no_false_negatives_for_inserted_keys_and_their_prefixes() {
        let mut set: Vec<Vec<u8>> = Vec::new();
        for i in 0..3_000u64 {
            let full = format!("U|entity|field|{i:08}|value").into_bytes();
            for n in 1..=full.len() { set.push(full[..n].to_vec()); }
        }
        set.sort();
        set.dedup();
        let mut builder = BloomBuilder::new(set.len() as u64, 12);
        for s in &set { builder.insert(s); }
        let bloom = builder.finish();
        assert_eq!(bloom.entry_count(), set.len() as u64);
        for s in &set {
            assert!(bloom.might_contain(s), "inserted key must never be rejected: {s:?}");
        }
    }

    #[test]
    fn zero_entry_filter_has_no_bits_set_and_rejects_everything() {
        let bloom = BloomBuilder::new(0, 10).finish();
        assert_eq!(bloom.entry_count(), 0);
        assert!(bloom.to_bytes()[HEADER..].iter().all(|&b| b == 0), "an empty filter's bit array is all zero");
        assert!(!bloom.might_contain(b""), "an empty filter rejects any key");
        assert!(!bloom.might_contain(b"anything"));
        assert!(!bloom.might_contain(&key(0)));
    }

    #[test]
    fn round_trips_through_bytes() {
        let mut builder = BloomBuilder::new(500, 10);
        for i in 0..500u64 { builder.insert(&key(i)); }
        let bloom = builder.finish();
        let bytes = bloom.to_bytes();
        let parsed = Bloom::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.bits_per_key(), 10);
        assert_eq!(parsed.hash_count(), bloom.hash_count());
        assert_eq!(parsed.entry_count(), 500);
        for i in 0..500u64 { assert!(parsed.might_contain(&key(i))); }
        assert_eq!(parsed.to_bytes(), bytes, "re-serialization is byte-identical");
    }

    #[test]
    fn from_bytes_rejects_structural_damage() {
        let mut builder = BloomBuilder::new(100, 10);
        for i in 0..100u64 { builder.insert(&key(i)); }
        let good = builder.finish().to_bytes();

        assert_eq!(Bloom::from_bytes(&good[..HEADER - 1]).unwrap_err().kind(), ErrorKind::InvalidData, "short header");
        assert_eq!(Bloom::from_bytes(&[]).unwrap_err().kind(), ErrorKind::InvalidData, "empty input");

        let mut bad_hash = good.clone();
        bad_hash[20] = 0;
        assert_eq!(Bloom::from_bytes(&bad_hash).unwrap_err().kind(), ErrorKind::InvalidData, "hash_count 0");
        let mut bad_hash = good.clone();
        bad_hash[20] = 33;
        assert_eq!(Bloom::from_bytes(&bad_hash).unwrap_err().kind(), ErrorKind::InvalidData, "hash_count 33");

        let mut bad_bpk = good.clone();
        bad_bpk[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(Bloom::from_bytes(&bad_bpk).unwrap_err().kind(), ErrorKind::InvalidData, "bits_per_key 0");
        let mut bad_bpk = good.clone();
        bad_bpk[16..20].copy_from_slice(&65u32.to_le_bytes());
        assert_eq!(Bloom::from_bytes(&bad_bpk).unwrap_err().kind(), ErrorKind::InvalidData, "bits_per_key 65");

        let mut short_array = good.clone();
        short_array.pop();
        assert_eq!(Bloom::from_bytes(&short_array).unwrap_err().kind(), ErrorKind::InvalidData, "array truncated");
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(Bloom::from_bytes(&trailing).unwrap_err().kind(), ErrorKind::InvalidData, "trailing byte");

        let mut reserved = good.clone();
        reserved[21] = 1;
        assert_eq!(Bloom::from_bytes(&reserved).unwrap_err().kind(), ErrorKind::InvalidData, "reserved byte set");

        let empty_with_bits = {
            let mut b = BloomBuilder::new(0, 10).finish().to_bytes();
            b[HEADER] = 1;
            b
        };
        assert_eq!(Bloom::from_bytes(&empty_with_bits).unwrap_err().kind(), ErrorKind::InvalidData, "zero entries with set bits");
    }

    /// Measures (and prints) the false-positive rate at 10 bits/key. Not a
    /// threshold assertion: FP is a performance property, not a correctness one.
    #[test]
    fn false_positive_rate_at_10_bits_per_key() {
        let n = 100_000u64;
        let mut builder = BloomBuilder::new(n, 10);
        for i in 0..n { builder.insert(&key(i)); }
        let bloom = builder.finish();
        assert_eq!(bloom.hash_count(), 7, "10 bits/key implies 7 probes");
        let probes = 100_000u64;
        let mut false_positives = 0u64;
        for i in n..n + probes {
            if bloom.might_contain(&key(i)) { false_positives += 1; }
        }
        let rate = false_positives as f64 / probes as f64;
        println!(
            "bloom false-positive rate at 10 bits/key: n={n} probes={probes} false_positives={false_positives} rate={rate:.5}"
        );
        assert!(bloom.might_contain(&key(0)), "no false negatives among inserted keys");
    }

    #[test]
    fn hash_count_tracks_bits_per_key_and_is_clamped() {
        assert_eq!(hash_count_for(1), 1);
        assert_eq!(hash_count_for(10), 7);
        assert_eq!(hash_count_for(64), 32);
        // The builder clamps a nonsensical knob before deriving the probe count.
        assert_eq!(BloomBuilder::new(10, 0).finish().bits_per_key(), 1);
        assert_eq!(BloomBuilder::new(10, 1000).finish().bits_per_key(), 64);
    }
}
