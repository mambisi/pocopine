//! DOM reconciler: turn an `(old_doc, new_doc)` pair into scoped DOM
//! mutations by walking the model tree and rendered DOM in lockstep.
//!
//! This is intentionally closer to ProseMirror's `ViewDesc` model than
//! the old diff-window patcher. Each model element is addressed by its
//! rendered `data-pos`, children are reconciled inside their owning DOM
//! parent, and node-view hosts keep their component chrome while pine
//! updates host attrs and model-owned content.

use crate::model::{Attrs, Node as RichNode};
use crate::render::{
    attr_value_to_string, render_children_to_html, render_doc_to_html, render_one_node_to_html,
};
use crate::runtime::EditorRuntime;

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, Node as DomNode};

/// What kind of DOM mutation the reconciler performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The old and new docs were equal; no DOM mutation happened.
    Unchanged,
    /// Plain inline text changed in place without reparsing element HTML.
    Text,
    /// Only reflected node attrs changed in place.
    NodeAttrs { pos: usize },
    /// One or more model children/subtrees were reconciled in place.
    Reconciled,
    /// The surface's full `innerHTML` was replaced after the live DOM
    /// no longer matched the model-owned structure.
    Full,
}

impl ReconcileOutcome {
    /// Whether this outcome changed the DOM.
    pub fn dom_changed(self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    /// Whether this patch should force the visible selection back to
    /// the model selection. Attr-only patches intentionally skip this
    /// so node-view chrome clicks do not move or recreate the cursor.
    pub fn should_sync_cursor(self) -> bool {
        matches!(self, Self::Text | Self::Reconciled | Self::Full)
    }

    /// Whether newly-rendered DOM may contain custom node-view tags
    /// that need mounting.
    pub fn should_mount_node_views(self) -> bool {
        matches!(self, Self::Reconciled | Self::Full)
    }

    /// Stable string used by JSON debug logging.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Text => "text",
            Self::NodeAttrs { .. } => "node_attrs",
            Self::Reconciled => "reconciled",
            Self::Full => "full",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ReconcileStats {
    attr_changes: usize,
    first_attr_pos: Option<usize>,
    text_changes: usize,
    structural_changes: usize,
}

impl ReconcileStats {
    fn record_attrs(&mut self, pos: usize) {
        self.attr_changes += 1;
        self.first_attr_pos.get_or_insert(pos);
    }

    fn record_text(&mut self) {
        self.text_changes += 1;
    }

    fn record_structural(&mut self) {
        self.structural_changes += 1;
    }

    fn outcome(self) -> ReconcileOutcome {
        if self.structural_changes > 0 {
            ReconcileOutcome::Reconciled
        } else if self.text_changes > 0 {
            ReconcileOutcome::Text
        } else if self.attr_changes > 0 {
            ReconcileOutcome::NodeAttrs {
                pos: self.first_attr_pos.unwrap_or(0),
            }
        } else {
            ReconcileOutcome::Unchanged
        }
    }
}

/// Reconcile a surface element against a model doc, scoped to a runtime.
/// Returns the outcome class so callers can decide whether to also sync
/// the visible selection / mount registered node views.
pub fn reconcile_surface_with_outcome(
    runtime: &EditorRuntime,
    surface: &Element,
    old_doc: &RichNode,
    new_doc: &RichNode,
) -> ReconcileOutcome {
    Reconciler::new(runtime).reconcile_surface(surface, old_doc, new_doc)
}

/// Stateless reconciler scoped to a single [`EditorRuntime`]. The
/// runtime supplies the per-instance node-view registry used by every
/// internal `node_view_*` resolution — same scope rules as
/// [`crate::render::Renderer`].
pub struct Reconciler<'a> {
    runtime: &'a EditorRuntime,
}

impl<'a> Reconciler<'a> {
    pub fn new(runtime: &'a EditorRuntime) -> Self {
        Self { runtime }
    }

    pub fn reconcile_surface(
        &self,
        surface: &Element,
        old_doc: &RichNode,
        new_doc: &RichNode,
    ) -> ReconcileOutcome {
        if old_doc == new_doc {
            return ReconcileOutcome::Unchanged;
        }

        let mut stats = ReconcileStats::default();
        let result = self.reconcile_children(surface, old_doc, new_doc, 0, &mut stats);
        if result.is_ok() {
            stats.outcome()
        } else {
            self.full_render(surface, new_doc)
        }
    }

    fn full_render(&self, surface: &Element, new_doc: &RichNode) -> ReconcileOutcome {
        let html = render_doc_to_html(self.runtime, new_doc);
        if set_inner_html(surface, &html) {
            ReconcileOutcome::Full
        } else {
            ReconcileOutcome::Unchanged
        }
    }

    fn reconcile_node(
        &self,
        dom: &Element,
        old_node: &RichNode,
        old_pos: usize,
        new_node: &RichNode,
        new_pos: usize,
        stats: &mut ReconcileStats,
    ) -> Result<(), ()> {
        if old_node == new_node && old_pos == new_pos {
            return Ok(());
        }

        if !self.dom_matches_node(dom, old_node) || !self.wrapper_compatible(old_node, new_node) {
            self.replace_element(dom, new_node, new_pos)?;
            stats.record_structural();
            return Ok(());
        }

        update_data_pos(dom, new_pos)?;
        if self.attrs_patchable(new_node) && old_node.attrs() != new_node.attrs() {
            patch_reflected_attrs(dom, old_node.attrs(), new_node.attrs())?;
            stats.record_attrs(new_pos);
        }

        if old_node == new_node {
            self.refresh_descendant_positions(dom, new_node, new_pos)?;
            return Ok(());
        }

        if self.renders_inline_children(new_node) {
            if self.patch_plain_text_inline_children(dom, old_node, new_node)? {
                stats.record_text();
                return Ok(());
            }
            let html = render_children_to_html(self.runtime, new_node, new_pos + 1);
            set_inner_html(dom, &html).then_some(()).ok_or(())?;
            stats.record_structural();
            return Ok(());
        }

        if new_node.type_name() == "code_block" {
            self.replace_element(dom, new_node, new_pos)?;
            stats.record_structural();
            return Ok(());
        }

        let content_root = self.content_root_for_node(dom, new_node)?;
        self.reconcile_children(&content_root, old_node, new_node, new_pos + 1, stats)
    }

    fn reconcile_children(
        &self,
        content_root: &Element,
        old_parent: &RichNode,
        new_parent: &RichNode,
        content_start: usize,
        stats: &mut ReconcileStats,
    ) -> Result<(), ()> {
        if old_parent.content() == new_parent.content() {
            return Ok(());
        }

        if old_parent.child_count() == 0 || new_parent.child_count() == 0 {
            let html = render_children_to_html(self.runtime, new_parent, content_start);
            set_inner_html(content_root, &html)
                .then_some(())
                .ok_or(())?;
            stats.record_structural();
            return Ok(());
        }

        let dom_children = direct_model_children(content_root)?;
        if dom_children.len() != old_parent.child_count() {
            return Err(());
        }

        let old_len = old_parent.child_count();
        let new_len = new_parent.child_count();
        let mut old_positions = child_positions(old_parent, content_start);
        let mut new_positions = child_positions(new_parent, content_start);

        let mut prefix = 0;
        while prefix < old_len && prefix < new_len {
            let old_child = old_parent.child(prefix).ok_or(())?;
            let new_child = new_parent.child(prefix).ok_or(())?;
            if old_child != new_child {
                break;
            }
            self.reconcile_node(
                &dom_children[prefix],
                old_child,
                old_positions[prefix],
                new_child,
                new_positions[prefix],
                stats,
            )?;
            prefix += 1;
        }

        let mut suffix = 0;
        while suffix < old_len.saturating_sub(prefix) && suffix < new_len.saturating_sub(prefix) {
            let old_index = old_len - suffix - 1;
            let new_index = new_len - suffix - 1;
            let old_child = old_parent.child(old_index).ok_or(())?;
            let new_child = new_parent.child(new_index).ok_or(())?;
            if old_child != new_child {
                break;
            }
            suffix += 1;
        }

        let old_mid_end = old_len - suffix;
        let new_mid_end = new_len - suffix;

        if old_mid_end - prefix == new_mid_end - prefix {
            for offset in 0..(old_mid_end - prefix) {
                let old_index = prefix + offset;
                let new_index = prefix + offset;
                self.reconcile_node(
                    &dom_children[old_index],
                    old_parent.child(old_index).ok_or(())?,
                    old_positions[old_index],
                    new_parent.child(new_index).ok_or(())?,
                    new_positions[new_index],
                    stats,
                )?;
            }
        } else {
            self.replace_child_range(
                content_root,
                new_parent,
                &dom_children,
                &new_positions,
                prefix,
                old_mid_end,
                new_mid_end,
            )?;
            stats.record_structural();
        }

        old_positions = child_positions(old_parent, content_start);
        new_positions = child_positions(new_parent, content_start);
        for offset in 0..suffix {
            let old_index = old_len - suffix + offset;
            let new_index = new_len - suffix + offset;
            self.reconcile_node(
                &dom_children[old_index],
                old_parent.child(old_index).ok_or(())?,
                old_positions[old_index],
                new_parent.child(new_index).ok_or(())?,
                new_positions[new_index],
                stats,
            )?;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_child_range(
        &self,
        content_root: &Element,
        new_parent: &RichNode,
        dom_children: &[Element],
        new_positions: &[usize],
        start: usize,
        old_end: usize,
        new_end: usize,
    ) -> Result<(), ()> {
        let reference = dom_children.get(old_end).map(|el| el.as_ref());
        for old_index in start..old_end {
            let child = dom_children.get(old_index).ok_or(())?;
            let parent = child.parent_node().ok_or(())?;
            parent.remove_child(child.as_ref()).map_err(|_| ())?;
        }

        if start == new_end {
            return Ok(());
        }

        let mut html = String::new();
        for (new_index, new_pos) in new_positions
            .iter()
            .copied()
            .enumerate()
            .take(new_end)
            .skip(start)
        {
            let child = new_parent.child(new_index).ok_or(())?;
            html.push_str(&render_one_node_to_html(self.runtime, child, new_pos));
        }
        insert_html_before(content_root, reference, &html)
    }

    fn refresh_descendant_positions(
        &self,
        dom: &Element,
        node: &RichNode,
        outer_pos: usize,
    ) -> Result<(), ()> {
        update_data_pos(dom, outer_pos)?;
        if node.child_count() == 0 || self.renders_inline_children(node) {
            if self.renders_inline_children(node)
                && node.content().iter().any(|child| !child.is_text())
            {
                let html = render_children_to_html(self.runtime, node, outer_pos + 1);
                set_inner_html(dom, &html).then_some(()).ok_or(())?;
            }
            return Ok(());
        }
        if node.type_name() == "code_block" {
            return Ok(());
        }

        let content_root = self.content_root_for_node(dom, node)?;
        let dom_children = direct_model_children(&content_root)?;
        if dom_children.len() != node.child_count() {
            return Err(());
        }
        let positions = child_positions(node, outer_pos + 1);
        for (index, child_dom) in dom_children.iter().enumerate() {
            self.refresh_descendant_positions(
                child_dom,
                node.child(index).ok_or(())?,
                positions[index],
            )?;
        }
        Ok(())
    }

    fn replace_element(
        &self,
        dom: &Element,
        new_node: &RichNode,
        new_pos: usize,
    ) -> Result<(), ()> {
        let parent = dom.parent_node().ok_or(())?;
        let html = render_one_node_to_html(self.runtime, new_node, new_pos);
        let nodes = parse_html_nodes(dom, &html)?;
        for node in nodes {
            parent
                .insert_before(&node, Some(dom.as_ref()))
                .map_err(|_| ())?;
        }
        parent.remove_child(dom.as_ref()).map_err(|_| ())?;
        Ok(())
    }

    fn content_root_for_node(&self, dom: &Element, node: &RichNode) -> Result<Element, ()> {
        if let Some(spec) = self.runtime.lookup_node_view(node.type_name())
            && let Some(selector) = &spec.content_selector
        {
            return dom.query_selector(selector).map_err(|_| ())?.ok_or(());
        }
        Ok(dom.clone())
    }

    fn attrs_patchable(&self, node: &RichNode) -> bool {
        !node.is_text()
            && (node.type_name() == "task_item"
                || self.runtime.lookup_node_view(node.type_name()).is_some())
    }

    fn renders_inline_children(&self, node: &RichNode) -> bool {
        !node.is_text()
            && node.type_name() != "code_block"
            && self.runtime.lookup_node_view(node.type_name()).is_none()
            && self
                .runtime
                .schema()
                .node_type(node.type_name())
                .is_ok_and(|node_type| node_type.inline_content(self.runtime.schema()))
    }

    fn patch_plain_text_inline_children(
        &self,
        dom: &Element,
        old_node: &RichNode,
        new_node: &RichNode,
    ) -> Result<bool, ()> {
        let Some(_) = single_unmarked_text_child(old_node) else {
            return Ok(false);
        };
        let Some(new_text) = single_unmarked_text_child(new_node) else {
            return Ok(false);
        };
        let children = dom.child_nodes();
        if children.length() != 1 {
            return Ok(false);
        }
        let Some(child) = children.item(0) else {
            return Ok(false);
        };
        if child.node_type() != DomNode::TEXT_NODE {
            return Ok(false);
        }
        child.set_node_value(Some(new_text));
        Ok(true)
    }

    fn wrapper_compatible(&self, old_node: &RichNode, new_node: &RichNode) -> bool {
        if old_node.is_text()
            || new_node.is_text()
            || old_node.type_name() != new_node.type_name()
            || old_node.is_leaf() != new_node.is_leaf()
        {
            return false;
        }

        if let Some(new_spec) = self.runtime.lookup_node_view(new_node.type_name()) {
            let Some(old_spec) = self.runtime.lookup_node_view(old_node.type_name()) else {
                return false;
            };
            return old_spec.tag == new_spec.tag;
        }

        match new_node.type_name() {
            "heading" => heading_level(old_node) == heading_level(new_node),
            "task_item" => true,
            _ => old_node.attrs() == new_node.attrs(),
        }
    }

    fn dom_matches_node(&self, dom: &Element, node: &RichNode) -> bool {
        self.expected_tag(node)
            .is_some_and(|tag| dom.tag_name().eq_ignore_ascii_case(&tag))
    }

    fn expected_tag(&self, node: &RichNode) -> Option<String> {
        if node.is_text() {
            return None;
        }
        if let Some(spec) = self.runtime.lookup_node_view(node.type_name()) {
            return Some(spec.tag.clone());
        }
        Some(match node.type_name() {
            "paragraph" => "p".to_string(),
            "blockquote" => "blockquote".to_string(),
            "bullet_list" | "task_list" => "ul".to_string(),
            "ordered_list" => "ol".to_string(),
            "list_item" | "task_item" => "li".to_string(),
            "heading" => format!("h{}", heading_level(node)),
            "code_block" => "pre".to_string(),
            "horizontal_rule" => "hr".to_string(),
            "hard_break" => "br".to_string(),
            "image" => "img".to_string(),
            _ => "span".to_string(),
        })
    }
}

fn insert_html_before(
    context: &Element,
    reference: Option<&DomNode>,
    html: &str,
) -> Result<(), ()> {
    let parent: &DomNode = context.as_ref();
    let nodes = parse_html_nodes(context, html)?;
    for node in nodes {
        parent.insert_before(&node, reference).map_err(|_| ())?;
    }
    Ok(())
}

fn parse_html_nodes(context: &Element, html: &str) -> Result<Vec<DomNode>, ()> {
    let document = context.owner_document().ok_or(())?;
    let container = document.create_element("div").map_err(|_| ())?;
    set_inner_html(&container, html).then_some(()).ok_or(())?;
    let container_node: &DomNode = container.as_ref();
    let mut nodes = Vec::new();
    while let Some(child) = container_node.first_child() {
        container_node.remove_child(&child).map_err(|_| ())?;
        nodes.push(child);
    }
    Ok(nodes)
}

fn set_inner_html(element: &Element, html: &str) -> bool {
    if let Ok(html_el) = element.clone().dyn_into::<HtmlElement>() {
        html_el.set_inner_html(html);
        true
    } else {
        false
    }
}

fn direct_model_children(root: &Element) -> Result<Vec<Element>, ()> {
    let children = root.child_nodes();
    let mut out = Vec::new();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if child.node_type() == DomNode::TEXT_NODE {
            if child
                .text_content()
                .is_some_and(|text| !text.trim().is_empty())
            {
                return Err(());
            }
            continue;
        }
        if child.node_type() == DomNode::COMMENT_NODE {
            continue;
        }
        let Some(child) = child.dyn_ref::<Element>() else {
            continue;
        };
        if child.has_attribute("data-pos") {
            out.push(child.clone());
        } else {
            return Err(());
        }
    }
    Ok(out)
}

fn child_positions(parent: &RichNode, content_start: usize) -> Vec<usize> {
    let mut pos = content_start;
    let mut positions = Vec::with_capacity(parent.child_count());
    for child in parent.content().iter() {
        positions.push(pos);
        pos += child.node_size();
    }
    positions
}

fn single_unmarked_text_child(node: &RichNode) -> Option<&str> {
    if node.child_count() != 1 {
        return None;
    }
    let child = node.child(0)?;
    if child.is_text() && child.marks().is_empty() {
        child.text()
    } else {
        None
    }
}

fn heading_level(node: &RichNode) -> u64 {
    node.attrs()
        .get("level")
        .and_then(|value| value.as_u64())
        .unwrap_or(1)
        .clamp(1, 6)
}

fn update_data_pos(dom: &Element, pos: usize) -> Result<(), ()> {
    let value = pos.to_string();
    if dom.get_attribute("data-pos").as_deref() == Some(value.as_str()) {
        return Ok(());
    }
    dom.set_attribute("data-pos", &value).map_err(|_| ())
}

fn patch_reflected_attrs(dom: &Element, old_attrs: &Attrs, new_attrs: &Attrs) -> Result<(), ()> {
    for key in old_attrs.keys() {
        if !new_attrs.contains_key(key) {
            dom.remove_attribute(&format!("data-{key}"))
                .map_err(|_| ())?;
        }
    }
    for (key, value) in new_attrs {
        dom.set_attribute(&format!("data-{key}"), &attr_value_to_string(value))
            .map_err(|_| ())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, split_block, toggle_mark};
    use crate::extension::RichTextExtension;
    use crate::model::{Fragment, MarkPolicy, NodeSpec};
    use crate::runtime::RuntimeBuilder;
    use crate::schema_basic;
    use crate::state::{EditorState, EditorStateConfig, Selection};
    use crate::transform::{AttrStep, Step};

    fn state_with_doc(doc: RichNode) -> EditorState {
        EditorState::create(EditorStateConfig::new(schema_basic::schema(), doc)).unwrap()
    }

    fn paragraph_text(value: &str) -> RichNode {
        schema_basic::paragraph(vec![schema_basic::text(value, Vec::new()).unwrap()]).unwrap()
    }

    fn doc(blocks: Vec<RichNode>) -> RichNode {
        schema_basic::doc(blocks).unwrap()
    }

    fn with_selection(state: EditorState, selection: Selection) -> EditorState {
        let mut tr = state.tr();
        tr.set_selection(selection).unwrap();
        state.apply(tr).unwrap()
    }

    fn apply_command(state: EditorState, command: &dyn Command) -> EditorState {
        let tr = command.apply(&state).expect("command applies");
        state.apply(tr).unwrap()
    }

    fn task_doc(checked: bool) -> RichNode {
        let item = schema_basic::task_item(
            checked,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("task", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        schema_basic::doc(vec![schema_basic::task_list(vec![item]).unwrap()]).unwrap()
    }

    struct TitleSchemaExtension;

    impl RichTextExtension for TitleSchemaExtension {
        fn name(&self) -> &str {
            "core_nodes"
        }

        fn nodes(&self) -> Vec<NodeSpec> {
            vec![
                NodeSpec::new("doc").content("title block*"),
                NodeSpec::new("title")
                    .group("title")
                    .content("inline*")
                    .marks(MarkPolicy::None)
                    .defining(),
                NodeSpec::new("paragraph").group("block").content("inline*"),
            ]
        }
    }

    #[test]
    fn task_item_attr_toggle_is_attr_only() {
        let state = state_with_doc(task_doc(false));
        let mut tr = state.tr();
        tr.step(Step::Attr(AttrStep {
            pos: 1,
            attr: "checked".to_string(),
            value: Some(serde_json::json!(true)),
        }))
        .unwrap();
        let next = state.apply(tr).unwrap();

        let runtime = crate::runtime::RuntimeBuilder::new().build();
        let rec = Reconciler::new(&runtime);
        let mut stats = ReconcileStats::default();
        reconcile_children_for_plan(&rec, state.doc(), next.doc(), 0, &mut stats);

        assert_eq!(stats.outcome(), ReconcileOutcome::NodeAttrs { pos: 1 },);
    }

    #[test]
    fn mark_toggle_reconciles_one_subtree_without_full_render() {
        let state = state_with_doc(doc(vec![
            paragraph_text("one two three"),
            paragraph_text("untouched"),
        ]));
        let state = with_selection(state, Selection::text_between(5, 8));

        let next = apply_command(state.clone(), &*toggle_mark(schema_basic::em().unwrap()));

        let runtime = crate::runtime::RuntimeBuilder::new().build();
        let rec = Reconciler::new(&runtime);
        let mut stats = ReconcileStats::default();
        reconcile_children_for_plan(&rec, state.doc(), next.doc(), 0, &mut stats);

        assert_eq!(stats.outcome(), ReconcileOutcome::Reconciled);
    }

    #[test]
    fn plain_text_insert_patches_inline_text_without_structural_html() {
        let state = state_with_doc(doc(vec![paragraph_text("hello")]));
        let state = with_selection(state, Selection::text(6));
        let mut tr = state.tr();
        tr.insert_text("!").unwrap();
        let next = state.apply(tr).unwrap();

        let runtime = crate::runtime::RuntimeBuilder::new().build();
        let rec = Reconciler::new(&runtime);
        let mut stats = ReconcileStats::default();
        reconcile_children_for_plan(&rec, state.doc(), next.doc(), 0, &mut stats);

        assert_eq!(stats.outcome(), ReconcileOutcome::Text);
    }

    #[test]
    fn block_split_at_end_reconciles_middle_insert() {
        let state = state_with_doc(doc(vec![
            paragraph_text("hello"),
            paragraph_text("survives"),
        ]));
        let state = with_selection(state, Selection::text(6));

        let next = apply_command(state.clone(), &*split_block());

        let runtime = crate::runtime::RuntimeBuilder::new().build();
        let rec = Reconciler::new(&runtime);
        let mut stats = ReconcileStats::default();
        reconcile_children_for_plan(&rec, state.doc(), next.doc(), 0, &mut stats);

        assert_eq!(stats.outcome(), ReconcileOutcome::Reconciled);
    }

    #[test]
    fn heading_level_change_replaces_node_not_surface() {
        let state = state_with_doc(doc(vec![
            schema_basic::heading(1, vec![schema_basic::text("Title", Vec::new()).unwrap()])
                .unwrap(),
        ]));
        let mut tr = state.tr();
        tr.step(Step::Attr(AttrStep {
            pos: 0,
            attr: "level".to_string(),
            value: Some(serde_json::json!(2)),
        }))
        .unwrap();
        let next = state.apply(tr).unwrap();

        let runtime = crate::runtime::RuntimeBuilder::new().build();
        let rec = Reconciler::new(&runtime);
        let mut stats = ReconcileStats::default();
        reconcile_children_for_plan(&rec, state.doc(), next.doc(), 0, &mut stats);

        assert_eq!(stats.outcome(), ReconcileOutcome::Reconciled);
    }

    #[test]
    fn custom_inline_textblock_uses_inline_reconcile_path() {
        let runtime = RuntimeBuilder::new().with(TitleSchemaExtension).build();
        let schema = runtime.schema();
        let title = schema
            .node(
                "title",
                Attrs::new(),
                Fragment::from(schema.text("Draft", Vec::new()).unwrap()),
            )
            .unwrap();

        let rec = Reconciler::new(&runtime);

        assert!(
            rec.renders_inline_children(&title),
            "custom title textblock should patch its inline children instead of falling through to DOM-child reconciliation"
        );
    }

    fn reconcile_children_for_plan(
        rec: &Reconciler<'_>,
        old_parent: &RichNode,
        new_parent: &RichNode,
        content_start: usize,
        stats: &mut ReconcileStats,
    ) {
        if old_parent.content() == new_parent.content() {
            return;
        }
        if old_parent.child_count() == 0 || new_parent.child_count() == 0 {
            stats.record_structural();
            return;
        }

        let old_len = old_parent.child_count();
        let new_len = new_parent.child_count();
        let old_positions = child_positions(old_parent, content_start);
        let new_positions = child_positions(new_parent, content_start);

        let mut prefix = 0;
        while prefix < old_len && prefix < new_len {
            let old_child = old_parent.child(prefix).unwrap();
            let new_child = new_parent.child(prefix).unwrap();
            if old_child != new_child {
                break;
            }
            reconcile_node_for_plan(
                rec,
                old_child,
                old_positions[prefix],
                new_child,
                new_positions[prefix],
                stats,
            );
            prefix += 1;
        }

        let mut suffix = 0;
        while suffix < old_len.saturating_sub(prefix) && suffix < new_len.saturating_sub(prefix) {
            let old_index = old_len - suffix - 1;
            let new_index = new_len - suffix - 1;
            let old_child = old_parent.child(old_index).unwrap();
            let new_child = new_parent.child(new_index).unwrap();
            if old_child != new_child {
                break;
            }
            suffix += 1;
        }

        let old_mid_end = old_len - suffix;
        let new_mid_end = new_len - suffix;
        if old_mid_end - prefix == new_mid_end - prefix {
            for offset in 0..(old_mid_end - prefix) {
                let old_index = prefix + offset;
                let new_index = prefix + offset;
                reconcile_node_for_plan(
                    rec,
                    old_parent.child(old_index).unwrap(),
                    old_positions[old_index],
                    new_parent.child(new_index).unwrap(),
                    new_positions[new_index],
                    stats,
                );
            }
        } else {
            stats.record_structural();
        }

        for offset in 0..suffix {
            let old_index = old_len - suffix + offset;
            let new_index = new_len - suffix + offset;
            reconcile_node_for_plan(
                rec,
                old_parent.child(old_index).unwrap(),
                old_positions[old_index],
                new_parent.child(new_index).unwrap(),
                new_positions[new_index],
                stats,
            );
        }
    }

    fn reconcile_node_for_plan(
        rec: &Reconciler<'_>,
        old_node: &RichNode,
        old_pos: usize,
        new_node: &RichNode,
        new_pos: usize,
        stats: &mut ReconcileStats,
    ) {
        if old_node == new_node && old_pos == new_pos {
            return;
        }
        if !rec.wrapper_compatible(old_node, new_node) {
            stats.record_structural();
            return;
        }
        if rec.attrs_patchable(new_node) && old_node.attrs() != new_node.attrs() {
            stats.record_attrs(new_pos);
        }
        if old_node == new_node {
            return;
        }
        if rec.renders_inline_children(new_node) {
            if single_unmarked_text_child(old_node).is_some()
                && single_unmarked_text_child(new_node).is_some()
            {
                stats.record_text();
            } else {
                stats.record_structural();
            }
            return;
        }
        if new_node.type_name() == "code_block" {
            stats.record_structural();
            return;
        }
        reconcile_children_for_plan(rec, old_node, new_node, new_pos + 1, stats);
    }
}
