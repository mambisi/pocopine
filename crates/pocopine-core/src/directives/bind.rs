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
//!
//! Both branches memoise the last-applied value so no-op effect ticks
//! (a watch firing whose *input* changed but whose *bound output*
//! didn't) skip the DOM write. A `setAttribute` is cheap but a
//! `class`/`style` mutation triggers a style recalc; eliding those
//! when nothing changed saves the most expensive browser work.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element};

use super::DirectiveCall;
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

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

    // Capture child-component target info at bind time. The scope
    // id is stable for the lifetime of the element; we use it at
    // each effect tick to consult `is_prop` on the child's state
    // so parents can't write through to `#[state]` fields.
    let child_target = crate::walker::child_component_scope(call.el);
    let child_field = super::normalize_prop_name(&attr);

    // Memo of the last value written to this attribute. Serialised
    // to a String so the compare is cheap + monomorphic (class and
    // style have to build a string anyway before `set_attribute`;
    // for plain attrs we serialise to the value we'd write).
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let id = effect(move || {
        with_current_el(&el.clone(), || {
            let v = expr::evaluate(&ast, &parent_proxy);
            match &child_target {
                Some((child_scope_id, cp)) => {
                    let target_field =
                        crate::model_runtime::resolve_model_key(*child_scope_id, &child_field)
                            .unwrap_or_else(|| child_field.clone());
                    // RFC-031 — only `#[prop]` fields are writable
                    // from the parent. Silently drop writes to
                    // state fields so accidental `<pine-thing
                    // loaded="true">` doesn't clobber child state.
                    let is_prop = crate::scope::Scope::find(*child_scope_id)
                        .map(|s| s.state.borrow().is_prop(&target_field))
                        .unwrap_or(false);
                    if !is_prop {
                        return;
                    }
                    crate::model_runtime::with_write_origin(
                        crate::model_runtime::WriteOrigin::ParentModelIn,
                        || {
                            let _ = Reflect::set(cp, &JsValue::from_str(&target_field), &v);
                        },
                    );
                }
                None => apply_memoised(&el, &attr, &v, &prev),
            }
        });
    });
    track_effect_on(call.el, id);
}

/// [`apply`] wrapped with a last-value memo. Skips `set_attribute`
/// / `remove_attribute` when the serialised form matches the last
/// write.
fn apply_memoised(el: &Element, attr: &str, v: &JsValue, prev: &Rc<RefCell<Option<String>>>) {
    // Shape handling diverges on whether this is a *state* attribute
    // (`data-*` / `aria-*`) or a classic HTML attribute, but ONLY
    // on the truthy side:
    //
    //   - State attrs render bool `true` as the literal string
    //     `"true"` so CSS value selectors like
    //     `[data-disabled="true"]` and ARIA consumers reading the
    //     value work without an author ternary. `aria-expanded`,
    //     `aria-hidden`, etc. all read by value.
    //   - Classic attrs keep upstream Alpine semantics: bool `true`
    //     renders present-with-empty string (`<input disabled>`).
    //
    // On the falsy side (bool `false`, `null`, `undefined`), both
    // paths REMOVE the attribute — presence-based CSS selectors
    // like `[data-disabled]` or `[hidden]` treat the attribute as a
    // "truthy if present" signal, and rendering `data-disabled="false"`
    // would falsely match them. The demo's Pine stylesheet mixes
    // both idioms (`[data-disabled]` on some primitives,
    // `[data-disabled="true"]` on others), so removal is the only
    // safe falsy shape across both.
    //
    // The removal path is memoed by storing an `Option<String>` where
    // `None` means "attribute is currently absent". A transition into
    // or out of the "absent" state must fire a DOM call exactly once.
    let should_remove = v.is_undefined() || v.is_null() || v == &JsValue::FALSE;
    if should_remove {
        let mut p = prev.borrow_mut();
        if p.is_none() {
            return;
        }
        *p = None;
        let _ = el.remove_attribute(attr);
        return;
    }
    // Compute the string we'd write. For class/style object form,
    // that string IS the joined output; for simple values it's the
    // attribute literal.
    let serialised: String = match attr {
        "class" => match serialise_class(v) {
            Some(s) => s,
            None => return, // shape we don't handle — leave DOM alone
        },
        "style" => match serialise_style(v) {
            Some(s) => s,
            None => return,
        },
        _ => match serialise_plain(attr, v) {
            Some(s) => s,
            None => return,
        },
    };
    {
        let p = prev.borrow();
        if p.as_deref() == Some(serialised.as_str()) {
            return;
        }
    }
    // Write + memo.
    let _ = el.set_attribute(attr, &serialised);
    *prev.borrow_mut() = Some(serialised);
}

/// `data-*` / `aria-*` — attributes read *by value* (CSS selectors,
/// ARIA consumers) rather than by presence. Bool values on these
/// render as the literal strings `"true"` / `"false"` so expressions
/// like `:data-selected="is_selected"` work without an explicit
/// `? 'true' : 'false'` ternary at the call site.
fn is_state_attr(attr: &str) -> bool {
    attr.starts_with("data-") || attr.starts_with("aria-")
}

fn serialise_class(v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if v.is_object() {
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
        return Some(out.join(" "));
    }
    None
}

fn serialise_style(v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
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
        return Some(out);
    }
    None
}

fn serialise_plain(attr: &str, v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if let Some(n) = v.as_f64() {
        return Some(n.to_string());
    }
    if let Some(b) = v.as_bool() {
        // `false` is routed through `remove_attribute` before this
        // function runs, on both state-attr and classic-attr paths.
        // So the only bool that reaches us is `true`: state attrs
        // render it as the literal `"true"` (CSS value selectors +
        // ARIA consumers read by value); classic attrs keep the
        // upstream `true → present-with-empty-string` shape.
        if is_state_attr(attr) {
            return Some(if b { "true".into() } else { "false".into() });
        }
        return Some(String::new());
    }
    // Fallback: JSON-stringify objects/arrays (matches the old
    // behaviour). Silently dropping non-serialisable values would
    // regress — return an empty string on serialisation failure,
    // same as the pre-memo path did.
    Some(
        js_sys::JSON::stringify(v)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_default(),
    )
}
