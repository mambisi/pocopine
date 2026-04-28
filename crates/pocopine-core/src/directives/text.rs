//! `pp-text="<expr>"` — set `textContent` from a template expression
//! (RFC-012).

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::expr::{self, Spanned};
use crate::mount::track_effect_on;
use crate::reactive::effect;
use crate::scope::with_current_el;

/// Install a `pp-text` effect on `el` that writes `expr`'s
/// stringified value into the element's `textContent` and
/// re-runs whenever the expression's reactive dependencies
/// change. The effect's lifetime is tracked to `el` via
/// [`crate::mount::track_effect_on`] so it's released when
/// the element's subtree is torn down.
///
/// This is the cleanup-safe install entry point. Callers (the
/// runtime mount, future generated views) parse and validate
/// `expr` first; this function does the install only.
pub fn install(el: &Element, proxy: &JsValue, ast: Spanned<expr::Expr>) {
    let el_owned = el.clone();
    let proxy_owned = proxy.clone();
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let id = effect(move || {
        let el_for_magic = el_owned.clone();
        let prev = prev.clone();
        with_current_el(&el_for_magic, || {
            let v = expr::evaluate(&ast, &proxy_owned);
            let next = js_to_string(&v);
            {
                let p = prev.borrow();
                if p.as_deref() == Some(next.as_str()) {
                    return;
                }
            }
            el_owned.set_text_content(Some(&next));
            *prev.borrow_mut() = Some(next);
        });
    });
    track_effect_on(el, id);
}

fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}
