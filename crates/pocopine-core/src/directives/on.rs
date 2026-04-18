//! `pp-on:event[.mod]*="handler"` — event listeners that dispatch to a
//! macro-generated handler on the scope's `ComponentState`.
//!
//! Supported modifiers:
//!
//! | modifier           | effect                                                |
//! |--------------------|-------------------------------------------------------|
//! | `prevent`          | calls `preventDefault()` before dispatch              |
//! | `stop`             | calls `stopPropagation()`                             |
//! | `self`             | only fires when `event.target === el`                  |
//! | `once`             | browser removes the listener after one fire           |
//! | `window`           | attach to `window` instead of `el`                    |
//! | `document`         | attach to `document` instead of `el`                  |
//! | `debounce[.<ms>]`  | wait `ms` (default 300) of quiet after the last event |

use std::cell::Cell;
use std::rc::Rc;

use js_sys::{Array, Function};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{AddEventListenerOptions, Event, EventTarget};

use super::DirectiveCall;
use crate::scope::invoke_handler;

pub fn run(call: &DirectiveCall) {
    let Some(event) = call.arg.clone() else { return };
    let handler = call.value.clone();
    let scope_id = call.scope_id;
    let el = call.el.clone();

    let prevent = call.modifiers.iter().any(|m| m == "prevent");
    let stop = call.modifiers.iter().any(|m| m == "stop");
    let self_only = call.modifiers.iter().any(|m| m == "self");
    let once = call.modifiers.iter().any(|m| m == "once");
    let on_window = call.modifiers.iter().any(|m| m == "window");
    let on_document = call.modifiers.iter().any(|m| m == "document");
    let debounce_ms: Option<u32> = parse_debounce(&call.modifiers);

    // Persistent closure used by `setTimeout` in the debounce branch.
    // Built once per listener so rapid events don't allocate a fresh
    // JS closure each time.
    let invoke_fn: Function = {
        let handler = handler.clone();
        let c = Closure::wrap(Box::new(move || {
            invoke_handler(scope_id, &handler, &Array::new());
        }) as Box<dyn FnMut()>);
        let f: Function = c.as_ref().unchecked_ref::<Function>().clone();
        c.forget();
        f
    };

    let window = web_sys::window().expect("window");
    let timer: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));

    let el_for_closure = el.clone();
    let closure = Closure::wrap(Box::new({
        let handler = handler.clone();
        let invoke_fn = invoke_fn.clone();
        let window = window.clone();
        let timer = timer.clone();
        move |ev: Event| {
            if prevent {
                ev.prevent_default();
            }
            if stop {
                ev.stop_propagation();
            }
            if self_only {
                if let Some(target) = ev.target() {
                    if target != *el_for_closure.as_ref() {
                        return;
                    }
                }
            }
            if let Some(ms) = debounce_ms {
                // Cancel any pending fire from a prior keystroke.
                if let Some(prev) = timer.take() {
                    window.clear_timeout_with_handle(prev);
                }
                let handle = window
                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                        &invoke_fn,
                        ms as i32,
                    )
                    .unwrap_or(0);
                timer.set(Some(handle));
            } else {
                let args = Array::new();
                args.push(&ev);
                invoke_handler(scope_id, &handler, &args);
            }
        }
    }) as Box<dyn FnMut(Event)>);

    let target: EventTarget = if on_window {
        web_sys::window().expect("window").into()
    } else if on_document {
        web_sys::window()
            .and_then(|w| w.document())
            .expect("document")
            .into()
    } else {
        el.clone().into()
    };

    let opts = AddEventListenerOptions::new();
    opts.set_once(once);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        &event,
        closure.as_ref().unchecked_ref(),
        &opts,
    );
    closure.forget();
}

/// Scan the modifier list for `debounce` and the optional numeric
/// modifier that follows it (`pp-on:input.debounce.500="foo"` →
/// 500ms; bare `debounce` → 300ms default).
fn parse_debounce(modifiers: &[String]) -> Option<u32> {
    for (i, m) in modifiers.iter().enumerate() {
        if m == "debounce" {
            let ms = modifiers
                .get(i + 1)
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(300);
            return Some(ms);
        }
    }
    None
}
