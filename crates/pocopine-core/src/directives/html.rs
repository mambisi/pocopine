//! `pp-html="<expr>"` — set `innerHTML` from a template expression
//! (RFC-012). As with Alpine's `x-html`, no sanitisation — authors
//! who drop untrusted strings in here own the consequences.

use wasm_bindgen::JsValue;
use web_sys::console;

use super::DirectiveCall;
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
    let el = call.el.clone();
    let proxy = call.proxy.clone();
    let ast: Spanned<expr::Expr> = match expr::parse_cached(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-html: {} (at {}..{})",
                e.message, e.span.start, e.span.end
            )));
            return;
        }
    };
    let id = effect(move || {
        with_current_el(&el.clone(), || {
            let v = expr::evaluate(&ast, &proxy);
            let s = v.as_string().unwrap_or_default();
            el.set_inner_html(&s);
        });
    });
    track_effect_on(call.el, id);
}
