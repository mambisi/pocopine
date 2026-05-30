//! Centralized cryptographic hash and checksum primitives for the pocopine
//! workspace.
//!
//! The point of this crate is that the rest of the workspace never reaches for
//! `sha2`, `md-5`, or `crc32c` directly and never re-implements hex encoding or
//! incremental hashing. Everything goes through one small, algorithm-agnostic
//! API:
//!
//! ```
//! use pocopine_crypto::{Algorithm, Hasher, digest_hex, sha256_hex};
//!
//! // One-shot.
//! assert_eq!(sha256_hex(b""),
//!     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
//! assert_eq!(digest_hex(Algorithm::Crc32c, b""), "00000000");
//!
//! // Streaming (for data that never fits in one buffer).
//! let mut hasher = Hasher::new(Algorithm::Sha256);
//! hasher.update(b"hello ");
//! hasher.update(b"world");
//! let _hex = hasher.finalize_hex();
//! ```
//!
//! The underlying crates are re-exported (`pocopine_crypto::{sha2, md5,
//! crc32c}`) for the rare case a caller needs a primitive this API does not
//! wrap yet — but prefer adding to this API over reaching for them.
//!
//! This crate is `no_std` (it only needs `alloc` for the returned hex
//! `String`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

pub use crc32c;
pub use md5;
pub use sha2;

use md5::Md5;
use sha2::{Digest, Sha256};

/// A digest/checksum algorithm supported across the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Algorithm {
    /// SHA-256 — 32-byte cryptographic digest (64 hex chars).
    Sha256,
    /// MD5 — 16-byte digest (32 hex chars). Non-cryptographic; content
    /// integrity / provider-compat only.
    Md5,
    /// CRC32C (Castagnoli) — 32-bit checksum (8 hex chars).
    Crc32c,
}

impl Algorithm {
    /// Lowercase canonical name (`"sha256"`, `"md5"`, `"crc32c"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::Md5 => "md5",
            Algorithm::Crc32c => "crc32c",
        }
    }
}

/// Streaming, algorithm-agnostic hasher.
///
/// Feed bytes with [`Hasher::update`] as they stream and call
/// [`Hasher::finalize_hex`] once. Use this when the data never fits in a single
/// buffer (e.g. hashing an object as it streams from a storage backend).
pub struct Hasher {
    inner: Inner,
}

enum Inner {
    Sha256(Sha256),
    Md5(Md5),
    Crc32c(u32),
}

impl Hasher {
    /// Create a streaming hasher for `algorithm`.
    pub fn new(algorithm: Algorithm) -> Self {
        let inner = match algorithm {
            Algorithm::Sha256 => Inner::Sha256(Sha256::new()),
            Algorithm::Md5 => Inner::Md5(Md5::new()),
            Algorithm::Crc32c => Inner::Crc32c(0),
        };
        Self { inner }
    }

    /// Absorb a chunk of bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        match &mut self.inner {
            Inner::Sha256(h) => h.update(bytes),
            Inner::Md5(h) => h.update(bytes),
            Inner::Crc32c(crc) => *crc = crc32c::crc32c_append(*crc, bytes),
        }
    }

    /// Finish hashing and return the lowercase-hex digest.
    pub fn finalize_hex(self) -> String {
        match self.inner {
            Inner::Sha256(h) => hex(h.finalize()),
            Inner::Md5(h) => hex(h.finalize()),
            Inner::Crc32c(crc) => format!("{crc:08x}"),
        }
    }
}

/// One-shot lowercase-hex digest of `bytes` under `algorithm`.
pub fn digest_hex(algorithm: Algorithm, bytes: &[u8]) -> String {
    match algorithm {
        Algorithm::Sha256 => hex(Sha256::digest(bytes)),
        Algorithm::Md5 => hex(Md5::digest(bytes)),
        Algorithm::Crc32c => format!("{:08x}", crc32c::crc32c(bytes)),
    }
}

/// One-shot SHA-256 hex digest.
pub fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(Algorithm::Sha256, bytes)
}

/// One-shot MD5 hex digest.
pub fn md5_hex(bytes: &[u8]) -> String {
    digest_hex(Algorithm::Md5, bytes)
}

/// One-shot CRC32C hex digest.
pub fn crc32c_hex(bytes: &[u8]) -> String {
    digest_hex(Algorithm::Crc32c, bytes)
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    use core::fmt::Write as _;
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // Writing a byte to a String is infallible.
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_sha256_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn known_md5_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn known_crc32c_vectors() {
        // CRC32C of the empty input is 0.
        assert_eq!(crc32c_hex(b""), "00000000");
        // CRC32C("123456789") == 0xE3069283 (Castagnoli check value).
        assert_eq!(crc32c_hex(b"123456789"), "e3069283");
    }

    #[test]
    fn streaming_matches_one_shot() {
        for alg in [Algorithm::Sha256, Algorithm::Md5, Algorithm::Crc32c] {
            let mut hasher = Hasher::new(alg);
            hasher.update(b"hello ");
            hasher.update(b"world");
            assert_eq!(hasher.finalize_hex(), digest_hex(alg, b"hello world"));
        }
    }
}
