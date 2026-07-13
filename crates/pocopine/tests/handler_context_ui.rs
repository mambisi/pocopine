//! Compile-time contract for the explicit handler-context parameter lane.

#![cfg(not(target_arch = "wasm32"))]

#[test]
fn handler_context_parameter_contract() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/handler_context_positions_pass.rs");
    cases.compile_fail("tests/ui/handler_context_missing_trait.rs");
    cases.compile_fail("tests/ui/handler_context_malformed_attr.rs");
}
