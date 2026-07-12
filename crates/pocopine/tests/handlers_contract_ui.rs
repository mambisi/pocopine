//! Compile-time contract for `#[handlers]` marker attributes.
//!
//! A `#[watch]`, lifecycle, or `#[computed]` method whose shape the macro
//! cannot install must be a compile error, never a silent skip. Before
//! this contract, a `#[watch(field)]` handler that omitted the
//! `(next, prev)` args compiled green and was simply never installed —
//! the watch never fired and the code read as a dead handler.
//!
//! Following the `store_computed_ui` convention: reject fixtures pin our
//! own `compile_error!` text on bare `#[handlers]` impls so the snapshot
//! stays stable across the CI and workspace nightlies. Regenerate with
//! `TRYBUILD=overwrite` if rustc framing drifts.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn handlers_marker_contract() {
    let cases = trybuild::TestCases::new();
    // The canonical `(next, prev)` shape compiles and installs.
    cases.pass("tests/ui/watch_pass.rs");
    // A handler that omits both value args is rejected, not skipped.
    cases.compile_fail("tests/ui/watch_no_args.rs");
    // A handler that omits `prev` is rejected with the same shape hint.
    cases.compile_fail("tests/ui/watch_one_arg.rs");
    // Bare `#[watch]` (no field) is rejected.
    cases.compile_fail("tests/ui/watch_bare_attr.rs");
    // `#[watch("field")]` (string literal, not an ident) is rejected.
    cases.compile_fail("tests/ui/watch_nonident_attr.rs");
    // Two `#[watch]` attributes on one method are rejected (previously
    // last-one-won silently).
    cases.compile_fail("tests/ui/watch_stacked.rs");
    // A watch handler without a reference receiver is rejected.
    cases.compile_fail("tests/ui/watch_no_receiver.rs");
    // A lifecycle-named method without `self` is rejected (previously
    // skipped before name matching — the hook silently never fired).
    cases.compile_fail("tests/ui/lifecycle_no_receiver.rs");
    // `#[computed]` takes no arguments (previously discarded silently).
    cases.compile_fail("tests/ui/computed_with_args.rs");
    // RFC-115 — multi-field watch: two-plus fields take the
    // payload-less `&mut self` shape and compile.
    cases.pass("tests/ui/watch_multi_pass.rs");
    // Value args on a multi-field watch are rejected (no single
    // (next, prev) exists).
    cases.compile_fail("tests/ui/watch_multi_with_args.rs");
    // Duplicated fields in one list are rejected.
    cases.compile_fail("tests/ui/watch_list_duplicate.rs");
}
