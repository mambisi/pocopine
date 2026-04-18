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
use web_sys::{Event, HtmlAnchorElement, MouseEvent};

use super::DirectiveCall;
use crate::router::navigate;

pub fn run(call: &DirectiveCall) {
    let el = call.el.clone();

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
        let Some(a) = el.dyn_ref::<HtmlAnchorElement>() else { return };

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

    let _ = call
        .el
        .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref());
    closure.forget();
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
