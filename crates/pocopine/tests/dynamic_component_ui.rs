//! Compile-time surface for RFC-112: typed component selection, required
//! reactive `:is`, and framework-reserved sentinel names.

#![cfg(not(target_arch = "wasm32"))]

#[test]
fn dynamic_component_contract() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/dc_typed_ref_pass.rs");
    cases.pass("tests/ui/dc_typed_computed_pass.rs");
    cases.compile_fail("tests/ui/dc_missing_is.rs");
    cases.compile_fail("tests/ui/dc_string_ref_fail.rs");
    cases.compile_fail("tests/ui/dc_string_computed_fail.rs");
    cases.compile_fail("tests/ui/dc_literal_ref_fail.rs");
    cases.compile_fail("tests/ui/dc_unlisted_child_fail.rs");
    cases.compile_fail("tests/ui/dc_wrong_host_ref_fail.rs");
    cases.compile_fail("tests/ui/dc_reserved_tag.rs");
}
