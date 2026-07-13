//! Typed component views for semantic rich-text nodes.
//!
//! The persisted node type and the Pocopine component are paired here with
//! Rust generics. Authors never provide a node name or component tag string,
//! and the erased runtime descriptor retains both `TypeId`s so runtime
//! composition can prove the association before an editor mounts.

use std::any::TypeId;
use std::fmt;

use pocopine::{
    App, MountError, MountInitError, MountableComponent, MountedSubtree,
    OwnedContentOutletComponent, resolve_owned_content_outlet,
};
use web_sys::{Element, Node as DomNode};

use crate::extension::RichTextExtension;
use crate::model::{Attrs, Fragment, Mark, Node};
use crate::{RichTextNodeType, TypedNodeAttrsError};

use super::node_view_handle::{ErasedNodeViewHandle, provide_node_view_context};

/// View-only half of an extension.
///
/// The model extension remains usable on native targets without Pocopine or
/// `web-sys`; a browser runtime opts into this companion contract explicitly
/// through [`crate::runtime::RuntimeBuilder::with_view`].
pub trait RichTextViewExtension: RichTextExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        Vec::new()
    }
}

/// How a component shares descendant DOM ownership with the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeViewKind {
    /// The component owns its complete DOM subtree and behaves as one editor
    /// unit.
    Atom,
    /// The component owns chrome around one compiled editor-owned content
    /// outlet.
    Editable,
}

/// Structural outer-host policy for a typed component view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NodeViewHost {
    /// Use a neutral `div`/`span` selected from the semantic inline flag. The
    /// component may render richer chrome, including a nested native element.
    #[default]
    Flow,
    /// Reuse the registered native DOM view's root tag and projected root
    /// attrs. Required for context-sensitive parents such as `ul > li`.
    Native,
}

/// The current model-selection relation to one node-view instance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeViewSelection {
    /// The selection does not intersect this node.
    #[default]
    Outside,
    /// An exact node selection targets this node.
    Node,
    /// A collapsed text caret is inside this editable node.
    CursorInside,
    /// Both endpoints of a non-collapsed range are inside this node.
    NodeContainsRange,
    /// The range fully contains this node.
    RangeContainsNode,
    /// Exactly one range endpoint is inside this node.
    CrossesBoundary,
    /// A rectangular semantic cell selection lies inside this node. Positions
    /// point immediately before the anchor/head cell nodes.
    Cells {
        anchor_cell: usize,
        head_cell: usize,
    },
}

/// Immutable semantic snapshot delivered to a node-view component.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeViewUpdate<A> {
    /// Fully decoded attrs for the exact semantic node type `N`.
    pub attrs: A,
    /// Marks applied to the semantic node.
    pub marks: Vec<Mark>,
    /// Arc-shared child fragment. Components may inspect but never mutate it.
    pub content: Fragment,
    /// Selection relation computed by the editor manager.
    pub selection: NodeViewSelection,
    /// Whether editor-owned content may currently be edited.
    pub editable: bool,
    /// Whether focus currently belongs to the editor surface or its view
    /// chrome.
    pub editor_focused: bool,
}

/// Typed component contract for one semantic node type.
pub trait RichTextNodeView<N: RichTextNodeType>: MountableComponent {
    /// Apply the complete current semantic snapshot. The first call happens
    /// before `on_setup`; subsequent calls update the same mounted component.
    fn sync_node(&mut self, update: NodeViewUpdate<N::Attrs>) -> Result<(), NodeViewError>;
}

/// Recoverable typed node-view failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeViewError {
    /// The runtime or manager paired a descriptor with the wrong node.
    SemanticTypeMismatch {
        expected: &'static str,
        actual: String,
    },
    /// The node's closed attr map could not be decoded as `N::Attrs`.
    Attrs {
        node_type: &'static str,
        message: String,
    },
    /// Pocopine could not register or mount the component subtree.
    Mount {
        component: &'static str,
        message: String,
    },
    /// The component rejected a semantic snapshot.
    Sync {
        component: &'static str,
        message: String,
    },
    /// The node host was removed or rebound to another generation.
    Stale { node_type: String },
    /// A compiled ownership invariant was violated.
    Invariant { node_type: String, message: String },
    /// Typed context extraction failed for a component/handler scope.
    Context {
        requested: &'static str,
        message: String,
    },
    /// A typed anchored transaction could not be built or committed.
    Dispatch { node_type: String, message: String },
}

impl fmt::Display for NodeViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SemanticTypeMismatch { expected, actual } => write!(
                formatter,
                "typed node view expected semantic node `{expected}`, found `{actual}`"
            ),
            Self::Attrs { node_type, message } => {
                write!(
                    formatter,
                    "typed node `{node_type}` attrs are invalid: {message}"
                )
            }
            Self::Mount { component, message } => {
                write!(
                    formatter,
                    "node-view component `{component}` failed to mount: {message}"
                )
            }
            Self::Sync { component, message } => {
                write!(
                    formatter,
                    "node-view component `{component}` rejected an update: {message}"
                )
            }
            Self::Stale { node_type } => {
                write!(
                    formatter,
                    "typed node-view handle for `{node_type}` is stale"
                )
            }
            Self::Invariant { node_type, message } => {
                write!(
                    formatter,
                    "typed node view `{node_type}` violated an invariant: {message}"
                )
            }
            Self::Context { requested, message } => write!(
                formatter,
                "typed node-view context `{requested}` is unavailable: {message}"
            ),
            Self::Dispatch { node_type, message } => write!(
                formatter,
                "typed node-view command for `{node_type}` failed: {message}"
            ),
        }
    }
}

impl std::error::Error for NodeViewError {}

impl From<TypedNodeAttrsError> for NodeViewError {
    fn from(error: TypedNodeAttrsError) -> Self {
        match error {
            TypedNodeAttrsError::UnknownKey { node_type, key } => Self::Attrs {
                node_type,
                message: format!("unknown attr `{key}`"),
            },
            TypedNodeAttrsError::Decode { node_type, source } => Self::Attrs {
                node_type,
                message: source.to_string(),
            },
        }
    }
}

#[derive(Clone, PartialEq)]
pub(crate) struct ErasedNodeViewUpdate {
    pub node_type: String,
    pub attrs: Attrs,
    pub marks: Vec<Mark>,
    pub content: Fragment,
    pub selection: NodeViewSelection,
    pub editable: bool,
    pub editor_focused: bool,
}

impl ErasedNodeViewUpdate {
    pub(crate) fn from_node(
        node: &Node,
        selection: NodeViewSelection,
        editable: bool,
        editor_focused: bool,
    ) -> Self {
        Self {
            node_type: node.type_name().to_string(),
            attrs: node.attrs().clone(),
            marks: node.marks().to_vec(),
            content: node.content().clone(),
            selection,
            editable,
            editor_focused,
        }
    }
}

fn decode_update<N: RichTextNodeType>(
    update: ErasedNodeViewUpdate,
) -> Result<NodeViewUpdate<N::Attrs>, NodeViewError> {
    if update.node_type != N::NAME {
        return Err(NodeViewError::SemanticTypeMismatch {
            expected: N::NAME,
            actual: update.node_type,
        });
    }
    let value = serde_json::to_value(&update.attrs).map_err(|error| NodeViewError::Attrs {
        node_type: N::NAME,
        message: error.to_string(),
    })?;
    let attrs = serde_json::from_value(value).map_err(|error| NodeViewError::Attrs {
        node_type: N::NAME,
        message: error.to_string(),
    })?;
    Ok(NodeViewUpdate {
        attrs,
        marks: update.marks,
        content: update.content,
        selection: update.selection,
        editable: update.editable,
        editor_focused: update.editor_focused,
    })
}

pub(crate) trait MountedNodeView {
    fn sync(&mut self, update: ErasedNodeViewUpdate) -> Result<(), NodeViewError>;
    fn is_active(&self) -> bool;
    fn unmount(self: Box<Self>);
}

struct MountedComponentNodeView<N, C>
where
    N: RichTextNodeType,
    C: RichTextNodeView<N>,
{
    mounted: Option<MountedSubtree<C>>,
    _semantic: std::marker::PhantomData<fn() -> N>,
}

impl<N, C> MountedNodeView for MountedComponentNodeView<N, C>
where
    N: RichTextNodeType,
    C: RichTextNodeView<N>,
{
    fn sync(&mut self, update: ErasedNodeViewUpdate) -> Result<(), NodeViewError> {
        let update = decode_update::<N>(update)?;
        let mounted = self
            .mounted
            .as_ref()
            .filter(|mounted| mounted.is_active())
            .ok_or_else(|| NodeViewError::Stale {
                node_type: N::NAME.to_string(),
            })?;
        mounted.handle().update(|component| {
            component
                .sync_node(update)
                .map_err(|error| NodeViewError::Sync {
                    component: C::NAME,
                    message: error.to_string(),
                })
        })
    }

    fn is_active(&self) -> bool {
        self.mounted.as_ref().is_some_and(MountedSubtree::is_active)
    }

    fn unmount(mut self: Box<Self>) {
        if let Some(mounted) = self.mounted.take() {
            mounted.unmount();
        }
    }
}

type RegisterFn = fn() -> Result<(), MountError>;
type MountFn = fn(
    &Element,
    ErasedNodeViewUpdate,
    ErasedNodeViewHandle,
) -> Result<Box<dyn MountedNodeView>, NodeViewError>;
type ResolveOutletFn = fn(&Element) -> Result<Element, NodeViewError>;

/// Type-erased, runtime-retained proof that semantic node `N` is rendered by
/// component `C`.
#[derive(Clone)]
pub struct NodeViewSpec {
    semantic_type_id: TypeId,
    semantic_rust_type: &'static str,
    node_type: &'static str,
    component_type_id: TypeId,
    component_rust_type: &'static str,
    component_name: &'static str,
    kind: NodeViewKind,
    host: NodeViewHost,
    owned_content_path: Option<&'static [u16]>,
    register: RegisterFn,
    mount: MountFn,
    resolve_outlet: Option<ResolveOutletFn>,
}

impl fmt::Debug for NodeViewSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NodeViewSpec")
            .field("node_type", &self.node_type)
            .field("semantic_rust_type", &self.semantic_rust_type)
            .field("component", &self.component_name)
            .field("component_rust_type", &self.component_rust_type)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

impl NodeViewSpec {
    /// Pair an atom semantic node with a Pocopine component. The component
    /// owns every descendant under the stable editor host.
    pub fn atom_component<N, C>() -> Self
    where
        N: RichTextNodeType,
        C: RichTextNodeView<N>,
    {
        Self::component::<N, C>(NodeViewKind::Atom, None)
    }

    /// Pair an editable semantic node with a component whose template has one
    /// compile-time-proven `pp-owned-content` outlet.
    pub fn editable_component<N, C>() -> Self
    where
        N: RichTextNodeType,
        C: RichTextNodeView<N> + OwnedContentOutletComponent,
    {
        Self::component::<N, C>(NodeViewKind::Editable, Some(resolve_outlet::<N, C>))
    }

    /// Mount directly into the semantic native DOM root contributed for `N`.
    /// Runtime construction rejects this policy when no native DOM spec exists.
    pub fn native_host(mut self) -> Self {
        self.host = NodeViewHost::Native;
        self
    }

    fn component<N, C>(kind: NodeViewKind, resolve_outlet: Option<ResolveOutletFn>) -> Self
    where
        N: RichTextNodeType,
        C: RichTextNodeView<N>,
    {
        Self {
            semantic_type_id: TypeId::of::<N>(),
            semantic_rust_type: std::any::type_name::<N>(),
            node_type: N::NAME,
            component_type_id: TypeId::of::<C>(),
            component_rust_type: std::any::type_name::<C>(),
            component_name: C::NAME,
            kind,
            host: NodeViewHost::Flow,
            owned_content_path: C::OWNED_CONTENT_OUTLET_PATH,
            register: C::register_mountable,
            mount: mount_component_view::<N, C>,
            resolve_outlet,
        }
    }

    pub fn semantic_type_id(&self) -> TypeId {
        self.semantic_type_id
    }

    pub fn semantic_rust_type(&self) -> &'static str {
        self.semantic_rust_type
    }

    pub fn node_type(&self) -> &'static str {
        self.node_type
    }

    pub fn component_type_id(&self) -> TypeId {
        self.component_type_id
    }

    pub fn component_rust_type(&self) -> &'static str {
        self.component_rust_type
    }

    pub fn component_name(&self) -> &'static str {
        self.component_name
    }

    pub fn kind(&self) -> NodeViewKind {
        self.kind
    }

    pub fn host(&self) -> NodeViewHost {
        self.host
    }

    pub fn owned_content_path(&self) -> Option<&'static [u16]> {
        self.owned_content_path
    }

    pub(crate) fn register_component(&self) -> Result<(), MountError> {
        (self.register)()
    }

    pub(crate) fn mount_component(
        &self,
        host: &Element,
        update: ErasedNodeViewUpdate,
        handle: ErasedNodeViewHandle,
    ) -> Result<Box<dyn MountedNodeView>, NodeViewError> {
        (self.mount)(host, update, handle)
    }

    pub(crate) fn resolve_content_outlet(
        &self,
        host: &Element,
    ) -> Result<Option<Element>, NodeViewError> {
        self.resolve_outlet.map(|resolve| resolve(host)).transpose()
    }
}

/// Per-runtime typed view registry. Two editors may pair the same semantic
/// node with different components without sharing manager state.
#[derive(Clone, Debug, Default)]
pub(crate) struct TypedNodeViewRegistry {
    entries: std::collections::BTreeMap<String, NodeViewSpec>,
}

impl TypedNodeViewRegistry {
    pub(crate) fn insert(&mut self, spec: NodeViewSpec) {
        self.entries.insert(spec.node_type().to_string(), spec);
    }

    pub(crate) fn get(&self, node_type: &str) -> Option<&NodeViewSpec> {
        self.entries.get(node_type)
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

fn mount_component_view<N, C>(
    host: &Element,
    update: ErasedNodeViewUpdate,
    handle: ErasedNodeViewHandle,
) -> Result<Box<dyn MountedNodeView>, NodeViewError>
where
    N: RichTextNodeType,
    C: RichTextNodeView<N>,
{
    let update = decode_update::<N>(update)?;
    handle.typed::<N>()?;
    let mounted = App::mount_subtree_with::<C, _>(host, move |component, setup| {
        provide_node_view_context(setup, handle);
        component
            .sync_node(update)
            .map_err(|error| MountInitError::new(error.to_string()))
    })
    .map_err(|error| NodeViewError::Mount {
        component: C::NAME,
        message: error.to_string(),
    })?;
    remove_host_formatting_whitespace(host);
    Ok(Box::new(MountedComponentNodeView::<N, C> {
        mounted: Some(mounted),
        _semantic: std::marker::PhantomData,
    }))
}

/// Component templates may contain indentation or a trailing newline outside
/// their single element root. Normal document layout collapses that formatting
/// whitespace, but a rich-text surface can deliberately use `white-space:
/// pre-wrap`; there it would become semantic-looking whitespace and can force
/// an inline atom onto its own line. The editor host owns exactly one component
/// root, so discard only its direct, whitespace-only text siblings.
fn remove_host_formatting_whitespace(host: &Element) {
    let children = host.child_nodes();
    let mut formatting = Vec::new();
    for index in 0..children.length() {
        let Some(child) = children.item(index) else {
            continue;
        };
        if child.node_type() == DomNode::TEXT_NODE
            && child
                .node_value()
                .is_some_and(|value| value.trim().is_empty())
        {
            formatting.push(child);
        }
    }
    for child in formatting {
        let _ = host.remove_child(&child);
    }
}

fn resolve_outlet<N, C>(host: &Element) -> Result<Element, NodeViewError>
where
    N: RichTextNodeType,
    C: RichTextNodeView<N> + OwnedContentOutletComponent,
{
    resolve_owned_content_outlet::<C>(host).map_err(|error| NodeViewError::Invariant {
        node_type: N::NAME.to_string(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_relations_are_distinct() {
        let relations = [
            NodeViewSelection::Outside,
            NodeViewSelection::Node,
            NodeViewSelection::CursorInside,
            NodeViewSelection::NodeContainsRange,
            NodeViewSelection::RangeContainsNode,
            NodeViewSelection::CrossesBoundary,
            NodeViewSelection::Cells {
                anchor_cell: 1,
                head_cell: 2,
            },
        ];
        for (index, relation) in relations.iter().enumerate() {
            assert!(!relations[index + 1..].contains(relation));
        }
    }
}
