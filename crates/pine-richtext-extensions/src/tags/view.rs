use pine_richtext::view::{
    NodeViewError, NodeViewSelection, NodeViewSpec, NodeViewUpdate, RichTextNodeView,
    RichTextViewExtension, use_node_view_handle,
};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use super::{TagNode, TagsExtension};

/// Built-in accessible chip view for [`TagNode`].
///
/// The component stores only a render snapshot. Durable identity, label, and
/// kind remain in the semantic node. Applications customize it through the
/// stable classes, `data-*` states, and `--pine-richtext-tag-*` variables in
/// `tag.css`. Setting `--pine-richtext-tag-remove-display: inline-flex` enables
/// the optional remove action without persisting a UI preference in the doc.
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineRichTextTag.poco",
    style = "tag.css",
    role = "visual",
    display = "contents"
)]
pub struct PineRichTextTag {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub selection_state: String,
    pub editable: bool,
    pub editor_focused: bool,
    pub accessible_label: String,
    pub remove_label: String,
}

impl RichTextNodeView<TagNode> for PineRichTextTag {
    fn sync_node(
        &mut self,
        update: NodeViewUpdate<<TagNode as pine_richtext::RichTextNodeType>::Attrs>,
    ) -> Result<(), NodeViewError> {
        self.id = update.attrs.id.to_string();
        self.label = update.attrs.label.to_string();
        self.kind = update
            .attrs
            .kind
            .map_or_else(|| "neutral".to_string(), |kind| kind.as_str().to_string());
        self.selection_state = selection_state(update.selection).to_string();
        self.editable = update.editable;
        self.editor_focused = update.editor_focused;
        self.accessible_label = format!("Tag: {}", update.attrs.label);
        self.remove_label = format!("Remove tag {}", update.attrs.label);
        Ok(())
    }
}

fn selection_state(selection: NodeViewSelection) -> &'static str {
    match selection {
        NodeViewSelection::Outside => "outside",
        NodeViewSelection::Node => "node",
        NodeViewSelection::CursorInside => "cursor-inside",
        NodeViewSelection::NodeContainsRange => "contains-range",
        NodeViewSelection::RangeContainsNode => "within-range",
        NodeViewSelection::CrossesBoundary => "crosses-boundary",
        NodeViewSelection::Cells { .. } => "cells",
    }
}

#[handlers]
impl PineRichTextTag {
    pub fn remove(&mut self) {
        if let Ok(handle) = use_node_view_handle::<TagNode>()
            && let Err(error) = handle.delete()
        {
            tracing::warn!(
                target: "pocopine.log",
                %error,
                "tag chip remove action failed"
            );
        }
    }
}

impl RichTextViewExtension for TagsExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        vec![NodeViewSpec::atom_component::<TagNode, PineRichTextTag>()]
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod browser_tests {
    use pine_richtext::view::NodeViewSelection;
    use pocopine::App;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    use super::*;
    use crate::tags::{TagAttrs, TagKind};

    wasm_bindgen_test_configure!(run_in_browser);

    fn document() -> web_sys::Document {
        web_sys::window().unwrap().document().unwrap()
    }

    #[wasm_bindgen_test]
    fn chip_mounts_with_accessible_semantic_state() {
        let host = document().create_element("span").unwrap();
        host.set_attribute("data-pine-node-type", "tag").unwrap();
        host.set_attribute("data-pos", "7").unwrap();
        document().body().unwrap().append_child(&host).unwrap();

        let attrs = TagAttrs::new("priority", "Priority")
            .unwrap()
            .with_kind(TagKind::Warning);
        let mounted = App::mount_subtree_with::<PineRichTextTag, _>(&host, move |component, _| {
            component
                .sync_node(NodeViewUpdate {
                    attrs,
                    marks: Vec::new(),
                    content: pine_richtext::model::Fragment::empty(),
                    selection: NodeViewSelection::Node,
                    editable: true,
                    editor_focused: true,
                })
                .map_err(|error| pocopine::MountInitError::new(error.to_string()))
        })
        .unwrap();

        let chip = host.query_selector(".pine-richtext-tag").unwrap().unwrap();
        assert_eq!(chip.tag_name(), "SPAN");
        assert_eq!(chip.get_attribute("role").as_deref(), Some("group"));
        assert_eq!(
            chip.get_attribute("aria-label").as_deref(),
            Some("Tag: Priority")
        );
        assert_eq!(chip.get_attribute("data-kind").as_deref(), Some("warning"));
        assert_eq!(
            chip.get_attribute("data-selection").as_deref(),
            Some("node")
        );
        assert_eq!(chip.text_content().as_deref(), Some("#Priority"));

        mounted.unmount();
        host.remove();
    }
}
