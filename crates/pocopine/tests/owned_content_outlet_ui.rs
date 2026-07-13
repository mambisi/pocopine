//! Compile-time diagnostics for the generic `pp-owned-content` contract.

#![cfg(not(target_arch = "wasm32"))]

#[test]
fn owned_content_outlet_contract() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/owned_content_component_tag.rs");
    cases.compile_fail("tests/ui/owned_content_structural.rs");
    cases.compile_fail("tests/ui/owned_content_duplicate.rs");
}
