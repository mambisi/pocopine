//! `pp-on:event[.mod]*="handler"` — event listeners that dispatch to a
//! macro-generated handler on the scope's `ComponentState`.
//!
//! Supported modifiers: `prevent`, `stop`, `once`, `self`, `window`,
//! `document`. More (debounce/throttle/passive) can slot in later.

use js_sys::Array;
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

    let el_for_closure = el.clone();
    let closure = Closure::wrap(Box::new(move |ev: Event| {
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
        let args = Array::new();
        args.push(&ev);
        invoke_handler(scope_id, &handler, &args);
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

    // The listener must outlive this call; hand ownership to JS.
    closure.forget();
}
