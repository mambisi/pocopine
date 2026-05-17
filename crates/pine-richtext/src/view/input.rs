//! Keystroke + beforeinput dispatch.
//!
//! The view installs two listeners on the contentEditable surface:
//! - **`keydown`** — looks the pressed key combo up in a small keymap
//!   built from `crate::commands::*`. Matched keys dispatch the
//!   corresponding command and `preventDefault()` so the browser
//!   doesn't also handle them.
//! - **`beforeinput`** — captures `insertText` events (typed
//!   characters), prevents the default DOM mutation, and inserts the
//!   text via `Transaction::insert_text` so the model stays the source
//!   of truth.
//!
//! Crucially the view does NOT listen on `selectionchange` to push the
//! DOM selection into the model. That would create a feedback loop:
//! every drag-move fires `selectionchange`, every fire dispatches a
//! Transaction, every Transaction re-renders + restores the cursor,
//! which kills the user's in-progress drag. Instead, the
//! [`state_provider`] passed to [`install_listeners`] reads
//! `window.getSelection()` at the moment a command actually needs it
//! and injects the resulting model `Selection` into the
//! reconstructed `EditorState`. Commands that need the live cursor
//! position (`delete_selection`, `toggle_mark`, `insert_text`, etc.)
//! get the up-to-date DOM selection that way.

use std::rc::Rc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Element, Event, InputEvent, KeyboardEvent, StaticRange};

use crate::commands::{self, Command};
use crate::state::{EditorState, Transaction};

use super::selection::dom_pos_to_model;

/// Lookup table from key combo (e.g. `"Mod-b"`, `"Enter"`, `"Backspace"`)
/// to a boxed command. Built by [`default_keymap`] and consulted on
/// every keydown.
pub struct KeyMap {
    bindings: Vec<(String, Box<dyn Command>)>,
}

impl KeyMap {
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn bind(mut self, key: impl Into<String>, cmd: Box<dyn Command>) -> Self {
        self.bindings.push((key.into(), cmd));
        self
    }

    pub fn lookup(&self, key: &str) -> Option<&dyn Command> {
        self.bindings
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, cmd)| cmd.as_ref())
    }
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::new()
    }
}

/// PM-style default keymap. Covers the keys most editors need:
/// Backspace / Delete / Enter, plus Mod-A for select-all.
///
/// Extensions can contribute extra bindings through
/// [`crate::extension::RichTextExtension::key_bindings`]; those are
/// merged in *after* the base 4 entries below so the base remains
/// inviolable (`KeyMap::lookup` is first-wins). Extension-vs-extension
/// collisions resolve registration-order; see
/// [`crate::extension::registry::merged_keymap_factories`].
pub fn default_keymap() -> KeyMap {
    let mut km = base_keymap();
    for (combo, factory) in crate::extension::registry::merged_keymap_factories() {
        km = km.bind(combo, factory());
    }
    km
}

/// The 4 hardcoded bindings the framework always installs. Separated
/// from [`default_keymap`] so tests can assert the base set
/// independently of any extension contributions.
fn base_keymap() -> KeyMap {
    KeyMap::new()
        .bind(
            "Backspace",
            commands::chain_commands(vec![
                commands::delete_selection(),
                commands::join_backward(),
                commands::select_node_backward(),
            ]),
        )
        .bind(
            "Delete",
            commands::chain_commands(vec![
                commands::delete_selection(),
                commands::join_forward(),
                commands::select_node_forward(),
            ]),
        )
        .bind(
            "Enter",
            commands::chain_commands(vec![
                commands::lift_empty_block(),
                commands::split_list_item(&["list_item", "task_item"]),
                commands::split_block(),
            ]),
        )
        .bind("Mod-a", commands::select_all())
}

/// Translate a `KeyboardEvent` into the combo string a [`KeyMap`]
/// expects. Examples: `"a"`, `"Backspace"`, `"Mod-b"`, `"Shift-Enter"`.
/// `Mod` collapses Cmd (Mac) and Ctrl (everywhere else) so apps don't
/// have to register both.
pub fn key_combo(event: &KeyboardEvent) -> String {
    let mut parts: Vec<&str> = Vec::with_capacity(4);
    let mod_key = event.meta_key() || event.ctrl_key();
    if mod_key {
        parts.push("Mod");
    }
    if event.shift_key() {
        parts.push("Shift");
    }
    if event.alt_key() {
        parts.push("Alt");
    }
    let key = event.key();
    // Normalize single-character lowercase: "B" with shift becomes
    // "Shift-b" not "Shift-B".
    let key_str = if key.chars().count() == 1 {
        key.to_lowercase()
    } else {
        key
    };
    parts.push(&key_str);
    parts.join("-")
}

/// Wire `keydown`/`beforeinput`/`selectionchange` listeners on the
/// `surface` element. `dispatch` is invoked with the state snapshot the
/// command used plus the transaction it produced; the caller (the
/// component) decides how to commit it (typically: `state.apply(tr)` then
/// write back to its JSON signal).
///
/// The returned vector holds the live `Closure`s — the caller MUST keep
/// it alive (typically by stashing it in the component's state) or the
/// closures get dropped and the listeners stop firing.
pub fn install_listeners<F>(
    surface: Element,
    state_provider: Rc<dyn Fn() -> Option<EditorState>>,
    keymap: Rc<KeyMap>,
    dispatch: F,
) -> Vec<Closure<dyn FnMut(Event)>>
where
    F: Fn(EditorState, Transaction) + 'static,
{
    let dispatch = Rc::new(dispatch);

    let mut closures: Vec<Closure<dyn FnMut(Event)>> = Vec::new();

    // keydown — keymap lookup.
    {
        let state_provider = state_provider.clone();
        let keymap = keymap.clone();
        let dispatch = dispatch.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            let Ok(ev) = event.clone().dyn_into::<KeyboardEvent>() else {
                return;
            };
            let combo = key_combo(&ev);
            let Some(cmd) = keymap.lookup(&combo) else {
                return;
            };
            let Some(state) = state_provider() else {
                return;
            };
            let Some(tr) = cmd.apply(&state) else { return };
            ev.prevent_default();
            dispatch(state, tr);
        }) as Box<dyn FnMut(Event)>);
        let _ = surface.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // beforeinput — capture typed characters AND deletions. We can't
    // rely on the keymap chain for "Backspace in the middle of text"
    // because none of the standard commands handle that case (it's
    // PM's "browser default" path). If we let the browser delete the
    // char from the contentEditable surface, the model stays out of
    // sync and the next keystroke runs against stale text — the
    // classic "deleted character reappears" bug.
    //
    // The fix: handle every `delete*` input type here, using
    // `InputEvent.getTargetRanges()` to learn what the browser would
    // have deleted. We translate those DOM ranges into a model
    // delete transaction so the model stays authoritative.
    {
        let surface_for_input = surface.clone();
        let state_provider = state_provider.clone();
        let dispatch = dispatch.clone();
        let cb = Closure::wrap(Box::new(move |event: Event| {
            let Ok(ev) = event.clone().dyn_into::<InputEvent>() else {
                return;
            };
            let input_type = ev.input_type();
            match input_type.as_str() {
                "insertText" => {
                    let Some(data) = ev.data() else { return };
                    if data.is_empty() {
                        return;
                    }
                    let Some(state) = state_provider() else {
                        return;
                    };
                    let mut tr = state.tr();
                    if tr.insert_text(data).is_err() {
                        return;
                    }
                    ev.prevent_default();
                    dispatch(state, tr);
                }
                "deleteContentBackward"
                | "deleteContentForward"
                | "deleteWordBackward"
                | "deleteWordForward"
                | "deleteSoftLineBackward"
                | "deleteSoftLineForward"
                | "deleteHardLineBackward"
                | "deleteHardLineForward"
                | "deleteContent"
                | "deleteByCut"
                | "deleteByDrag" => {
                    let Some(state) = state_provider() else {
                        return;
                    };
                    let Some((from, to)) =
                        target_range_to_model(&surface_for_input, &ev, &state, &input_type)
                    else {
                        return;
                    };
                    if from == to {
                        return;
                    }
                    let mut tr = state.tr();
                    if tr.delete(from, to).is_err() {
                        return;
                    }
                    ev.prevent_default();
                    dispatch(state, tr);
                }
                _ => {}
            }
        }) as Box<dyn FnMut(Event)>);
        let _ =
            surface.add_event_listener_with_callback("beforeinput", cb.as_ref().unchecked_ref());
        closures.push(cb);
    }

    // No selectionchange listener — see module docs. Drag-select would
    // be broken if we tried to round-trip every move through the model.

    let _ = surface; // keep `surface` named even after we drop the
                     // selectionchange listener that used it.
    closures
}

/// Translate the first range in `InputEvent.getTargetRanges()` into a
/// `(from, to)` model position pair. Browsers populate that array on
/// every `delete*` input type so userland editors don't have to redo
/// caret/word boundary logic.
///
/// Falls back to a 1-char delete around the current model selection
/// when `getTargetRanges()` is empty or doesn't map cleanly — that
/// covers older browsers and the rare case where a deletion's anchor
/// node lives outside the surface.
fn target_range_to_model(
    surface: &Element,
    event: &InputEvent,
    state: &EditorState,
    input_type: &str,
) -> Option<(usize, usize)> {
    let ranges = event.get_target_ranges();
    if ranges.length() > 0 {
        if let Ok(range) = ranges.get(0).dyn_into::<StaticRange>() {
            let start = dom_pos_to_model(surface, &range.start_container(), range.start_offset());
            let end = dom_pos_to_model(surface, &range.end_container(), range.end_offset());
            if let (Some(a), Some(b)) = (start, end) {
                let (from, to) = if a <= b { (a, b) } else { (b, a) };
                return Some((from, to));
            }
        }
    }
    // Fallback: derive a sensible single-step range from the model
    // selection. Used when the browser didn't supply target ranges
    // (older WebKit on `deleteWordBackward`, e.g.) or when the range
    // pointed outside the surface.
    let sel = state.selection();
    let from = sel.from(state.doc());
    let to = sel.to(state.doc());
    if from != to {
        return Some((from, to));
    }
    let backward = matches!(
        input_type,
        "deleteContentBackward"
            | "deleteWordBackward"
            | "deleteSoftLineBackward"
            | "deleteHardLineBackward"
    );
    if backward {
        if from == 0 {
            return None;
        }
        Some((from - 1, from))
    } else {
        let doc_size = state.doc().content_size();
        if to >= doc_size {
            return None;
        }
        Some((to, to + 1))
    }
}

/// Read the live `window.getSelection()` range and translate it into a
/// model [`crate::state::Selection`]. Returns `None` if there's no
/// selection or if it doesn't live inside `surface`. Used by
/// [`super::root::PineRichTextRoot`]'s state provider to inject the
/// up-to-date cursor into the state every time a command runs.
pub fn read_dom_selection(surface: &Element) -> Option<crate::state::Selection> {
    let sel = web_sys::window().and_then(|w| w.get_selection().ok().flatten())?;
    let anchor_node = sel.anchor_node()?;
    if !surface.contains(Some(&anchor_node)) {
        return None;
    }
    let focus_node = sel.focus_node()?;
    if !surface.contains(Some(&focus_node)) {
        return None;
    }
    let anchor = dom_pos_to_model(surface, &anchor_node, sel.anchor_offset())?;
    let head = dom_pos_to_model(surface, &focus_node, sel.focus_offset())?;
    Some(crate::state::Selection::text_between(anchor, head))
}
