//! Shared encoding / codec utilities for the pocopine workspace.
//!
//! The point of this crate is that crates never re-implement encoding helpers or
//! their serde adapters and never reach for `base64` or `percent-encoding`
//! directly. Add new codecs here rather than inlining them per crate.
//!
//! ```
//! use pocopine_codec::{base64_decode, base64_encode};
//!
//! assert_eq!(base64_encode(b"hi"), "aGk=");
//! assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
//! ```
//!
//! Percent-encoding goes through one component encoder (RFC 3986
//! "unreserved" set — what URL path segments, query parts, and fragments
//! want) plus a lossy decoder. Pass a custom [`AsciiSet`] to
//! [`percent_encode_set`] when a backend needs a different escape set.
//!
//! ```
//! use pocopine_codec::{percent_decode, percent_encode};
//!
//! assert_eq!(percent_encode("a b/c~d"), "a%20b%2Fc~d");
//! assert_eq!(percent_decode("a%20b+c", true), "a b c");
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

use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use base64;
pub use percent_encoding;
// Re-exported so callers can express a custom percent-encode set
// (e.g. a backend-specific path-segment set) without depending on
// `percent-encoding` directly. Pair with [`percent_encode_set`].
pub use percent_encoding::{AsciiSet, CONTROLS, NON_ALPHANUMERIC};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use percent_encoding::{percent_decode_str, utf8_percent_encode};

/// Encode bytes as a standard (padded) base64 string.
pub fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

/// Decode a standard (padded) base64 string.
pub fn base64_decode(encoded: &str) -> Result<Vec<u8>, base64::DecodeError> {
    STANDARD.decode(encoded)
}

/// RFC 3986 "component" encode set: everything **except** the unreserved
/// set `A-Z` / `a-z` / `0-9` / `-` / `.` / `_` / `~` is percent-encoded
/// (uppercase hex). Non-ASCII bytes are always encoded. This is the
/// conservative encoder URL path segments, query keys/values, and
/// fragments want — equivalent to JS `encodeURIComponent` minus its
/// `!*'()` carve-outs.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Percent-encode `s` with the RFC 3986 component set (see [`COMPONENT`]).
///
/// Use this for a single URL path segment, query key/value, or fragment.
pub fn percent_encode(s: &str) -> String {
    utf8_percent_encode(s, COMPONENT).to_string()
}

/// Percent-encode `s` with the component set, appending into `out`.
///
/// Allocation-free variant of [`percent_encode`] for callers building a
/// URL piece by piece.
pub fn percent_encode_into(out: &mut String, s: &str) {
    out.extend(utf8_percent_encode(s, COMPONENT));
}

/// Percent-encode `s` with a caller-supplied [`AsciiSet`].
///
/// For the common case prefer [`percent_encode`]. Reach for this only when
/// a backend needs a different escape set — build one from the re-exported
/// [`CONTROLS`] / [`NON_ALPHANUMERIC`] + [`AsciiSet::add`]/[`AsciiSet::remove`].
pub fn percent_encode_set(s: &str, set: &'static AsciiSet) -> String {
    utf8_percent_encode(s, set).to_string()
}

/// Percent-decode `s`, lossily (invalid UTF-8 becomes U+FFFD, and a `%`
/// not followed by two hex digits is passed through verbatim).
///
/// When `plus_as_space` is set, `+` is first replaced with a space — the
/// `application/x-www-form-urlencoded` query semantics. Leave it `false`
/// for path segments, where `+` is a literal plus.
pub fn percent_decode(s: &str, plus_as_space: bool) -> String {
    if plus_as_space && s.contains('+') {
        let replaced = s.replace('+', " ");
        percent_decode_str(&replaced)
            .decode_utf8_lossy()
            .into_owned()
    } else {
        percent_decode_str(s).decode_utf8_lossy().into_owned()
    }
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
    fn percent_encode_keeps_unreserved_and_escapes_the_rest() {
        // Unreserved set passes through untouched.
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        // Reserved / delimiter bytes become uppercase %XX.
        assert_eq!(percent_encode("a b/c?d#e"), "a%20b%2Fc%3Fd%23e");
        // Non-ASCII is encoded byte-by-byte (UTF-8).
        assert_eq!(percent_encode("é"), "%C3%A9");
    }

    #[test]
    fn percent_encode_into_appends() {
        let mut out = String::from("p/");
        percent_encode_into(&mut out, "a b");
        assert_eq!(out, "p/a%20b");
    }

    #[test]
    fn percent_encode_set_honors_custom_set() {
        // A set that only escapes spaces leaves `/` and `~` alone.
        const SPACE_ONLY: &AsciiSet = &CONTROLS.add(b' ');
        assert_eq!(percent_encode_set("a b/c~d", SPACE_ONLY), "a%20b/c~d");
        // NON_ALPHANUMERIC escapes the unreserved punctuation too.
        assert_eq!(percent_encode_set("a-b.c", NON_ALPHANUMERIC), "a%2Db%2Ec");
    }

    #[test]
    fn percent_decode_round_trips_component() {
        for input in ["", "plain", "a b/c?d#e", "é", "100%done"] {
            assert_eq!(percent_decode(&percent_encode(input), false), input);
        }
    }

    #[test]
    fn percent_decode_plus_semantics() {
        // Query semantics: `+` is a space.
        assert_eq!(percent_decode("a+b%20c", true), "a b c");
        // Path semantics: `+` is a literal plus.
        assert_eq!(percent_decode("a+b%20c", false), "a+b c");
    }

    #[test]
    fn percent_decode_is_lossy_and_passes_through_stray_percent() {
        // Invalid UTF-8 becomes the replacement character.
        assert_eq!(percent_decode("%FF", false), "\u{FFFD}");
        // A `%` not followed by two hex digits is left verbatim.
        assert_eq!(percent_decode("100%zz", false), "100%zz");
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
