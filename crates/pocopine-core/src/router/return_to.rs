//! Validated app-local redirect intent.
//!
//! `ReturnTo` captures the user's current location (path + query
//! only — no fragment, no host, no scheme) so a route-rejection
//! handler can build a "after sign-in, send me back here" link.
//! Per RFC-078 §5.10.2 the value is **path-only** and must pass a
//! strict validator before any plugin gets to use it; smuggled
//! protocol-relative URLs, double-encoded redirects, control
//! characters, and similar tricks are silently dropped (the
//! handler builds its redirect without the param).
//!
//! Validation rules — anything not passing every rule yields
//! [`ReturnTo::none`]:
//!
//! 1. Non-empty, length ≤ 2048 bytes.
//! 2. Matches `^/$` or `^/[^/].*$` — a single `/` (root) or `/`
//!    followed by a non-`/` character. Rejects `//evil.com`
//!    (protocol-relative) and anything that doesn't start with `/`
//!    (`https://evil.com`, `javascript:foo`, `data:foo`,
//!    `mailto:foo`).
//! 3. Contains only printable ASCII (`< 0x20`, `0x7f`, and high
//!    bytes rejected — high bytes mean a downstream decoder may
//!    read the value differently).
//! 4. Doesn't contain `\` (Windows-path smuggling vector).
//! 5. Survives **two** rounds of percent-decoding without violating
//!    rules 1–4 (defends against `%2F%2Fevil` and
//!    `%252F%252Fevil` smuggling).

use crate::app::RouteTarget;

const MAX_LEN: usize = 2048;

/// Validated app-local redirect intent. See module docs for the
/// validation contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReturnTo {
    intent: Option<String>,
}

impl ReturnTo {
    /// Empty / rejected intent. Plugins that build a redirect with
    /// `append_to` from this value get the original target back —
    /// no return param is appended.
    pub const fn none() -> Self {
        Self { intent: None }
    }

    /// Validate an explicit path string and produce a `ReturnTo`.
    /// Returns [`ReturnTo::none`] if validation fails — the API
    /// is intentionally infallible so plugin code can pipe
    /// suspect input through without writing error handling.
    pub fn from_path(path: impl AsRef<str>) -> Self {
        let path = path.as_ref();
        if !validate(path) {
            return Self::none();
        }
        Self {
            intent: Some(path.to_string()),
        }
    }

    /// Capture the current `window.location.pathname + search`.
    /// Returns [`ReturnTo::none`] if the platform is unavailable
    /// (non-wasm test) or the captured path fails validation.
    #[cfg(target_arch = "wasm32")]
    pub fn current() -> Self {
        let Some(window) = web_sys::window() else {
            return Self::none();
        };
        let location = window.location();
        let path = location.pathname().unwrap_or_default();
        let search = location.search().unwrap_or_default();
        let combined = format!("{path}{search}");
        Self::from_path(&combined)
    }

    /// Non-wasm fallback so tests and host-side code can compile
    /// without a special cfg. Always returns [`ReturnTo::none`] —
    /// host code should call [`Self::from_path`] with an explicit
    /// value if it wants a populated intent.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn current() -> Self {
        Self::none()
    }

    pub fn is_some(&self) -> bool {
        self.intent.is_some()
    }

    pub fn is_none(&self) -> bool {
        self.intent.is_none()
    }

    pub fn as_str(&self) -> Option<&str> {
        self.intent.as_deref()
    }

    pub fn into_inner(self) -> Option<String> {
        self.intent
    }

    /// Build a redirect target by appending `?<param>=<encoded>` (or
    /// `&<param>=<encoded>` if `target` already has a query) to
    /// `target`. If `self` is none, returns `target` unchanged.
    ///
    /// The returned target is re-validated as an app-local path; if
    /// the appended query string somehow produced an invalid result,
    /// the original `target` is returned unchanged so the redirect
    /// still works.
    pub fn append_to(&self, target: RouteTarget, param: &str) -> RouteTarget {
        let Some(intent) = self.intent.as_deref() else {
            return target;
        };
        let separator = if target.as_str().contains('?') {
            '&'
        } else {
            '?'
        };
        let encoded = encode_uri_component(intent);
        let combined = format!("{}{separator}{}={}", target.as_str(), param, encoded);
        RouteTarget::new(combined).unwrap_or(target)
    }
}

impl Default for ReturnTo {
    fn default() -> Self {
        Self::none()
    }
}

fn validate(s: &str) -> bool {
    if s.is_empty() || s.len() > MAX_LEN {
        return false;
    }
    if !passes_byte_checks(s) {
        return false;
    }
    let Some(once) = percent_decode_strict(s) else {
        return false;
    };
    if !passes_byte_checks(&once) {
        return false;
    }
    let Some(twice) = percent_decode_strict(&once) else {
        return false;
    };
    if !passes_byte_checks(&twice) {
        return false;
    }
    true
}

/// Layer-1 string checks reused by every percent-decode round.
/// Implements the RFC-078 §5.10.2 contract: `^/[^/].*$` (or the
/// single-character `/` root path), no backslashes, no
/// control characters, ASCII only. The "no `//` prefix" rule is
/// what blocks protocol-relative redirects (`//evil.com`); embedded
/// double slashes mid-path are same-origin paths and pass.
fn passes_byte_checks(s: &str) -> bool {
    let bytes = s.as_bytes();
    match bytes {
        // Pure root path is always allowed.
        b"/" => {}
        // Anything else: must be `/` followed by at least one
        // non-`/` character. Rejects `//evil.com`, `/\foo`, and
        // `\\foo` (which doesn't start with `/` at all).
        [b'/', second, ..] if *second != b'/' => {}
        _ => return false,
    }
    for &byte in bytes {
        // Reject control chars, DEL, and high bytes. The
        // path-and-query window of a URL is ASCII-only by spec; any
        // high byte means a downstream decoder may read the value
        // differently.
        if byte < 0x20 || byte == 0x7f || !byte.is_ascii() {
            return false;
        }
        // Backslash is never legal in URL paths and is a known
        // smuggling vector against Windows-path-aware code.
        if byte == b'\\' {
            return false;
        }
    }
    true
}

/// Strict percent-decoder. Returns `None` for any malformed `%`
/// sequence (non-hex, truncated). The output is validated as a
/// `String`; non-UTF-8 byte sequences (e.g. `%FF`) are rejected
/// because we require the input to remain ASCII at every layer.
fn percent_decode_strict(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_digit(bytes[i + 1])?;
            let lo = hex_digit(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `encodeURIComponent`-style encoder: encode every byte except
/// the unreserved set `[A-Za-z0-9_.~-]`. Used to embed the
/// validated path-with-query into a query parameter.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_upper(byte >> 4));
            out.push(hex_upper(byte & 0x0f));
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + nibble - 10) as char,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_path_accepts_app_local_paths() {
        assert_eq!(
            ReturnTo::from_path("/dashboard").as_str(),
            Some("/dashboard")
        );
        assert_eq!(
            ReturnTo::from_path("/users/42?tab=info").as_str(),
            Some("/users/42?tab=info")
        );
        assert_eq!(ReturnTo::from_path("/").as_str(), Some("/"));
    }

    #[test]
    fn from_path_rejects_protocol_relative_urls() {
        // The classic open-redirect attack: `Location: //evil.com`
        // is treated as protocol-relative by browsers, jumping to
        // a different origin.
        assert!(ReturnTo::from_path("//evil.com/admin").is_none());
        assert!(ReturnTo::from_path("//evil.com").is_none());
        assert!(ReturnTo::from_path("///foo").is_none());
    }

    #[test]
    fn from_path_rejects_explicit_external_urls() {
        // These don't start with `/`, so rule 1 rejects them. Covers
        // the council's required test set: `https://`, `javascript:`,
        // `data:`, `mailto:`.
        assert!(ReturnTo::from_path("https://evil.com").is_none());
        assert!(ReturnTo::from_path("javascript:alert(1)").is_none());
        assert!(ReturnTo::from_path("data:text/html,<script>").is_none());
        assert!(ReturnTo::from_path("mailto:victim@example.com").is_none());
    }

    #[test]
    fn from_path_allows_embedded_double_slash_mid_path() {
        // `/path//tail` is a same-origin path with an empty middle
        // segment. RFC §5.10.2 only blocks the `//` prefix; embedded
        // double slashes are downstream-router concerns, not
        // open-redirect concerns.
        assert!(ReturnTo::from_path("/path//tail").is_some());
    }

    #[test]
    fn from_path_allows_root_path() {
        // The single-char `/` is a legitimate post-redirect target.
        assert_eq!(ReturnTo::from_path("/").as_str(), Some("/"));
    }

    #[test]
    fn from_path_rejects_backslash() {
        assert!(ReturnTo::from_path("/foo\\bar").is_none());
        assert!(ReturnTo::from_path("\\foo").is_none());
    }

    #[test]
    fn from_path_rejects_control_characters() {
        assert!(ReturnTo::from_path("/foo\nbar").is_none());
        assert!(ReturnTo::from_path("/foo\rbar").is_none());
        assert!(ReturnTo::from_path("/foo\tbar").is_none());
        assert!(ReturnTo::from_path("/foo\x00bar").is_none());
        assert!(ReturnTo::from_path("/foo\x7fbar").is_none());
    }

    #[test]
    fn from_path_rejects_high_bytes() {
        // The fragment shell embeds non-ASCII via percent encoding;
        // raw high bytes mean a downstream decoder will read the
        // value differently than this validator did.
        let with_unicode = "/foo\u{00e9}bar";
        assert!(ReturnTo::from_path(with_unicode).is_none());
    }

    #[test]
    fn from_path_rejects_relative_paths() {
        assert!(ReturnTo::from_path("dashboard").is_none());
        assert!(ReturnTo::from_path("./foo").is_none());
        assert!(ReturnTo::from_path("../foo").is_none());
    }

    #[test]
    fn from_path_rejects_empty_and_oversized() {
        assert!(ReturnTo::from_path("").is_none());
        let long = format!("/{}", "a".repeat(MAX_LEN));
        assert!(ReturnTo::from_path(&long).is_none());
    }

    #[test]
    fn from_path_rejects_single_encoded_open_redirect() {
        // `/%2F%2Fevil.com` decodes to `///evil.com` — `//` prefix
        // after consuming the leading slash. Rule 1 rejects on the
        // post-decode pass.
        assert!(ReturnTo::from_path("/%2F%2Fevil.com").is_none());
        assert!(ReturnTo::from_path("/%2Fevil.com").is_none());
        // `%5C` decodes to `\` — explicit backslash byte.
        assert!(ReturnTo::from_path("/path%5Cattack").is_none());
        // Control chars hidden behind percent-encoding.
        assert!(ReturnTo::from_path("/path%00x").is_none());
        assert!(ReturnTo::from_path("/path%0A").is_none());
        // High byte after decoding (would be reinterpreted by a
        // downstream decoder).
        assert!(ReturnTo::from_path("/path%FF").is_none());
    }

    #[test]
    fn from_path_rejects_double_encoded_open_redirect() {
        // `/%252F%252Fevil.com` decodes once to `/%2F%2Fevil.com`,
        // twice to `///evil.com` — protocol-relative on the second
        // pass. The double-decode round catches this.
        assert!(ReturnTo::from_path("/%252F%252Fevil.com").is_none());
        // Paranoid alternative encodings of the same attack.
        assert!(ReturnTo::from_path("/%25%32%46%25%32%46evil.com").is_none());
    }

    #[test]
    fn from_path_rejects_malformed_percent_sequences() {
        assert!(ReturnTo::from_path("/path%").is_none());
        assert!(ReturnTo::from_path("/path%2").is_none());
        assert!(ReturnTo::from_path("/path%ZZ").is_none());
    }

    #[test]
    fn append_to_appends_query_param() {
        let intent = ReturnTo::from_path("/dashboard?id=42");
        let target = RouteTarget::path("/login");
        let with_intent = intent.append_to(target, "next");
        // `?id=42` becomes `%3Fid%3D42`, `/` stays as `%2F`.
        assert_eq!(with_intent.as_str(), "/login?next=%2Fdashboard%3Fid%3D42");
    }

    #[test]
    fn append_to_uses_ampersand_when_target_already_has_query() {
        let intent = ReturnTo::from_path("/dashboard");
        let target = RouteTarget::path("/login?reason=stale");
        let with_intent = intent.append_to(target, "next");
        assert_eq!(
            with_intent.as_str(),
            "/login?reason=stale&next=%2Fdashboard"
        );
    }

    #[test]
    fn append_to_passthrough_when_intent_is_none() {
        let target = RouteTarget::path("/login");
        let result = ReturnTo::none().append_to(target.clone(), "next");
        assert_eq!(result, target);
    }

    #[test]
    fn current_returns_none_off_wasm() {
        // Host build path — the API exists for cross-target
        // compilation; only wasm sees a populated value from
        // `window.location`.
        assert!(ReturnTo::current().is_none());
    }
}
