//! RFC-097 §6 — compile-fail driver for the `#[component]` rejection
//! that keeps the `&self`-skip sound: interior-mutability field types
//! (which would let a `&self` handler mutate state). Field names need
//! no rejection — accessors live on a dedicated `<Name>Fields` struct,
//! so they share no namespace with `Handle` and can't collide.
//!
//! Following the `#[query]` ui-test convention, the contract under test
//! is "this input MUST NOT compile"; the committed `.stderr` snapshot
//! pins our own `compile_error!` text (regenerate with
//! `TRYBUILD=overwrite` if rustc framing drifts).
//!
//! Host-only: trybuild shells out to cargo and isn't a wasm target.
#![cfg(not(target_arch = "wasm32"))]

#[test]
fn field_handle_rejected_inputs_fail_to_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/fh_cell_field.rs");
    // Nested interior mutability (Option<Cell<_>>) + a preceding
    // non-path field — guards the recursive walk and the no-early-return
    // fix.
    cases.compile_fail("tests/ui/fh_nested_cell_field.rs");
}
