//! `pp-html="<expr>"` — set `innerHTML` from a template expression
//! (RFC-012). As with Alpine's `x-html`, no sanitisation — authors
//! who drop untrusted strings in here own the consequences.

use std::rc::Rc;
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::expr::{self, Spanned};
use crate::mount::track_effect_on;
use crate::reactive::effect;
use crate::scope::with_current_el;

/// Install a `pp-html` effect on `el` that writes `expr`'s
/// stringified value into the element's `innerHTML` and re-runs
/// whenever the expression's reactive dependencies change. The
/// effect's lifetime is tracked to `el` via
/// [`crate::mount::track_effect_on`] so it's released when the
/// element's subtree is torn down. Cleanup-safe install entry
/// point.
pub fn install(el: &Element, proxy: &JsValue, ast: Spanned<expr::Expr>) {
    install_eval(el, proxy, Rc::new(move |scope| expr::evaluate(&ast, scope)));
}

#[doc(hidden)]
pub fn install_eval(el: &Element, proxy: &JsValue, evaluator: Rc<dyn Fn(&JsValue) -> JsValue>) {
    let el_owned = el.clone();
    let proxy_owned = proxy.clone();
    let id = effect(move || {
        with_current_el(&el_owned.clone(), || {
            let v = evaluator(&proxy_owned);
            let s = v.as_string().unwrap_or_default();
            el_owned.set_inner_html(&s);
        });
    });
    track_effect_on(el, id);
}
