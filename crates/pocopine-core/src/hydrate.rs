//! RFC-099 Phase 2c — client-side hydration entry.
//!
//! Attaches reactivity to a **server-rendered** subtree without creating
//! any DOM: instantiate the component's scope, load the serialized
//! server state (`data-pp-state` island) into it so bindings
//! re-evaluate to exactly what the server rendered, bind the scope to
//! the existing root, and run the claim walk
//! ([`crate::templates_plan::hydrate_plan`]).
//!
//! Phase-2 scope: bindings / interps / listeners / refs on the static
//! structure. Structural controllers (`pp-if`/`pp-for`/`pp-match`) and
//! child-component mounts resolve client-side and are not claimed yet.

use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::JsValue;

use crate::reactive::ScopeId;

/// Hydrate the server-rendered `host` element as component `tag`,
/// loading `state` (the deserialized island) into the scope. Returns the
/// new scope id, or `None` if `tag` isn't registered (no plan).
pub fn hydrate_subtree(host: &web_sys::Element, tag: &str, state: &Value) -> Option<ScopeId> {
    let scope = crate::registry::instantiate(tag)?;
    let plan = crate::templates_plan::template_plan_for(tag)?;

    // Load the server state into the fresh scope so every binding
    // re-evaluates to the value the server already rendered. A
    // JSON-compatible serializer keeps object fields as JS objects (not
    // ES `Map`s), matching how the proxy/`ComponentState::set` read them.
    if let Value::Object(map) = state {
        let ser = serde_wasm_bindgen::Serializer::json_compatible();
        let mut st = scope.state.borrow_mut();
        for (k, v) in map {
            if let Ok(js) = v.serialize(&ser) {
                st.set(k, js);
            }
        }
    }

    let proxy = if plan.needs_proxy {
        scope.into_proxy()
    } else {
        JsValue::UNDEFINED
    };
    crate::mount::bind_scope_to(host, scope.id, &proxy);
    crate::templates_plan::hydrate_plan(host, scope.id, &proxy, plan, tag);
    Some(scope.id)
}
