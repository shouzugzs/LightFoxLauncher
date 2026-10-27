// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2025 LightFoxLauncher contributors
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! SHA-1 + MD5, hex helpers, and file hashing.
//! OWNER: net-core. API frozen by docs/CONTRACT.md.
//!
//! Both digests are implemented from FIPS 180-4 / RFC 1321 with fixed constant
//! tables only (no big lookup tables), so the release binary stays small.

use crate::util::Res;
use std::fs::File;
use std::io::Read;
use std::path::Path;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// Streaming SHA-1.
#[derive(Clone)]
pub struct Sha1 {
    h: [u32; 5],
    buf: [u8; 64],
    used: usize,
    total: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha1 {
    pub fn new() -> Self {
        Sha1 {
            h: [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0],
            buf: [0u8; 64],
            used: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut d: &[u8]) {
        self.total = self.total.wrapping_add(d.len() as u64);
        // Fill a partial block first.
        if self.used > 0 {
            let take = (64 - self.used).min(d.len());
            self.buf[self.used..self.used + take].copy_from_slice(&d[..take]);
            self.used += take;
            d = &d[take..];
            if self.used == 64 {
                let block = self.buf;
                compress_sha1(&mut self.h, &block);
                self.used = 0;
            }
        }
        // Whole blocks straight from the input.
        while d.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&d[..64]);
            compress_sha1(&mut self.h, &block);
            d = &d[64..];
        }
        if !d.is_empty() {
            self.buf[..d.len()].copy_from_slice(d);
            self.used = d.len();
        }
    }

    pub fn finalize(mut self) -> [u8; 20] {
        let bits = self.total.wrapping_mul(8);
        // Padding: 0x80, zeros, then the 64-bit big-endian bit count.
        self.update_no_len(&[0x80]);
        while self.used != 56 {
            self.update_no_len(&[0]);
        }
        let block = self.buf;
        // buf is now full up to 56; append the length in the last 8 bytes.
        let mut last = [0u8; 64];
        last[..56].copy_from_slice(&block[..56]);
        last[56..].copy_from_slice(&bits.to_be_bytes());
        compress_sha1(&mut self.h, &last);

        let mut out = [0u8; 20];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    /// Feed padding bytes without touching the message length counter.
    fn update_no_len(&mut self, d: &[u8]) {
        for &b in d {
            self.buf[self.used] = b;
            self.used += 1;
            if self.used == 64 {
                let block = self.buf;
                compress_sha1(&mut self.h, &block);
                self.used = 0;
            }
        }
    }
}

fn compress_sha1(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let t = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = t;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut s = Sha1::new();
    s.update(data);
    s.finalize()
}

pub fn sha1_hex(data: &[u8]) -> String {
    hex(&sha1(data))
}

// ---------------------------------------------------------------- MD5

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// `floor(abs(sin(i + 1)) * 2^32)` for i in 0..64 (RFC 1321, section 3.4).
const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

pub fn md5(data: &[u8]) -> [u8; 16] {
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    let bits = (data.len() as u64).wrapping_mul(8);

    let mut chunks = data.chunks_exact(64);
    for chunk in chunks.by_ref() {
        let mut block = [0u8; 64];
        block.copy_from_slice(chunk);
        md5_block(&mut h, &block);
    }
    let rest = chunks.remainder();

    // Length padding. Careful: length is the original message length.
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let pad_len = if rest.len() < 56 { 64 } else { 128 };
    tail[pad_len - 8..pad_len].copy_from_slice(&bits.to_le_bytes());
    for blk in 0..pad_len / 64 {
        let mut block = [0u8; 64];
        block.copy_from_slice(&tail[blk * 64..blk * 64 + 64]);
        md5_block(&mut h, &block);
    }

    let mut out = [0u8; 16];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn md5_block(h: &mut [u32; 4], block: &[u8; 64]) {
    let mut m = [0u32; 16];
    for i in 0..16 {
        m[i] = u32::from_le_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
    for i in 0..64 {
        let (f, g) = match i {
            0..=15 => ((b & c) | (!b & d), i),
            16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let tmp = d;
        d = c;
        c = b;
        b = b.wrapping_add(
            a.wrapping_add(f)
                .wrapping_add(MD5_K[i])
                .wrapping_add(m[g])
                .rotate_left(MD5_S[i]),
        );
        a = tmp;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
}

// ---------------------------------------------------------------- files

/// Streaming SHA-1 of a file as lowercase hex. Error names the path.
pub fn file_sha1(path: &Path) -> Res<String> {
    let mut f = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

/// True when the file exists and its SHA-1 matches `sha1_hex` (case-insensitive).
/// Missing/unreadable file => false. Empty `sha1_hex` => true when the file exists.
/// Feed the first `limit` bytes of a file into fresh 64 KiB blocks.
///
/// Used to re-establish the running digest of a resumed download: the bytes that
/// are already on disk must be hashed again before the remaining ones arrive, and
/// returning blocks keeps the caller's hasher streaming rather than buffering the
/// whole prefix.
pub fn file_sha1_prefix(path: &Path, limit: u64) -> Res<Vec<Vec<u8>>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)
        .map_err(|e| format!("cannot open {} for re-hashing: {e}", path.display()))?;
    let mut left = limit;
    let mut blocks = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = f
            .read(&mut buf[..want])
            .map_err(|e| format!("cannot read {} for re-hashing: {e}", path.display()))?;
        if n == 0 {
            return Err(format!(
                "{} ended after {} of {} bytes while re-hashing",
                path.display(),
                limit - left,
                limit
            ));
        }
        blocks.push(buf[..n].to_vec());
        left -= n as u64;
    }
    Ok(blocks)
}

pub fn file_matches(path: &Path, sha1_hex: &str) -> bool {
    if sha1_hex.is_empty() {
        return path.exists();
    }
    match file_sha1(path) {
        Ok(got) => hex_eq(&got, sha1_hex),
        Err(_) => false,
    }
}

/// ASCII case-insensitive hex comparison.
pub fn hex_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("lfl-hash-test-{}-{name}", std::process::id()));
        p
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[]), "");
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xab]), "000fffab");
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert!(!hex(&md5(b"")).contains('A'), "hex must be lowercase");
    }

    #[test]
    fn sha1_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(
            sha1_hex(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"),
            "a49b2446a02c645bf419f995b67091253a04a259"
        );
    }

    #[test]
    fn sha1_million_a() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(sha1_hex(&data), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn sha1_streaming_matches_oneshot() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i * 37 % 251) as u8).collect();
        let want = sha1_hex(&data);
        // 1-byte chunks.
        let mut h = Sha1::new();
        for b in &data {
            h.update(std::slice::from_ref(b));
        }
        assert_eq!(hex(&h.finalize()), want);
        // Odd chunk sizes, including > 64 and partial blocks.
        for chunk in [3usize, 7, 63, 64, 65, 127, 128, 200, 999, 1000] {
            let mut h = Sha1::new();
            for part in data.chunks(chunk.max(1)) {
                h.update(part);
            }
            assert_eq!(hex(&h.finalize()), want, "chunk size {chunk}");
        }
        // Empty updates are harmless.
        let mut h = Sha1::new();
        h.update(b"");
        h.update(b"abc");
        h.update(b"");
        assert_eq!(hex(&h.finalize()), sha1_hex(b"abc"));
        // Default impl agrees with new().
        let mut h = Sha1::default();
        h.update(b"abc");
        assert_eq!(hex(&h.finalize()), sha1_hex(b"abc"));
    }

    #[test]
    fn md5_vectors() {
        assert_eq!(hex(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&md5(b"message digest")),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            hex(&md5(b"abcdefghijklmnopqrstuvwxyz")),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            hex(&md5(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            )),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        // Block-boundary lengths: 55, 56, 63, 64, 65 bytes.
        assert_eq!(hex(&md5(&[b'a'; 55])), "ef1772b6dff9a122358552954ad0df65");
        assert_eq!(hex(&md5(&[b'a'; 56])), "3b0c8ac703f828b04c6c197006d17218");
        assert_eq!(hex(&md5(&[b'a'; 63])), "b06521f39153d618550606be297466d5");
        assert_eq!(hex(&md5(&[b'a'; 64])), "014842d480b571495a4a0363793f7367");
        assert_eq!(hex(&md5(&[b'a'; 65])), "c743a45e0d2e6a95cb859adae0248435");
    }

    #[test]
    fn file_sha1_and_matches() {
        let p = tmp("a.bin");
        let mut f = File::create(&p).expect("create temp file");
        f.write_all(b"abc").expect("write");
        drop(f);

        let want = "a9993e364706816aba3e25717850c26c9cd0d89d";
        assert_eq!(file_sha1(&p).as_deref(), Ok(want));
        assert!(file_matches(&p, want));
        assert!(file_matches(&p, &want.to_uppercase()), "case-insensitive");
        assert!(!file_matches(&p, "0000000000000000000000000000000000000000"));
        assert!(!file_matches(&p, "a9993e"), "short hash must not match");
        // Empty sha1 only requires existence.
        assert!(file_matches(&p, ""));
        // Missing file.
        let missing = tmp("missing.bin");
        assert!(!file_matches(&missing, ""));
        assert!(!file_matches(&missing, want));
        assert!(file_sha1(&missing).is_err());
        let e = file_sha1(&missing).unwrap_err();
        assert!(e.contains("missing.bin"), "error must name the path: {e}");

        // Larger file (multiple 64 KiB reads).
        let p2 = tmp("big.bin");
        let data = vec![0x5au8; 200 * 1024];
        let mut f = File::create(&p2).expect("create big");
        f.write_all(&data).expect("write big");
        drop(f);
        assert_eq!(file_sha1(&p2).as_deref(), Ok(sha1_hex(&data).as_str()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&p2);
    }

    #[test]
    fn hex_eq_compares() {
        assert!(hex_eq("abc", "ABC"));
        assert!(hex_eq("", ""));
        assert!(!hex_eq("abc", "abcd"));
        assert!(!hex_eq("abc", "abd"));
    }
}
