//! Delegated click + hover handlers on the panel root.
//!
//! A single `click` listener on the root avoids per-row leaks —
//! every render blows away inner HTML, so per-node listeners would
//! pile up fast. The delegate walks `data-action="..."` ancestors,
//! dispatches shell-level actions inline, forwards everything else
//! to the active panel via [`super::panel::dispatch_action_to_active`].
//!
//! A separate `mouseover`/`mouseleave` pair lights up the DOM element
//! owning the scope the pointer is currently over — that's the "hover
//! a tree row → outline the live element on the page" feature.

use std::cell::RefCell;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{window, Element, Event};

use crate::reactive::ScopeId;

use super::{highlight, panel, shell};

type EventClosure = Closure<dyn FnMut(Event)>;

thread_local! {
    static CLICK_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static OVER_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static LEAVE_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
}

/// Install the click + hover delegates on `root`. Called once, from
/// the shell's `ensure_root` path.
pub(super) fn attach(root: &Element) {
    attach_click(root);
    attach_hover(root);
}

fn attach_click(root: &Element) {
    let cb: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else {
            return;
        };
        let mut cur = Some(start);
        while let Some(el) = cur {
            if let Some(action) = el.get_attribute("data-action") {
                handle_action(&action, &el);
                return;
            }
            if let Some(value) = el.get_attribute("data-copy") {
                copy_to_clipboard(&value);
                flash_copied(&el);
                return;
            }
            cur = el.parent_element();
        }
    }));
    let _ = root.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
    CLICK_CB.with(|c| *c.borrow_mut() = Some(cb));
}

fn attach_hover(root: &Element) {
    // mouseover — walk up to the nearest `data-scope-id` ancestor and
    // light up the corresponding element on the page.
    let over: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else {
            return;
        };
        let mut cur = Some(start);
        while let Some(el) = cur {
            if let Some(id_str) = el.get_attribute("data-scope-id") {
                if let Ok(n) = id_str.parse::<u64>() {
                    highlight::set_from_scope(Some(ScopeId(n)));
                    return;
                }
            }
            cur = el.parent_element();
        }
    }));
    let _ = root.add_event_listener_with_callback("mouseover", over.as_ref().unchecked_ref());
    OVER_CB.with(|c| *c.borrow_mut() = Some(over));

    let leave: EventClosure = Closure::wrap(Box::new(move |_ev: Event| {
        highlight::set_from_scope(None);
    }));
    let _ = root.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());
    LEAVE_CB.with(|c| *c.borrow_mut() = Some(leave));
}

/// Shell-level actions are handled inline here. Anything unknown
/// forwards to the active panel. Shell and panel action names must
/// not collide (today: "close", "toggle-inspect", "toggle-collapse",
/// "select-panel" are shell; "set-mode", "select-scope" are scope
/// panel).
fn handle_action(action: &str, el: &Element) {
    match action {
        "close" => {
            super::toggle();
            return;
        }
        "toggle-inspect" => {
            super::inspect::toggle();
            super::render();
            return;
        }
        "toggle-collapse" => {
            shell::toggle_collapse();
            super::render();
            return;
        }
        "select-panel" => {
            if let Some(idx_str) = el.get_attribute("data-panel-idx") {
                if let Ok(idx) = idx_str.parse::<usize>() {
                    panel::set_active(idx);
                    panel::invalidate_active();
                    super::render();
                }
            }
            return;
        }
        _ => {}
    }
    // Fall through to the active panel.
    if panel::dispatch_action_to_active(action, el) {
        super::render();
    }
}

fn copy_to_clipboard(value: &str) {
    let Some(win) = window() else { return };
    let clipboard = win.navigator().clipboard();
    // Fire-and-forget; we don't await the promise.
    let _ = clipboard.write_text(value);
}

/// Briefly tag the clicked element with `__pp_dev_copied` so the
/// user sees the copy happened. The class clears on a 450ms
/// `setTimeout`; the next render (≤200ms) will also overwrite the
/// node anyway.
fn flash_copied(el: &Element) {
    let cls = el.class_name();
    let needs_add = !cls.split_whitespace().any(|c| c == "__pp_dev_copied");
    if needs_add {
        el.set_class_name(format!("{cls} __pp_dev_copied").trim());
    }
    let el_for_reset = el.clone();
    let cb: Closure<dyn FnMut()> = Closure::once(Box::new(move || {
        let kept: String = el_for_reset
            .class_name()
            .split_whitespace()
            .filter(|c| *c != "__pp_dev_copied")
            .collect::<Vec<_>>()
            .join(" ");
        el_for_reset.set_class_name(&kept);
    }));
    if let Some(win) = window() {
        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            450,
        );
    }
    cb.forget();
}

/// Programmatic helper for the inspect-picker: after an inspect
/// click commits a scope selection, scroll its tree row into view
/// and flash it so the user sees where it landed.
pub(super) fn scroll_into_view_and_flash(scope_id: ScopeId) {
    let Some(root) = shell::panel_root() else {
        return;
    };
    let sel = format!("[data-scope-id=\"{}\"]", scope_id.0);
    if let Ok(Some(row)) = root.query_selector(&sel) {
        row.scroll_into_view();
        flash_copied(&row);
    }
}
