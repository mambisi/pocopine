//! `pp-text="<expr>"` — set `textContent` from a template expression
//! (RFC-012).

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::mount::track_effect_on;

/// Install a `pp-text` effect on `el` that writes `expr`'s
/// stringified value into the element's `textContent` and
/// re-runs whenever the expression's reactive dependencies
/// change. The effect's lifetime is tracked to `el` via
/// [`crate::mount::track_effect_on`] so it's released when
/// the element's subtree is torn down.
///
/// This is the cleanup-safe install entry point. Callers (the
/// plan applier) parse and validate the expression first; this
/// function does the install only.
/// RFC-096 S3 — typed fast lane for `pp-text="<single field>"`:
/// scalar fields render `Rust value → String → set_text_content`
/// with no serde-to-JS and no `JsValue`. Compound fields fall
/// back to `evaluator` (the projection path) — decided per run
/// by the state's own `field_as_text`, which is stable for a
/// field's lifetime.
#[doc(hidden)]
pub fn install_fast(
    el: &Element,
    scope_id: crate::reactive::ScopeId,
    key: &'static str,
    proxy: &JsValue,
    evaluator: Rc<dyn Fn(&JsValue) -> JsValue>,
) {
    let el_owned = el.clone();
    let proxy_owned = proxy.clone();
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let id = crate::reactive::effect_install(move |suppressed| {
        let next = match crate::scope::read_field_text(scope_id, key) {
            Some(Some(s)) => s,
            Some(None) => js_to_string(&evaluator(&proxy_owned)),
            None => return,
        };
        {
            let p = prev.borrow();
            if p.as_deref() == Some(next.as_str()) {
                return;
            }
        }
        // RFC-099 — self-healing claim: on the hydration pass skip the
        // write only if the server already rendered exactly this text;
        // otherwise (a value derived in `on_setup`, which the server
        // doesn't run) the DOM differs and we write to fill it in.
        if suppressed && el_owned.text_content().as_deref() == Some(next.as_str()) {
            *prev.borrow_mut() = Some(next);
            return;
        }
        el_owned.set_text_content(Some(&next));
        *prev.borrow_mut() = Some(next);
    });
    track_effect_on(el, id);
}

#[doc(hidden)]
pub fn install_eval(el: &Element, proxy: &JsValue, evaluator: Rc<dyn Fn(&JsValue) -> JsValue>) {
    let el_owned = el.clone();
    let proxy_owned = proxy.clone();
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let id = crate::reactive::effect_install(move |suppressed| {
        // RFC-095 — no ambient-element wrap: binding expressions
        // are pure reads (`$el` is gone; calls only resolve in
        // pp-on, which binds its own ambient context).
        let v = evaluator(&proxy_owned);
        let next = js_to_string(&v);
        {
            let p = prev.borrow();
            if p.as_deref() == Some(next.as_str()) {
                return;
            }
        }
        // RFC-099 — self-healing claim: skip the write only if the server
        // already rendered exactly this text; otherwise fill it in.
        if suppressed && el_owned.text_content().as_deref() == Some(next.as_str()) {
            *prev.borrow_mut() = Some(next);
            return;
        }
        el_owned.set_text_content(Some(&next));
        *prev.borrow_mut() = Some(next);
    });
    track_effect_on(el, id);
}

fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(crate::text::js_number_string))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}
