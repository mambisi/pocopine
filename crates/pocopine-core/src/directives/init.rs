//! `pp-init="handler"` — run a handler once after the scope is bound.

use js_sys::Array;
use web_sys::Element;

use crate::reactive::ScopeId;
use crate::scope::{invoke_handler, with_current_el};

/// Compiled-path entry. Invokes `handler` against `scope_id` with
/// the element bound as the current `with_current_el`. Used by
/// `mount::fire_deferred_init` (the post-order drain that runs
/// stashed `pp-init` values after descendants have bound) and
/// `apply_static_plan`'s init step.
pub fn install(el: &Element, scope_id: ScopeId, handler: &str) {
    let el_owned = el.clone();
    let handler = handler.to_string();
    with_current_el(&el_owned, || {
        invoke_handler(scope_id, &handler, &Array::new());
    });
}
