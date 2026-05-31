//! Shared encoding / codec utilities for the pocopine workspace.
//!
//! The point of this crate is that crates never re-implement encoding helpers or
//! their serde adapters. Today it covers base64; add new codecs here rather than
//! inlining them per crate.
//!
//! ```
//! use pocopine_codec::{base64_decode, base64_encode};
//!
//! assert_eq!(base64_encode(b"hi"), "aGk=");
//! assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
//! ```
//!
//! For a `Vec<u8>` struct field that should serialize as a base64 string, use the
//! [`base64_bytes`] serde adapter:
//!
//! ```
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Serialize, Deserialize)]
//! struct Chunk {
//!     #[serde(with = "pocopine_codec::base64_bytes", default, skip_serializing_if = "Vec::is_empty")]
//!     payload: Vec<u8>,
//! }
//! ```
//!
//! This crate is `no_std` (it only needs `alloc`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub use base64;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

/// Encode bytes as a standard (padded) base64 string.
pub fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decode a standard (padded) base64 string.
pub fn base64_decode(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(encoded)
}

/// Serde adapter for a `Vec<u8>` field encoded as a base64 string.
///
/// Use via `#[serde(with = "pocopine_codec::base64_bytes")]`. Pair with
/// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` to omit empty values.
pub mod base64_bytes {
    use alloc::string::String;
    use alloc::vec::Vec;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&super::base64_encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        super::base64_decode(&encoded).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for input in [b"".as_slice(), b"a", b"hello world", &[0u8, 255, 128, 1]] {
            let encoded = base64_encode(input);
            assert_eq!(base64_decode(&encoded).unwrap(), input);
        }
    }

    #[test]
    fn known_base64_vectors() {
        assert_eq!(base64_encode(b"hi"), "aGk=");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
    }

    #[test]
    fn invalid_base64_is_rejected() {
        assert!(base64_decode("not valid base64!!!").is_err());
    }

    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Chunk {
        #[serde(with = "base64_bytes", default, skip_serializing_if = "Vec::is_empty")]
        payload: Vec<u8>,
    }

    #[test]
    fn serde_adapter_round_trips_and_omits_empty() {
        let chunk = Chunk {
            payload: alloc::vec![1, 2, 3, 4],
        };
        let json = serde_json::to_string(&chunk).unwrap();
        assert!(json.contains("AQIDBA=="));
        assert_eq!(serde_json::from_str::<Chunk>(&json).unwrap(), chunk);

        let empty = Chunk {
            payload: Vec::new(),
        };
        let json = serde_json::to_string(&empty).unwrap();
        assert_eq!(json, "{}");
        assert_eq!(serde_json::from_str::<Chunk>("{}").unwrap(), empty);
    }
}
