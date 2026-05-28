//! Compile-fail driver for the `#[query]` macro.
//!
//! Trybuild verifies that selector inputs the macro explicitly
//! rejects DO fail to compile. We deliberately don't pin the exact
//! `.stderr` output (rustc spans + clippy versions drift faster than
//! the macro itself); the test asserts "this case must not compile",
//! which is the contract that matters.

#[test]
fn query_rejected_inputs_fail_to_compile() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/query_impl_trait_top_level.rs");
    cases.compile_fail("tests/ui/query_impl_trait_nested.rs");
    cases.compile_fail("tests/ui/query_impl_trait_assoc.rs");
    cases.compile_fail("tests/ui/query_async_fn.rs");
}
