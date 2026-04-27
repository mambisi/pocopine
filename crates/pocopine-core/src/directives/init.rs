//! `pp-init="handler"` — run a handler once after the scope is bound.

use js_sys::Array;
use web_sys::Element;

use super::DirectiveCall;
use crate::reactive::ScopeId;
use crate::scope::{invoke_handler, with_current_el};

pub fn run(call: &DirectiveCall) {
    install(&call.el.clone(), call.scope_id, &call.value);
}

/// Compiled-path entry. Invokes `handler` against `scope_id` with
/// the element bound as the current `with_current_el`. Used by
/// `walker::fire_deferred_init` (the post-order drain that runs
/// stashed `pp-init` values after descendants have bound) and
/// `apply_static_plan`'s init step. Avoids the directive-registry
/// `DirectiveCall` round-trip so compiled-only builds don't
/// depend on `walker::dispatch`.
pub fn install(el: &Element, scope_id: ScopeId, handler: &str) {
    let el_owned = el.clone();
    let handler = handler.to_string();
    with_current_el(&el_owned, || {
        invoke_handler(scope_id, &handler, &Array::new());
    });
}
