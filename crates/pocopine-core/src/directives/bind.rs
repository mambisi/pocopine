//! `pp-bind:attr="field"` — bind an HTML attribute (or a child component's
//! prop) to a scope field.
//!
//! Two branches inside the effect body:
//!
//! * **Prop write** — when the target element is a registered component
//!   tag, the value is written to the child's proxy field. That flows
//!   through the child's `set` trap and triggers the child's effects.
//! * **Attribute write** — otherwise, follow upstream Alpine semantics
//!   (`class` / `style` special-cased for string-or-object, everything
//!   else a plain `setAttribute`).

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element};

use super::DirectiveCall;
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::{child_component_proxy, track_effect_on};

pub fn run(call: &DirectiveCall) {
    let Some(attr) = call.arg.clone() else { return };
    let el = call.el.clone();
    let parent_proxy = call.proxy.clone();
    let ast: Spanned<expr::Expr> = match expr::parse(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-bind:{}: {} (at {}..{})",
                attr, e.message, e.span.start, e.span.end
            )));
            return;
        }
    };

    // Capture whether the target is a child component at bind time. The
    // child's proxy is stable for the lifetime of the element.
    let child_proxy = child_component_proxy(call.el);

    let id = effect(move || {
        with_current_el(&el.clone(), || {
            let v = expr::evaluate(&ast, &parent_proxy);
            match &child_proxy {
                Some(cp) => {
                    // Prop write — the set trap on the child's proxy
                    // takes care of triggering downstream effects.
                    let _ = Reflect::set(cp, &JsValue::from_str(&attr), &v);
                }
                None => apply(&el, &attr, &v),
            }
        });
    });
    track_effect_on(call.el, id);
}

fn apply(el: &Element, attr: &str, v: &JsValue) {
    match attr {
        "class" => set_class(el, v),
        "style" => set_style(el, v),
        _ => {
            if v.is_undefined() || v.is_null() || v == &JsValue::FALSE {
                let _ = el.remove_attribute(attr);
            } else if let Some(s) = v.as_string() {
                let _ = el.set_attribute(attr, &s);
            } else if let Some(n) = v.as_f64() {
                let _ = el.set_attribute(attr, &n.to_string());
            } else if v == &JsValue::TRUE {
                let _ = el.set_attribute(attr, "");
            } else {
                // Fallback: JSON-stringify objects/arrays.
                let s = js_sys::JSON::stringify(v)
                    .ok()
                    .and_then(|s| s.as_string())
                    .unwrap_or_default();
                let _ = el.set_attribute(attr, &s);
            }
        }
    }
}

fn set_class(el: &Element, v: &JsValue) {
    if let Some(s) = v.as_string() {
        let _ = el.set_attribute("class", &s);
        return;
    }
    if v.is_object() {
        // Object form: { 'name': truthy } -> join truthy keys.
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out: Vec<String> = Vec::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            let truthy = Reflect::get(&obj, &k)
                .map(|val| val.as_bool().unwrap_or(!val.is_falsy()))
                .unwrap_or(false);
            if truthy {
                if let Some(s) = k.as_string() {
                    out.push(s);
                }
            }
        }
        let _ = el.set_attribute("class", &out.join(" "));
    }
}

fn set_style(el: &Element, v: &JsValue) {
    if let Some(s) = v.as_string() {
        let _ = el.set_attribute("style", &s);
        return;
    }
    if v.is_object() {
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out = String::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            if let (Some(name), Ok(val)) = (k.as_string(), Reflect::get(&obj, &k)) {
                let val_s = val.as_string().unwrap_or_default();
                out.push_str(&format!("{name}:{val_s};"));
            }
        }
        let _ = el.set_attribute("style", &out);
    }
}
