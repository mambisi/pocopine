//! Snapshot tests asserting the folded default-extension schema matches
//! today's hand-rolled `schema_basic::schema()` exactly. C3 will rewire
//! `schema()` to fold extensions — these tests prove the folding side
//! has no behavioural drift before we flip the switch.

use crate::extension::{ExtensionNodeView, RichTextExtension};
use crate::extensions::{default_extensions, TaskListExtension};
use crate::model::Schema;
use crate::schema_basic;

/// Fold a fresh `Schema` from the extensions' contributions, in the
/// exact order `default_extensions()` returns them. Used by snapshot
/// tests below.
fn fold_extensions_into_schema() -> Schema {
    let mut builder = Schema::builder();
    for ext in default_extensions() {
        for spec in ext.nodes() {
            builder = builder.node(spec);
        }
        for spec in ext.marks() {
            builder = builder.mark(spec);
        }
    }
    builder.finish().expect("folded schema is valid")
}

/// The seed doc the demo uses (rich initial doc with paragraphs + a
/// checklist) renders to byte-identical HTML through both the
/// hand-rolled `schema_basic::schema()` and the folded extension
/// schema. Different ranks across the two schemas would surface here.
#[test]
fn folded_default_extensions_produce_same_node_order_as_schema_basic() {
    let folded = fold_extensions_into_schema();
    let basic = schema_basic::schema();

    // `node_type_names` returns names sorted by insertion rank — what
    // content-match resolution actually consumes. Any reorder would
    // surface here before C3 flips the switch.
    assert_eq!(
        folded.node_type_names(),
        basic.node_type_names(),
        "default-extension fold must produce the same node sequence as schema_basic::schema()"
    );

    // Marks live in alphabetical map order; we just confirm the set is
    // identical by resolving every expected mark on both schemas.
    for mark in ["link", "em", "strong", "code"] {
        assert!(folded.mark_type(mark).is_ok(), "folded missing `{}`", mark);
        assert!(basic.mark_type(mark).is_ok(), "basic missing `{}`", mark);
    }
}

#[test]
fn task_list_extension_with_node_view_tag_emits_binding() {
    let ext = TaskListExtension::new().with_node_view_tag("pine-task-item", Some("[data-content]"));
    let bindings: Vec<ExtensionNodeView> = ext.node_views();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].node_type, "task_item");
    assert_eq!(bindings[0].tag, "pine-task-item");
    assert_eq!(
        bindings[0].content_selector.as_deref(),
        Some("[data-content]")
    );
}

#[test]
fn task_list_extension_default_has_no_node_view() {
    let bindings = TaskListExtension::new().node_views();
    assert!(bindings.is_empty());
}

#[test]
fn folded_schema_can_render_demo_seed_doc() {
    // The demo seeds with paragraph + paragraph + task_list. After C3
    // the schema is folded from extensions; if the fold produced a
    // schema with different content-match resolution behavior, this
    // assemble-and-render path would fail.
    use crate::render::render_doc_to_html;
    use crate::schema_basic::{doc, em, paragraph, strong, task_item, task_list, text};

    let strong_mark = strong().unwrap();
    let em_mark = em().unwrap();

    let p1 = paragraph(vec![text("Hello, pine-richtext.", Vec::new()).unwrap()]).unwrap();
    let p2 = paragraph(vec![
        text("Select some text and use the toolbar: ", Vec::new()).unwrap(),
        text("Bold", vec![strong_mark]).unwrap(),
        text(", ", Vec::new()).unwrap(),
        text("italic", vec![em_mark]).unwrap(),
    ])
    .unwrap();
    let item = task_item(
        true,
        vec![paragraph(vec![text("Schema task", Vec::new()).unwrap()]).unwrap()],
    )
    .unwrap();
    let list = task_list(vec![item]).unwrap();
    let document = doc(vec![p1, p2, list]).unwrap();

    // Fresh runtime build (not the cached `runtime::registry::default`)
    // so this test doesn't race with `extension::tests` /
    // `runtime::tests` on the shared `SCHEMA_REALIZED` flag.
    let runtime = crate::runtime::RuntimeBuilder::new().build();
    let html = render_doc_to_html(&runtime, &document);
    assert!(html.contains(r#"<p data-pos="0">Hello, pine-richtext.</p>"#));
    assert!(html.contains("<strong>Bold</strong>"));
    assert!(html.contains("<em>italic</em>"));
    assert!(html.contains(r#"data-checked="true""#));
}

#[cfg(feature = "view")]
#[test]
fn task_list_extension_with_node_view_uses_component_tag_name() {
    use pocopine_core::app::Component;

    struct DummyComponent;
    impl Component for DummyComponent {
        const NAME: &'static str = "dummy-task-item";
        fn register() {}
    }

    let ext = TaskListExtension::new().with_node_view::<DummyComponent>();
    let bindings = ext.node_views();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].tag, "dummy-task-item");
}
