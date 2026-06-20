//! `pp-html="<expr>"` — set `innerHTML` from a template expression
//! (RFC-012). As with Alpine's `x-html`, no sanitisation — authors
//! who drop untrusted strings in here own the consequences.

use std::rc::Rc;
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::mount::track_effect_on;

/// Install a `pp-html` effect on `el` that writes `expr`'s
/// stringified value into the element's `innerHTML` and re-runs
/// whenever the expression's reactive dependencies change. The
/// effect's lifetime is tracked to `el` via
/// [`crate::mount::track_effect_on`] so it's released when the
/// element's subtree is torn down. Cleanup-safe install entry
/// point.
#[doc(hidden)]
pub fn install_eval(el: &Element, proxy: &JsValue, evaluator: Rc<dyn Fn(&JsValue) -> JsValue>) {
    let el_owned = el.clone();
    let proxy_owned = proxy.clone();
    let id = crate::reactive::effect_install(move |suppressed| {
        let v = evaluator(&proxy_owned);
        let s = v.as_string().unwrap_or_default();
        // RFC-099 — the hydration claim is self-healing: if the server
        // already rendered exactly this inner HTML, skip the redundant
        // write (zero DOM mutation). If it did NOT — e.g. a value derived
        // in `on_setup`, which the server doesn't run, like pine-icon's
        // looked-up SVG — the DOM differs and we DO write, filling it in.
        if suppressed && el_owned.inner_html() == s {
            return;
        }
        el_owned.set_inner_html(&s);
    });
    track_effect_on(el, id);
}
