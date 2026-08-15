//! RFC-116 — compile-behaviour contract for the inline `poco!` form.
//!
//! The point of these cases is that the inline form is not a shortcut around
//! the compile-time ladder: a `poco!` body reaches the same parser, the same
//! single-root rule, and the same RFC-111 path assertions that a `.poco` file
//! does. A pass-only suite could not tell the difference between "validated"
//! and "not validated at all", so the reject cases carry the contract.
//!
//! Regenerate snapshots with `TRYBUILD=overwrite` when rustc's framing drifts.
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn poco_inline_contract() {
    let cases = trybuild::TestCases::new();
    // Bare HTML, sugar, quoted prose, fragments, const position.
    cases.pass("tests/ui/poco_pass.rs");
    // Template-path validation reaches the inline body.
    cases.compile_fail("tests/ui/poco_unknown_field.rs");
    // A component template still needs exactly one root.
    cases.compile_fail("tests/ui/poco_multi_root.rs");
    // An empty body is a hard error, not an empty template.
    cases.compile_fail("tests/ui/poco_empty.rs");
    // `template` is one key: a path and a `poco!` body collide.
    cases.compile_fail("tests/ui/poco_dup_template.rs");
    // The removed `template_inline` key points at its replacement.
    cases.compile_fail("tests/ui/poco_template_inline_removed.rs");
}
