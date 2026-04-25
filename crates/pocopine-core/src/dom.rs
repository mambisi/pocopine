//! Tiny DOM accessors — `dom::window()` / `dom::document()` /
//! `dom::body()`. Exists purely to dedupe the
//! `web_sys::window().and_then(|w| w.document())` chain that
//! showed up ~30 times across pine.
//!
//! Each helper returns `Option` rather than panicking. The bare
//! `web_sys` call sites ignored failure with `.and_then` chains
//! anyway — the abstraction matches that semantic.
//!
//! When you need to assume the host *is* a browser (most of pine),
//! `.expect("…")` at the call site keeps the failure mode visible.

use web_sys::{Document, HtmlElement, Window};

/// `web_sys::window()` — the browser's `Window`, or `None` in
/// non-browser hosts (SSR, wasm-bindgen-test --node without a
/// global window).
#[inline]
pub fn window() -> Option<Window> {
    web_sys::window()
}

/// `window.document` — `None` if there's no window, or the
/// extremely rare browser state with a window but no document
/// (e.g. inside a `Worker`).
#[inline]
pub fn document() -> Option<Document> {
    window().and_then(|w| w.document())
}

/// `document.body` — `None` in the same conditions as
/// [`document`], plus the brief moment before `<body>` exists
/// (parsed before `</head>`).
#[inline]
pub fn body() -> Option<HtmlElement> {
    document().and_then(|d| d.body())
}

/// `document.documentElement` (the `<html>` element). Useful for
/// theme attribute toggles + scroll-locking the page.
#[inline]
pub fn document_element() -> Option<web_sys::Element> {
    document().and_then(|d| d.document_element())
}
