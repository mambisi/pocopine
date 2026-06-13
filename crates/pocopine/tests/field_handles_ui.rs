//! RFC-097 §6 — compile-fail driver for the two `#[component]`
//! rejections that make field handles + the `&self`-skip sound:
//! reserved field names (would shadow a `Handle` inherent method) and
//! interior-mutability field types (would let `&self` mutate state).
//!
//! Following the `#[query]` ui-test convention, the contract under test
//! is "these inputs MUST NOT compile"; the committed `.stderr` snapshots
//! pin our own `compile_error!` text (regenerate with
//! `TRYBUILD=overwrite` if rustc framing drifts).

#[test]
fn field_handle_rejected_inputs_fail_to_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/fh_field_named_update.rs");
    cases.compile_fail("tests/ui/fh_cell_field.rs");
}
