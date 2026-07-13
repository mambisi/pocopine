//! Weak, generation-checked capabilities for mounted semantic node views.

use std::any::TypeId;
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use pocopine::ContextKey;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

use crate::model::{Attrs, Node};
use crate::runtime::EditorRuntime;
use crate::state::{EditorState, Selection, Transaction};
use crate::transform::{AttrStep, Step};
use crate::{RichTextNodeAttrs, RichTextNodeType};

use super::typed_node_views::NodeViewError;

pub(crate) type StateProvider = dyn Fn(bool) -> Option<EditorState>;
pub(crate) type TransactionDispatch = dyn Fn(EditorState, Transaction, bool);

/// Per-editor typed bridge shared weakly by every node handle.
pub(crate) struct NodeViewEditorBinding {
    runtime: Arc<EditorRuntime>,
    surface: Element,
    state_provider: Rc<StateProvider>,
    dispatch: Rc<TransactionDispatch>,
    active: Cell<bool>,
}

impl NodeViewEditorBinding {
    pub(crate) fn new(
        runtime: Arc<EditorRuntime>,
        surface: Element,
        state_provider: Rc<StateProvider>,
        dispatch: Rc<TransactionDispatch>,
    ) -> Rc<Self> {
        Rc::new(Self {
            runtime,
            surface,
            state_provider,
            dispatch,
            active: Cell::new(true),
        })
    }

    pub(crate) fn invalidate(&self) {
        self.active.set(false);
    }
}

#[derive(Clone)]
pub(crate) struct ErasedNodeViewHandle {
    binding: Weak<NodeViewEditorBinding>,
    host: Element,
    generation: u64,
    semantic_type_id: TypeId,
    semantic_name: &'static str,
}

impl ErasedNodeViewHandle {
    pub(crate) fn new(
        binding: &Rc<NodeViewEditorBinding>,
        host: Element,
        generation: u64,
        semantic_type_id: TypeId,
        semantic_name: &'static str,
    ) -> Self {
        Self {
            binding: Rc::downgrade(binding),
            host,
            generation,
            semantic_type_id,
            semantic_name,
        }
    }

    pub(crate) fn typed<N: RichTextNodeType>(&self) -> Result<NodeViewHandle<N>, NodeViewError> {
        if self.semantic_type_id != TypeId::of::<N>() || self.semantic_name != N::NAME {
            return Err(NodeViewError::SemanticTypeMismatch {
                expected: N::NAME,
                actual: self.semantic_name.to_string(),
            });
        }
        Ok(NodeViewHandle {
            binding: self.binding.clone(),
            host: self.host.clone(),
            generation: self.generation,
            _semantic: PhantomData,
        })
    }
}

thread_local! {
    static NODE_VIEW_CONTEXT: ContextKey<ErasedNodeViewHandle> =
        ContextKey::new("pine-richtext-node-view");
}

pub(crate) fn provide_node_view_context(
    setup: &mut pocopine::MountSetup,
    handle: ErasedNodeViewHandle,
) {
    NODE_VIEW_CONTEXT.with(|key| setup.provide(key, handle));
}

/// Resolve the exact typed node-view capability provided to the current
/// component scope.
pub fn use_node_view_handle<N: RichTextNodeType>() -> Result<NodeViewHandle<N>, NodeViewError> {
    if pocopine::current_scope_id().is_none() {
        return Err(NodeViewError::Context {
            requested: std::any::type_name::<N>(),
            message: "called outside a mounted component scope".to_string(),
        });
    }
    let erased =
        NODE_VIEW_CONTEXT
            .with(pocopine::inject)
            .ok_or_else(|| NodeViewError::Context {
                requested: std::any::type_name::<N>(),
                message: "no typed rich-text node view is provided in this scope".to_string(),
            })?;
    erased.typed::<N>()
}

/// A live capability anchored to one mounted semantic node host.
pub struct NodeViewHandle<N: RichTextNodeType> {
    binding: Weak<NodeViewEditorBinding>,
    host: Element,
    generation: u64,
    _semantic: PhantomData<fn() -> N>,
}

impl<N: RichTextNodeType> pocopine::FromHandlerContext for NodeViewHandle<N> {
    fn from_handler_context(
        context: &pocopine::HandlerContext,
    ) -> Result<Self, pocopine::HandlerExtractError> {
        use_node_view_handle::<N>().map_err(|error| context.extraction_error(error.to_string()))
    }
}

impl<N: RichTextNodeType> Clone for NodeViewHandle<N> {
    fn clone(&self) -> Self {
        Self {
            binding: self.binding.clone(),
            host: self.host.clone(),
            generation: self.generation,
            _semantic: PhantomData,
        }
    }
}

impl<N: RichTextNodeType> NodeViewHandle<N> {
    pub fn position(&self) -> Result<usize, NodeViewError> {
        let binding = self.live_binding()?;
        self.validate_host(&binding)
    }

    pub fn attrs(&self) -> Result<N::Attrs, NodeViewError> {
        let (_, _, node) = self.resolve(false)?;
        decode_attrs::<N>(&node)
    }

    pub fn update_attrs(&self, update: impl FnOnce(&mut N::Attrs)) -> Result<(), NodeViewError> {
        let (binding, state, node) = self.resolve(false)?;
        let position = self.validate_host(&binding)?;
        let mut attrs = decode_attrs::<N>(&node)?;
        update(&mut attrs);
        let next = encode_attrs::<N>(&attrs)?;
        let mut transaction = state.tr();
        let mut changed = false;
        for key in N::Attrs::KEYS {
            let before = node.attrs().get(*key);
            let after = next.get(*key);
            if before == after {
                continue;
            }
            changed = true;
            transaction
                .step(Step::Attr(AttrStep {
                    pos: position,
                    attr: (*key).to_string(),
                    value: after.cloned(),
                }))
                .map_err(|error| dispatch_error::<N>(error))?;
        }
        if changed {
            (binding.dispatch)(state, transaction, false);
        }
        Ok(())
    }

    pub fn delete(&self) -> Result<(), NodeViewError> {
        let (binding, state, node) = self.resolve(false)?;
        let position = self.validate_host(&binding)?;
        let mut transaction = state.tr();
        transaction
            .delete(position, position.saturating_add(node.node_size()))
            .map_err(dispatch_error::<N>)?;
        (binding.dispatch)(state, transaction, false);
        Ok(())
    }

    pub fn select_node(&self) -> Result<(), NodeViewError> {
        let (binding, state, _) = self.resolve(false)?;
        let position = self.validate_host(&binding)?;
        let mut transaction = state.tr();
        transaction
            .set_selection(Selection::node(position))
            .map_err(dispatch_error::<N>)?;
        (binding.dispatch)(state, transaction, false);
        Ok(())
    }

    pub fn focus_editor(&self) -> Result<(), NodeViewError> {
        let binding = self.live_binding()?;
        self.validate_host(&binding)?;
        let html = binding
            .surface
            .clone()
            .dyn_into::<HtmlElement>()
            .map_err(|_| NodeViewError::Dispatch {
                node_type: N::NAME.to_string(),
                message: "editor surface is not focusable HTML".to_string(),
            })?;
        html.focus().map_err(|error| NodeViewError::Dispatch {
            node_type: N::NAME.to_string(),
            message: format!("editor focus failed: {error:?}"),
        })
    }

    pub fn dispatch<K: NodeCommand<N>>(&self, command: K) -> Result<(), NodeViewError> {
        let (binding, state, node) = self.resolve(false)?;
        let position = self.validate_host(&binding)?;
        let target = NodeCommandTarget {
            position,
            attrs: decode_attrs::<N>(&node)?,
            node,
            _semantic: PhantomData,
        };
        if let Some(transaction) = command.apply(&state, target)? {
            (binding.dispatch)(state, transaction, false);
        }
        Ok(())
    }

    fn live_binding(&self) -> Result<Rc<NodeViewEditorBinding>, NodeViewError> {
        let binding = self.binding.upgrade().ok_or_else(stale::<N>)?;
        if !binding.active.get() {
            return Err(stale::<N>());
        }
        Ok(binding)
    }

    fn validate_host(&self, binding: &NodeViewEditorBinding) -> Result<usize, NodeViewError> {
        let generation = self
            .host
            .get_attribute("data-pine-node-view-id")
            .and_then(|value| value.parse::<u64>().ok());
        if generation != Some(self.generation)
            || !self.host.is_connected()
            || !binding.surface.contains(Some(self.host.as_ref()))
        {
            return Err(stale::<N>());
        }
        self.host
            .get_attribute("data-pos")
            .and_then(|value| value.parse().ok())
            .ok_or_else(stale::<N>)
    }

    fn resolve(
        &self,
        live_selection: bool,
    ) -> Result<(Rc<NodeViewEditorBinding>, EditorState, Node), NodeViewError> {
        let binding = self.live_binding()?;
        let position = self.validate_host(&binding)?;
        let version = binding
            .runtime
            .typed_node::<N>()
            .ok_or_else(|| NodeViewError::SemanticTypeMismatch {
                expected: N::NAME,
                actual: "runtime has no matching typed semantic node".to_string(),
            })?
            .version();
        let state =
            (binding.state_provider)(live_selection).ok_or_else(|| NodeViewError::Dispatch {
                node_type: N::NAME.to_string(),
                message: "editor state is unavailable".to_string(),
            })?;
        let node = state
            .doc()
            .node_at(position)
            .map_err(dispatch_error::<N>)?
            .filter(|node| node.type_name() == N::NAME && node.version() == Some(version))
            .cloned()
            .ok_or_else(stale::<N>)?;
        Ok((binding, state, node))
    }
}

/// Fresh, type-checked target supplied to an anchored node command.
pub struct NodeCommandTarget<N: RichTextNodeType> {
    pub position: usize,
    pub node: Node,
    pub attrs: N::Attrs,
    _semantic: PhantomData<fn() -> N>,
}

pub trait NodeCommand<N: RichTextNodeType>: 'static {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<N>,
    ) -> Result<Option<Transaction>, NodeViewError>;
}

fn decode_attrs<N: RichTextNodeType>(node: &Node) -> Result<N::Attrs, NodeViewError> {
    serde_json::from_value(
        serde_json::to_value(node.attrs()).map_err(|error| attrs_error::<N>(error))?,
    )
    .map_err(attrs_error::<N>)
}

fn encode_attrs<N: RichTextNodeType>(attrs: &N::Attrs) -> Result<Attrs, NodeViewError> {
    let value = serde_json::to_value(attrs).map_err(attrs_error::<N>)?;
    let object = value.as_object().ok_or_else(|| NodeViewError::Attrs {
        node_type: N::NAME,
        message: "typed attrs did not serialize as an object".to_string(),
    })?;
    if let Some(key) = object
        .keys()
        .find(|key| !N::Attrs::KEYS.contains(&key.as_str()))
    {
        return Err(NodeViewError::Attrs {
            node_type: N::NAME,
            message: format!("serializer emitted undeclared attr `{key}`"),
        });
    }
    if let Some(key) = N::Attrs::KEYS
        .iter()
        .find(|key| !object.contains_key(**key))
    {
        return Err(NodeViewError::Attrs {
            node_type: N::NAME,
            message: format!("serializer omitted declared attr `{key}`"),
        });
    }
    serde_json::from_value(value).map_err(attrs_error::<N>)
}

fn stale<N: RichTextNodeType>() -> NodeViewError {
    NodeViewError::Stale {
        node_type: N::NAME.to_string(),
    }
}

fn attrs_error<N: RichTextNodeType>(error: serde_json::Error) -> NodeViewError {
    NodeViewError::Attrs {
        node_type: N::NAME,
        message: error.to_string(),
    }
}

fn dispatch_error<N: RichTextNodeType>(error: impl std::fmt::Display) -> NodeViewError {
    NodeViewError::Dispatch {
        node_type: N::NAME.to_string(),
        message: error.to_string(),
    }
}
