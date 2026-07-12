//! Compile-time diagnostics for structural bodies in nested template plans.

#![cfg(not(target_arch = "wasm32"))]

#[test]
fn nested_template_plan_diagnostics() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/plan_nested_branch_roots.rs");
}
