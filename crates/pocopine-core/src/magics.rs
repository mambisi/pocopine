//! Magics — `$store`, `$route`, `$event`. Resolved by the proxy's
//! `get` trap whenever a key starting with `$` is read.
//!
//! RFC-095 — the Alpine-era magics `$el`, `$refs`, `$dispatch`,
//! and `$id` were removed: zero usage across pine, docs, examples,
//! and tests, and each had a superseding Rust-side surface
//! (`LifecycleContext` / typed `Refs` accessors for `$el`/`$refs`,
//! the `emit*` family for `$dispatch`, `ctx.scope_id` for `$id`).
//! The survivors are the ones templates genuinely read: `$store`
//! (app stores), `$route` (router state), and `$event` (the DOM
//! event inside a `pp-on` expression).

use std::cell::RefCell;

use wasm_bindgen::JsValue;
use web_sys::console;

use crate::reactive::ScopeId;
use crate::router::route_proxy;
use crate::store::stores_object;

thread_local! {
    /// `$event` inside a `pp-on` value resolves to whatever this
    /// cell holds. `directives::on` sets it before evaluating the
    /// expression and clears it after via [`with_current_event`].
    /// Outside a pp-on dispatch, reads resolve to `undefined`.
    static CURRENT_EVENT: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

pub fn resolve(key: &str, _scope_id: ScopeId) -> JsValue {
    match key {
        "$store" => stores_object(),
        "$route" => route_proxy(),
        "$event" => CURRENT_EVENT
            .with(|c| c.borrow().clone())
            .unwrap_or(JsValue::UNDEFINED),
        _ => {
            console::warn_1(&JsValue::from_str(&format!(
                "pocopine: unknown magic `{key}` (valid: $store, $route, $event; \
                 $el/$refs/$dispatch/$id were removed — use LifecycleContext, \
                 typed refs, `emit`, or `ctx.scope_id` from Rust instead)"
            )));
            JsValue::UNDEFINED
        }
    }
}

/// Run `f` with `$event` resolving to `ev`. Mirrors
/// [`crate::scope::with_current_el`] / `with_current_scope_id` —
/// the previous value is saved + restored so nested `pp-on`
/// dispatches don't clobber each other.
pub fn with_current_event<R>(ev: &JsValue, f: impl FnOnce() -> R) -> R {
    let prev = CURRENT_EVENT.with(|c| c.replace(Some(ev.clone())));
    let out = f();
    CURRENT_EVENT.with(|c| *c.borrow_mut() = prev);
    out
}
