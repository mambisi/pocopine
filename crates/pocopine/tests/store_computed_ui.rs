//! Issue #260 — compile-time contract for `#[computed]` on `#[store]`
//! singletons. Store computed reuses the shared `#[handlers]` computed
//! machinery, so the positive case is pinned on a real `#[store]` while
//! the rejections are pinned on the shared path they inherit.
//!
//! Following the `field_handles_ui` convention, the committed `.stderr`
//! snapshots pin our own `compile_error!` text (regenerate with
//! `TRYBUILD=overwrite` if rustc framing drifts). The reject fixtures use
//! a bare `#[handlers]` impl on purpose: it isolates the snapshot to our
//! `compile_error!`, which is stable across the CI (`nightly-2025-12-18`)
//! and workspace (`rust-toolchain.toml`) nightlies. A `#[store]` target
//! for the same rejection additionally cascades a rustc
//! `HandlerDispatch`-not-implemented error whose framing differs between
//! those two nightlies, which no single snapshot can satisfy.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn store_computed_contract() {
    let cases = trybuild::TestCases::new();
    // A `#[store]` may declare a raw-field computed and a computed that
    // depends on another computed.
    cases.pass("tests/ui/store_computed_pass.rs");
    // `#[computed]` must not read through `self` (shared contract).
    cases.compile_fail("tests/ui/store_computed_self.rs");
    // A computed cycle is rejected at compile time (shared contract).
    cases.compile_fail("tests/ui/store_computed_cycle.rs");
}
