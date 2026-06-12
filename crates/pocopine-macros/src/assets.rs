//! RFC-100 — expansion logic for the `asset!` macro.
//!
//! Compile-time half of the asset pipeline: validate the relative
//! path, resolve it under the **calling crate's** `assets/`
//! directory (`$CARGO_MANIFEST_DIR/assets/<path>`), hash the bytes,
//! and emit a call to the runtime URL builder with both literals
//! baked in:
//!
//! ```text
//! asset!("blog/video.webm")
//!     ▼
//! ::pocopine::__private::asset_url("blog/video.webm", "a3f81c2d")
//! ```
//!
//! # Rebuild correctness (RFC-100 §6)
//!
//! When `POCOPINE_ASSETS_FINGERPRINT` is set at expansion time (the
//! `pocopine build`/`run`/`dev` paths set it to a combined hash of
//! the app's `assets/` tree before every cargo invocation), the
//! expansion additionally emits
//!
//! ```ignore
//! const _: ::core::option::Option<&str> =
//!     ::core::option_env!("POCOPINE_ASSETS_FINGERPRINT");
//! ```
//!
//! inside the expression block. rustc records every `option_env!`
//! expansion as an env dependency in the crate's dep-info
//! (`# env-dep:` lines), and cargo re-fingerprints the crate when a
//! recorded env var changes value — so the next `pocopine build`
//! after an asset edit recompiles the calling crate and re-expands
//! `asset!`, refreshing the baked hash. The mechanism is proven
//! end-to-end (tracked vs untracked control) in
//! `crates/pocopine-cli/tests/fingerprint_tracking.rs`.
//!
//! The emission is deliberately **gated** on the env being set: bare
//! `cargo build` / rust-analyzer runs (no fingerprint env) emit no
//! tracking const, so alternating CLI and IDE builds don't rebuild
//! ping-pong on the set↔unset flip. The cost is that tracking only
//! arms once a crate has been compiled through the pocopine CLI; the
//! dev server's `409` stale-hash answer remains the backstop for
//! bare-cargo workflows.
//!
//! TODO(RFC-100): compile-fail coverage (missing file, absolute
//! path, `..` segment) — pocopine-macros has no trybuild harness
//! yet; add one when the diagnostics stabilize.

use std::path::{Component, Path, PathBuf};

use proc_macro::TokenStream;
use quote::quote;
use syn::LitStr;

/// RFC-100 — body of the `#[proc_macro] asset` entry
/// point in `lib.rs`.
pub(crate) fn expand(input: TokenStream) -> TokenStream {
    let lit = match syn::parse::<LitStr>(input) {
        Ok(lit) => lit,
        Err(err) => return err.to_compile_error().into(),
    };
    let path = lit.value();

    if let Err(message) = validate_asset_path(&path) {
        return syn::Error::new(lit.span(), message)
            .to_compile_error()
            .into();
    }

    let manifest_dir = match std::env::var("CARGO_MANIFEST_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            return syn::Error::new(
                lit.span(),
                "asset!: CARGO_MANIFEST_DIR is not set — cannot resolve \
                 the calling crate's `assets/` directory",
            )
            .to_compile_error()
            .into();
        }
    };
    let resolved = manifest_dir.join("assets").join(&path);
    let bytes = match std::fs::read(&resolved) {
        Ok(bytes) => bytes,
        Err(err) => {
            return syn::Error::new(
                lit.span(),
                format!(
                    "asset!(\"{path}\"): no readable file at {} ({err}) — \
                     asset paths resolve against the calling crate's \
                     `assets/` directory",
                    resolved.display()
                ),
            )
            .to_compile_error()
            .into();
        }
    };
    let hash = asset_hash_prefix(&bytes);

    // RFC-100 §6 — rebuild tracking. Only armed when the pocopine CLI
    // is driving the build (it sets the fingerprint env before every
    // cargo invocation); see the module docs for the dep-info
    // mechanism and why bare-cargo builds stay untracked.
    if std::env::var_os(FINGERPRINT_ENV).is_some() {
        quote! {{
            const _: ::core::option::Option<&str> = ::core::option_env!(#FINGERPRINT_ENV);
            ::pocopine::__private::asset_url(#path, #hash)
        }}
        .into()
    } else {
        quote! { ::pocopine::__private::asset_url(#path, #hash) }.into()
    }
}

/// Env var carrying the combined `assets/`-tree fingerprint. Set by
/// `pocopine build`/`run`/`dev`/`deploy` before invoking cargo; the
/// name must stay in lockstep with `pocopine-cli`'s
/// `assets_sync::FINGERPRINT_ENV`.
const FINGERPRINT_ENV: &str = "POCOPINE_ASSETS_FINGERPRINT";

/// 8-lowercase-hex prefix of the sha256 digest — the same shape as
/// `pocopine_core::assets::asset_hash`. Duplicated (4 lines) because
/// a proc-macro crate can't link the wasm runtime crate; both sides
/// go through `pocopine_crypto::sha256_hex`, and parity is asserted
/// end-to-end in `crates/pocopine/tests/asset_macro.rs`.
fn asset_hash_prefix(bytes: &[u8]) -> String {
    let mut hex = pocopine_crypto::sha256_hex(bytes);
    hex.truncate(8);
    hex
}

/// Reject paths that would escape the calling crate's `assets/`
/// directory: absolute paths, drive/root prefixes, and `..`
/// segments. `./` segments are tolerated (they resolve in place).
fn validate_asset_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("asset!: path must not be empty".to_string());
    }
    let as_path = Path::new(path);
    if as_path.is_absolute() || path.starts_with('/') || path.starts_with('\\') {
        return Err(format!(
            "asset!(\"{path}\"): absolute paths are not allowed — paths \
             resolve relative to the calling crate's `assets/` directory"
        ));
    }
    for component in as_path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(format!(
                    "asset!(\"{path}\"): `..` and root/prefix segments are \
                     not allowed — paths resolve relative to the calling \
                     crate's `assets/` directory"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_prefix_matches_core_asset_hash_shape() {
        // Known vectors mirrored in `pocopine_core::assets::tests` —
        // the two implementations must stay in lockstep.
        assert_eq!(asset_hash_prefix(b""), "e3b0c442");
        assert_eq!(asset_hash_prefix(b"hello world"), "b94d27b9");
    }

    #[test]
    fn relative_paths_validate() {
        assert!(validate_asset_path("logo.svg").is_ok());
        assert!(validate_asset_path("blog/video.webm").is_ok());
        assert!(validate_asset_path("./blog/video.webm").is_ok());
    }

    #[test]
    fn absolute_and_parent_paths_are_rejected() {
        assert!(validate_asset_path("").is_err());
        assert!(validate_asset_path("/etc/passwd").is_err());
        assert!(validate_asset_path("\\windows\\system32").is_err());
        assert!(validate_asset_path("../outside.txt").is_err());
        assert!(validate_asset_path("blog/../../outside.txt").is_err());
    }
}
