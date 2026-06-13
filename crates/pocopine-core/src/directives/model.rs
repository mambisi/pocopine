//! `pp-model[:<child-field>]="field"` — two-way input binding.
//!
//! Two code paths:
//!
//! * **Native input** (`<input>`, `<textarea>`, `<select>`). Effect
//!   writes element value from `proxy[key]`; `input`/`change`
//!   listener writes element value back through the scoped writer.
//! * **Registered component tag** (`<pine-input pp-model="name">`).
//!   Per [RFC-009](../../../../rfcs/rfc-009-pp-model-components.md):
//!   effect mirrors parent's `proxy[key]` into the child's
//!   `<child-field>` prop (default: `model`); listener on
//!   `pp:update:<child-field>` (or `pp:update:model` for the
//!   default arg-less case) writes `event.detail` back to
//!   `proxy[key]`.
//!
//! `pp-model:open="dialog_open"` writes parent's `dialog_open` to
//! child's `open` field — Vue-3-style `v-model:prop` shape. Without
//! the arg, the child field is `model` for backward compatibility.

use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use web_sys::{
    CustomEvent, Element, Event, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement,
};

use crate::mount::{track_effect_on, track_listener_on};
use crate::reactive::effect;

/// Compiled-path entry used by compiled child-host
/// child-host model dispatch. Routes through the component or
/// native install depending on whether `el` resolves to a
/// registered child component tag.
pub fn install_compiled(
    el: &Element,
    scope_id: crate::reactive::ScopeId,
    proxy: &JsValue,
    arg: Option<&str>,
    modifiers: &[&str],
    value: &str,
) {
    // RFC-096 S1 — route off the stamped child scope ID; no proxy
    // is minted onto the child just to decide the path.
    if let Some(child_scope_id) = crate::mount::child_component_scope_id(el) {
        install_component(el, scope_id, proxy, arg, value, child_scope_id);
    } else {
        let number = modifiers.contains(&"number");
        let lazy = modifiers.contains(&"lazy");
        install_native(el, scope_id, proxy, value.to_string(), number, lazy);
    }
}

// ─── component-boundary path (RFC-009) ────────────────────────────

fn install_component(
    el: &Element,
    parent_scope_id: crate::reactive::ScopeId,
    parent_proxy: &JsValue,
    arg: Option<&str>,
    value: &str,
    child_scope_id: crate::reactive::ScopeId,
) {
    let parent_proxy = parent_proxy.clone();
    let key = value.to_string();
    let el = el.clone();
    // Directive arg picks the child's target field; defaults to
    // `model` so plain `pp-model="name"` keeps working unchanged.
    let child_field = arg.map(|s| s.to_string()).unwrap_or_else(|| "model".into());
    let child_field = child_field.replace('-', "_");
    let parent_access = crate::scope::scoped_root_reader(parent_scope_id);

    // Parent → child: mirror proxy[key] into the child's
    // `<child_field>` prop. Same shape as pp-bind's child-prop path,
    // with the same RFC-031 gate — writes only land on
    // `#[prop]` fields. The child → parent leg below uses a
    // fresh CustomEvent, not a proxy write, and stays unaffected.
    let parent_r = parent_proxy.clone();
    let key_r = key.clone();
    let access_r = parent_access.clone();
    let child_field_w = child_field.clone();
    let id = effect(move || {
        let v = crate::path::resolve_path_with(&parent_r, access_r.as_ref(), &key_r);
        let target_field = crate::model_runtime::resolve_model_key(child_scope_id, &child_field_w)
            .unwrap_or_else(|| child_field_w.clone());
        let is_prop = crate::scope::Scope::find(child_scope_id)
            .map(|s| s.state.borrow().is_prop(&target_field))
            .unwrap_or(false);
        if !is_prop {
            return;
        }
        crate::model_runtime::with_write_origin(
            crate::model_runtime::WriteOrigin::ParentModelIn,
            || {
                // RFC-096 S1 — the scoped writer; same body the
                // child's set trap delegates to.
                let _ = crate::scope::write_field(child_scope_id, &target_field, &v);
            },
        );
    });
    track_effect_on(&el, id);

    // Child → parent: named `pp:update:<field>` channel for
    // `pp-model:<field>`, falling back to `pp:update:model` for the
    // arg-less default. `event.detail` is the new value. Write it
    // back through the scoped writer so dotted keys (`$store.foo.bar`)
    // continue to work.
    let parent_w = parent_proxy;
    let key_w = key;
    let access_w = parent_access;
    let update_event = if child_field == "model" {
        "pp:update:model".to_string()
    } else {
        format!("pp:update:{child_field}")
    };
    let listener = Closure::wrap(Box::new(move |ev: Event| {
        let Ok(ce) = ev.dyn_into::<CustomEvent>() else {
            return;
        };
        let detail = ce.detail();
        let _ = crate::path::write_path_with(&parent_w, access_w.as_ref(), &key_w, &detail);
    }) as Box<dyn FnMut(Event)>);
    let target: web_sys::EventTarget = el.clone().into();
    track_listener_on(&el, target, &update_event, false, listener);
}

// ─── native input path ────────────────────────────────────────────

/// RFC-058 Phase 6.5 — compiled-path entry for `pp-model` on a
/// native input/textarea/select. Same effect + listener wiring
/// as the runtime mount dispatch (`run_native`); extracted so
/// compiled plan code can install it directly without going
/// through the directive registry. The macro emits one
/// [`crate::directives::for_plan::StaticNativeModel`] per
/// `<input pp-model="field">` site and the runtime applier
/// routes through here, dropping the runtime mount dependency
/// for native model bindings.
pub fn install_native(
    el: &web_sys::Element,
    scope_id: crate::reactive::ScopeId,
    proxy: &JsValue,
    key: String,
    number: bool,
    lazy: bool,
) {
    let proxy = proxy.clone();
    let el = el.clone();
    let access = crate::scope::scoped_root_reader(scope_id);

    // Read side: field -> element value, through the scoped
    // reader (`$`-roots fall back to the proxy).
    let proxy_r = proxy.clone();
    let key_r = key.clone();
    let access_r = access.clone();
    let el_r = el.clone();
    let id = effect(move || {
        let v = crate::path::resolve_path_with(&proxy_r, access_r.as_ref(), &key_r);
        write_to_element(&el_r, &v);
    });
    track_effect_on(&el, id);

    // Write side: input event -> field, through the scoped
    // writer (RFC-096 S1).
    let proxy_w = proxy.clone();
    let key_w = key.clone();
    let access_w = access;
    let el_w = el.clone();
    let handler = Closure::wrap(Box::new(move |_ev: Event| {
        let v = read_from_element(&el_w, number);
        let _ = crate::path::write_path_with(&proxy_w, access_w.as_ref(), &key_w, &v);
    }) as Box<dyn FnMut(Event)>);

    let event_name = if lazy { "change" } else { "input" };
    let target: web_sys::EventTarget = el.clone().into();
    track_listener_on(&el, target, event_name, false, handler);
}

fn write_to_element(el: &web_sys::Element, v: &JsValue) {
    if let Some(inp) = el.dyn_ref::<HtmlInputElement>() {
        match inp.type_().as_str() {
            "checkbox" => inp.set_checked(!v.is_falsy()),
            "radio" => {
                let want = v.as_string().unwrap_or_default();
                inp.set_checked(inp.value() == want);
            }
            _ => inp.set_value(&as_string(v)),
        }
        return;
    }
    if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
        s.set_value(&as_string(v));
        return;
    }
    if let Some(t) = el.dyn_ref::<HtmlTextAreaElement>() {
        t.set_value(&as_string(v));
    }
}

fn read_from_element(el: &web_sys::Element, number: bool) -> JsValue {
    if let Some(inp) = el.dyn_ref::<HtmlInputElement>() {
        return match inp.type_().as_str() {
            "checkbox" => JsValue::from_bool(inp.checked()),
            _ => coerce(inp.value(), number),
        };
    }
    if let Some(s) = el.dyn_ref::<HtmlSelectElement>() {
        return coerce(s.value(), number);
    }
    if let Some(t) = el.dyn_ref::<HtmlTextAreaElement>() {
        return coerce(t.value(), number);
    }
    JsValue::UNDEFINED
}

fn coerce(s: String, number: bool) -> JsValue {
    if number && let Ok(n) = s.parse::<f64>() {
        return JsValue::from_f64(n);
    }
    JsValue::from_str(&s)
}

fn as_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(|n| n.to_string()))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_default()
}
