//! Compile-fail tests: misuse of the macros produces clear diagnostics.
#[test]
fn ui() {
    trybuild::TestCases::new().compile_fail("tests/compile_fail/*.rs");
}
