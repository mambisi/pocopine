//! Read browser-owned text edits back as one model transaction.
//!
//! Autocomplete, spellcheck and IME can omit `beforeinput` or make it
//! non-cancelable. Read actual DOM text on `input`/`compositionend`, keeping
//! marks and inline atoms in the model instead of parsing component chrome.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use web_sys::{Element, InputEvent, Node as DomNode, StaticRange};

use crate::model::Node;
use crate::runtime::EditorRuntime;
use crate::state::{EditorState, Selection, Transaction};
use crate::text_diff::char_splice;

use super::input::read_dom_selection;
use super::node_view_manager::NodeViewManager;
use super::reconciler::Reconciler;

#[derive(Default)]
pub(crate) struct NativeInput {
    composing: Cell<bool>,
    pending: Cell<bool>,
    block: RefCell<Option<NativeBlock>>,
    repair: Cell<bool>,
    continue_composition: Cell<bool>,
    previous_composition_doc: RefCell<Option<Node>>,
    ending_generation: Cell<u64>,
}

struct NativeBlock {
    element: Element,
    position: usize,
    model: Node,
    document: Node,
}

impl NativeInput {
    pub fn is_composing(&self) -> bool {
        self.composing.get()
    }

    pub fn is_active(&self) -> bool {
        self.is_composing() || self.pending.get()
    }

    pub fn take_repair(&self) -> bool {
        self.repair.replace(false)
    }

    pub fn continue_composition(&self) {
        self.continue_composition.set(true);
    }

    pub fn end_composition_turn(self: &Rc<Self>) {
        // Some engines deliver final insertText after compositionend. Keep
        // that continuation eligible until this browser event turn finishes,
        // then prevent later autocomplete/typing from joining the undo event.
        let generation = self.ending_generation.get().wrapping_add(1);
        self.ending_generation.set(generation);
        let weak = Rc::downgrade(self);
        pocopine_core::tick::after_flush(move || {
            if let Some(native) = weak.upgrade()
                && native.ending_generation.get() == generation
            {
                native.previous_composition_doc.borrow_mut().take();
            }
        });
    }

    pub fn begin(
        &self,
        surface: &Element,
        runtime: &EditorRuntime,
        state: Option<EditorState>,
        event: Option<&InputEvent>,
        composing: bool,
    ) {
        self.pending.set(true);
        if composing {
            self.composing.set(true);
            self.previous_composition_doc.borrow_mut().take();
        }
        if self.block.borrow().is_none() {
            *self.block.borrow_mut() =
                state.and_then(|state| capture_block(surface, runtime, &state, event));
        }
    }

    pub fn finish(
        &self,
        surface: &Element,
        runtime: &EditorRuntime,
        manager: &RefCell<NodeViewManager>,
        state_provider: &Rc<dyn Fn(bool) -> Option<EditorState>>,
        dispatch: &impl Fn(EditorState, Transaction, bool),
    ) -> &'static str {
        let composing = self.composing.replace(false);
        self.pending.set(false);
        let Some(state) = state_provider(false) else {
            self.block.borrow_mut().take();
            return "missing_state";
        };
        let previous = self.previous_composition_doc.borrow_mut().take();
        let continuing =
            self.continue_composition.replace(false) && previous.as_ref() == Some(state.doc());
        let block = self
            .block
            .borrow_mut()
            .take()
            .or_else(|| capture_block(surface, runtime, &state, None));
        let transaction = block
            .as_ref()
            .and_then(|block| native_transaction(surface, runtime, manager, &state, block));
        let (mut transaction, reason) = match transaction {
            Some(transaction) => (transaction, "committed"),
            None => {
                // Unrepresentable structure or a concurrent document change:
                // restore the authoritative document, never leave DOM-only text.
                self.repair.set(true);
                (state.tr(), "native_structure_rejected")
            }
        };
        if composing || continuing {
            transaction.set_meta(crate::history::HISTORY_COMPOSITION_META, continuing.into());
            *self.previous_composition_doc.borrow_mut() = Some(transaction.doc().clone());
        }
        // Even a canceled composition must resume a deferred reconcile. An
        // empty transaction does not create a document-history entry.
        dispatch(state, transaction, false);
        reason
    }
}

fn capture_block(
    surface: &Element,
    runtime: &EditorRuntime,
    state: &EditorState,
    event: Option<&InputEvent>,
) -> Option<NativeBlock> {
    let target = event
        .and_then(|event| {
            event
                .get_target_ranges()
                .get(0)
                .dyn_into::<StaticRange>()
                .ok()
                .map(|range| range.start_container())
        })
        .or_else(|| web_sys::window()?.get_selection().ok()??.anchor_node())?;
    let mut element = target
        .dyn_ref::<Element>()
        .cloned()
        .or_else(|| target.parent_element());
    while let Some(current) = element {
        if !surface.contains(Some(current.as_ref())) || &current == surface {
            return None;
        }
        if let Some(position) = current
            .get_attribute("data-pos")
            .and_then(|value| value.parse::<usize>().ok())
            && let Some(model) = state.doc().node_at(position).ok().flatten()
            && runtime
                .schema()
                .node_type(model.type_name())
                .ok()?
                .inline_content(runtime.schema())
        {
            return Some(NativeBlock {
                element: current,
                position,
                model: model.clone(),
                document: state.doc().clone(),
            });
        }
        element = current.parent_element();
    }
    None
}

fn native_transaction(
    surface: &Element,
    runtime: &EditorRuntime,
    manager: &RefCell<NodeViewManager>,
    state: &EditorState,
    block: &NativeBlock,
) -> Option<Transaction> {
    if state.doc() != &block.document || !surface.contains(Some(block.element.as_ref())) {
        return None;
    }
    let content = Reconciler::with_manager(runtime, manager)
        .content_root_for_node(&block.element, &block.model)
        .ok()?;
    let mut old = vec![String::new()];
    let mut atoms = Vec::new();
    let mut position = block.position + 1;
    let mut starts = vec![position];
    for child in block.model.content().iter() {
        if let Some(text) = child.text() {
            old.last_mut()?.push_str(text);
        } else {
            // Opaque inline nodes must survive with their existing identity.
            // Text on either side can change without counting chrome as text.
            atoms.push(position);
            old.push(String::new());
            starts.push(position + child.node_size());
        }
        position += child.node_size();
    }
    let mut new = vec![String::new()];
    read_inline_text(content.as_ref(), &content, &atoms, &mut new)?;
    if new.len() != old.len() {
        return None;
    }

    // Capture the browser's final caret against its new text before rendering.
    // Model positions use Unicode scalars; the DOM bridge converts UTF-16.
    let selection = read_dom_selection(surface);
    let mut transaction = state.tr();
    for index in (0..old.len()).rev() {
        if old[index] == new[index] {
            continue;
        }
        let (offset, count, replacement) = char_splice(&old[index], &new[index]);
        let from = starts[index] + offset;
        transaction
            .set_selection(Selection::text_between(from, from + count))
            .ok()?;
        if replacement.is_empty() {
            transaction.delete_selection().ok()?;
        } else {
            transaction.insert_text(replacement).ok()?;
        }
    }
    if let Some(selection) = selection {
        transaction.set_selection(selection).ok()?;
    }
    Some(transaction)
}

fn read_inline_text(
    node: &DomNode,
    root: &Element,
    atoms: &[usize],
    text: &mut Vec<String>,
) -> Option<()> {
    if node.node_type() == DomNode::TEXT_NODE {
        text.last_mut()?
            .push_str(&node.node_value().unwrap_or_default());
        return Some(());
    }
    if node.node_type() == DomNode::COMMENT_NODE {
        return Some(());
    }
    let element = node.dyn_ref::<Element>()?;
    if element != root {
        if let Some(position) = element.get_attribute("data-pos") {
            if atoms.get(text.len() - 1).copied() != position.parse().ok() {
                return None;
            }
            text.push(String::new());
            return Some(());
        }
        // A browser may leave a caret placeholder after deleting all text.
        if element.tag_name() == "BR" && root.text_content().unwrap_or_default().is_empty() {
            return Some(());
        }
        if element.get_attribute("contenteditable").as_deref() == Some("false")
            || !matches!(
                element.tag_name().as_str(),
                "SPAN"
                    | "STRONG"
                    | "B"
                    | "EM"
                    | "I"
                    | "CODE"
                    | "A"
                    | "S"
                    | "STRIKE"
                    | "U"
                    | "SUB"
                    | "SUP"
                    | "MARK"
                    | "FONT"
            )
        {
            return None;
        }
    }
    let children = node.child_nodes();
    for index in 0..children.length() {
        read_inline_text(&children.item(index)?, root, atoms, text)?;
    }
    Some(())
}
