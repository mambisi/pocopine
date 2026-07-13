//! Checklist nodes (`task_list`, `task_item`) + commands that wrap
//! content into them and toggle their `checked` attribute.
//!
//! [`TaskItemNode`] owns the persisted `task_item` schema while a
//! [`crate::render::NodeDomSpec`] declares its native `<li>` fallback. Browser
//! apps can pair that semantic type with a Pocopine component through
//! [`TaskListExtension::with_typed_node_view`], without naming either a wire
//! node or a custom-element tag as a string.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{BoxedCommand, wrap_in_list};
use crate::extension::{NamedCommand, RichTextExtension};
use crate::markdown::{EventSink, NodeEmitter};
use crate::model::{Attrs, NodeSpec};
use crate::render::{DomAttrBinding, NodeDomSpec};
use crate::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    TextProjection,
};
use crate::state::{EditorState, Transaction};
use crate::transform::{AttrStep, Step};
use crate::{RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec};
use pulldown_cmark::{Event as MdEvent, Tag as MdTag, TagEnd as MdTagEnd};

/// Closed persisted attributes for one checklist item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TaskItemAttrs {
    #[serde(default)]
    pub checked: bool,
}

/// Typed semantic identity for the built-in `task_item` node.
pub struct TaskItemNode;

impl RichTextNodeType for TaskItemNode {
    const NAME: &'static str = "task_item";
    const VERSION: u32 = 1;
    type Attrs = TaskItemAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .content("paragraph block*")
            .defining()
            .attr("checked", json!(false))
    }
}

/// Checklist model contribution, optionally paired with one typed component
/// view. `View = ()` means model/native-DOM only.
pub struct TaskListExtension<View = ()> {
    _view: PhantomData<fn() -> View>,
}

impl<View> Default for TaskListExtension<View> {
    fn default() -> Self {
        Self { _view: PhantomData }
    }
}

impl TaskListExtension<()> {
    /// Build the extension without a custom node view (default
    /// `<li class="task-item">` rendering).
    pub fn new() -> Self {
        Self::default()
    }

    /// Pair [`TaskItemNode`] with a typed editable Pocopine component.
    ///
    /// `C` must implement the exact semantic node-view contract and expose one
    /// compile-time-proven `pp-owned-content` outlet. No node-name, component
    /// tag, content selector, or document position crosses the app boundary.
    #[cfg(feature = "view")]
    pub fn with_typed_node_view<C>(self) -> TaskListExtension<C>
    where
        C: crate::view::RichTextNodeView<TaskItemNode> + pocopine::OwnedContentOutletComponent,
    {
        TaskListExtension { _view: PhantomData }
    }
}

impl<View> TaskListExtension<View> {
    fn wrap_command() -> NamedCommand {
        Arc::new(|args: Value| -> Option<BoxedCommand> {
            let attrs = args
                .get("attrs")
                .cloned()
                .and_then(|v| serde_json::from_value::<Attrs>(v).ok())
                .unwrap_or_default();
            Some(wrap_in_list("task_list", "task_item", attrs))
        })
    }

    fn set_checked_command() -> NamedCommand {
        Arc::new(|args: Value| -> Option<BoxedCommand> {
            let pos = args.get("pos")?.as_u64()? as usize;
            let checked = args.get("checked")?.as_bool()?;
            Some(Box::new(
                move |state: &EditorState| -> Option<Transaction> {
                    let mut tr = state.tr();
                    tr.step(Step::Attr(AttrStep {
                        pos,
                        attr: "checked".into(),
                        value: Some(json!(checked)),
                    }))
                    .ok()?;
                    Some(tr)
                },
            ))
        })
    }
}

impl<View: 'static> RichTextExtension for TaskListExtension<View> {
    fn name(&self) -> &str {
        "task_list"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("task_list")
                .group("block")
                .content("task_item+"),
        ]
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<TaskItemNode>()]
    }

    fn dom_views(&self) -> Vec<NodeDomSpec> {
        vec![
            NodeDomSpec::content::<TaskItemNode>("li")
                .class("task-item")
                .bind_attr(DomAttrBinding::boolean("data-checked", "checked")),
        ]
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<TaskItemNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::content::<TaskItemNode>("li")
                        .class("task-item")
                        .bind_attr(DomAttrBinding::boolean("data-checked", "checked")),
                ))
                .plain_text(PlainTextPolicy::projected(TextProjection::sequence([
                    TextProjection::boolean("checked", "[x] ", "[ ] "),
                    TextProjection::content_separated("\n"),
                ])))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        vec![
            ("wrap_in_task_list".into(), Self::wrap_command()),
            ("set_task_checked".into(), Self::set_checked_command()),
        ]
    }

    fn list_item_types(&self) -> &'static [&'static str] {
        &["task_item"]
    }

    fn markdown_node_emitters(&self) -> Vec<(String, NodeEmitter)> {
        // `task_list` renders as a bullet list whose items each
        // open with a GFM `[ ] ` / `[x] ` marker. The
        // `TaskListMarker` event is recognized by
        // `pulldown_cmark_to_cmark` and emits as `[ ]` or `[x]`
        // followed by a space — exactly the shape GFM expects.
        vec![
            (
                "task_list".into(),
                Arc::new(|node, _parent, _index, sink: &mut EventSink<'_>| {
                    sink.push(MdEvent::Start(MdTag::List(None)));
                    sink.render_content(node);
                    sink.push(MdEvent::End(MdTagEnd::List(false)));
                }),
            ),
            (
                "task_item".into(),
                Arc::new(|node, _parent, _index, sink: &mut EventSink<'_>| {
                    let checked = node
                        .attrs()
                        .get("checked")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    sink.push(MdEvent::Start(MdTag::Item));
                    sink.push(MdEvent::TaskListMarker(checked));
                    sink.render_content(node);
                    sink.push(MdEvent::End(MdTagEnd::Item));
                }),
            ),
        ]
    }
}

#[cfg(feature = "view")]
impl<C> crate::view::RichTextViewExtension for TaskListExtension<C>
where
    C: crate::view::RichTextNodeView<TaskItemNode> + pocopine::OwnedContentOutletComponent,
{
    fn typed_node_views(&self) -> Vec<crate::view::NodeViewSpec> {
        vec![crate::view::NodeViewSpec::editable_component::<TaskItemNode, C>().native_host()]
    }
}
