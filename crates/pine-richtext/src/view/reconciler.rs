//! DOM reconciler: turn an `(old_doc, new_doc)` pair into scoped DOM
//! mutations by walking the model tree and rendered DOM in lockstep.
//!
//! This is intentionally closer to ProseMirror's `ViewDesc` model than
//! the old diff-window patcher. Each model element is addressed by its
//! rendered `data-pos`, children are reconciled inside their owning DOM
//! parent, and node-view hosts keep their component chrome while pine
//! updates host attrs and model-owned content.

use std::cell::RefCell;

use crate::model::Node as RichNode;
use crate::render::{render_children_to_html, render_doc_to_html, render_one_node_plan};
use crate::runtime::EditorRuntime;
use crate::view::node_view_manager::NodeViewManager;

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

/// Reconcile while synchronously notifying the per-editor typed view manager
/// before any owned host leaves the DOM.
pub(crate) fn reconcile_surface_with_manager(
    runtime: &EditorRuntime,
    manager: &RefCell<NodeViewManager>,
    surface: &Element,
    old_doc: &RichNode,
    new_doc: &RichNode,
) -> ReconcileOutcome {
    Reconciler::with_manager(runtime, manager).reconcile_surface(surface, old_doc, new_doc)
}

/// Stateless reconciler scoped to a single [`EditorRuntime`]. The
/// runtime supplies the per-instance node-view registry used by every
/// internal `node_view_*` resolution — same scope rules as
/// [`crate::render::Renderer`].
pub struct Reconciler<'a> {
    runtime: &'a EditorRuntime,
    manager: Option<&'a RefCell<NodeViewManager>>,
}

impl<'a> Reconciler<'a> {
    #[cfg(test)]
    pub fn new(runtime: &'a EditorRuntime) -> Self {
        Self {
            runtime,
            manager: None,
        }
    }

    pub(crate) fn with_manager(
        runtime: &'a EditorRuntime,
        manager: &'a RefCell<NodeViewManager>,
    ) -> Self {
        Self {
            runtime,
            manager: Some(manager),
        }
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
        self.will_remove_subtree(surface);
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
        if !self.dom_matches_node(dom, old_node) || !self.wrapper_compatible(old_node, new_node) {
            self.replace_element(dom, new_node, new_pos)?;
            stats.record_structural();
            return Ok(());
        }

        if old_node == new_node {
            // A change before this subtree shifts every model position by the
            // same delta. Preserve the entire live subtree (including mounted
            // inline components) and update only its position markers.
            if self
                .shift_node_positions(dom, old_node, old_pos, new_pos)
                .is_err()
            {
                // If external DOM mutation made this subtree impossible to
                // address, replace only its nearest model wrapper.
                self.replace_element(dom, new_node, new_pos)?;
                stats.record_structural();
            }
            return Ok(());
        }

        update_data_pos(dom, new_pos)?;
        if self.attrs_patchable(new_node) && old_node.attrs() != new_node.attrs() {
            stats.record_attrs(new_pos);
        }

        if let Some(spec) = self.runtime.lookup_typed_node_view(new_node.type_name())
            && matches!(spec.kind(), crate::view::NodeViewKind::Atom)
        {
            // The manager receives the new semantic snapshot after this DOM
            // pass. Atom descendants are component-owned and opaque here.
            return Ok(());
        }

        if self.renders_inline_children(new_node) {
            let content_root = self.content_root_for_node(dom, new_node)?;
            if self
                .reconcile_inline_children(&content_root, old_node, new_node, new_pos + 1, stats)
                .is_err()
            {
                // Recover a corrupt/mutated textblock at that content root;
                // never bubble a local inline mismatch into a full-surface
                // editor reconstruction.
                let html = render_children_to_html(self.runtime, new_node, new_pos + 1);
                self.will_remove_subtree(&content_root);
                set_inner_html(&content_root, &html)
                    .then_some(())
                    .ok_or(())?;
                stats.record_structural();
            }
            return Ok(());
        }

        if new_node.type_name() == "code_block" {
            self.replace_element(dom, new_node, new_pos)?;
            stats.record_structural();
            return Ok(());
        }

        let content_root = self.content_root_for_node(dom, new_node)?;
        if self
            .reconcile_children(&content_root, old_node, new_node, new_pos + 1, stats)
            .is_err()
        {
            // A mismatch below this node should not escalate into replacing
            // the whole editor surface. Recover at the nearest model-owned
            // wrapper and leave unrelated siblings and node views intact.
            self.replace_element(dom, new_node, new_pos)?;
            stats.record_structural();
        }
        Ok(())
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
            self.will_remove_subtree(content_root);
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

    /// Reconcile one textblock's direct inline children without replacing the
    /// textblock's `innerHTML`. A model inline node always renders as exactly
    /// one direct DOM node: plain text as `Text`, marked text as its outer mark
    /// element, and inline atoms/views as their stable host element.
    fn reconcile_inline_children(
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

        // Empty textblocks use a structural `<br>` placeholder. Crossing the
        // empty boundary is uncommon and necessarily changes that one content
        // root, but never the parent block or editor surface.
        if old_parent.child_count() == 0 || new_parent.child_count() == 0 {
            let html = render_children_to_html(self.runtime, new_parent, content_start);
            self.will_remove_subtree(content_root);
            set_inner_html(content_root, &html)
                .then_some(())
                .ok_or(())?;
            stats.record_structural();
            return Ok(());
        }

        let dom_children = direct_inline_children(content_root)?;
        if dom_children.len() != old_parent.child_count() {
            // `old_parent` is what this reconciler last rendered, so a DOM that
            // no longer matches it means something edited the surface behind the
            // model's back. That is always a bug, and it used to be completely
            // silent: the caller escalates to a full repaint, which paints the
            // model over whatever the user actually typed, and the only trace
            // was an ordinary "patch: full" line behind a debug flag —
            // indistinguishable from a legitimate structural repaint.
            //
            // Say so unconditionally. Drift is rare, so this does not chatter;
            // and when it does fire it names the one thing worth knowing, which
            // is that the DOM and the model have diverged and the user is about
            // to lose whatever the DOM held.
            tracing::warn!(
                target: "pocopine.log",
                dom_children = dom_children.len(),
                model_children = old_parent.child_count(),
                block = old_parent.type_name(),
                "pine-richtext: DOM diverged from the last rendered model; \
                 repainting will discard the DOM-only content"
            );
            return Err(());
        }

        let old_len = old_parent.child_count();
        let new_len = new_parent.child_count();
        let old_positions = child_positions(old_parent, content_start);
        let new_positions = child_positions(new_parent, content_start);

        let mut prefix = 0;
        while prefix < old_len && prefix < new_len {
            let old_child = old_parent.child(prefix).ok_or(())?;
            let new_child = new_parent.child(prefix).ok_or(())?;
            if old_child != new_child {
                break;
            }
            self.sync_unchanged_inline_position(
                &dom_children[prefix],
                old_child,
                old_positions[prefix],
                new_positions[prefix],
            )?;
            prefix += 1;
        }

        let mut suffix = 0;
        while suffix < old_len.saturating_sub(prefix) && suffix < new_len.saturating_sub(prefix) {
            let old_index = old_len - suffix - 1;
            let new_index = new_len - suffix - 1;
            if old_parent.child(old_index).ok_or(())? != new_parent.child(new_index).ok_or(())? {
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
                self.reconcile_inline_node(
                    content_root,
                    &dom_children[old_index],
                    old_parent.child(old_index).ok_or(())?,
                    old_positions[old_index],
                    new_parent.child(new_index).ok_or(())?,
                    new_positions[new_index],
                    stats,
                )?;
            }
        } else {
            self.replace_inline_child_range(
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

        for offset in 0..suffix {
            let old_index = old_len - suffix + offset;
            let new_index = new_len - suffix + offset;
            self.sync_unchanged_inline_position(
                &dom_children[old_index],
                old_parent.child(old_index).ok_or(())?,
                old_positions[old_index],
                new_positions[new_index],
            )?;
        }
        Ok(())
    }

    /// Shift framework position markers for an unchanged model subtree while
    /// preserving every DOM node. Traversal follows the model-owned content
    /// outlet instead of querying arbitrary `[data-pos]` descendants, so
    /// component chrome or a nested editor can never be rewritten by accident.
    fn shift_node_positions(
        &self,
        dom: &Element,
        node: &RichNode,
        old_pos: usize,
        new_pos: usize,
    ) -> Result<(), ()> {
        update_data_pos(dom, new_pos)?;
        if old_pos == new_pos || node.child_count() == 0 || node.type_name() == "code_block" {
            return Ok(());
        }
        if self
            .runtime
            .lookup_typed_node_view(node.type_name())
            .is_some_and(|view| matches!(view.kind(), crate::view::NodeViewKind::Atom))
        {
            return Ok(());
        }

        let content_root = self.content_root_for_node(dom, node)?;
        let old_positions = child_positions(node, old_pos + 1);
        let new_positions = child_positions(node, new_pos + 1);
        if self.renders_inline_children(node) {
            let children = direct_inline_children(&content_root)?;
            if children.len() != node.child_count() {
                return Err(());
            }
            for (index, child_dom) in children.iter().enumerate() {
                let child = node.child(index).ok_or(())?;
                if child.is_text() {
                    continue;
                }
                let child_dom = child_dom.dyn_ref::<Element>().ok_or(())?;
                self.shift_node_positions(
                    child_dom,
                    child,
                    old_positions[index],
                    new_positions[index],
                )?;
            }
            return Ok(());
        }

        let children = direct_model_children(&content_root)?;
        if children.len() != node.child_count() {
            return Err(());
        }
        for (index, child_dom) in children.iter().enumerate() {
            self.shift_node_positions(
                child_dom,
                node.child(index).ok_or(())?,
                old_positions[index],
                new_positions[index],
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn reconcile_inline_node(
        &self,
        content_root: &Element,
        dom: &DomNode,
        old_node: &RichNode,
        old_pos: usize,
        new_node: &RichNode,
        new_pos: usize,
        stats: &mut ReconcileStats,
    ) -> Result<(), ()> {
        if old_node.is_text() && new_node.is_text() && old_node.marks() == new_node.marks() {
            let old_text = old_node.text().ok_or(())?;
            let new_text = new_node.text().ok_or(())?;
            if old_text != new_text {
                patch_inline_text(dom, old_text, new_text)?;
                stats.record_text();
            }
            return Ok(());
        }

        if !old_node.is_text()
            && !new_node.is_text()
            && let Some(element) = dom.dyn_ref::<Element>()
        {
            return self.reconcile_node(element, old_node, old_pos, new_node, new_pos, stats);
        }

        self.replace_inline_dom_node(content_root, dom, new_node, new_pos)?;
        stats.record_structural();
        Ok(())
    }

    fn sync_unchanged_inline_position(
        &self,
        dom: &DomNode,
        node: &RichNode,
        old_pos: usize,
        new_pos: usize,
    ) -> Result<(), ()> {
        if node.is_text() || old_pos == new_pos {
            return Ok(());
        }
        let element = dom.dyn_ref::<Element>().ok_or(())?;
        self.shift_node_positions(element, node, old_pos, new_pos)
    }

    #[allow(clippy::too_many_arguments)]
    fn replace_inline_child_range(
        &self,
        content_root: &Element,
        new_parent: &RichNode,
        dom_children: &[DomNode],
        new_positions: &[usize],
        start: usize,
        old_end: usize,
        new_end: usize,
    ) -> Result<(), ()> {
        let reference = dom_children.get(old_end);
        for child in dom_children.iter().take(old_end).skip(start) {
            if let Some(element) = child.dyn_ref::<Element>() {
                self.will_remove_subtree(element);
            }
            content_root.remove_child(child).map_err(|_| ())?;
        }
        for (new_index, new_pos) in new_positions
            .iter()
            .copied()
            .enumerate()
            .take(new_end)
            .skip(start)
        {
            let child = new_parent.child(new_index).ok_or(())?;
            for node in self.dom_nodes_for_model_node(content_root, child, new_pos)? {
                content_root
                    .insert_before(node.as_ref(), reference)
                    .map_err(|_| ())?;
            }
        }
        Ok(())
    }

    fn replace_inline_dom_node(
        &self,
        content_root: &Element,
        dom: &DomNode,
        new_node: &RichNode,
        new_pos: usize,
    ) -> Result<(), ()> {
        for node in self.dom_nodes_for_model_node(content_root, new_node, new_pos)? {
            content_root
                .insert_before(node.as_ref(), Some(dom))
                .map_err(|_| ())?;
        }
        if let Some(element) = dom.dyn_ref::<Element>() {
            self.will_remove_subtree(element);
        }
        content_root.remove_child(dom).map_err(|_| ())?;
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
            self.will_remove_subtree(child);
            parent.remove_child(child.as_ref()).map_err(|_| ())?;
        }

        if start == new_end {
            return Ok(());
        }

        for (new_index, new_pos) in new_positions
            .iter()
            .copied()
            .enumerate()
            .take(new_end)
            .skip(start)
        {
            let child = new_parent.child(new_index).ok_or(())?;
            for node in self.dom_nodes_for_model_node(content_root, child, new_pos)? {
                content_root
                    .insert_before(node.as_ref(), reference)
                    .map_err(|_| ())?;
            }
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
        let nodes = self.dom_nodes_for_model_node(dom, new_node, new_pos)?;
        for node in nodes {
            parent
                .insert_before(&node, Some(dom.as_ref()))
                .map_err(|_| ())?;
        }
        self.will_remove_subtree(dom);
        parent.remove_child(dom.as_ref()).map_err(|_| ())?;
        Ok(())
    }

    fn dom_nodes_for_model_node(
        &self,
        context: &Element,
        node: &RichNode,
        position: usize,
    ) -> Result<Vec<DomNode>, ()> {
        let document = context.owner_document().ok_or(())?;
        let plan = render_one_node_plan(self.runtime, node, position);
        let node = plan.materialize(&document).map_err(|_| ())?;
        Ok(vec![node])
    }

    fn will_remove_subtree(&self, root: &Element) {
        if let Some(manager) = self.manager {
            manager.borrow_mut().will_remove_subtree(root);
        }
    }

    fn content_root_for_node(&self, dom: &Element, node: &RichNode) -> Result<Element, ()> {
        if self
            .runtime
            .lookup_typed_node_view(node.type_name())
            .is_some()
        {
            return self
                .manager
                .and_then(|manager| manager.borrow().content_outlet(dom))
                .ok_or(());
        }
        if let Some(spec) = self.runtime.lookup_dom_view(node.type_name()) {
            let mut current = dom.clone();
            if let Some(path) = spec.content_hole_path() {
                for index in path {
                    current = current.children().item(u32::from(index)).ok_or(())?;
                }
            }
            return Ok(current);
        }
        Ok(dom.clone())
    }

    fn attrs_patchable(&self, node: &RichNode) -> bool {
        !node.is_text()
            && self
                .runtime
                .lookup_typed_node_view(node.type_name())
                .is_some()
    }

    fn renders_inline_children(&self, node: &RichNode) -> bool {
        !node.is_text()
            && node.type_name() != "code_block"
            && self
                .runtime
                .schema()
                .node_type(node.type_name())
                .is_ok_and(|node_type| node_type.inline_content(self.runtime.schema()))
    }

    fn wrapper_compatible(&self, old_node: &RichNode, new_node: &RichNode) -> bool {
        if old_node.is_text()
            || new_node.is_text()
            || old_node.type_name() != new_node.type_name()
            || old_node.is_leaf() != new_node.is_leaf()
        {
            return false;
        }

        if let Some(new_spec) = self.runtime.lookup_typed_node_view(new_node.type_name()) {
            let Some(old_spec) = self.runtime.lookup_typed_node_view(old_node.type_name()) else {
                return false;
            };
            return old_spec.component_type_id() == new_spec.component_type_id()
                && old_spec.kind() == new_spec.kind();
        }

        if self.runtime.lookup_dom_view(new_node.type_name()).is_some() {
            return old_node.attrs() == new_node.attrs();
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
        if let Some(view) = self.runtime.lookup_typed_node_view(node.type_name()) {
            if view.host() == crate::view::NodeViewHost::Native {
                return self
                    .runtime
                    .lookup_dom_view(node.type_name())
                    .map(|spec| spec.root_tag().to_string());
            }
            let inline = self
                .runtime
                .lookup_typed_node(node.type_name())
                .is_some_and(|typed| typed.spec().is_inline());
            return Some(if inline { "span" } else { "div" }.to_string());
        }
        if let Some(spec) = self.runtime.lookup_dom_view(node.type_name()) {
            return Some(spec.root_tag().to_string());
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

pub(crate) fn parse_html_nodes(context: &Element, html: &str) -> Result<Vec<DomNode>, ()> {
    let document = context.owner_document().ok_or(())?;
    let range = document.create_range().map_err(|_| ())?;
    range
        .select_node_contents(context.as_ref())
        .map_err(|_| ())?;
    let fragment = range.create_contextual_fragment(html).map_err(|_| ())?;
    let fragment_node: &DomNode = fragment.as_ref();
    let mut nodes = Vec::new();
    while let Some(child) = fragment_node.first_child() {
        fragment_node.remove_child(&child).map_err(|_| ())?;
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

fn direct_inline_children(root: &Element) -> Result<Vec<DomNode>, ()> {
    let children = root.child_nodes();
    let mut out = Vec::new();
    for index in 0..children.length() {
        let child = children.item(index).ok_or(())?;
        match child.node_type() {
            DomNode::TEXT_NODE | DomNode::ELEMENT_NODE => out.push(child),
            DomNode::COMMENT_NODE => {}
            _ => return Err(()),
        }
    }
    Ok(out)
}

/// Patch the sole text leaf represented by one model text node. Marked text
/// has a one-child wrapper chain (`<em><strong>text</strong></em>`), while
/// unmarked text starts at the `Text` node itself.
fn patch_inline_text(dom: &DomNode, old_text: &str, new_text: &str) -> Result<(), ()> {
    let mut leaf = dom.clone();
    loop {
        if leaf.node_type() == DomNode::TEXT_NODE {
            break;
        }
        if leaf.node_type() != DomNode::ELEMENT_NODE || leaf.child_nodes().length() != 1 {
            return Err(());
        }
        leaf = leaf.first_child().ok_or(())?;
    }

    // Splice only the changed span instead of rewriting the whole text node.
    // `text_splice` operates on chars, so surrogate pairs are never split.
    if let Some(character_data) = leaf.dyn_ref::<web_sys::CharacterData>() {
        let (offset, count, replacement) = crate::text_diff::text_splice(old_text, new_text);
        if character_data
            .replace_data(offset, count, &replacement)
            .is_ok()
        {
            return Ok(());
        }
    }
    leaf.set_node_value(Some(new_text));
    Ok(())
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

#[cfg(test)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command, split_block, toggle_mark};
    use crate::extension::RichTextExtension;
    use crate::model::{Attrs, Fragment, MarkPolicy, NodeSpec};
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
    fn task_item_native_dom_attr_toggle_reconciles_its_wrapper() {
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

        // The model-only runtime owns a native `<li data-checked>` projection,
        // so changing an attr may replace that one structural wrapper. A
        // component-backed task runtime takes the retained NodeAttrs path and
        // delivers the new typed attrs through `sync_node`.
        assert_eq!(stats.outcome(), ReconcileOutcome::Reconciled);
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
