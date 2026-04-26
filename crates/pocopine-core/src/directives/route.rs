//! `pp-route` — intercepts clicks on an `<a>` (or any element with an
//! `href`) and calls the router's `navigate` with the href, skipping a
//! full page reload.
//!
//! Falls through to normal browser behaviour when:
//!
//! * a modifier key is held (`ctrl`/`cmd`/`shift`/`alt`),
//! * `target="_blank"` is set (opens in a new tab),
//! * the `href` is an absolute URL (`http:`, `https:`, `//`, `mailto:`,
//!   `tel:`, `data:`), OR
//! * the `href` is a server-function route (`/_pocopine/...`).

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    AddEventListenerOptions, Element, Event, EventTarget, HtmlAnchorElement, MouseEvent,
};

use super::DirectiveCall;
use crate::router::navigate;

pub fn run(call: &DirectiveCall) {
    install(call.el);
}

/// Install a `pp-route` click listener on `el` that intercepts
/// in-app navigation and routes through `router::navigate`,
/// falling through to the browser default for modifier-key
/// clicks, `target="_blank"`, absolute URLs, and server-fn
/// hrefs.
///
/// Cleanup-safe install entry point — the listener's lifetime
/// ties to `el` via [`crate::walker::track_listener_on_with_opts`]
/// and drops when the element's subtree unmounts.
///
/// Replaces the historical `closure.forget()` path that leaked
/// the click listener past unmount: route links in a
/// teleported portal or a `pp-if` branch used to hold their
/// closure (and the captured Element clone) for the lifetime
/// of the wasm module. The new install path lets
/// `release_subtree` drop both.
pub fn install(el: &Element) {
    let el_for_closure = el.clone();
    let closure = Closure::wrap(Box::new(move |ev: Event| {
        // Modifier keys / middle-click → let the browser do its thing.
        if let Some(mouse) = ev.dyn_ref::<MouseEvent>() {
            if mouse.ctrl_key()
                || mouse.meta_key()
                || mouse.shift_key()
                || mouse.alt_key()
                || mouse.button() != 0
            {
                return;
            }
        }

        // Only anchors in v0 — any element with an href could work, but
        // anchors are the canonical case and keep the behaviour
        // predictable.
        let Some(a) = el_for_closure.dyn_ref::<HtmlAnchorElement>() else {
            return;
        };

        if a.target() == "_blank" {
            return;
        }

        let href = a.get_attribute("href").unwrap_or_default();
        if is_external(&href) || href.starts_with("/_pocopine/") {
            return;
        }

        ev.prevent_default();
        navigate(&href);
    }) as Box<dyn FnMut(Event)>);

    let target: EventTarget = el.clone().into();
    let opts = AddEventListenerOptions::new();
    crate::walker::track_listener_on_with_opts(el, target, "click", &opts, closure);
}

fn is_external(href: &str) -> bool {
    // Cheap prefix check — full URL parsing is overkill here.
    href.starts_with("http://")
        || href.starts_with("https://")
        || href.starts_with("//")
        || href.starts_with("mailto:")
        || href.starts_with("tel:")
        || href.starts_with("data:")
        || href.starts_with("ws://")
        || href.starts_with("wss://")
}
