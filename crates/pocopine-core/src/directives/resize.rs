//! `pp-resize="handler"` — RFC-016.
//!
//! Installs a `ResizeObserver` on the host element (or
//! `document.documentElement` with `.document`) and dispatches the
//! named handler with two `f64` args: width and height in CSS
//! pixels. Content-box by default; `.border-box` switches to
//! border-box via `element.getBoundingClientRect()`.

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, ResizeObserver, ResizeObserverEntry};

use crate::reactive::ScopeId;
use crate::scope::invoke_handler;

/// Key under which the installed [`ResizeObserver`] is stored on the
/// host element — symmetric to `__pp_teleported`. Inspected by
/// [`release`].
const OBS_KEY: &str = "__pp_resize_obs";

/// Compiled-path install entry. Called by the compiled plan installer for
/// each `pp-resize="handler"` site the macro lifted into the
/// template plan.
pub fn install_opaque(
    el: &Element,
    _arg: Option<&str>,
    modifiers: &[&str],
    value: &str,
    scope_id: ScopeId,
    _proxy: &JsValue,
) {
    let handler = value.to_string();
    let host_el = el.clone();
    let on_document = modifiers.contains(&"document");
    let border_box = modifiers.contains(&"border-box");

    let target: Element = if on_document {
        match web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            Some(el) => el,
            None => return,
        }
    } else {
        host_el.clone()
    };

    let cb_target = target.clone();
    let closure = Closure::wrap(Box::new(move |entries: JsValue, _obs: JsValue| {
        let Ok(entries) = entries.dyn_into::<Array>() else {
            return;
        };
        let Some(entry) = entries.get(0).dyn_into::<ResizeObserverEntry>().ok() else {
            return;
        };
        let (w, h) = if border_box {
            let rect = cb_target.get_bounding_client_rect();
            (rect.width(), rect.height())
        } else {
            let rect = entry.content_rect();
            (rect.width(), rect.height())
        };
        let args = Array::new();
        args.push(&JsValue::from_f64(w));
        args.push(&JsValue::from_f64(h));
        invoke_handler(scope_id, &handler, &args);
    }) as Box<dyn FnMut(JsValue, JsValue)>);

    let Ok(observer) = ResizeObserver::new(closure.as_ref().unchecked_ref()) else {
        return;
    };
    observer.observe(&target);
    closure.forget();

    // Stash on the host element so `release` can disconnect when the
    // element tears down (even when `.document` re-routes observation
    // — the *directive* still belongs to the host).
    let _ = Reflect::set(host_el.as_ref(), &OBS_KEY.into(), observer.as_ref());
}

/// Called by `mount::release_subtree` on every released element.
/// Disconnects any ResizeObserver stashed under [`OBS_KEY`].
pub fn release(el: &Element) {
    let Ok(v) = Reflect::get(el.as_ref(), &OBS_KEY.into()) else {
        return;
    };
    if v.is_undefined() || v.is_null() {
        return;
    }
    if let Ok(obs) = v.dyn_into::<ResizeObserver>() {
        obs.disconnect();
    }
    let _ = Reflect::set(el.as_ref(), &OBS_KEY.into(), &JsValue::UNDEFINED);
}
