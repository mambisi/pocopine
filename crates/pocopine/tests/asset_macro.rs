//! RFC-100 — end-to-end check of the `asset!` macro.
//!
//! `asset!("test.txt")` resolves against this test crate's manifest
//! dir (`crates/pocopine/assets/test.txt`), hashes the fixture at
//! expansion time, and expands to a call through
//! `pocopine::__private::asset_url`. The hash parity assertion is
//! load-bearing: the macro duplicates the 8-hex sha256-prefix shape
//! of `pocopine_core::assets::asset_hash` (a proc-macro crate can't
//! link the wasm runtime crate), and this test is what keeps the two
//! in lockstep.
//!
//! `asset_url` on the host reads `POCOPINE_ASSET_BASE` at runtime;
//! integration-test binaries run in their own process, so mutating
//! the var here can't race other test crates — but the two tests in
//! THIS binary run on parallel threads, hence the `ENV_LOCK`.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Mutex;

use pocopine::asset;
use pocopine_core::assets::asset_hash;

const FIXTURE: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/test.txt"));

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn asset_macro_emits_content_addressed_url() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env access is serialized by ENV_LOCK.
    unsafe { std::env::remove_var("POCOPINE_ASSET_BASE") };
    let url = asset!("test.txt");

    let expected_hash = asset_hash(FIXTURE);
    assert_eq!(url, format!("/assets/{expected_hash}/test.txt"));

    let hash_segment = url
        .strip_prefix("/assets/")
        .and_then(|rest| rest.split('/').next())
        .expect("URL has the /assets/<hash>/<path> shape");
    assert_eq!(hash_segment.len(), 8);
    assert!(hash_segment
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
}

#[test]
fn asset_macro_url_respects_runtime_base() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: env access is serialized by ENV_LOCK.
    unsafe { std::env::set_var("POCOPINE_ASSET_BASE", "https://cdn.example.com/") };
    let url = asset!("test.txt");
    // SAFETY: env access is serialized by ENV_LOCK.
    unsafe { std::env::remove_var("POCOPINE_ASSET_BASE") };

    let expected_hash = asset_hash(FIXTURE);
    assert_eq!(
        url,
        format!("https://cdn.example.com/{expected_hash}/test.txt")
    );
}
