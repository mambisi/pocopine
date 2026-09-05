//! Focus utilities — RFC-014.
//!
//! Imperative primitives every overlay / dialog / popover component
//! needs: save the currently-focused element, restore it later, trap
//! focus inside a container while a modal is open, blur the active
//! element, auto-focus the first focusable inside a freshly-mounted
//! container.

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{Element, FocusEvent, HtmlElement, KeyboardEvent, Node};

/// CSS selector matching everything the trap / auto-focus treat as
/// focusable.
pub const FOCUSABLE_SELECTOR: &str = concat!(
    "a[href], area[href], button:not([disabled]), ",
    "input:not([disabled]):not([type=hidden]), ",
    "select:not([disabled]), textarea:not([disabled]), ",
    "[tabindex]:not([tabindex=\"-1\"])"
);

/// Snapshot of the currently-focused element, returned by [`save`].
pub struct Saved(Option<HtmlElement>);

/// Capture `document.activeElement` for later restoration.
pub fn save() -> Saved {
    Saved(active_html_element())
}

/// Refocus the element captured by [`save`]. No-op when the original
/// element is gone from the DOM.
pub fn restore(saved: Saved) {
    let Some(el) = saved.0 else { return };
    let node: &Node = el.as_ref();
    if !node.is_connected() {
        return;
    }
    focus_no_scroll(&el);
}

/// Programmatic focus that does not scroll the element into view.
///
/// Default `Element.focus()` calls `scrollIntoView` if the focused
/// element isn't already visible — but browsers can also scroll
/// the page *even when the target is visible* (rounding errors,
/// stacking-context edges). For keyboard / click-driven focus
/// transfers inside overlays (trap, auto-focus, roving arrow
/// navigation), we never want the page to jump. This calls
/// `el.focus({ preventScroll: true })` unconditionally.
pub fn focus_no_scroll(el: &HtmlElement) {
    let opts = Object::new();
    let _ = Reflect::set(&opts, &JsValue::from_str("preventScroll"), &JsValue::TRUE);
    let Ok(v) = Reflect::get(el.as_ref(), &JsValue::from_str("focus")) else {
        let _ = el.focus();
        return;
    };
    let Ok(f) = v.dyn_into::<Function>() else {
        let _ = el.focus();
        return;
    };
    let _ = f.call1(el.as_ref(), &opts);
}

/// Programmatic focus for a generic DOM element.
///
/// This is the [`Element`] counterpart to [`focus_no_scroll`]. It is
/// useful when the caller got the target through `refs::get_on` and
/// does not need to name a concrete `web_sys` element type.
pub fn focus_element_no_scroll(el: &Element) {
    let Ok(html) = el.clone().dyn_into::<HtmlElement>() else {
        return;
    };
    focus_no_scroll(&html);
}

/// Blur whatever currently has focus. No-op when nothing is focused
/// or the active element is `<body>` (browsers report `body` as
/// `activeElement` when "nothing" is focused; blurring it has
/// surprising side-effects on Safari scroll).
pub fn blur() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else { return };
    let Some(active) = doc.active_element() else {
        return;
    };
    if let Some(body) = doc.body() {
        let body_el: &Element = body.as_ref();
        if active == *body_el {
            return;
        }
    }
    if let Ok(html) = active.dyn_into::<HtmlElement>() {
        let _ = html.blur();
    }
}

/// Find the first focusable element inside `container` and focus it.
/// Returns `true` iff a target was found.
pub fn auto_focus_first(container: &Element) -> bool {
    let Ok(Some(first)) = container.query_selector(FOCUSABLE_SELECTOR) else {
        return false;
    };
    if let Ok(html) = first.dyn_into::<HtmlElement>() {
        focus_no_scroll(&html);
        return true;
    }
    false
}

/// Install a focus trap inside `container`. Tab / Shift+Tab cycle
/// within the container's focusable descendants; `focusin` pulls
/// focus back if it escapes via mouse / JS.
pub fn trap(container: &Element) -> TrapHandle {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return TrapHandle::empty();
    };

    let container_for_key = container.clone();
    let keydown: Closure<dyn FnMut(KeyboardEvent)> =
        Closure::wrap(Box::new(move |ev: KeyboardEvent| {
            if ev.key() != "Tab" {
                return;
            }
            let container = &container_for_key;
            let node: &Node = container.as_ref();
            if !node.is_connected() {
                return;
            }
            let focusables = collect_focusables(container);
            if focusables.is_empty() {
                ev.prevent_default();
                return;
            }
            let last_idx = focusables.len() - 1;
            let cur_idx =
                active_html_element().and_then(|a| focusables.iter().position(|f| *f == a));
            let next_idx = match (ev.shift_key(), cur_idx) {
                (false, None) => 0,
                (false, Some(i)) if i >= last_idx => 0,
                (false, Some(i)) => i + 1,
                (true, None) => last_idx,
                (true, Some(0)) => last_idx,
                (true, Some(i)) => i - 1,
            };
            ev.prevent_default();
            focus_no_scroll(&focusables[next_idx]);
        }) as Box<dyn FnMut(KeyboardEvent)>);

    // `dyn Fn`, not `dyn FnMut`: the corrective `focus()` below dispatches
    // the next `focusin` synchronously, before this listener has returned,
    // so the listener re-enters itself. wasm-bindgen refuses re-entry into
    // an `FnMut` closure ("closure invoked recursively or after being
    // dropped") and the nested dispatch became an uncaught error on every
    // correction. The body only reads `container`, so nothing needs `&mut`.
    let container_for_in = container.clone();
    let focusin: Closure<dyn Fn(FocusEvent)> = Closure::wrap(Box::new(move |ev: FocusEvent| {
        let container = &container_for_in;
        let node: &Node = container.as_ref();
        if !node.is_connected() {
            return;
        }
        let Some(target) = ev.target() else { return };
        let Ok(target_node) = target.dyn_into::<Node>() else {
            return;
        };
        if container.contains(Some(&target_node)) {
            return;
        }
        let focusables = collect_focusables(container);
        if let Some(first) = focusables.first() {
            focus_no_scroll(first);
        }
    }) as Box<dyn Fn(FocusEvent)>);

    let target: &web_sys::EventTarget = doc.as_ref();
    let _ = target.add_event_listener_with_callback("keydown", keydown.as_ref().unchecked_ref());
    let _ = target.add_event_listener_with_callback("focusin", focusin.as_ref().unchecked_ref());

    TrapHandle {
        inner: Some(TrapInner {
            doc,
            keydown,
            focusin,
        }),
    }
}

/// Handle returned by [`trap`]. Drop or call [`TrapHandle::release`]
/// to remove the document listeners.
pub struct TrapHandle {
    inner: Option<TrapInner>,
}

struct TrapInner {
    doc: web_sys::Document,
    keydown: Closure<dyn FnMut(KeyboardEvent)>,
    focusin: Closure<dyn Fn(FocusEvent)>,
}

impl TrapHandle {
    fn empty() -> Self {
        Self { inner: None }
    }

    pub fn release(mut self) {
        self.tear_down();
    }

    fn tear_down(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let target: &web_sys::EventTarget = inner.doc.as_ref();
        let _ = target
            .remove_event_listener_with_callback("keydown", inner.keydown.as_ref().unchecked_ref());
        let _ = target
            .remove_event_listener_with_callback("focusin", inner.focusin.as_ref().unchecked_ref());
    }
}

impl Drop for TrapHandle {
    fn drop(&mut self) {
        self.tear_down();
    }
}

fn active_html_element() -> Option<HtmlElement> {
    let window = web_sys::window()?;
    let doc = window.document()?;
    let active = doc.active_element()?;
    active.dyn_into::<HtmlElement>().ok()
}

fn collect_focusables(container: &Element) -> Vec<HtmlElement> {
    let Ok(list) = container.query_selector_all(FOCUSABLE_SELECTOR) else {
        return Vec::new();
    };
    let len = list.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let Some(node) = list.item(i) else { continue };
        if let Ok(el) = node.dyn_into::<HtmlElement>() {
            out.push(el);
        }
    }
    out
}

#[doc(hidden)]
pub fn __focusable_count(container: &Element) -> usize {
    collect_focusables(container).len()
}
