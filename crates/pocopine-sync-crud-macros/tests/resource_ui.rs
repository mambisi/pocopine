#[test]
fn resource_attribute_compile_failures() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/resource_module_override.rs");
    cases.compile_fail("tests/ui/resource_inherent_impl.rs");
    cases.compile_fail("tests/ui/resource_invalid_name.rs");
    cases.compile_fail("tests/ui/resource_empty_name.rs");
    cases.compile_fail("tests/ui/resource_control_name.rs");
    cases.compile_fail("tests/ui/resource_unknown_key.rs");
    cases.compile_fail("tests/ui/resource_extra_tokens.rs");
    cases.compile_fail("tests/ui/resource_name_requires_module.rs");
    cases.compile_fail("tests/ui/resource_missing_draft.rs");
    cases.compile_fail("tests/ui/resource_generic_impl.rs");
}
