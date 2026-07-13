use pine_richtext::extension::RichTextExtension;
use pine_richtext::model::{Attrs, Fragment, NodeSpec, Slice};
use pine_richtext::render::{DomOutputSpec, NodeDomSpec};
use pine_richtext::runtime::{RuntimeBuildError, RuntimeBuilder};
use pine_richtext::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    SerializationError, TextProjection,
};
use pine_richtext::{
    NodeMigration, NodeMigrationError, RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec, WireNode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
struct BadgeAttrs {
    label: String,
}

struct BadgeNode;

fn migrate_badge(mut node: WireNode) -> Result<WireNode, NodeMigrationError> {
    let label = node
        .attrs
        .remove("name")
        .ok_or_else(|| NodeMigrationError::new("missing legacy name"))?;
    node.attrs.insert("label".to_string(), label);
    node.version = Some(2);
    Ok(node)
}

impl RichTextNodeType for BadgeNode {
    const NAME: &'static str = "badge";
    const VERSION: u32 = 2;
    type Attrs = BadgeAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .required_attr("label")
    }

    fn migrations() -> &'static [NodeMigration] {
        static MIGRATIONS: &[NodeMigration] = &[NodeMigration::new(1, 2, migrate_badge)];
        MIGRATIONS
    }
}

struct BadgeImpostor;

impl RichTextNodeType for BadgeImpostor {
    const NAME: &'static str = BadgeNode::NAME;
    const VERSION: u32 = BadgeNode::VERSION;
    type Attrs = BadgeAttrs;

    fn spec() -> NodeSpec {
        BadgeNode::spec()
    }

    fn migrations() -> &'static [NodeMigration] {
        BadgeNode::migrations()
    }
}

#[derive(Clone, Copy)]
enum PolicyFixture {
    Complete,
    Missing,
    Incomplete,
    Mismatched,
    Duplicate,
}

struct BadgeExtension(PolicyFixture);

fn complete_policy() -> NodeSerializationSpec {
    NodeSerializationSpec::for_node::<BadgeNode>()
        .markdown(MarkdownPolicy::Unsupported)
        .html(SemanticHtmlPolicy::dom(NodeDomSpec::nested::<BadgeNode>(
            DomOutputSpec::element("aside").bind_text("label"),
        )))
        .plain_text(PlainTextPolicy::projected(
            TextProjection::attr("label").prefixed("badge: "),
        ))
        .clipboard(ClipboardPolicy::Semantic)
}

impl RichTextExtension for BadgeExtension {
    fn name(&self) -> &str {
        "badge-fixture"
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<BadgeNode>()]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        match self.0 {
            PolicyFixture::Complete => vec![complete_policy()],
            PolicyFixture::Missing => Vec::new(),
            PolicyFixture::Incomplete => vec![
                NodeSerializationSpec::for_node::<BadgeNode>()
                    .markdown(MarkdownPolicy::Unsupported),
            ],
            PolicyFixture::Mismatched => vec![
                NodeSerializationSpec::for_node::<BadgeImpostor>()
                    .markdown(MarkdownPolicy::Unsupported)
                    .html(SemanticHtmlPolicy::Unsupported)
                    .plain_text(PlainTextPolicy::Unsupported)
                    .clipboard(ClipboardPolicy::Unsupported),
            ],
            PolicyFixture::Duplicate => vec![complete_policy(), complete_policy()],
        }
    }
}

#[test]
fn runtime_rejects_missing_incomplete_mismatched_and_duplicate_policies() {
    let missing = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Missing))
        .try_build()
        .unwrap_err();
    assert!(matches!(
        missing,
        RuntimeBuildError::MissingNodeSerialization { .. }
    ));

    let incomplete = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Incomplete))
        .try_build()
        .unwrap_err();
    assert!(matches!(
        incomplete,
        RuntimeBuildError::IncompleteNodeSerialization { .. }
    ));

    let mismatched = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Mismatched))
        .try_build()
        .unwrap_err();
    assert!(matches!(
        mismatched,
        RuntimeBuildError::NodeSerializationTypeMismatch { .. }
    ));

    let duplicate = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Duplicate))
        .try_build()
        .unwrap_err();
    assert!(matches!(
        duplicate,
        RuntimeBuildError::DuplicateNodeSerialization { .. }
    ));
}

#[test]
fn model_exports_are_component_independent_and_unsupported_markdown_is_loud() {
    let runtime = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Complete))
        .try_build()
        .unwrap();
    let badge = runtime
        .schema()
        .node(
            BadgeNode::NAME,
            Attrs::from([("label".to_string(), json!("<Release>"))]),
            Fragment::empty(),
        )
        .unwrap();
    let doc = runtime
        .schema()
        .node(
            runtime.schema().top_node_name(),
            Attrs::new(),
            Fragment::from(badge.clone()),
        )
        .unwrap();

    assert_eq!(
        runtime.export_semantic_html(&doc).unwrap(),
        "<aside>&lt;Release&gt;</aside>"
    );
    assert_eq!(runtime.export_plain_text(&doc).unwrap(), "badge: <Release>");
    assert!(matches!(
        runtime.export_markdown(&doc),
        Err(SerializationError::Unsupported { .. })
    ));

    let clipboard = runtime
        .export_clipboard(&Slice::new(Fragment::from(badge), 0, 0))
        .unwrap();
    assert_eq!(clipboard.html, "<aside>&lt;Release&gt;</aside>");
    assert_eq!(clipboard.plain_text, "badge: <Release>");
    assert!(!clipboard.html.contains("data-pine-node-view"));
}

#[test]
fn clipboard_json_migrates_then_validates_closed_attrs() {
    let runtime = RuntimeBuilder::new()
        .with(BadgeExtension(PolicyFixture::Complete))
        .try_build()
        .unwrap();
    let legacy = json!({
        "content": [{
            "type": "badge",
            "version": 1,
            "attrs": { "name": "Legacy" },
            "leaf": true
        }],
        "openStart": 0,
        "openEnd": 0
    })
    .to_string();
    let migrated = runtime.import_clipboard_json(&legacy).unwrap();
    let badge = migrated.content.child(0).unwrap();
    assert_eq!(badge.version(), Some(2));
    assert_eq!(badge.attrs().get("label"), Some(&json!("Legacy")));

    let invalid = json!({
        "content": [{
            "type": "badge",
            "version": 2,
            "attrs": { "label": "ok", "onclick": "steal()" },
            "leaf": true
        }]
    })
    .to_string();
    assert!(matches!(
        runtime.import_clipboard_json(&invalid),
        Err(SerializationError::ClipboardJson(_))
    ));
}

#[test]
fn semantic_html_rejects_unsafe_builtin_urls() {
    let runtime = RuntimeBuilder::new().build();
    let link = pine_richtext::schema_basic::link("javascript:alert(1)", None::<String>).unwrap();
    let text = pine_richtext::schema_basic::text("click", vec![link]).unwrap();
    let paragraph = pine_richtext::schema_basic::paragraph(vec![text]).unwrap();
    let doc = pine_richtext::schema_basic::doc(vec![paragraph]).unwrap();
    assert!(matches!(
        runtime.export_semantic_html(&doc),
        Err(SerializationError::UnsafeUrl(_))
    ));
}

#[test]
fn semantic_html_structurally_serializes_marks_images_and_code_text() {
    let runtime = RuntimeBuilder::new().build();
    let link = pine_richtext::schema_basic::link(
        "https://example.test/?a=1&b=2",
        Some("quoted \"title\""),
    )
    .unwrap();
    let strong = pine_richtext::schema_basic::strong().unwrap();
    let marked = pine_richtext::schema_basic::text("<safe & sound>", vec![link, strong]).unwrap();
    let image = pine_richtext::schema_basic::image(
        "/image?a=1&b=2",
        Some("<alt>"),
        Some("image \"title\""),
    )
    .unwrap();
    let paragraph = pine_richtext::schema_basic::paragraph(vec![marked, image]).unwrap();
    let code =
        pine_richtext::schema_basic::code_block("<script>alert('not HTML')</script>").unwrap();
    let doc = pine_richtext::schema_basic::doc(vec![paragraph, code]).unwrap();

    assert_eq!(
        runtime.export_semantic_html(&doc).unwrap(),
        concat!(
            r#"<p><a href="https://example.test/?a=1&amp;b=2" title="quoted &quot;title&quot;"><strong>&lt;safe &amp; sound&gt;</strong></a>"#,
            // html5ever keeps `<` literal inside a quoted attribute (where it
            // has no tag-opening semantics) while escaping the delimiters that
            // can leave the attribute: `&` and `"`.
            r#"<img alt="<alt>" src="/image?a=1&amp;b=2" title="image &quot;title&quot;"></p>"#,
            "<pre><code>&lt;script&gt;alert('not HTML')&lt;/script&gt;</code></pre>"
        )
    );
}
