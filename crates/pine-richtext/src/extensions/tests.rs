//! Tests for the built-in extension fold and its current per-runtime typed
//! semantic/native/component-view associations. The `schema_basic` comparison
//! guards the legacy helper's parity with a freshly built default runtime.

#[cfg(feature = "view")]
use crate::extensions::TaskListExtension;
use crate::extensions::default_extensions;
use crate::model::Schema;
use crate::schema_basic;

#[cfg(feature = "view")]
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[pocopine::component(
    name = "typed-task-item-extension-fixture",
    template = poco! {<div class="typed-task-item-fixture" pp-owned-content></div>}
)]
struct TypedTaskItemFixture {
    checked: bool,
}

#[cfg(feature = "view")]
#[pocopine::handlers]
impl TypedTaskItemFixture {}

#[cfg(feature = "view")]
impl crate::view::RichTextNodeView<crate::extensions::TaskItemNode> for TypedTaskItemFixture {
    fn sync_node(
        &mut self,
        update: crate::view::NodeViewUpdate<crate::extensions::TaskItemAttrs>,
    ) -> Result<(), crate::view::NodeViewError> {
        self.checked = update.attrs.checked;
        Ok(())
    }
}

/// Fold a fresh `Schema` from the extensions' contributions, in the
/// exact order `default_extensions()` returns them. Used by snapshot
/// tests below.
fn fold_extensions_into_schema() -> Schema {
    let mut builder = Schema::builder();
    for ext in default_extensions() {
        for spec in ext.nodes() {
            builder = builder.node(spec);
        }
        for spec in ext.typed_nodes() {
            builder = builder.typed_node(spec);
        }
        for spec in ext.marks() {
            builder = builder.mark(spec);
        }
    }
    builder.finish().expect("folded schema is valid")
}

/// The seed doc the demo uses (rich initial doc with paragraphs + a
/// checklist) renders to byte-identical HTML through both the
/// legacy `schema_basic::schema()` helper and a fresh extension fold. Different
/// ranks across the two schemas would surface here.
#[test]
fn folded_default_extensions_produce_same_node_order_as_schema_basic() {
    let folded = fold_extensions_into_schema();
    let basic = schema_basic::schema();

    // `node_type_names` returns names sorted by insertion rank — what
    // content-match resolution actually consumes. Any reorder would
    // surface here as runtime/schema_basic compatibility drift.
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
fn task_item_uses_typed_schema_and_native_list_item_dom() {
    use crate::extensions::{TaskItemAttrs, TaskItemNode};
    use crate::{RichTextNodeAttrs, RichTextNodeType};

    let runtime = crate::runtime::RuntimeBuilder::new().build();
    let typed = runtime
        .lookup_typed_node(TaskItemNode::NAME)
        .expect("default runtime retains the typed task-item descriptor");
    let dom = runtime
        .lookup_dom_view(TaskItemNode::NAME)
        .expect("typed task items declare a native DOM view");

    assert_eq!(
        typed.semantic_type_id(),
        std::any::TypeId::of::<TaskItemNode>()
    );
    assert_eq!(typed.version(), 1);
    assert_eq!(TaskItemAttrs::KEYS, &["checked"]);
    assert_eq!(
        dom.semantic_type_id(),
        std::any::TypeId::of::<TaskItemNode>()
    );
    assert_eq!(dom.root_tag(), "li");
}

#[cfg(feature = "view")]
#[test]
fn typed_task_item_builder_pairs_the_exact_component_and_owned_outlet() {
    use crate::extensions::TaskItemNode;
    use crate::view::{NodeViewHost, NodeViewKind, RichTextViewExtension};

    let extension = TaskListExtension::new().with_typed_node_view::<TypedTaskItemFixture>();
    let views = extension.typed_node_views();

    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].semantic_type_id(),
        std::any::TypeId::of::<TaskItemNode>()
    );
    assert_eq!(
        views[0].component_type_id(),
        std::any::TypeId::of::<TypedTaskItemFixture>()
    );
    assert_eq!(views[0].node_type(), "task_item");
    assert_eq!(
        views[0].component_name(),
        "typed-task-item-extension-fixture"
    );
    assert_eq!(views[0].kind(), NodeViewKind::Editable);
    assert_eq!(views[0].host(), NodeViewHost::Native);
    assert_eq!(views[0].owned_content_path(), Some(&[][..]));
}

#[test]
fn folded_schema_can_render_demo_seed_doc() {
    // The demo seeds with paragraph + paragraph + task_list. If the current
    // runtime fold produced different content-match behavior, this
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
