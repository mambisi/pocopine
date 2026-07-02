//! Compile-time template-path validation (RFC-111) — the contract:
//! every expression root a template can evaluate must resolve to a
//! struct field, an explicit flatten leaf, a `#[computed]` field, or
//! (for `pp-on` targets) a `#[handlers]` method; `$`-magics and
//! locally-bound `pp-for`/`pp-let` names are exempt, and
//! `unchecked_paths = "true"` opts a component out.
//!
//! The reject snapshots pin rustc's missing-associated-item framing
//! (E0599 naming the `__poc_bindable_*` / `__poc_handler_*` marker) —
//! that framing can drift between the CI and workspace nightlies;
//! regenerate with `TRYBUILD=overwrite` if it does.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn template_path_contract() {
    let cases = trybuild::TestCases::new();
    // Fields, computed, handlers, magics, loop/let locals resolve.
    cases.pass("tests/ui/tp_pass.rs");
    // The escape hatch skips validation entirely.
    cases.pass("tests/ui/tp_unchecked_escape.rs");
    // A typo'd field root is a compile error.
    cases.compile_fail("tests/ui/tp_unknown_field.rs");
    // A typo'd handler name is a compile error.
    cases.compile_fail("tests/ui/tp_unknown_handler.rs");
}
