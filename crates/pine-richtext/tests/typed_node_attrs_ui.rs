#[test]
fn rich_text_node_attrs_rejects_open_or_asymmetric_serde_shapes() {
    trybuild::TestCases::new().compile_fail("tests/ui/rich_text_node_attrs/*.rs");
}
