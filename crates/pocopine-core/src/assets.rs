//! RFC-100 — content-addressed asset URLs.
//!
//! Runtime half of the asset pipeline: the `asset!` macro
//! (pocopine-macros) hashes the file at expansion time and expands
//! to a call through [`asset_url`], which prepends the asset base
//! resolved at **runtime**:
//!
//! ```text
//! asset!("blog/video.webm")
//!    │ expansion (compile time): hash crates/<caller>/assets/blog/video.webm
//!    ▼
//! asset_url("blog/video.webm", "a3f81c2d")
//!    │ runtime: base = window.__POCOPINE_ASSET_BASE (wasm)
//!    │               | $POCOPINE_ASSET_BASE          (server)
//!    │               | "/assets"                     (default)
//!    ▼
//! "<base>/a3f81c2d/blog/video.webm"
//! ```
//!
//! Keeping base resolution at runtime means the same compiled
//! artefact serves dev (`/assets` on the dev server), SSR, and a
//! CDN-fronted deploy — only the base changes.

use pocopine_crypto::sha256_hex;

/// Fallback base when no override is configured.
const DEFAULT_ASSET_BASE: &str = "/assets";

/// RFC-100 — content hash of an asset: the first 8
/// lowercase hex chars of its sha256 digest (via `pocopine-crypto`).
///
/// This is the single definition of the asset-hash shape; the
/// `asset!` macro and the dev server reproduce it (8-char prefix of
/// `pocopine_crypto::sha256_hex`) and parity is asserted end-to-end
/// in `crates/pocopine/tests/asset_macro.rs`.
pub fn asset_hash(bytes: &[u8]) -> String {
    let mut hex = sha256_hex(bytes);
    hex.truncate(8);
    hex
}

/// RFC-100 — build the URL for a content-addressed asset:
/// `<base>/<hash>/<path>`.
///
/// The base is resolved at **runtime** on every call:
///
/// * wasm32 — `window.__POCOPINE_ASSET_BASE` when it is a non-empty
///   string, else [`DEFAULT_ASSET_BASE`];
/// * native — the `POCOPINE_ASSET_BASE` env var when non-empty,
///   else [`DEFAULT_ASSET_BASE`].
///
/// Trailing slashes on the configured base are trimmed before
/// joining, so `https://cdn.example.com/` and
/// `https://cdn.example.com` produce the same URL.
///
/// Called by `asset!`-expanded code (via `pocopine::__private`);
/// both arguments are macro-emitted string literals, hence
/// `&'static str`.
pub fn asset_url(path: &'static str, hash: &'static str) -> String {
    let base = asset_base();
    let base = base.trim_end_matches('/');
    format!("{base}/{hash}/{path}")
}

#[cfg(target_arch = "wasm32")]
fn asset_base() -> String {
    let configured = web_sys::window()
        .and_then(|window| {
            js_sys::Reflect::get(
                window.as_ref(),
                &wasm_bindgen::JsValue::from_str("__POCOPINE_ASSET_BASE"),
            )
            .ok()
        })
        .and_then(|value| value.as_string())
        .filter(|base| !base.is_empty());
    configured.unwrap_or_else(|| DEFAULT_ASSET_BASE.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn asset_base() -> String {
    std::env::var("POCOPINE_ASSET_BASE")
        .ok()
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_ASSET_BASE.to_string())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// `POCOPINE_ASSET_BASE` is process-global; tests mutating it
    /// must not interleave.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_asset_base<R>(base: Option<&str>, f: impl FnOnce() -> R) -> R {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: env access is serialized by ENV_LOCK, held for this fn.
        match base {
            Some(base) => unsafe { std::env::set_var("POCOPINE_ASSET_BASE", base) },
            None => unsafe { std::env::remove_var("POCOPINE_ASSET_BASE") },
        }
        let result = f();
        // SAFETY: serialized by ENV_LOCK (see above).
        unsafe { std::env::remove_var("POCOPINE_ASSET_BASE") };
        result
    }

    #[test]
    fn asset_hash_is_8_hex_prefix_of_sha256() {
        // sha256("") = e3b0c442…; sha256("hello world") = b94d27b9….
        assert_eq!(asset_hash(b""), "e3b0c442");
        assert_eq!(asset_hash(b"hello world"), "b94d27b9");
    }

    #[test]
    fn asset_url_defaults_to_slash_assets() {
        let url = with_asset_base(None, || asset_url("blog/video.webm", "a3f81c2d"));
        assert_eq!(url, "/assets/a3f81c2d/blog/video.webm");
    }

    #[test]
    fn asset_url_uses_env_base_without_trailing_slash() {
        let url = with_asset_base(Some("https://cdn.example.com"), || {
            asset_url("blog/video.webm", "a3f81c2d")
        });
        assert_eq!(url, "https://cdn.example.com/a3f81c2d/blog/video.webm");
    }

    #[test]
    fn asset_url_trims_trailing_slash_from_env_base() {
        let url = with_asset_base(Some("https://cdn.example.com/"), || {
            asset_url("blog/video.webm", "a3f81c2d")
        });
        assert_eq!(url, "https://cdn.example.com/a3f81c2d/blog/video.webm");
    }

    #[test]
    fn asset_url_treats_empty_env_base_as_unset() {
        let url = with_asset_base(Some(""), || asset_url("logo.svg", "deadbeef"));
        assert_eq!(url, "/assets/deadbeef/logo.svg");
    }
}
