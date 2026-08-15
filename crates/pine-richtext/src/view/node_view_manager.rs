//! Per-editor ownership and lifecycle for typed component node views.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use web_sys::{Element, Event};

use crate::model::Node;
use crate::runtime::EditorRuntime;
use crate::state::Selection;

use super::event_boundary::{self, BoundaryHost, BoundaryHostKind, NodeViewEventBoundary};
use super::node_view_handle::{ErasedNodeViewHandle, NodeViewEditorBinding};
use super::typed_node_views::{
    ErasedNodeViewUpdate, MountedNodeView, NodeViewError, NodeViewSelection,
};

const INSTANCE_ATTR: &str = "data-pine-node-view-id";
const TYPE_ATTR: &str = "data-pine-node-type";
const ERROR_ATTR: &str = "data-pine-node-view-error";
const SYNC_ERROR_ATTR: &str = "data-pine-node-view-sync-error";

struct MountedInstance {
    host: Element,
    node_type: String,
    position: usize,
    node_size: usize,
    content_outlet: Option<Element>,
    last_update: ErasedNodeViewUpdate,
    mounted: Box<dyn MountedNodeView>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelectionSyncOutcome {
    pub considered: usize,
    pub synced: usize,
    pub global: bool,
}

/// Owns all typed component views mounted under one editor surface.
///
/// The manager is deliberately not stored on [`EditorRuntime`]: runtimes are
/// immutable and shareable, while component receipts, positions, selection
/// relations, and teardown belong to one concrete editor DOM tree.
pub(crate) struct NodeViewManager {
    runtime: Arc<EditorRuntime>,
    next_id: Cell<u64>,
    instances: HashMap<u64, MountedInstance>,
    /// Current model position -> mounted instance. Selection-only updates use
    /// range queries over this index instead of walking the DOM or every view.
    positions: BTreeMap<usize, u64>,
    last_selection: Option<Selection>,
    last_editable: Option<bool>,
    last_editor_focused: Option<bool>,
    binding: Option<Rc<NodeViewEditorBinding>>,
}

impl NodeViewManager {
    pub(crate) fn new(runtime: Arc<EditorRuntime>) -> Self {
        Self {
            runtime,
            next_id: Cell::new(1),
            instances: HashMap::new(),
            positions: BTreeMap::new(),
            last_selection: None,
            last_editable: None,
            last_editor_focused: None,
            binding: None,
        }
    }

    pub(crate) fn set_binding(&mut self, binding: Rc<NodeViewEditorBinding>) {
        self.binding = Some(binding);
    }

    pub(crate) fn has_view(&self, node_type: &str) -> bool {
        self.runtime.lookup_typed_node_view(node_type).is_some()
    }

    pub(crate) fn mount(
        &mut self,
        host: &Element,
        node: &Node,
        position: usize,
        selection: &Selection,
        editable: bool,
        editor_focused: bool,
    ) -> Result<(), NodeViewError> {
        let Some(spec) = self.runtime.lookup_typed_node_view(node.type_name()) else {
            return Ok(());
        };
        if host.has_attribute(INSTANCE_ATTR) {
            return Err(NodeViewError::Invariant {
                node_type: spec.node_type().to_string(),
                message: "host already owns a typed node-view instance".to_string(),
            });
        }

        let relation = selection_relation(selection, node, position);
        let update = ErasedNodeViewUpdate::from_node(node, relation, editable, editor_focused);
        // Prove the editor-side command binding before touching the host. An
        // invariant failure here must leave the empty rendered host eligible
        // for a later valid mount, with no forged instance/type stamps.
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: "editor binding was not installed before node-view mount".to_string(),
            })?;
        let id = self.next_id.get();
        self.next_id.set(id.wrapping_add(1).max(1));
        host.set_attribute(INSTANCE_ATTR, &id.to_string())
            .map_err(|_| NodeViewError::Invariant {
                node_type: spec.node_type().to_string(),
                message: "could not stamp the node-view instance id".to_string(),
            })?;
        host.set_attribute(TYPE_ATTR, spec.node_type())
            .map_err(|_| NodeViewError::Invariant {
                node_type: spec.node_type().to_string(),
                message: "could not stamp the semantic node type".to_string(),
            })?;
        if matches!(spec.kind(), super::typed_node_views::NodeViewKind::Atom) {
            let _ = host.set_attribute("contenteditable", "false");
        }

        let handle = ErasedNodeViewHandle::new(
            binding,
            host.clone(),
            id,
            spec.semantic_type_id(),
            spec.node_type(),
        );
        match spec.mount_component(host, update.clone(), handle) {
            Ok(mounted) => {
                let content_outlet = match spec.resolve_content_outlet(host) {
                    Ok(outlet) => outlet,
                    Err(error) => {
                        mounted.unmount();
                        clear_instance_stamps(host);
                        render_failure(&self.runtime, host, node, position, &error);
                        return Err(error);
                    }
                };
                if let Some(outlet) = &content_outlet {
                    let materialized =
                        (|| {
                            if outlet.has_child_nodes() {
                                return Err(NodeViewError::Invariant {
                                node_type: node.type_name().to_string(),
                                message:
                                    "compiled owned-content outlet was not empty at initial mount"
                                        .to_string(),
                            });
                            }
                            let html = crate::render::render_children_to_html(
                                &self.runtime,
                                node,
                                position.saturating_add(1),
                            );
                            let children = super::reconciler::parse_html_nodes(outlet, &html)
                                .map_err(|_| NodeViewError::Invariant {
                                    node_type: node.type_name().to_string(),
                                    message:
                                        "could not materialize semantic children in outlet context"
                                            .to_string(),
                                })?;
                            for child in children {
                                outlet.append_child(child.as_ref()).map_err(|_| {
                                    NodeViewError::Invariant {
                                        node_type: node.type_name().to_string(),
                                        message: "could not append semantic child to owned outlet"
                                            .to_string(),
                                    }
                                })?;
                            }
                            Ok(())
                        })();
                    if let Err(error) = materialized {
                        mounted.unmount();
                        clear_instance_stamps(host);
                        render_failure(&self.runtime, host, node, position, &error);
                        return Err(error);
                    }
                }
                self.instances.insert(
                    id,
                    MountedInstance {
                        host: host.clone(),
                        node_type: node.type_name().to_string(),
                        position,
                        node_size: node.node_size(),
                        content_outlet,
                        last_update: update,
                        mounted,
                    },
                );
                self.positions.insert(position, id);
                Ok(())
            }
            Err(error) => {
                clear_instance_stamps(host);
                render_failure(&self.runtime, host, node, position, &error);
                Err(error)
            }
        }
    }

    /// Walk the model-owned DOM in position order and mount/sync the exact
    /// typed hosts produced by the structured renderer. This never searches
    /// by component tag and never mounts an unrelated application component.
    pub(crate) fn sync_tree(
        &mut self,
        surface: &Element,
        doc: &Node,
        selection: &Selection,
        editable: bool,
        editor_focused: bool,
    ) {
        self.sync_children(surface, doc, 0, selection, editable, editor_focused);
        self.last_selection = Some(selection.clone());
        self.last_editable = Some(editable);
        self.last_editor_focused = Some(editor_focused);
    }

    fn sync_children(
        &mut self,
        dom_parent: &Element,
        model_parent: &Node,
        content_start: usize,
        selection: &Selection,
        editable: bool,
        editor_focused: bool,
    ) {
        // A diagnostic belongs to one attempted model/DOM walk. Clear a stale
        // marker when that parent is visited again; any still-broken branch
        // below deterministically writes it back during this pass.
        let _ = dom_parent.remove_attribute(SYNC_ERROR_ATTR);
        let positioned = positioned_children(dom_parent);
        let mut position = content_start;
        for child in model_parent.content().iter() {
            if child.is_text() {
                position = position.saturating_add(child.node_size());
                continue;
            }
            let Some(host) = positioned
                .iter()
                .find(|element| data_position(element) == Some(position))
                .cloned()
            else {
                if self.subtree_has_view(child) {
                    report_sync_invariant(
                        dom_parent,
                        child.type_name(),
                        position,
                        "expected a direct model host with the matching `data-pos`, but none was found",
                    );
                }
                position = position.saturating_add(child.node_size());
                continue;
            };
            let _ = host.remove_attribute(SYNC_ERROR_ATTR);

            if self.has_view(child.type_name()) {
                let mut view_ready = !host.has_attribute(ERROR_ATTR);
                if !host.has_attribute(ERROR_ATTR) {
                    let result = if host.has_attribute(INSTANCE_ATTR) {
                        self.retained(&host, child, position, selection, editable, editor_focused)
                    } else {
                        self.mount(&host, child, position, selection, editable, editor_focused)
                    };
                    if let Err(error) = result {
                        view_ready = false;
                        report_sync_error(&host, child.type_name(), position, &error);
                    }
                }
                if view_ready {
                    match self.checked_content_outlet(&host, child) {
                        Ok(Some(outlet)) => self.sync_children(
                            &outlet,
                            child,
                            position.saturating_add(1),
                            selection,
                            editable,
                            editor_focused,
                        ),
                        Ok(None) => {}
                        Err(error) => {
                            report_sync_error(&host, child.type_name(), position, &error);
                        }
                    }
                }
                position = position.saturating_add(child.node_size());
                continue;
            }

            if child.child_count() > 0 && child.type_name() != "code_block" {
                let content_root = if let Some(spec) =
                    self.runtime.lookup_dom_view(child.type_name())
                {
                    let mut current = host.clone();
                    if let Some(path) = spec.content_hole_path() {
                        match resolve_element_path(&current, &path) {
                            Ok(resolved) => current = resolved,
                            Err((depth, index)) => {
                                if self.subtree_has_view(child) {
                                    report_sync_invariant(
                                        &host,
                                        child.type_name(),
                                        position,
                                        &format!(
                                            "native content-hole path is missing child index {index} at depth {depth}"
                                        ),
                                    );
                                }
                                position = position.saturating_add(child.node_size());
                                continue;
                            }
                        }
                    }
                    current
                } else {
                    host.clone()
                };
                self.sync_children(
                    &content_root,
                    child,
                    position.saturating_add(1),
                    selection,
                    editable,
                    editor_focused,
                );
            }
            position = position.saturating_add(child.node_size());
        }
    }

    fn subtree_has_view(&self, node: &Node) -> bool {
        self.has_view(node.type_name())
            || node
                .content()
                .iter()
                .any(|child| self.subtree_has_view(child))
    }

    fn checked_content_outlet(
        &self,
        host: &Element,
        node: &Node,
    ) -> Result<Option<Element>, NodeViewError> {
        let view = self
            .runtime
            .lookup_typed_node_view(node.type_name())
            .ok_or_else(|| NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: "manager attempted to resolve an unregistered typed view".to_string(),
            })?;
        let id = instance_id(host).ok_or_else(|| NodeViewError::Invariant {
            node_type: node.type_name().to_string(),
            message: "mounted typed host lost its manager instance id".to_string(),
        })?;
        let instance = self
            .instances
            .get(&id)
            .ok_or_else(|| NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: format!("host references missing manager instance {id}"),
            })?;
        if instance.host != *host {
            return Err(NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: format!("manager instance {id} belongs to a different host"),
            });
        }

        match view.kind() {
            super::typed_node_views::NodeViewKind::Atom => {
                if instance.content_outlet.is_some() {
                    return Err(NodeViewError::Invariant {
                        node_type: node.type_name().to_string(),
                        message: "atomic view unexpectedly owns a content outlet".to_string(),
                    });
                }
                Ok(None)
            }
            super::typed_node_views::NodeViewKind::Editable => {
                let outlet =
                    instance
                        .content_outlet
                        .clone()
                        .ok_or_else(|| NodeViewError::Invariant {
                            node_type: node.type_name().to_string(),
                            message: "editable view has no compiled owned-content outlet"
                                .to_string(),
                        })?;
                if outlet != *host && !host.contains(Some(outlet.as_ref())) {
                    return Err(NodeViewError::Invariant {
                        node_type: node.type_name().to_string(),
                        message: "editable view's compiled owned-content outlet is detached from its host"
                            .to_string(),
                    });
                }
                Ok(Some(outlet))
            }
        }
    }

    pub(crate) fn retained(
        &mut self,
        host: &Element,
        node: &Node,
        position: usize,
        selection: &Selection,
        editable: bool,
        editor_focused: bool,
    ) -> Result<(), NodeViewError> {
        let id = instance_id(host).ok_or_else(|| NodeViewError::Invariant {
            node_type: node.type_name().to_string(),
            message: "retained typed host has no manager instance id".to_string(),
        })?;
        let valid = self.instances.get(&id).is_some_and(|instance| {
            instance.node_type == node.type_name() && instance.mounted.is_active()
        });
        if !valid {
            return Err(NodeViewError::Stale {
                node_type: node.type_name().to_string(),
            });
        }

        sync_semantic_host_attrs(&self.runtime, host, node)?;

        let next = ErasedNodeViewUpdate::from_node(
            node,
            selection_relation(selection, node, position),
            editable,
            editor_focused,
        );
        let (previous_position, sync_error) = {
            let instance = self.instances.get_mut(&id).expect("validated instance");
            let previous_position = instance.position;
            instance.position = position;
            instance.node_size = node.node_size();
            let sync_error = if next == instance.last_update {
                None
            } else {
                match instance.mounted.sync(next.clone()) {
                    Ok(()) => {
                        instance.last_update = next;
                        None
                    }
                    Err(error) => Some(error),
                }
            };
            (previous_position, sync_error)
        };
        self.reindex(id, previous_position, position);
        let Some(error) = sync_error else {
            return Ok(());
        };
        let failed = self.take_instance(id).expect("instance still present");
        self.will_remove_subtree(&failed.host);
        failed.mounted.unmount();
        clear_instance_stamps(&failed.host);
        render_failure(&self.runtime, &failed.host, node, position, &error);
        Err(error)
    }

    fn reindex(&mut self, id: u64, previous_position: usize, position: usize) {
        if previous_position != position && self.positions.get(&previous_position) == Some(&id) {
            self.positions.remove(&previous_position);
        }
        self.positions.insert(position, id);
    }

    fn take_instance(&mut self, id: u64) -> Option<MountedInstance> {
        let instance = self.instances.remove(&id)?;
        if self.positions.get(&instance.position) == Some(&id) {
            self.positions.remove(&instance.position);
        }
        Some(instance)
    }

    /// Update view snapshots for a selection/focus/editability-only change.
    ///
    /// When focus/editability are unchanged, only positions affected by the
    /// previous or next selection are resolved through the position index.
    /// Structural document changes continue to use [`Self::sync_tree`].
    pub(crate) fn sync_selection(
        &mut self,
        doc: &Node,
        selection: &Selection,
        editable: bool,
        editor_focused: bool,
    ) -> SelectionSyncOutcome {
        let global = self.last_editable != Some(editable)
            || self.last_editor_focused != Some(editor_focused);
        let affected = if global || self.last_selection.is_none() {
            self.positions.keys().copied().collect::<BTreeSet<_>>()
        } else {
            selection_transition_positions(
                &self.positions,
                doc,
                self.runtime.schema(),
                self.last_selection.as_ref(),
                selection,
            )
        };

        self.last_selection = Some(selection.clone());
        self.last_editable = Some(editable);
        self.last_editor_focused = Some(editor_focused);

        let indexed = affected
            .into_iter()
            .filter_map(|position| {
                self.positions
                    .get(&position)
                    .copied()
                    .map(|id| (position, id))
            })
            .collect::<Vec<_>>();
        let mut outcome = SelectionSyncOutcome {
            considered: indexed.len(),
            synced: 0,
            global,
        };

        for (position, id) in indexed {
            let sync_error = {
                let Some(instance) = self.instances.get_mut(&id) else {
                    continue;
                };
                if !instance.mounted.is_active() {
                    continue;
                }
                let mut next = instance.last_update.clone();
                next.selection =
                    selection_relation_for_size(selection, instance.node_size, instance.position);
                next.editable = editable;
                next.editor_focused = editor_focused;
                if next == instance.last_update {
                    continue;
                }
                match instance.mounted.sync(next.clone()) {
                    Ok(()) => {
                        instance.last_update = next;
                        outcome.synced += 1;
                        None
                    }
                    Err(error) => Some(error),
                }
            };

            let Some(error) = sync_error else {
                continue;
            };
            let Some(failed) = self.take_instance(id) else {
                continue;
            };
            self.will_remove_subtree(&failed.host);
            failed.mounted.unmount();
            clear_instance_stamps(&failed.host);
            if let Ok(Some(node)) = doc.node_at(position) {
                render_failure(&self.runtime, &failed.host, node, position, &error);
            } else {
                let _ = failed.host.set_attribute(ERROR_ATTR, &error.to_string());
            }
            tracing::error!(
                target: "pocopine.log",
                node_type = failed.node_type,
                position,
                %error,
                "typed rich-text node view rejected selection/focus update"
            );
        }

        outcome
    }

    pub(crate) fn content_outlet(&self, host: &Element) -> Option<Element> {
        let id = instance_id(host)?;
        self.instances.get(&id)?.content_outlet.clone()
    }

    /// Classify a browser event against the nearest manager-stamped typed
    /// node-view host in its composed path.
    pub(crate) fn event_boundary(&self, event: &Event) -> NodeViewEventBoundary {
        event_boundary::classify_event(
            event,
            |host| self.boundary_host(host),
            |instance_id| {
                self.instances
                    .get(&instance_id)
                    .and_then(|instance| instance.content_outlet.clone())
            },
        )
    }

    fn boundary_host(&self, host: &Element) -> Option<BoundaryHost> {
        let instance_id = instance_id(host)?;
        if let Some(instance) = self.instances.get(&instance_id) {
            return Some(BoundaryHost {
                instance_id,
                position: Some(instance.position),
                kind: if instance.content_outlet.is_some() {
                    BoundaryHostKind::Editable
                } else {
                    BoundaryHostKind::Atom
                },
            });
        }

        // A stale stamp must fail closed as component chrome. It cannot prove
        // an owned outlet or a generation-current model position, so it may
        // suppress editor input but must not select whatever later node happens
        // to occupy the old `data-pos`.
        Some(BoundaryHost {
            instance_id,
            position: None,
            kind: if host.get_attribute("contenteditable").as_deref() == Some("false") {
                BoundaryHostKind::Atom
            } else {
                BoundaryHostKind::Editable
            },
        })
    }

    /// Release every manager-owned host contained by `root`, deepest first.
    /// This is used before a native subtree replacement; it does not discover
    /// mounts by component tag, only manager-issued instance ids.
    pub(crate) fn will_remove_subtree(&mut self, root: &Element) {
        let mut ids = self
            .instances
            .iter()
            .filter_map(|(id, instance)| {
                (root == &instance.host || root.contains(Some(instance.host.as_ref())))
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        ids.sort_unstable_by_key(|id| {
            std::cmp::Reverse(self.instances.get(id).map_or(0, |view| view.position))
        });
        for id in ids {
            if let Some(instance) = self.take_instance(id) {
                instance.mounted.unmount();
                clear_instance_stamps(&instance.host);
            }
        }
    }

    pub(crate) fn drain(&mut self) {
        let mut instances = std::mem::take(&mut self.instances)
            .into_values()
            .collect::<Vec<_>>();
        self.positions.clear();
        self.last_selection = None;
        self.last_editable = None;
        self.last_editor_focused = None;
        instances.sort_unstable_by_key(|view| std::cmp::Reverse(view.position));
        for instance in instances {
            instance.mounted.unmount();
            clear_instance_stamps(&instance.host);
        }
    }
}

impl Drop for NodeViewManager {
    fn drop(&mut self) {
        self.drain();
    }
}

fn instance_id(host: &Element) -> Option<u64> {
    host.get_attribute(INSTANCE_ATTR)?.parse().ok()
}

fn data_position(host: &Element) -> Option<usize> {
    host.get_attribute("data-pos")?.parse().ok()
}

fn positioned_children(root: &Element) -> Vec<Element> {
    let children = root.children();
    let mut positioned = Vec::new();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if child.has_attribute("data-pos") {
            positioned.push(child);
        }
    }
    positioned
}

fn resolve_element_path(root: &Element, path: &[u16]) -> Result<Element, (usize, u16)> {
    let mut current = root.clone();
    for (depth, index) in path.iter().copied().enumerate() {
        let Some(next) = current.children().item(u32::from(index)) else {
            return Err((depth, index));
        };
        current = next;
    }
    Ok(current)
}

fn report_sync_invariant(target: &Element, node_type: &str, position: usize, message: &str) {
    let error = sync_invariant_error(node_type, position, message);
    report_sync_error(target, node_type, position, &error);
}

fn sync_invariant_error(node_type: &str, position: usize, message: &str) -> NodeViewError {
    NodeViewError::Invariant {
        node_type: node_type.to_string(),
        message: format!("model position {position}: {message}"),
    }
}

fn report_sync_error(target: &Element, node_type: &str, position: usize, error: &NodeViewError) {
    let diagnostic = error.to_string();
    let _ = target.set_attribute(SYNC_ERROR_ATTR, &diagnostic);
    tracing::error!(
        target: "pocopine.log",
        node_type,
        position,
        %error,
        "typed rich-text node-view model/DOM sync invariant failed"
    );
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&diagnostic));
}

fn clear_instance_stamps(host: &Element) {
    let _ = host.remove_attribute(INSTANCE_ATTR);
    // `data-pine-node-type` describes the semantic host, not the mounted
    // component receipt. Keep it on production fallbacks so diagnostics, CSS,
    // and accessibility tooling can still identify the preserved node.
}

fn sync_semantic_host_attrs(
    runtime: &EditorRuntime,
    host: &Element,
    node: &Node,
) -> Result<(), NodeViewError> {
    if runtime
        .lookup_typed_node_view(node.type_name())
        .is_none_or(|view| view.host() != super::NodeViewHost::Native)
    {
        return Ok(());
    }
    let Some(spec) = runtime.lookup_dom_view(node.type_name()) else {
        return Ok(());
    };
    for name in spec.root_binding_destinations() {
        host.remove_attribute(name)
            .map_err(|error| NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: format!("could not clear projected host attr `{name}`: {error:?}"),
            })?;
    }
    let plan = spec
        .root_element_spec(node)
        .map_err(|error| NodeViewError::Invariant {
            node_type: node.type_name().to_string(),
            message: format!("could not project semantic host attrs: {error}"),
        })?;
    for (name, value) in plan.attrs() {
        host.set_attribute(name, value)
            .map_err(|error| NodeViewError::Invariant {
                node_type: node.type_name().to_string(),
                message: format!("could not set projected host attr `{name}`: {error:?}"),
            })?;
    }
    Ok(())
}

fn render_failure(
    runtime: &EditorRuntime,
    host: &Element,
    node: &Node,
    position: usize,
    error: &NodeViewError,
) {
    while let Some(child) = host.first_child() {
        let _ = host.remove_child(child.as_ref());
    }
    let mut fallback = String::new();
    if let Some(spec) = runtime.lookup_dom_view(node.type_name()) {
        let content =
            crate::render::render_children_to_html(runtime, node, position.saturating_add(1));
        let uses_native_host = runtime
            .lookup_typed_node_view(node.type_name())
            .is_some_and(|view| view.host() == super::NodeViewHost::Native);
        if uses_native_host {
            let _ = sync_semantic_host_attrs(runtime, host, node);
            let _ = spec.render_inner_into(node, &content, &mut fallback);
        } else {
            let _ = spec.render_into(node, position, &content, &mut fallback);
        }
    } else if node.child_count() > 0 {
        fallback =
            crate::render::render_children_to_html(runtime, node, position.saturating_add(1));
    }
    if fallback.is_empty() {
        host.set_text_content(Some(&format!("[{} view unavailable]", node.type_name())));
    } else if let Ok(nodes) = super::reconciler::parse_html_nodes(host, &fallback) {
        for child in nodes {
            let _ = host.append_child(child.as_ref());
        }
    }
    let _ = host.set_attribute(ERROR_ATTR, &error.to_string());
    web_sys::console::error_1(&wasm_bindgen::JsValue::from_str(&error.to_string()));
}

fn selection_transition_positions(
    positions: &BTreeMap<usize, u64>,
    doc: &Node,
    schema: &crate::model::Schema,
    previous: Option<&Selection>,
    next: &Selection,
) -> BTreeSet<usize> {
    if previous == Some(next) {
        return BTreeSet::new();
    }
    let mut affected = BTreeSet::new();
    if let Some(previous) = previous {
        add_selection_positions(positions, doc, schema, previous, &mut affected);
    }
    add_selection_positions(positions, doc, schema, next, &mut affected);
    affected
}

fn add_selection_positions(
    positions: &BTreeMap<usize, u64>,
    doc: &Node,
    schema: &crate::model::Schema,
    selection: &Selection,
    affected: &mut BTreeSet<usize>,
) {
    if matches!(selection, Selection::All) {
        affected.extend(positions.keys().copied());
        return;
    }

    let ranges = selection.ranges(doc, schema).unwrap_or_else(|_| {
        vec![crate::state::SelectionRange::new(
            selection.from(doc),
            selection.to(doc),
        )]
    });
    for range in ranges {
        if range.from < range.to {
            affected.extend(
                positions
                    .range(range.from..range.to)
                    .map(|(position, _)| *position),
            );
        }
        add_ancestor_positions(positions, doc, range.from, affected);
        add_ancestor_positions(positions, doc, range.to, affected);
    }

    match selection {
        Selection::Node { anchor } => {
            if positions.contains_key(anchor) {
                affected.insert(*anchor);
            }
        }
        Selection::Cells {
            anchor_cell,
            head_cell,
        } => {
            add_ancestor_positions(positions, doc, *anchor_cell, affected);
            add_ancestor_positions(positions, doc, *head_cell, affected);
        }
        Selection::Text { .. } | Selection::All => {}
    }
}

fn add_ancestor_positions(
    positions: &BTreeMap<usize, u64>,
    doc: &Node,
    position: usize,
    affected: &mut BTreeSet<usize>,
) {
    let Ok(resolved) = doc.resolve(position.min(doc.content_size())) else {
        return;
    };
    for depth in 1..=resolved.depth() {
        let Some(before) = resolved.before(depth) else {
            continue;
        };
        if positions.contains_key(&before) {
            affected.insert(before);
        }
    }
}

pub(crate) fn selection_relation(
    selection: &Selection,
    node: &Node,
    position: usize,
) -> NodeViewSelection {
    selection_relation_for_size(selection, node.node_size(), position)
}

fn selection_relation_for_size(
    selection: &Selection,
    node_size: usize,
    position: usize,
) -> NodeViewSelection {
    let outer_end = position.saturating_add(node_size);
    let content_start = position.saturating_add(1);
    let content_end = outer_end.saturating_sub(1);
    match selection {
        Selection::Node { anchor } if *anchor == position => NodeViewSelection::Node,
        Selection::Node { .. } => NodeViewSelection::Outside,
        Selection::Text { anchor, head } if anchor == head => {
            if *anchor >= content_start && *anchor <= content_end {
                NodeViewSelection::CursorInside
            } else {
                NodeViewSelection::Outside
            }
        }
        Selection::Text { anchor, head } => {
            let (from, to) = if anchor <= head {
                (*anchor, *head)
            } else {
                (*head, *anchor)
            };
            let anchor_inside = *anchor >= content_start && *anchor <= content_end;
            let head_inside = *head >= content_start && *head <= content_end;
            if anchor_inside && head_inside {
                NodeViewSelection::NodeContainsRange
            } else if from <= position && to >= outer_end {
                NodeViewSelection::RangeContainsNode
            } else if anchor_inside || head_inside || (from < outer_end && to > position) {
                NodeViewSelection::CrossesBoundary
            } else {
                NodeViewSelection::Outside
            }
        }
        Selection::All => NodeViewSelection::RangeContainsNode,
        Selection::Cells {
            anchor_cell,
            head_cell,
        } if *anchor_cell > position
            && *anchor_cell < outer_end
            && *head_cell > position
            && *head_cell < outer_end =>
        {
            NodeViewSelection::Cells {
                anchor_cell: *anchor_cell,
                head_cell: *head_cell,
            }
        }
        _ => NodeViewSelection::Outside,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_basic;

    fn index(positions: &[usize]) -> BTreeMap<usize, u64> {
        positions
            .iter()
            .enumerate()
            .map(|(index, position)| (*position, index as u64 + 1))
            .collect()
    }

    fn paragraph() -> Node {
        schema_basic::paragraph(vec![schema_basic::text("hello", Vec::new()).unwrap()]).unwrap()
    }

    #[test]
    fn sync_invariant_diagnostic_names_semantic_node_and_model_position() {
        let error = sync_invariant_error(
            "task_item",
            17,
            "expected a direct model host with the matching `data-pos`, but none was found",
        );
        assert_eq!(
            error.to_string(),
            "typed node view `task_item` violated an invariant: model position 17: expected a direct model host with the matching `data-pos`, but none was found"
        );
    }

    #[test]
    fn selection_relation_partitions_cursor_and_range_cases() {
        let node = paragraph();
        let pos = 4;
        assert_eq!(
            selection_relation(&Selection::text(pos + 2), &node, pos),
            NodeViewSelection::CursorInside
        );
        assert_eq!(
            selection_relation(&Selection::text_between(pos + 1, pos + 4), &node, pos),
            NodeViewSelection::NodeContainsRange
        );
        assert_eq!(
            selection_relation(
                &Selection::text_between(0, pos + node.node_size()),
                &node,
                pos
            ),
            NodeViewSelection::RangeContainsNode
        );
        assert_eq!(
            selection_relation(&Selection::text_between(0, pos + 2), &node, pos),
            NodeViewSelection::CrossesBoundary
        );
        assert_eq!(
            selection_relation(&Selection::node(pos), &node, pos),
            NodeViewSelection::Node
        );
    }

    #[test]
    fn transition_only_collects_old_and_new_selection_ancestors() {
        let first = paragraph();
        let first_size = first.node_size();
        let second = paragraph();
        let document = schema_basic::doc(vec![first, second]).unwrap();
        let positions = index(&[0, first_size]);

        let affected = selection_transition_positions(
            &positions,
            &document,
            &schema_basic::schema(),
            Some(&Selection::text(2)),
            &Selection::text(first_size + 2),
        );
        assert_eq!(affected, BTreeSet::from([0, first_size]));
    }

    #[test]
    fn nested_ancestor_paths_are_included_without_unrelated_views() {
        let nested_paragraph = paragraph();
        let quote = schema_basic::blockquote(vec![nested_paragraph]).unwrap();
        let quote_size = quote.node_size();
        let trailing = paragraph();
        let document = schema_basic::doc(vec![quote, trailing]).unwrap();
        let positions = index(&[0, 1, quote_size]);

        let caret = selection_transition_positions(
            &positions,
            &document,
            &schema_basic::schema(),
            None,
            &Selection::text(3),
        );
        assert_eq!(caret, BTreeSet::from([0, 1]));

        let range = selection_transition_positions(
            &positions,
            &document,
            &schema_basic::schema(),
            Some(&Selection::text(3)),
            &Selection::text_between(3, quote_size + 2),
        );
        assert_eq!(range, BTreeSet::from([0, 1, quote_size]));
    }

    #[test]
    fn all_selection_is_the_only_selection_that_plans_every_indexed_view() {
        let document = schema_basic::doc(vec![paragraph(), paragraph()]).unwrap();
        let positions = index(&[0, 7, 14, 21]);
        let affected = selection_transition_positions(
            &positions,
            &document,
            &schema_basic::schema(),
            Some(&Selection::text(2)),
            &Selection::All,
        );
        assert_eq!(affected, positions.keys().copied().collect::<BTreeSet<_>>());
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod selection_sync_browser_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

    use super::*;
    use crate::runtime::RuntimeBuilder;
    use crate::schema_basic;

    wasm_bindgen_test_configure!(run_in_browser);

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    #[pocopine::component(
        name = "node-view-manager-task-item-fixture",
        template = poco! {<div class="manager-task-item-fixture" pp-owned-content></div>}
    )]
    struct ManagerTaskItemFixture {
        checked: bool,
    }

    #[pocopine::handlers]
    impl ManagerTaskItemFixture {}

    impl crate::view::RichTextNodeView<crate::extensions::TaskItemNode> for ManagerTaskItemFixture {
        fn sync_node(
            &mut self,
            update: crate::view::NodeViewUpdate<crate::extensions::TaskItemAttrs>,
        ) -> Result<(), NodeViewError> {
            self.checked = update.attrs.checked;
            Ok(())
        }
    }

    struct RecordingView {
        updates: Rc<RefCell<Vec<ErasedNodeViewUpdate>>>,
    }

    impl MountedNodeView for RecordingView {
        fn sync(&mut self, update: ErasedNodeViewUpdate) -> Result<(), NodeViewError> {
            self.updates.borrow_mut().push(update);
            Ok(())
        }

        fn is_active(&self) -> bool {
            true
        }

        fn unmount(self: Box<Self>) {}
    }

    fn task_runtime() -> Arc<EditorRuntime> {
        RuntimeBuilder::new()
            .with_view(
                crate::extensions::TaskListExtension::new()
                    .with_typed_node_view::<ManagerTaskItemFixture>(),
            )
            .build()
    }

    fn task_document() -> (Node, Node) {
        let paragraph =
            schema_basic::paragraph(vec![schema_basic::text("task", Vec::new()).unwrap()]).unwrap();
        let item = schema_basic::task_item(false, vec![paragraph]).unwrap();
        let list = schema_basic::task_list(vec![item.clone()]).unwrap();
        (schema_basic::doc(vec![list]).unwrap(), item)
    }

    #[wasm_bindgen_test]
    fn missing_typed_host_is_marked_and_a_repaired_pass_clears_the_marker() {
        let browser = web_sys::window().unwrap().document().unwrap();
        let surface = browser.create_element("div").unwrap();
        let list = browser.create_element("ul").unwrap();
        list.set_attribute("data-pos", "0").unwrap();
        surface.append_child(list.as_ref()).unwrap();
        let (document, _) = task_document();
        let mut manager = NodeViewManager::new(task_runtime());

        manager.sync_tree(&surface, &document, &Selection::text(0), true, false);
        let diagnostic = list.get_attribute(SYNC_ERROR_ATTR).unwrap();
        assert!(diagnostic.contains("task_item"));
        assert!(diagnostic.contains("model position 1"));
        assert!(diagnostic.contains("data-pos"));

        let fallback = browser.create_element("li").unwrap();
        fallback.set_attribute("data-pos", "1").unwrap();
        fallback
            .set_attribute(ERROR_ATTR, "known fallback")
            .unwrap();
        list.append_child(fallback.as_ref()).unwrap();
        manager.sync_tree(&surface, &document, &Selection::text(0), true, false);
        assert!(!list.has_attribute(SYNC_ERROR_ATTR));
    }

    #[wasm_bindgen_test]
    fn detached_editable_outlet_is_marked_on_the_exact_typed_host() {
        let browser = web_sys::window().unwrap().document().unwrap();
        let surface = browser.create_element("div").unwrap();
        let list = browser.create_element("ul").unwrap();
        list.set_attribute("data-pos", "0").unwrap();
        let host = browser.create_element("li").unwrap();
        host.set_attribute("data-pos", "1").unwrap();
        host.set_attribute(INSTANCE_ATTR, "1").unwrap();
        list.append_child(host.as_ref()).unwrap();
        surface.append_child(list.as_ref()).unwrap();

        let (document, item) = task_document();
        let update = ErasedNodeViewUpdate::from_node(
            &item,
            selection_relation(&Selection::text(0), &item, 1),
            true,
            false,
        );
        let detached_outlet = browser.create_element("div").unwrap();
        let mut manager = NodeViewManager::new(task_runtime());
        manager.instances.insert(
            1,
            MountedInstance {
                host: host.clone(),
                node_type: item.type_name().to_string(),
                position: 1,
                node_size: item.node_size(),
                content_outlet: Some(detached_outlet),
                last_update: update,
                mounted: Box::new(RecordingView {
                    updates: Rc::new(RefCell::new(Vec::new())),
                }),
            },
        );
        manager.positions.insert(1, 1);

        manager.sync_tree(&surface, &document, &Selection::text(0), true, false);
        let diagnostic = host.get_attribute(SYNC_ERROR_ATTR).unwrap();
        assert!(diagnostic.contains("task_item"));
        assert!(diagnostic.contains("detached from its host"));
    }

    #[wasm_bindgen_test]
    fn indexed_selection_sync_touches_two_views_then_global_focus_touches_all() {
        let first = schema_basic::paragraph(vec![schema_basic::text("first", Vec::new()).unwrap()])
            .unwrap();
        let first_size = first.node_size();
        let second =
            schema_basic::paragraph(vec![schema_basic::text("second", Vec::new()).unwrap()])
                .unwrap();
        let document = schema_basic::doc(vec![first.clone(), second.clone()]).unwrap();
        let runtime = RuntimeBuilder::new().build();
        let mut manager = NodeViewManager::new(runtime);
        let browser = web_sys::window().unwrap().document().unwrap();
        let first_updates = Rc::new(RefCell::new(Vec::new()));
        let second_updates = Rc::new(RefCell::new(Vec::new()));

        insert_recording_instance(
            &mut manager,
            &browser,
            1,
            0,
            &first,
            NodeViewSelection::CursorInside,
            first_updates.clone(),
        );
        insert_recording_instance(
            &mut manager,
            &browser,
            2,
            first_size,
            &second,
            NodeViewSelection::Outside,
            second_updates.clone(),
        );
        manager.last_selection = Some(Selection::text(2));
        manager.last_editable = Some(true);
        manager.last_editor_focused = Some(true);

        let moved = manager.sync_selection(&document, &Selection::text(first_size + 2), true, true);
        assert_eq!(moved.considered, 2);
        assert_eq!(moved.synced, 2);
        assert!(!moved.global);
        assert_eq!(
            first_updates.borrow().last().unwrap().selection,
            NodeViewSelection::Outside
        );
        assert_eq!(
            second_updates.borrow().last().unwrap().selection,
            NodeViewSelection::CursorInside
        );

        let unchanged =
            manager.sync_selection(&document, &Selection::text(first_size + 2), true, true);
        assert_eq!(unchanged.considered, 0);
        assert_eq!(unchanged.synced, 0);

        let blurred =
            manager.sync_selection(&document, &Selection::text(first_size + 2), true, false);
        assert!(blurred.global);
        assert_eq!(blurred.considered, 2);
        assert_eq!(blurred.synced, 2);
        assert!(!first_updates.borrow().last().unwrap().editor_focused);
        assert!(!second_updates.borrow().last().unwrap().editor_focused);
    }

    fn insert_recording_instance(
        manager: &mut NodeViewManager,
        document: &web_sys::Document,
        id: u64,
        position: usize,
        node: &Node,
        selection: NodeViewSelection,
        updates: Rc<RefCell<Vec<ErasedNodeViewUpdate>>>,
    ) {
        let host = document.create_element("div").unwrap();
        host.set_attribute(INSTANCE_ATTR, &id.to_string()).unwrap();
        host.set_attribute("data-pos", &position.to_string())
            .unwrap();
        let update = ErasedNodeViewUpdate::from_node(node, selection, true, true);
        manager.instances.insert(
            id,
            MountedInstance {
                host,
                node_type: node.type_name().to_string(),
                position,
                node_size: node.node_size(),
                content_outlet: None,
                last_update: update,
                mounted: Box::new(RecordingView { updates }),
            },
        );
        manager.positions.insert(position, id);
    }
}
