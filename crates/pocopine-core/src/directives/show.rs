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
use crate::reactive::effect;

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
    let id = effect(move || {
        let truthy = !evaluator(&proxy_owned).is_falsy();
        let first_run = initial.replace(false);
        if truthy {
            super::style_state::set_visible(&html_el, true);
            if !first_run {
                transition::enter_subtree(html_el.as_ref(), || {});
            }
        } else if first_run {
            super::style_state::set_visible(&html_el, false);
        } else {
            let el_for_leave = html_el.clone();
            transition::leave_subtree(html_el.as_ref(), move || {
                super::style_state::set_visible(&el_for_leave, false);
            });
        }
    });
    track_effect_on(el, id);
}
