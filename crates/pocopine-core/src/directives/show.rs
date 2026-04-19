//! `pp-show="<expr>"` — toggle display by truthiness (RFC-012).
//!
//! If the element also carries any `pp-transition:*` attributes, the
//! toggle routes through [`crate::directives::transition`] so the
//! enter / leave class sequence runs at the right moment; the
//! `display: none` is deferred until the leave animation completes.

use wasm_bindgen::prelude::*;
use web_sys::{console, HtmlElement};

use super::transition;
use super::DirectiveCall;
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
    let Ok(html_el): Result<HtmlElement, _> = call.el.clone().dyn_into() else { return };
    let proxy = call.proxy.clone();
    let ast: Spanned<expr::Expr> = match expr::parse(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-show: {} (at {}..{})",
                e.message, e.span.start, e.span.end
            )));
            return;
        }
    };
    let el_for_track = call.el.clone();

    let id = effect(move || {
        with_current_el(&el_for_track.clone(), || {
            let truthy = expr::evaluate_truthy(&ast, &proxy);
            let style = html_el.style();
            if truthy {
                let _ = style.remove_property("display");
                transition::enter(html_el.as_ref(), || {});
            } else {
                let style_for_leave = style.clone();
                transition::leave(html_el.as_ref(), move || {
                    let _ = style_for_leave.set_property("display", "none");
                });
            }
        });
    });
    track_effect_on(call.el, id);
}
