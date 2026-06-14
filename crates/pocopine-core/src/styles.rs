//! Per-component stylesheet injection.
//!
//! [`inject_style`] appends a `<style data-pp-component="<name>">` to
//! `<head>` the first time it's called for a given component name. Later
//! calls with the same name are no-ops, so the generated `register()`
//! function can call it unconditionally.
//!
//! RFC-099 — on the host (SSR) there is no document, so instead of
//! injecting, `inject_style` *records* each component's CSS in
//! registration order. [`collected_style_tags`] then renders them as
//! `<style>` blocks the SSR document embeds, so the server-rendered page
//! paints fully styled before the wasm bundle boots. The wasm side dedups
//! against those server-emitted tags so hydration doesn't double-inject.

use std::cell::RefCell;
use std::collections::HashSet;

thread_local! {
    static INJECTED: RefCell<HashSet<&'static str>> =
        RefCell::new(HashSet::new());
}

#[cfg(not(target_arch = "wasm32"))]
thread_local! {
    /// Host (SSR) CSS registry: `(component, css)` in registration order.
    static COLLECTED: RefCell<Vec<(&'static str, String)>> = const { RefCell::new(Vec::new()) };
}

/// Inject a component's CSS into the document head. Idempotent per name.
/// On the host (no document) the CSS is recorded for SSR emission instead.
pub fn inject_style(component: &'static str, css: &str) {
    let newly = INJECTED.with(|s| s.borrow_mut().insert(component));
    if !newly {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        COLLECTED.with(|c| c.borrow_mut().push((component, css.to_string())));
    }
    #[cfg(target_arch = "wasm32")]
    {
        inject_style_dom(component, css);
    }
}

/// RFC-099 — render every component's CSS recorded on the host into the
/// `<style data-pp-component>` blocks the SSR document embeds. Returns the
/// empty string before any component registered, and always on the client
/// (which injects styles live into the DOM, with no host registry).
pub fn collected_style_tags() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        COLLECTED.with(|c| {
            c.borrow()
                .iter()
                .map(|(name, css)| format!("<style data-pp-component=\"{name}\">{css}</style>"))
                .collect()
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        String::new()
    }
}

#[cfg(target_arch = "wasm32")]
fn inject_style_dom(component: &'static str, css: &str) {
    use wasm_bindgen::JsCast;
    use web_sys::HtmlElement;

    let Some(doc) = window_document() else {
        return;
    };
    // RFC-099 — if the server already emitted this component's <style>
    // (SSR), adopt it instead of injecting a duplicate.
    if let Ok(Some(_)) = doc.query_selector(&format!("style[data-pp-component=\"{component}\"]")) {
        return;
    }
    let Some(head) = doc.head() else { return };
    let Ok(style_el) = doc.create_element("style") else {
        return;
    };
    let _ = style_el.set_attribute("data-pp-component", component);
    if let Ok(style) = style_el.clone().dyn_into::<HtmlElement>() {
        style.set_inner_text(css);
    } else {
        style_el.set_text_content(Some(css));
    }
    let _ = head.append_child(&style_el);
}

#[cfg(target_arch = "wasm32")]
fn window_document() -> Option<web_sys::Document> {
    web_sys::window().and_then(|w| w.document())
}
