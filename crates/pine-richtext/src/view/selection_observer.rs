//! Read-only editor-selection snapshots and subscriptions.
//!
//! A browser `selectionchange` event fires repeatedly while the user drags a
//! selection. Feeding every one of those events back through an editor
//! transaction would repaint the DOM and interrupt the drag. This module keeps
//! the two concerns separate: it reads the live DOM selection and the current
//! model state, but never dispatches a transaction or asks the reconciler to
//! repaint.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Document, DomRect, Element, Event, HtmlElement, Node as DomNode};

use super::root::CHANGE_EVENT;
use crate::state::{EditorState, Selection};

/// A rectangle in viewport coordinates.
///
/// The shape mirrors the useful read-only fields of `DOMRect`. For a ranged
/// selection this is the union of every client rectangle produced by every DOM
/// range. A collapsed caret may have zero width and non-zero height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewportRect {
    /// Horizontal viewport coordinate of the rectangle's origin.
    pub x: f64,
    /// Vertical viewport coordinate of the rectangle's origin.
    pub y: f64,
    /// Rectangle width in CSS pixels.
    pub width: f64,
    /// Rectangle height in CSS pixels.
    pub height: f64,
    /// Minimum vertical viewport coordinate.
    pub top: f64,
    /// Maximum horizontal viewport coordinate.
    pub right: f64,
    /// Maximum vertical viewport coordinate.
    pub bottom: f64,
    /// Minimum horizontal viewport coordinate.
    pub left: f64,
}

impl ViewportRect {
    fn from_dom(rect: &DomRect) -> Self {
        Self {
            x: rect.x(),
            y: rect.y(),
            width: rect.width(),
            height: rect.height(),
            top: rect.top(),
            right: rect.right(),
            bottom: rect.bottom(),
            left: rect.left(),
        }
    }

    fn is_visible(self) -> bool {
        self.width > 0.0 || self.height > 0.0
    }

    fn union(self, other: Self) -> Self {
        let left = self.left.min(other.left);
        let top = self.top.min(other.top);
        let right = self.right.max(other.right);
        let bottom = self.bottom.max(other.bottom);
        Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
            top,
            right,
            bottom,
            left,
        }
    }
}

/// A cheap, read-only snapshot of the editor's current selection context.
///
/// `selection`, `from`, and `to` use Pine's model positions. `rect` uses
/// viewport coordinates and is present only while both DOM selection endpoints
/// are inside this editor surface. `active_mark_names` contains mark types
/// active at a caret, or present anywhere in a ranged selection.
/// `enclosing_block_types` is the outer-to-inner non-inline ancestor path at
/// the selection head (the selected node is appended for a node selection).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionSnapshot {
    /// Current Pine model selection, overlaid with the live DOM selection when
    /// both browser endpoints are inside the editor.
    pub selection: Selection,
    /// Normalized inclusive start position in the model document.
    pub from: usize,
    /// Normalized exclusive end position in the model document.
    pub to: usize,
    /// Whether the model selection has no selected content.
    pub empty: bool,
    /// Stable mark-type names active at the caret or present in the range.
    pub active_mark_names: Vec<String>,
    /// Non-inline ancestor type names at the selection head, outer to inner.
    pub enclosing_block_types: Vec<String>,
    /// Union of the live DOM selection's client rectangles, when it belongs to
    /// this editor.
    pub rect: Option<ViewportRect>,
    /// Whether the document's active element is this surface or a descendant.
    pub focused: bool,
    /// Whether the browser currently treats the surface as content-editable.
    pub editable: bool,
}

type SharedSnapshotCallback = Rc<RefCell<Box<dyn FnMut(SelectionSnapshot)>>>;

/// Subscription returned by `Editor::on_selection_change`.
///
/// Drop the guard (or call [`detach`](Self::detach)) to remove the document and
/// editor listeners and cancel any queued animation-frame callback.
#[must_use = "drop the guard to detach the selection observer"]
pub struct SelectionChangeSubscription {
    document: Option<Document>,
    state_target: Option<Element>,
    selection_listener: Option<Closure<dyn FnMut(Event)>>,
    focus_listener: Option<Closure<dyn FnMut(Event)>>,
    blur_listener: Option<Closure<dyn FnMut(Event)>>,
    change_listener: Option<Closure<dyn FnMut(Event)>>,
    frame_callback: Option<Closure<dyn FnMut(f64)>>,
    pending_frame: Rc<Cell<Option<i32>>>,
    active: Rc<Cell<bool>>,
}

impl SelectionChangeSubscription {
    pub(crate) fn subscribe<Read, Callback>(
        state_target: Element,
        surface: Option<Element>,
        read: Read,
        callback: Callback,
    ) -> Self
    where
        Read: Fn() -> Option<SelectionSnapshot> + 'static,
        Callback: FnMut(SelectionSnapshot) + 'static,
    {
        let Some(surface) = surface else {
            return Self::inert();
        };
        let Some(document) = surface.owner_document() else {
            return Self::inert();
        };
        let Some(window) = document.default_view() else {
            return Self::inert();
        };

        let pending_frame = Rc::new(Cell::new(None));
        let active = Rc::new(Cell::new(true));
        let read: Rc<dyn Fn() -> Option<SelectionSnapshot>> = Rc::new(read);
        let callback: SharedSnapshotCallback = Rc::new(RefCell::new(Box::new(callback)));

        let pending_for_frame = pending_frame.clone();
        let active_for_frame = active.clone();
        let frame_callback = Closure::wrap(Box::new(move |_timestamp: f64| {
            pending_for_frame.set(None);
            if !active_for_frame.get() {
                return;
            }
            if let Some(snapshot) = read() {
                callback.borrow_mut()(snapshot);
            }
        }) as Box<dyn FnMut(f64)>);

        let frame_function: js_sys::Function = frame_callback
            .as_ref()
            .unchecked_ref::<js_sys::Function>()
            .clone();
        let pending_for_schedule = pending_frame.clone();
        let active_for_schedule = active.clone();
        let schedule: Rc<dyn Fn()> = Rc::new(move || {
            if !active_for_schedule.get() || pending_for_schedule.get().is_some() {
                return;
            }
            if let Ok(frame) = window.request_animation_frame(&frame_function) {
                pending_for_schedule.set(Some(frame));
            }
        });

        // `selectionchange` is dispatched at `document`, not at the editable
        // element. Deliver while the selection is inside this surface, plus one
        // final refresh when it leaves so consumers can hide anchored UI.
        let selection_was_inside = Rc::new(Cell::new(selection_is_inside(&surface)));
        let selection_listener = {
            let surface = surface.clone();
            let selection_was_inside = selection_was_inside.clone();
            let schedule = schedule.clone();
            Closure::wrap(Box::new(move |_event: Event| {
                let inside = selection_is_inside(&surface);
                let was_inside = selection_was_inside.replace(inside);
                if inside || was_inside {
                    schedule();
                }
            }) as Box<dyn FnMut(Event)>)
        };

        // Focus and blur do not bubble, so listen in the capture phase and
        // filter their targets to this editor. Moving focus between descendants
        // naturally coalesces into the same animation frame.
        let focus_listener = surface_event_listener(surface.clone(), schedule.clone());
        let blur_listener = surface_event_listener(surface, schedule.clone());

        // Marks and the enclosing block can change while the DOM selection
        // stays still (for example, clicking Bold in an external toolbar).
        // Refresh after a committed editor transaction as well.
        let change_listener = Closure::wrap(Box::new(move |_event: Event| {
            schedule();
        }) as Box<dyn FnMut(Event)>);

        let _ = document.add_event_listener_with_callback(
            "selectionchange",
            selection_listener.as_ref().unchecked_ref(),
        );
        let _ = document.add_event_listener_with_callback_and_bool(
            "focus",
            focus_listener.as_ref().unchecked_ref(),
            true,
        );
        let _ = document.add_event_listener_with_callback_and_bool(
            "blur",
            blur_listener.as_ref().unchecked_ref(),
            true,
        );
        let _ = state_target.add_event_listener_with_callback(
            CHANGE_EVENT,
            change_listener.as_ref().unchecked_ref(),
        );

        Self {
            document: Some(document),
            state_target: Some(state_target),
            selection_listener: Some(selection_listener),
            focus_listener: Some(focus_listener),
            blur_listener: Some(blur_listener),
            change_listener: Some(change_listener),
            frame_callback: Some(frame_callback),
            pending_frame,
            active,
        }
    }

    fn inert() -> Self {
        Self {
            document: None,
            state_target: None,
            selection_listener: None,
            focus_listener: None,
            blur_listener: None,
            change_listener: None,
            frame_callback: None,
            pending_frame: Rc::new(Cell::new(None)),
            active: Rc::new(Cell::new(false)),
        }
    }

    /// Manually detach the observer. Equivalent to dropping the guard.
    pub fn detach(self) {
        drop(self);
    }
}

impl Drop for SelectionChangeSubscription {
    fn drop(&mut self) {
        self.active.set(false);

        if let Some(document) = &self.document {
            if let Some(listener) = &self.selection_listener {
                let _ = document.remove_event_listener_with_callback(
                    "selectionchange",
                    listener.as_ref().unchecked_ref(),
                );
            }
            if let Some(listener) = &self.focus_listener {
                let _ = document.remove_event_listener_with_callback_and_bool(
                    "focus",
                    listener.as_ref().unchecked_ref(),
                    true,
                );
            }
            if let Some(listener) = &self.blur_listener {
                let _ = document.remove_event_listener_with_callback_and_bool(
                    "blur",
                    listener.as_ref().unchecked_ref(),
                    true,
                );
            }
            if let Some(frame) = self.pending_frame.take()
                && let Some(window) = document.default_view()
            {
                let _ = window.cancel_animation_frame(frame);
            }
        }

        if let (Some(target), Some(listener)) = (&self.state_target, &self.change_listener) {
            let _ = target.remove_event_listener_with_callback(
                CHANGE_EVENT,
                listener.as_ref().unchecked_ref(),
            );
        }

        // Drop listeners before the reusable rAF callback. The event closures
        // retain a JS Function handle to that callback through `schedule`.
        self.selection_listener.take();
        self.focus_listener.take();
        self.blur_listener.take();
        self.change_listener.take();
        self.frame_callback.take();
    }
}

pub(crate) fn capture(state: &EditorState, surface: &Element) -> SelectionSnapshot {
    let selection = state.selection().clone();
    let from = selection.from(state.doc());
    let to = selection.to(state.doc());
    SelectionSnapshot {
        empty: selection.is_empty(state.doc()),
        active_mark_names: active_mark_names(state),
        enclosing_block_types: enclosing_block_types(state),
        rect: selection_union_rect(surface, state),
        focused: surface_is_focused(surface),
        editable: surface
            .dyn_ref::<HtmlElement>()
            .is_some_and(HtmlElement::is_content_editable),
        selection,
        from,
        to,
    }
}

fn active_mark_names(state: &EditorState) -> Vec<String> {
    let mut names = BTreeSet::new();
    let from = state.selection().from(state.doc());
    let to = state.selection().to(state.doc());
    if state.selection().is_cells() {
        let ranges = state
            .selection()
            .ranges(state.doc(), state.schema())
            .unwrap_or_default();
        for range in ranges {
            let _ = state
                .doc()
                .nodes_between(range.from, range.to, |node, _, _, _| {
                    names.extend(node.marks().iter().map(|mark| mark.type_name().to_string()));
                    true
                });
        }
        return names.into_iter().collect();
    }
    if from == to {
        let marks = state
            .stored_marks()
            .map(<[_]>::to_vec)
            .or_else(|| {
                state
                    .doc()
                    .resolve(from)
                    .ok()
                    .map(|pos| pos.marks(state.schema()))
            })
            .unwrap_or_default();
        names.extend(marks.into_iter().map(|mark| mark.type_name().to_string()));
    } else {
        let _ = state.doc().nodes_between(from, to, |node, _, _, _| {
            names.extend(node.marks().iter().map(|mark| mark.type_name().to_string()));
            true
        });
    }
    names.into_iter().collect()
}

fn enclosing_block_types(state: &EditorState) -> Vec<String> {
    let position = match state.selection() {
        Selection::Text { head, .. } => *head,
        Selection::Node { anchor } => *anchor,
        Selection::Cells { head_cell, .. } => *head_cell,
        Selection::All => 0,
    }
    .min(state.doc().content_size());

    let Ok(resolved) = state.doc().resolve(position) else {
        return Vec::new();
    };
    let mut types = Vec::new();
    // Depth zero is the top-level doc, which is not useful toolbar context.
    for depth in 1..=resolved.depth() {
        let Some(node) = resolved.node(depth) else {
            continue;
        };
        if state
            .schema()
            .node_type(node.type_name())
            .is_ok_and(|ty| !ty.is_inline())
        {
            types.push(node.type_name().to_string());
        }
    }

    if matches!(state.selection(), Selection::Node { .. })
        && let Some(node) = resolved.node_after()
        && state
            .schema()
            .node_type(node.type_name())
            .is_ok_and(|ty| !ty.is_inline())
        && types.last().is_none_or(|name| name != node.type_name())
    {
        types.push(node.type_name().to_string());
    }
    types
}

fn selection_union_rect(surface: &Element, state: &EditorState) -> Option<ViewportRect> {
    let model_selection = state.selection();
    if model_selection.is_cells() {
        let mut union = None;
        for position in model_selection
            .cell_positions(state.doc(), state.schema())
            .ok()?
        {
            let node = surface
                .query_selector(&format!(r#"[data-pos="{position}"]"#))
                .ok()??;
            let rect = ViewportRect::from_dom(&node.get_bounding_client_rect());
            if rect.is_visible() {
                union = Some(union.map_or(rect, |current: ViewportRect| current.union(rect)));
            }
        }
        return union;
    }
    if !selection_is_inside(surface) {
        return None;
    }
    // Pine represents a node selection as a collapsed DOM range at the node's
    // model position. Anchor component UI to the selected node itself instead
    // of exposing that caret-sized browser rectangle.
    if let Selection::Node { anchor } = model_selection
        && let Ok(Some(node)) = surface.query_selector(&format!(r#"[data-pos="{anchor}"]"#))
    {
        let rect = ViewportRect::from_dom(&node.get_bounding_client_rect());
        if rect.is_visible() {
            return Some(rect);
        }
    }
    let window = surface.owner_document()?.default_view()?;
    let selection = window.get_selection().ok().flatten()?;
    let mut union: Option<ViewportRect> = None;

    for index in 0..selection.range_count() {
        let Ok(range) = selection.get_range_at(index) else {
            continue;
        };
        let mut range_had_visible_rect = false;
        if let Some(rects) = range.get_client_rects() {
            for rect_index in 0..rects.length() {
                let Some(rect) = rects.item(rect_index) else {
                    continue;
                };
                let rect = ViewportRect::from_dom(&rect);
                if !rect.is_visible() {
                    continue;
                }
                range_had_visible_rect = true;
                union = Some(union.map_or(rect, |current| current.union(rect)));
            }
        }
        if !range_had_visible_rect {
            let rect = ViewportRect::from_dom(&range.get_bounding_client_rect());
            if rect.is_visible() {
                union = Some(union.map_or(rect, |current| current.union(rect)));
            }
        }
    }
    union
}

fn selection_is_inside(surface: &Element) -> bool {
    let Some(window) = surface.owner_document().and_then(|doc| doc.default_view()) else {
        return false;
    };
    let Some(selection) = window.get_selection().ok().flatten() else {
        return false;
    };
    let (Some(anchor), Some(focus)) = (selection.anchor_node(), selection.focus_node()) else {
        return false;
    };
    node_is_inside(surface, &anchor) && node_is_inside(surface, &focus)
}

fn surface_is_focused(surface: &Element) -> bool {
    surface
        .owner_document()
        .and_then(|document| document.active_element())
        .is_some_and(|active| node_is_inside(surface, active.as_ref()))
}

fn node_is_inside(surface: &Element, node: &DomNode) -> bool {
    surface.unchecked_ref::<DomNode>().is_same_node(Some(node)) || surface.contains(Some(node))
}

fn surface_event_listener(surface: Element, schedule: Rc<dyn Fn()>) -> Closure<dyn FnMut(Event)> {
    Closure::wrap(Box::new(move |event: Event| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(target) = target.dyn_into::<DomNode>() else {
            return;
        };
        if node_is_inside(&surface, &target) {
            schedule();
        }
    }) as Box<dyn FnMut(Event)>)
}

#[cfg(test)]
mod tests {
    use super::{ViewportRect, active_mark_names, enclosing_block_types};
    use crate::model::Attrs;
    use crate::schema_basic::{doc, heading, text};
    use crate::state::{EditorState, EditorStateConfig, Selection};

    #[test]
    fn viewport_rect_union_covers_both_rectangles() {
        let left = ViewportRect {
            x: 10.0,
            y: 20.0,
            width: 20.0,
            height: 10.0,
            top: 20.0,
            right: 30.0,
            bottom: 30.0,
            left: 10.0,
        };
        let right = ViewportRect {
            x: 25.0,
            y: 15.0,
            width: 20.0,
            height: 25.0,
            top: 15.0,
            right: 45.0,
            bottom: 40.0,
            left: 25.0,
        };
        assert_eq!(
            left.union(right),
            ViewportRect {
                x: 10.0,
                y: 15.0,
                width: 35.0,
                height: 25.0,
                top: 15.0,
                right: 45.0,
                bottom: 40.0,
                left: 10.0,
            }
        );
    }

    #[test]
    fn snapshot_context_uses_live_selection_head_and_marks() {
        let schema = crate::schema_basic::schema();
        let strong = schema.mark("strong", Attrs::new()).unwrap();
        let content = text("bold", vec![strong]).unwrap();
        let document = doc(vec![heading(2, vec![content]).unwrap()]).unwrap();
        let state = EditorState::create(
            EditorStateConfig::new(schema, document).selection(Selection::text_between(1, 5)),
        )
        .unwrap();

        assert_eq!(active_mark_names(&state), ["strong"]);
        assert_eq!(enclosing_block_types(&state), ["heading"]);
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use js_sys::Promise;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::JsValue;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
    use web_sys::{Document, Element, Event};

    use super::{SelectionChangeSubscription, SelectionSnapshot};
    use crate::state::Selection;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test(async)]
    async fn observer_filters_coalesces_and_detaches() {
        let window = web_sys::window().unwrap();
        let document = window.document().unwrap();
        let body = document.body().unwrap();
        let surface = editable(&document, "observer surface");
        let other = editable(&document, "other surface");
        body.append_child(&surface).unwrap();
        body.append_child(&other).unwrap();

        let calls = Rc::new(Cell::new(0));
        let calls_for_callback = calls.clone();
        let subscription = SelectionChangeSubscription::subscribe(
            surface.clone(),
            Some(surface.clone()),
            || Some(test_snapshot()),
            move |_| calls_for_callback.set(calls_for_callback.get() + 1),
        );

        select_text(&document, &other);
        document
            .dispatch_event(&Event::new("selectionchange").unwrap())
            .unwrap();
        next_frame().await;
        assert_eq!(calls.get(), 0, "another editor must be ignored");

        select_text(&document, &surface);
        let event = Event::new("selectionchange").unwrap();
        document.dispatch_event(&event).unwrap();
        document.dispatch_event(&event).unwrap();
        next_frame().await;
        assert_eq!(calls.get(), 1, "same-frame events must coalesce");

        select_text(&document, &other);
        document
            .dispatch_event(&Event::new("selectionchange").unwrap())
            .unwrap();
        next_frame().await;
        assert_eq!(calls.get(), 2, "leaving emits one final refresh");

        select_text(&document, &surface);
        document
            .dispatch_event(&Event::new("selectionchange").unwrap())
            .unwrap();
        drop(subscription);
        next_frame().await;
        assert_eq!(calls.get(), 2, "drop cancels the queued frame");

        surface.remove();
        other.remove();
    }

    fn editable(document: &Document, text: &str) -> Element {
        let element = document.create_element("div").unwrap();
        element.set_attribute("contenteditable", "true").unwrap();
        element.set_text_content(Some(text));
        element
    }

    fn select_text(document: &Document, element: &Element) {
        let text = element.first_child().unwrap();
        let range = document.create_range().unwrap();
        range.set_start(&text, 0).unwrap();
        range.set_end(&text, 1).unwrap();
        let selection = document
            .default_view()
            .unwrap()
            .get_selection()
            .unwrap()
            .unwrap();
        selection.remove_all_ranges().unwrap();
        selection.add_range(&range).unwrap();
    }

    async fn next_frame() {
        let promise = Promise::new(&mut |resolve, _reject| {
            let callback = Closure::once_into_js(move |_timestamp: f64| {
                let _ = resolve.call0(&JsValue::NULL);
            });
            web_sys::window()
                .unwrap()
                .request_animation_frame(callback.unchecked_ref())
                .unwrap();
        });
        JsFuture::from(promise).await.unwrap();
    }

    fn test_snapshot() -> SelectionSnapshot {
        SelectionSnapshot {
            selection: Selection::text(0),
            from: 0,
            to: 0,
            empty: true,
            active_mark_names: Vec::new(),
            enclosing_block_types: Vec::new(),
            rect: None,
            focused: true,
            editable: true,
        }
    }
}
