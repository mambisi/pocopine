//! Centralized cryptographic hash and checksum primitives for the pocopine
//! workspace.
//!
//! The point of this crate is that the rest of the workspace never reaches for
//! `blake3`, `sha2`, `md-5`, or `crc32c` directly and never re-implements hex
//! encoding or incremental hashing. Everything goes through one small,
//! algorithm-agnostic API:
//!
//! ```
//! use pocopine_crypto::{Algorithm, Hasher, blake3_hex, digest_hex, sha256_hex};
//!
//! // One-shot.
//! assert_eq!(sha256_hex(b""),
//!     "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
//! assert_eq!(blake3_hex(b""),
//!     "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262");
//! assert_eq!(digest_hex(Algorithm::Crc32c, b""), "00000000");
//!
//! // Streaming (for data that never fits in one buffer).
//! let mut hasher = Hasher::new(Algorithm::Sha256);
//! hasher.update(b"hello ");
//! hasher.update(b"world");
//! let _hex = hasher.finalize_hex();
//! ```
//!
//! Raw digests are available too — [`sha256`] for a one-shot `[u8; 32]`,
//! [`Hasher::finalize_bytes`] for the streaming case — alongside keyed
//! [`hmac_sha256`] for request-signing paths (e.g. Azure Shared-Key / SAS).
//!
//! The underlying crates are re-exported (`pocopine_crypto::{blake3, sha2,
//! md5, crc32c, hmac}`) for the rare case a caller needs a primitive this API
//! does not wrap yet — but prefer adding to this API over reaching for them.
//!
//! This crate is `no_std` (it only needs `alloc` for the returned hex
//! `String` / `Vec`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub use blake3;
pub use crc32c;
pub use hmac;
pub use md5;
pub use sha2;

mod secret;
pub use secret::SecretString;

use hmac::{Hmac, Mac};
use md5::Md5;
use sha2::{Digest, Sha256};

/// A digest/checksum algorithm supported across the workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Algorithm {
    /// BLAKE3 — 32-byte cryptographic digest (64 hex chars).
    Blake3,
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
            Algorithm::Blake3 => "blake3",
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
    // `blake3::Hasher` carries a sizeable CV stack. Keep the public
    // algorithm-agnostic hasher small instead of inflating every SHA/MD5/CRC
    // instance to the BLAKE3 variant's size.
    Blake3(Box<blake3::Hasher>),
    Sha256(Sha256),
    Md5(Md5),
    Crc32c(u32),
}

impl Hasher {
    /// Create a streaming hasher for `algorithm`.
    pub fn new(algorithm: Algorithm) -> Self {
        let inner = match algorithm {
            Algorithm::Blake3 => Inner::Blake3(Box::default()),
            Algorithm::Sha256 => Inner::Sha256(Sha256::new()),
            Algorithm::Md5 => Inner::Md5(Md5::new()),
            Algorithm::Crc32c => Inner::Crc32c(0),
        };
        Self { inner }
    }

    /// Absorb a chunk of bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        match &mut self.inner {
            Inner::Blake3(h) => {
                h.update(bytes);
            }
            Inner::Sha256(h) => h.update(bytes),
            Inner::Md5(h) => h.update(bytes),
            Inner::Crc32c(crc) => *crc = crc32c::crc32c_append(*crc, bytes),
        }
    }

    /// Finish hashing and return the lowercase-hex digest.
    pub fn finalize_hex(self) -> String {
        match self.inner {
            Inner::Blake3(h) => hex(h.finalize().as_bytes()),
            Inner::Sha256(h) => hex(h.finalize()),
            Inner::Md5(h) => hex(h.finalize()),
            Inner::Crc32c(crc) => format!("{crc:08x}"),
        }
    }

    /// Finish hashing and return the raw digest bytes.
    ///
    /// Length is algorithm-dependent: 32 bytes for BLAKE3 and SHA-256, 16 for
    /// MD5, 4 (big-endian) for CRC32C — the same bytes [`finalize_hex`]
    /// renders. Use this when a downstream API wants the raw digest (e.g. a
    /// `[u8; 32]` token-hash key) instead of a hex string.
    ///
    /// [`finalize_hex`]: Hasher::finalize_hex
    pub fn finalize_bytes(self) -> Vec<u8> {
        match self.inner {
            Inner::Blake3(h) => h.finalize().as_bytes().to_vec(),
            Inner::Sha256(h) => h.finalize().to_vec(),
            Inner::Md5(h) => h.finalize().to_vec(),
            Inner::Crc32c(crc) => crc.to_be_bytes().to_vec(),
        }
    }
}

/// One-shot lowercase-hex digest of `bytes` under `algorithm`.
pub fn digest_hex(algorithm: Algorithm, bytes: &[u8]) -> String {
    match algorithm {
        Algorithm::Blake3 => hex(blake3::hash(bytes).as_bytes()),
        Algorithm::Sha256 => hex(Sha256::digest(bytes)),
        Algorithm::Md5 => hex(Md5::digest(bytes)),
        Algorithm::Crc32c => format!("{:08x}", crc32c::crc32c(bytes)),
    }
}

/// One-shot BLAKE3 hex digest.
pub fn blake3_hex(bytes: &[u8]) -> String {
    digest_hex(Algorithm::Blake3, bytes)
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

/// One-shot raw SHA-256 digest (`[u8; 32]`).
///
/// The raw-bytes counterpart to [`sha256_hex`]. Use this where a downstream
/// API keys on the raw digest — e.g. a `token_hash: [u8; 32]` store.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(bytes));
    out
}

/// Keyed HMAC-SHA256 of `msg` under `key`, as a raw 32-byte MAC.
///
/// HMAC accepts a key of any length, so this never fails. Base64- or
/// hex-encode the result for the wire (e.g. an Azure Shared-Key
/// `Authorization` header or a SAS `sig` query value).
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(msg);
    let mut out = [0u8; 32];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

/// Streaming FNV-1a 64-bit hash — **non-cryptographic**.
///
/// For in-memory equality fingerprints (dirty-checking reactive
/// state, cache keys, dedup) where speed on small inputs matters
/// and an adversary is not in the threat model. Never use this
/// for signing, content addressing, or anything security-bearing
/// — that's [`sha256`] / [`hmac_sha256`] territory.
///
/// Implements [`core::hash::Hasher`], so it plugs into anything
/// that feeds a standard hasher:
///
/// ```
/// use core::hash::Hasher as _;
/// let mut h = pocopine_crypto::Fnv64::new();
/// h.write(b"hello");
/// assert_eq!(h.finish(), pocopine_crypto::fnv64(b"hello"));
/// ```
///
/// Deterministic across processes and platforms (fixed offset
/// basis / prime, no per-instance keying) — equal byte streams
/// always produce equal fingerprints.
#[derive(Clone, Copy, Debug)]
pub struct Fnv64(u64);

const FNV64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

impl Fnv64 {
    pub fn new() -> Self {
        Fnv64(FNV64_OFFSET_BASIS)
    }
}

impl Default for Fnv64 {
    fn default() -> Self {
        Self::new()
    }
}

impl core::hash::Hasher for Fnv64 {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut state = self.0;
        for &b in bytes {
            state ^= u64::from(b);
            state = state.wrapping_mul(FNV64_PRIME);
        }
        self.0 = state;
    }
}

/// One-shot [`Fnv64`] over `bytes`. Non-cryptographic — see the
/// type docs for the intended (and forbidden) uses.
pub fn fnv64(bytes: &[u8]) -> u64 {
    use core::hash::Hasher as _;
    let mut h = Fnv64::new();
    h.write(bytes);
    h.finish()
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
    fn known_blake3_vectors() {
        assert_eq!(
            blake3_hex(b""),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
        assert_eq!(
            blake3_hex(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

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
        for alg in [
            Algorithm::Blake3,
            Algorithm::Sha256,
            Algorithm::Md5,
            Algorithm::Crc32c,
        ] {
            let mut hasher = Hasher::new(alg);
            hasher.update(b"hello ");
            hasher.update(b"world");
            assert_eq!(hasher.finalize_hex(), digest_hex(alg, b"hello world"));
        }
    }

    #[test]
    fn raw_sha256_matches_hex() {
        assert_eq!(sha256(b"").len(), 32);
        assert_eq!(hex(sha256(b"")), sha256_hex(b""));
        assert_eq!(hex(sha256(b"abc")), sha256_hex(b"abc"));
    }

    #[test]
    fn finalize_bytes_matches_finalize_hex() {
        for alg in [
            Algorithm::Blake3,
            Algorithm::Sha256,
            Algorithm::Md5,
            Algorithm::Crc32c,
        ] {
            let mut hasher = Hasher::new(alg);
            hasher.update(b"hello world");
            assert_eq!(
                hex(hasher.finalize_bytes()),
                digest_hex(alg, b"hello world")
            );
        }
        // CRC32C raw digest is the 4 big-endian bytes of the checksum.
        let mut hasher = Hasher::new(Algorithm::Crc32c);
        hasher.update(b"123456789");
        assert_eq!(hasher.finalize_bytes(), [0xe3, 0x06, 0x92, 0x83]);
    }

    #[test]
    fn hmac_sha256_rfc4231_test_case_2() {
        // RFC 4231 §4.3: key "Jefe", data "what do ya want for nothing?".
        assert_eq!(
            hex(hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn fnv64_known_vectors() {
        // Canonical FNV-1a 64 vectors (Landon Curt Noll's reference set).
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn fnv64_streaming_matches_one_shot() {
        use core::hash::Hasher as _;
        let mut h = Fnv64::new();
        h.write(b"hello ");
        h.write(b"world");
        assert_eq!(h.finish(), fnv64(b"hello world"));
    }
}
