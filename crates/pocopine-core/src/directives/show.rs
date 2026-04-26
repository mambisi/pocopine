//! `pp-show="<expr>"` — toggle display by truthiness (RFC-012).
//!
//! If the element also carries any `pp-transition:*` attributes, the
//! toggle routes through [`crate::directives::transition`] so the
//! enter / leave class sequence runs at the right moment; the
//! `display: none` is deferred until the leave animation completes.

use wasm_bindgen::prelude::*;
use web_sys::{console, Element, HtmlElement};

use super::transition;
use super::DirectiveCall;
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
    let ast: Spanned<expr::Expr> = match expr::parse_cached(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-show: {} (at {}..{})",
                e.message, e.span.start, e.span.end
            )));
            return;
        }
    };
    install(call.el, call.proxy, ast);
}

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
pub fn install(el: &Element, proxy: &JsValue, ast: Spanned<expr::Expr>) {
    let Ok(html_el): Result<HtmlElement, _> = el.clone().dyn_into() else {
        return;
    };
    let el_for_track = el.clone();
    let proxy_owned = proxy.clone();
    let id = effect(move || {
        with_current_el(&el_for_track.clone(), || {
            let truthy = expr::evaluate_truthy(&ast, &proxy_owned);
            let style = html_el.style();
            if truthy {
                let _ = style.remove_property("display");
                transition::enter_subtree(html_el.as_ref(), || {});
            } else {
                let style_for_leave = style.clone();
                transition::leave_subtree(html_el.as_ref(), move || {
                    let _ = style_for_leave.set_property("display", "none");
                });
            }
        });
    });
    track_effect_on(el, id);
}
