//! Checklist nodes (`task_list`, `task_item`) + commands that wrap
//! content into them and toggle their `checked` attribute.
//!
//! Optional custom-element binding via [`TaskListExtension::with_node_view`]
//! lets apps replace the default `<li>` rendering with a pocopine
//! component (e.g. `PineTaskItem`). The builder method is gated behind
//! the `view` feature because the `pocopine::Component` trait it uses
//! to derive the tag name is itself view-only.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::commands::{BoxedCommand, wrap_in_list};
use crate::extension::{ExtensionNodeView, NamedCommand, RichTextExtension};
use crate::markdown::{EventSink, NodeEmitter};
use crate::model::{Attrs, NodeSpec};
use crate::state::{EditorState, Transaction};
use crate::transform::{AttrStep, Step};
use pulldown_cmark::{Event as MdEvent, Tag as MdTag, TagEnd as MdTagEnd};

/// Default custom-element selector identifying where pocopine should
/// inject content children inside a node-view component. Apps that
/// register their own task-item component should use the same
/// `data-pine-richtext-content` attribute on the host's content slot.
pub const DEFAULT_TASK_ITEM_CONTENT_SELECTOR: &str = "[data-pine-richtext-content]";

#[derive(Default)]
pub struct TaskListExtension {
    node_view: Option<ExtensionNodeView>,
}

impl TaskListExtension {
    /// Build the extension without a custom node view (default
    /// `<li class="task-item">` rendering).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a pocopine component as the custom element rendered for
    /// every `task_item` node. The component's
    /// [`pocopine_core::app::Component::NAME`] becomes the tag.
    /// Requires the `view` feature.
    #[cfg(feature = "view")]
    pub fn with_node_view<C: pocopine_core::app::Component>(mut self) -> Self {
        self.node_view = Some(ExtensionNodeView {
            node_type: "task_item".into(),
            tag: C::NAME.into(),
            content_selector: Some(DEFAULT_TASK_ITEM_CONTENT_SELECTOR.into()),
        });
        self
    }

    /// Bind a custom element by tag name directly. Lower-level than
    /// [`Self::with_node_view`]; useful for non-view callers and tests
    /// that don't have a pocopine component to thread.
    pub fn with_node_view_tag(
        mut self,
        tag: impl Into<String>,
        content_selector: Option<impl Into<String>>,
    ) -> Self {
        self.node_view = Some(ExtensionNodeView {
            node_type: "task_item".into(),
            tag: tag.into(),
            content_selector: content_selector.map(Into::into),
        });
        self
    }

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

impl RichTextExtension for TaskListExtension {
    fn name(&self) -> &str {
        "task_list"
    }

    fn nodes(&self) -> Vec<NodeSpec> {
        vec![
            NodeSpec::new("task_list")
                .group("block")
                .content("task_item+"),
            NodeSpec::new("task_item")
                .content("paragraph block*")
                .defining()
                .attr("checked", json!(false)),
        ]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        vec![
            ("wrap_in_task_list".into(), Self::wrap_command()),
            ("set_task_checked".into(), Self::set_checked_command()),
        ]
    }

    fn node_views(&self) -> Vec<ExtensionNodeView> {
        self.node_view
            .as_ref()
            .map(|nv| {
                vec![ExtensionNodeView {
                    node_type: nv.node_type.clone(),
                    tag: nv.tag.clone(),
                    content_selector: nv.content_selector.clone(),
                }]
            })
            .unwrap_or_default()
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
