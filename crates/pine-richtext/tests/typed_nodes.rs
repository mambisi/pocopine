use std::any::TypeId;

use pine_richtext::extension::RichTextExtension;
use pine_richtext::model::{Attrs, NodeSpec, Schema};
use pine_richtext::render::NodeDomSpec;
use pine_richtext::runtime::{RuntimeBuildError, RuntimeBuilder};
use pine_richtext::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    TextProjection,
};
use pine_richtext::{
    NodeMigration, NodeMigrationError, RichTextError, RichTextNodeAttrs, RichTextNodeType,
    TypedNodeAttrsError, TypedNodeSpec, WireNode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

fn default_theme() -> String {
    "light".to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
#[serde(rename_all = "camelCase")]
struct DiagramAttrs {
    diagram_id: String,
    #[serde(default = "default_theme", rename = "display-theme")]
    theme: String,
    #[serde(default)]
    show_grid: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE", default)]
struct LoudAttrs {
    first_value: String,
    second_value: bool,
}

impl Default for LoudAttrs {
    fn default() -> Self {
        Self {
            first_value: "default".to_string(),
            second_value: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
struct GenericAttrs<T> {
    value: T,
}

struct DiagramNode;

impl RichTextNodeType for DiagramNode {
    const NAME: &'static str = "diagram";
    const VERSION: u32 = 2;
    type Attrs = DiagramAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .required_attr("diagramId")
            .attr("display-theme", json!("light"))
            .attr("showGrid", json!(false))
    }

    fn migrations() -> &'static [NodeMigration] {
        static MIGRATIONS: &[NodeMigration] = &[NodeMigration::new(1, 2, migrate_v1_to_v2)];
        MIGRATIONS
    }
}

struct DiagramNodeImpostor;

impl RichTextNodeType for DiagramNodeImpostor {
    const NAME: &'static str = DiagramNode::NAME;
    const VERSION: u32 = DiagramNode::VERSION;
    type Attrs = DiagramAttrs;

    fn spec() -> NodeSpec {
        DiagramNode::spec()
    }

    fn migrations() -> &'static [NodeMigration] {
        DiagramNode::migrations()
    }
}

struct BrokenDiagramNode;

impl RichTextNodeType for BrokenDiagramNode {
    const NAME: &'static str = "broken_diagram";
    const VERSION: u32 = 2;
    type Attrs = DiagramAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .atom()
            .required_attr("diagramId")
            .attr("display-theme", json!("light"))
            .attr("showGrid", json!(false))
    }
}

struct DiagramExtension;

impl RichTextExtension for DiagramExtension {
    fn name(&self) -> &str {
        "diagram"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![NodeSpec::new("doc").content("diagram*")]
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<DiagramNode>()]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<DiagramNode>()
                .markdown(MarkdownPolicy::Unsupported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::atom::<DiagramNode>("figure").bind_text("diagramId"),
                ))
                .plain_text(PlainTextPolicy::projected(TextProjection::attr(
                    "diagramId",
                )))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }
}

struct BrokenDiagramExtension;

impl RichTextExtension for BrokenDiagramExtension {
    fn name(&self) -> &str {
        "broken_diagram"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![NodeSpec::new("doc").content("broken_diagram*")]
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<BrokenDiagramNode>()]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<BrokenDiagramNode>()
                .markdown(MarkdownPolicy::Unsupported)
                .html(SemanticHtmlPolicy::Unsupported)
                .plain_text(PlainTextPolicy::Unsupported)
                .clipboard(ClipboardPolicy::Unsupported),
        ]
    }
}

fn migrate_v1_to_v2(mut node: WireNode) -> Result<WireNode, NodeMigrationError> {
    if node.name != DiagramNode::NAME {
        return Err(NodeMigrationError::new("wrong semantic node type"));
    }
    node.version = Some(2);
    node.attrs
        .entry("showGrid".to_string())
        .or_insert(json!(false));
    Ok(node)
}

#[test]
fn derive_emits_the_closed_serde_wire_key_set() {
    assert_eq!(
        DiagramAttrs::KEYS,
        &["diagramId", "display-theme", "showGrid"]
    );
    assert_eq!(LoudAttrs::KEYS, &["FIRST-VALUE", "SECOND-VALUE"]);
    assert_eq!(GenericAttrs::<String>::KEYS, &["value"]);

    let attrs = DiagramAttrs {
        diagram_id: "dg_42".to_string(),
        theme: default_theme(),
        show_grid: false,
    };
    let object = serde_json::to_value(attrs)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let declared = DiagramAttrs::KEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(object, declared, "derive keys must match serde output");
}

#[test]
fn typed_spec_retains_identity_schema_and_typed_decoder() {
    let spec = TypedNodeSpec::of::<DiagramNode>();
    assert_eq!(spec.semantic_type_id(), TypeId::of::<DiagramNode>());
    assert!(spec.semantic_rust_type().ends_with("DiagramNode"));
    assert_eq!(spec.name(), "diagram");
    assert_eq!(spec.version(), 2);
    assert_eq!(spec.attr_keys(), DiagramAttrs::KEYS);
    assert_eq!(spec.migrations().len(), 1);
    let _ = spec.spec();

    let mut attrs = Attrs::new();
    attrs.insert("diagramId".to_string(), json!("dg_42"));
    spec.validate_attrs(&attrs).unwrap();

    attrs.insert("futureField".to_string(), json!(true));
    assert!(matches!(
        spec.validate_attrs(&attrs),
        Err(TypedNodeAttrsError::UnknownKey {
            node_type: "diagram",
            key,
        }) if key == "futureField"
    ));
}

#[test]
fn typed_decoder_reports_missing_required_fields() {
    let error = TypedNodeSpec::of::<DiagramNode>()
        .validate_attrs(&Attrs::new())
        .unwrap_err();
    assert!(matches!(error, TypedNodeAttrsError::Decode { .. }));
    assert!(error.to_string().contains("diagramId"));
}

#[test]
fn wire_nodes_round_trip_and_migrate_without_materializing_model_nodes() {
    let mut wire = WireNode::new("diagram", Some(1));
    wire.leaf = true;
    wire.attrs.insert("diagramId".to_string(), json!("dg_42"));

    let encoded = serde_json::to_value(&wire).unwrap();
    assert_eq!(encoded["type"], "diagram");
    assert_eq!(encoded["version"], 1);
    assert_eq!(serde_json::from_value::<WireNode>(encoded).unwrap(), wire);

    let migrated = (DiagramNode::migrations()[0].apply)(wire).unwrap();
    assert_eq!(migrated.version, Some(2));
    assert_eq!(migrated.attrs["showGrid"], false);
}

fn diagram_schema() -> Schema {
    Schema::builder()
        .node(NodeSpec::new("doc").content("diagram*"))
        .typed_node(TypedNodeSpec::of::<DiagramNode>())
        .finish()
        .unwrap()
}

fn diagram_wire(version: u32) -> WireNode {
    let mut wire = WireNode::new(DiagramNode::NAME, Some(version));
    wire.leaf = true;
    wire.attrs.insert("diagramId".to_string(), json!("dg_42"));
    wire
}

fn document_wire(child: WireNode) -> WireNode {
    let mut document = WireNode::new("doc", None);
    document.content.push(child);
    document
}

#[test]
fn schema_materialization_migrates_nested_typed_nodes_and_stamps_current_version() {
    let document = diagram_schema()
        .materialize_wire_node(document_wire(diagram_wire(1)))
        .unwrap();
    let diagram = document.child(0).unwrap();

    assert_eq!(diagram.version(), Some(2));
    assert_eq!(diagram.attrs()["diagramId"], "dg_42");
    assert_eq!(diagram.attrs()["showGrid"], false);

    let json = serde_json::to_value(document).unwrap();
    assert_eq!(json["content"][0]["version"], 2);
}

#[test]
fn schema_materialization_rejects_future_versions_at_the_exact_child_path() {
    let error = diagram_schema()
        .materialize_wire_node(document_wire(diagram_wire(3)))
        .unwrap_err();

    assert!(matches!(
        error,
        RichTextError::WireNode { ref path, ref message }
            if path == "$.content[0].version"
                && message.contains("newer version 3")
    ));
}

#[test]
fn schema_materialization_rejects_unknown_current_attrs_at_the_exact_child_path() {
    let mut diagram = diagram_wire(2);
    diagram.attrs.insert("futureField".to_string(), json!(true));
    let error = diagram_schema()
        .materialize_wire_node(document_wire(diagram))
        .unwrap_err();

    assert!(matches!(
        error,
        RichTextError::WireNode { ref path, ref message }
            if path == "$.content[0].attrs"
                && message.contains("futureField")
    ));
}

#[test]
fn runtime_retains_exact_semantic_type_identity() {
    let runtime = RuntimeBuilder::new()
        .without_defaults()
        .with(DiagramExtension)
        .try_build()
        .unwrap();

    assert!(runtime.typed_node::<DiagramNode>().is_some());
    assert!(runtime.typed_node::<DiagramNodeImpostor>().is_none());
    assert_eq!(runtime.lookup_typed_node("diagram").unwrap().version(), 2);
}

#[test]
fn runtime_rejects_an_incomplete_typed_migration_chain_before_mount() {
    let error = RuntimeBuilder::new()
        .without_defaults()
        .with(BrokenDiagramExtension)
        .try_build()
        .unwrap_err();

    assert!(matches!(
        error,
        RuntimeBuildError::InvalidTypedNodeMigration {
            ref node_type,
            expected_from: 1,
            found: None,
            ..
        } if node_type == "broken_diagram"
    ));
}
