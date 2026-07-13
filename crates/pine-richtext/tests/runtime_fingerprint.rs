use pine_richtext::extension::RichTextExtension;
use pine_richtext::model::NodeSpec;
use pine_richtext::render::NodeDomSpec;
use pine_richtext::runtime::RuntimeBuilder;
use pine_richtext::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    TextProjection,
};
use pine_richtext::{RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
struct FingerprintAttrs {
    label: String,
}

struct FingerprintNode;

impl RichTextNodeType for FingerprintNode {
    const NAME: &'static str = "fingerprint_block";
    const VERSION: u32 = 1;
    type Attrs = FingerprintAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .required_attr("label")
    }
}

struct FingerprintExtension {
    class: &'static str,
}

impl RichTextExtension for FingerprintExtension {
    fn name(&self) -> &str {
        "fingerprint-fixture"
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<FingerprintNode>()]
    }

    fn dom_views(&self) -> Vec<NodeDomSpec> {
        vec![
            NodeDomSpec::atom::<FingerprintNode>("aside")
                .class(self.class)
                .bind_text("label"),
        ]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<FingerprintNode>()
                .markdown(MarkdownPolicy::Unsupported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::atom::<FingerprintNode>("aside").bind_text("label"),
                ))
                .plain_text(PlainTextPolicy::projected(TextProjection::attr("label")))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
struct ExtraAttrs {
    enabled: bool,
}

struct ExtraNode;

impl RichTextNodeType for ExtraNode {
    const NAME: &'static str = "fingerprint_extra";
    const VERSION: u32 = 1;
    type Attrs = ExtraAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .attr("enabled", serde_json::json!(false))
    }
}

struct ExtraExtension;

impl RichTextExtension for ExtraExtension {
    fn name(&self) -> &str {
        "fingerprint-extra-fixture"
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<ExtraNode>()]
    }

    fn dom_views(&self) -> Vec<NodeDomSpec> {
        vec![NodeDomSpec::atom::<ExtraNode>("div")]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<ExtraNode>()
                .markdown(MarkdownPolicy::Unsupported)
                .html(SemanticHtmlPolicy::dom(NodeDomSpec::atom::<ExtraNode>(
                    "div",
                )))
                .plain_text(PlainTextPolicy::projected(TextProjection::boolean(
                    "enabled", "enabled", "disabled",
                )))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }
}

#[test]
fn runtime_name_and_dom_view_are_excluded_from_wire_fingerprint() {
    let first = RuntimeBuilder::new()
        .name("alpha")
        .with(FingerprintExtension { class: "first" })
        .try_build()
        .unwrap();
    let second = RuntimeBuilder::new()
        .name("beta")
        .with(FingerprintExtension { class: "second" })
        .try_build()
        .unwrap();

    assert_eq!(first.wire_descriptor(), second.wire_descriptor());
    assert_eq!(first.wire_fingerprint(), second.wire_fingerprint());
    assert_eq!(first.wire_fingerprint().len(), 64);
    assert!(
        first
            .wire_fingerprint()
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
}

#[test]
fn semantic_schema_change_changes_wire_fingerprint() {
    let base = RuntimeBuilder::new()
        .with(FingerprintExtension { class: "same" })
        .try_build()
        .unwrap();
    let changed = RuntimeBuilder::new()
        .with(FingerprintExtension { class: "same" })
        .with(ExtraExtension)
        .try_build()
        .unwrap();

    assert_ne!(base.wire_descriptor(), changed.wire_descriptor());
    assert_ne!(base.wire_fingerprint(), changed.wire_fingerprint());
}
