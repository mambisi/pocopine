//! RFC-112 — compile contract for `#[derive(PathAccess)]` + the
//! `#[component]`/`#[store]` nested-path dispatch. Behavior is
//! covered by `pocopine-core`'s `nested_signals` wasm suite; this
//! pins that the full macro stack (derive + autoref dispatch +
//! RFC-111 validation) compiles together.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn path_access_compiles() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pa_derive_pass.rs");
}
