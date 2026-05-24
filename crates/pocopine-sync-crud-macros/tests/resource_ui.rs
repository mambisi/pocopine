#[test]
fn resource_attribute_compile_failures() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/resource_inherent_impl.rs");
    cases.compile_fail("tests/ui/resource_invalid_name.rs");
    cases.compile_fail("tests/ui/resource_empty_name.rs");
    cases.compile_fail("tests/ui/resource_control_name.rs");
    cases.compile_fail("tests/ui/resource_unknown_key.rs");
    cases.compile_fail("tests/ui/resource_extra_tokens.rs");
}
