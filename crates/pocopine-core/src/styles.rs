//! Per-component stylesheet injection.
//!
//! [`inject_style`] appends a `<style data-pp-component="<name>">` to
//! `<head>` the first time it's called for a given component name. Later
//! calls with the same name are no-ops, so the generated `register()`
//! function can call it unconditionally.

use std::cell::RefCell;
use std::collections::HashSet;

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, HtmlElement};

thread_local! {
    static INJECTED: RefCell<HashSet<&'static str>> =
        RefCell::new(HashSet::new());
}

/// Inject a component's CSS into the document head. Idempotent per name.
pub fn inject_style(component: &'static str, css: &str) {
    let already = INJECTED.with(|s| s.borrow().contains(component));
    if already {
        return;
    }
    let Some(doc) = window_document() else { return };
    let Some(head) = doc.head() else { return };
    let Ok(style_el) = doc.create_element("style") else {
        return;
    };
    let _ = style_el.set_attribute("data-pp-component", component);
    if let Ok(style) = style_el.clone().dyn_into::<HtmlElement>() {
        style.set_inner_text(css);
    } else {
        let _: Element = style_el.clone();
        style_el.set_text_content(Some(css));
    }
    let _ = head.append_child(&style_el);
    INJECTED.with(|s| {
        s.borrow_mut().insert(component);
    });
}

fn window_document() -> Option<Document> {
    // RFC-099 — on the host (SSR), there is no document: `web_sys::window`
    // touches a wasm-imported global and panics off-wasm. Return `None`
    // so `inject_style` (called from every `register()`) is a no-op —
    // server-rendered CSS ships as a `<link>` stylesheet, not injected.
    #[cfg(not(target_arch = "wasm32"))]
    {
        None
    }
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window().and_then(|w| w.document())
    }
}
