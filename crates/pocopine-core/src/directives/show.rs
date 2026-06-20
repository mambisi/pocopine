//! `pp-show="<expr>"` — toggle display by truthiness (RFC-012).
//!
//! If the element also carries any `pp-transition:*` attributes, the
//! toggle routes through [`crate::directives::transition`] so the
//! enter / leave class sequence runs at the right moment; the
//! `display: none` is deferred until the leave animation completes.

use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement};

use super::transition;
use crate::mount::track_effect_on;

/// Install a `pp-show` effect on `el` that toggles its `display`
/// style by the truthiness of `expr`. If the element carries any
/// `pp-transition:*` attributes the toggle routes through
/// [`crate::directives::transition`] so the enter/leave class
/// sequence runs at the right moment; the `display: none` is
/// deferred until the leave animation completes.
///
/// Silently no-ops if `el` is not an `HtmlElement` (the only
/// element flavour with a `style` accessor). Cleanup-safe install
/// entry point.
#[doc(hidden)]
pub fn install_eval(el: &Element, proxy: &JsValue, evaluator: Rc<dyn Fn(&JsValue) -> JsValue>) {
    let Ok(html_el): Result<HtmlElement, _> = el.clone().dyn_into() else {
        return;
    };
    let proxy_owned = proxy.clone();
    // The first effect run is the initial mount. Apply the display
    // state directly — no enter/leave transition. Without this guard an
    // element that starts hidden runs the LEAVE sequence on its first
    // paint (visible → fade/slide out → `display: none`), the
    // "flash on refresh". Mirrors Alpine's `x-show` + `x-transition`,
    // which never animates the initial render (no implicit `appear`).
    let initial = Cell::new(true);
    let id = crate::reactive::effect_install(move |suppressed| {
        let truthy = !evaluator(&proxy_owned).is_falsy();
        // RFC-099 — self-healing claim: on the hydration pass the server
        // already rendered a display state. Consume the `initial` flag
        // (so later changes don't flash an enter/leave), then skip the
        // write only if the DOM already matches; otherwise apply the
        // correct display directly (no transition) to heal it.
        if suppressed {
            initial.set(false);
            let style = html_el.style();
            let dom_hidden = style
                .get_property_value("display")
                .map(|d| d == "none")
                .unwrap_or(false);
            let want_hidden = !truthy;
            if dom_hidden != want_hidden {
                if want_hidden {
                    let _ = style.set_property("display", "none");
                } else {
                    let _ = style.remove_property("display");
                }
            }
            return;
        }
        let style = html_el.style();
        let first_run = initial.replace(false);
        if truthy {
            let _ = style.remove_property("display");
            if !first_run {
                transition::enter_subtree(html_el.as_ref(), || {});
            }
        } else if first_run {
            let _ = style.set_property("display", "none");
        } else {
            let style_for_leave = style.clone();
            transition::leave_subtree(html_el.as_ref(), move || {
                let _ = style_for_leave.set_property("display", "none");
            });
        }
    });
    track_effect_on(el, id);
}
