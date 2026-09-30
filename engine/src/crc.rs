//! CRC-32C (Castagnoli) shared by the WAL, manifest, and tile framing.
//!
//! Every on-disk checksum uses the same code: reflected polynomial
//! `0x82F63B78`, init `!0`, final `!0`, so the standard check value holds:
//! `crc32c(b"123456789") == 0xE3069283`. This module replaces the three
//! byte-at-a-time table copies that made a sequential tile scan CPU-bound
//! below the device rate.
//!
//! The fast path is the x86_64 SSE4.2 CRC32 instruction, selected at runtime
//! via [`std::arch::is_x86_feature_detected!`]. Every other target (and any
//! x86_64 CPU without SSE4.2) uses the portable slice-by-16 table fallback.
//! Both backends are byte-for-byte equivalent and are checked against a
//! byte-at-a-time reference in this module's tests.

use std::sync::LazyLock;

/// One-shot CRC-32C over `data`.
///
/// Equivalent to `Crc32c::new().update(data).finish()`.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut c = Crc32c::new();
    c.update(data);
    c.finish()
}

/// Incremental CRC-32C.
///
/// Feeding the same bytes in any number of `update` calls, then `finish`,
/// yields the same value as one-shot [`crc32c`].
pub struct Crc32c {
    state: u32,
}

impl Crc32c {
    /// Start a new CRC. The raw state is initialised to `!0`.
    pub fn new() -> Self {
        Self { state: !0 }
    }

    /// Fold `data` into the running checksum.
    pub fn update(&mut self, data: &[u8]) {
        self.state = update(self.state, data);
    }

    /// Consume the checksum and return the inverted final value.
    pub fn finish(self) -> u32 {
        !self.state
    }
}

impl Default for Crc32c {
    fn default() -> Self {
        Self::new()
    }
}

/// Fold `data` into a raw (uninverted) CRC state.
fn update(state: u32, data: &[u8]) -> u32 {
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.2") {
            // SAFETY: guarded by the runtime CPU feature check just above.
            return unsafe { update_sse42(state, data) };
        }
    }
    update_slice16(state, data)
}

/// SSE4.2 hardware CRC32 over a raw state. `crc32q` folds eight bytes per
/// instruction; the final `< 8` bytes use `crc32b`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.2")]
unsafe fn update_sse42(state: u32, mut data: &[u8]) -> u32 {
    use std::arch::x86_64::{_mm_crc32_u8, _mm_crc32_u64};

    let mut crc = state as u64;
    while data.len() >= 8 {
        let word = u64::from_le_bytes(data[..8].try_into().unwrap());
        crc = _mm_crc32_u64(crc, word);
        data = &data[8..];
    }
    let mut crc = crc as u32;
    for &byte in data {
        crc = _mm_crc32_u8(crc, byte);
    }
    crc
}

/// Portable slice-by-16 table CRC over a raw state. Used on every non-SSE4.2
/// target and kept directly testable so the fallback is exercised on hosts
/// that do have SSE4.2.
fn update_slice16(mut crc: u32, data: &[u8]) -> u32 {
    let t = tables();
    let mut chunks = data.chunks_exact(16);
    for chunk in &mut chunks {
        crc ^= u32::from_le_bytes(chunk[..4].try_into().unwrap());
        crc = t[15][(crc & 0xff) as usize]
            ^ t[14][((crc >> 8) & 0xff) as usize]
            ^ t[13][((crc >> 16) & 0xff) as usize]
            ^ t[12][(crc >> 24) as usize]
            ^ t[11][chunk[4] as usize]
            ^ t[10][chunk[5] as usize]
            ^ t[9][chunk[6] as usize]
            ^ t[8][chunk[7] as usize]
            ^ t[7][chunk[8] as usize]
            ^ t[6][chunk[9] as usize]
            ^ t[5][chunk[10] as usize]
            ^ t[4][chunk[11] as usize]
            ^ t[3][chunk[12] as usize]
            ^ t[2][chunk[13] as usize]
            ^ t[1][chunk[14] as usize]
            ^ t[0][chunk[15] as usize];
    }
    for &byte in chunks.remainder() {
        crc = t[0][((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

/// The 16 slicing tables. `tables()[0]` is the byte-at-a-time table; each
/// higher table folds one more zero byte so a 16-byte chunk can be consumed
/// with 16 lookups instead of 128.
fn tables() -> &'static [[u32; 256]; 16] {
    static T: LazyLock<[[u32; 256]; 16]> = LazyLock::new(|| {
        let mut t = [[0u32; 256]; 16];
        for (i, slot) in t[0].iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        for k in 1..16 {
            for n in 0..256 {
                let prev = t[k - 1][n];
                t[k][n] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            }
        }
        t
    });
    &T
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original byte-at-a-time table implementation, retained only as an
    /// independent oracle. The fast backends must equal this for all inputs.
    fn reference(data: &[u8]) -> u32 {
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
        let mut c = !0u32;
        for &byte in data {
            c = T[((c ^ byte as u32) & 0xFF) as usize] ^ (c >> 8);
        }
        !c
    }

    /// Deterministic pseudo-random bytes so failures are reproducible.
    fn sample(len: usize, seed: u64) -> Vec<u8> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 32) as u8
            })
            .collect()
    }

    /// Every length in 0..=1024 (covers all alignment and tail cases) plus a
    /// few large and block-boundary lengths.
    fn lengths() -> Vec<usize> {
        let mut v: Vec<usize> = (0..=1024).collect();
        v.extend_from_slice(&[4096, 4097, 65536, 1_048_576]);
        v
    }

    #[test]
    fn known_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn one_shot_and_incremental_match_reference() {
        for len in lengths() {
            let data = sample(len, len as u64 ^ 0x9E37_79B9);
            let want = reference(&data);
            assert_eq!(crc32c(&data), want, "one-shot crc32c mismatch at len {len}");
            let mut inc = Crc32c::new();
            inc.update(&data);
            assert_eq!(inc.finish(), want, "incremental Crc32c mismatch at len {len}");
        }
    }

    #[test]
    fn slice16_fallback_matches_reference() {
        // Exercise the portable backend directly even though this host has
        // SSE4.2, so the fallback is not a correctness assumption.
        for len in lengths() {
            let data = sample(len, len as u64 ^ 0x5DEE_CE66);
            assert_eq!(!update_slice16(!0, &data), reference(&data), "slice-by-16 mismatch at len {len}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn sse42_backend_matches_reference() {
        if !std::arch::is_x86_feature_detected!("sse4.2") {
            return;
        }
        for len in lengths() {
            let data = sample(len, len as u64 ^ 0xC2B2_AE35);
            let got = !unsafe { update_sse42(!0, &data) };
            assert_eq!(got, reference(&data), "SSE4.2 mismatch at len {len}");
        }
    }

    #[test]
    fn one_byte_updates_compose_like_one_shot() {
        for len in lengths() {
            let data = sample(len, len as u64 ^ 0x27D4_EB2F);
            let one_shot = crc32c(&data);
            let mut inc = Crc32c::new();
            for &byte in &data {
                inc.update(std::slice::from_ref(&byte));
            }
            assert_eq!(inc.finish(), one_shot, "1-byte updates diverged at len {len}");
        }
    }
}
