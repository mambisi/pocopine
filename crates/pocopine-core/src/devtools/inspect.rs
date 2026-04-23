//! "Pick an element on the page" mode.
//!
//! Flipping this on attaches document-level mouseover + capture-phase
//! click listeners that let the user pick a DOM element. On click:
//!   - prevent default + stop propagation (so app handlers don't run),
//!   - walk up to the nearest scope-owning element,
//!   - set the devtools selection to that scope,
//!   - flip inspect mode back off,
//!   - scroll the matching tree row into view and flash it.
//!
//! The two listeners live on `document` — hovering isn't constrained
//! to a specific subtree. They store their `Closure`s in a thread-
//! local so they can be removed cleanly when inspect mode turns off.

use std::cell::{Cell, RefCell};

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{window, Element, Event, Node};

use super::{event as dev_event, highlight, panel, shell};
use crate::walker;

type EventClosure = Closure<dyn FnMut(Event)>;

thread_local! {
    static INSPECT_MODE: Cell<bool> = const { Cell::new(false) };
    static DOC_OVER_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static DOC_CLICK_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
}

pub(super) fn is_on() -> bool {
    INSPECT_MODE.with(|c| c.get())
}

/// Flip inspect mode. When turning on, install the doc listeners;
/// when turning off, remove them and clear any highlight.
pub(super) fn toggle() {
    let next = !INSPECT_MODE.with(|c| c.get());
    INSPECT_MODE.with(|c| c.set(next));
    if next {
        attach();
    } else {
        detach();
        highlight::apply(None);
    }
    panel::invalidate_active();
}

fn attach() {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return;
    };

    let over: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        if !INSPECT_MODE.with(|c| c.get()) {
            return;
        }
        let Some(target) = ev.target() else { return };
        let Ok(el) = target.dyn_into::<Element>() else {
            return;
        };
        if is_inside_panel(&el) {
            return;
        }
        highlight::apply(Some(el));
    }));
    let _ = doc.add_event_listener_with_callback("mouseover", over.as_ref().unchecked_ref());
    DOC_OVER_CB.with(|c| *c.borrow_mut() = Some(over));

    // Capture-phase click: swallow the click before any app handler
    // runs while the user is picking.
    let click: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        if !INSPECT_MODE.with(|c| c.get()) {
            return;
        }
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else {
            return;
        };
        if is_inside_panel(&start) {
            return;
        }
        ev.prevent_default();
        ev.stop_propagation();

        let maybe_scope = walker::enclosing_scope(&start).map(|(id, _)| id);
        if let Some(scope_id) = maybe_scope {
            // Scope panel is the only consumer for now; we dispatch
            // a synthetic selection directly through its state. Once
            // more panels want a "pick" signal, factor into a panel
            // hook.
            super::panels::scope::select(scope_id);
            highlight::set_from_scope(Some(scope_id));
        } else {
            highlight::apply(None);
        }

        INSPECT_MODE.with(|c| c.set(false));
        detach();
        super::render();

        if let Some(scope_id) = maybe_scope {
            dev_event::scroll_into_view_and_flash(scope_id);
        }
    }));
    let _ = doc.add_event_listener_with_callback_and_bool(
        "click",
        click.as_ref().unchecked_ref(),
        true,
    );
    DOC_CLICK_CB.with(|c| *c.borrow_mut() = Some(click));
}

fn detach() {
    let Some(doc) = window().and_then(|w| w.document()) else {
        return;
    };
    if let Some(cb) = DOC_OVER_CB.with(|c| c.borrow_mut().take()) {
        let _ = doc.remove_event_listener_with_callback("mouseover", cb.as_ref().unchecked_ref());
    }
    if let Some(cb) = DOC_CLICK_CB.with(|c| c.borrow_mut().take()) {
        let _ = doc.remove_event_listener_with_callback_and_bool(
            "click",
            cb.as_ref().unchecked_ref(),
            true,
        );
    }
}

fn is_inside_panel(el: &Element) -> bool {
    let Some(root) = shell::panel_root() else {
        return false;
    };
    let root_node: &Node = root.as_ref();
    let el_node: &Node = el.as_ref();
    root_node.contains(Some(el_node))
}
