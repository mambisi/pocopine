//! `pp-text="<expr>"` — set `textContent` from a template expression
//! (RFC-012).

use std::cell::RefCell;
use std::rc::Rc;

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
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let ast: Spanned<expr::Expr> = match expr::parse_cached(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-text: {} (at {}..{})",
                e.message, e.span.start, e.span.end
            )));
            return;
        }
    };
    let id = effect(move || {
        let el_for_magic = el.clone();
        let prev = prev.clone();
        with_current_el(&el_for_magic, || {
            let v = expr::evaluate(&ast, &proxy);
            let next = js_to_string(&v);
            {
                let p = prev.borrow();
                if p.as_deref() == Some(next.as_str()) {
                    return;
                }
            }
            el.set_text_content(Some(&next));
            *prev.borrow_mut() = Some(next);
        });
    });
    track_effect_on(call.el, id);
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
