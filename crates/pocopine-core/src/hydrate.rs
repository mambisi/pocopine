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

/// Hydrate a server-rendered component root `host` by reading its own
/// `data-pp-state` island from the DOM — the document-level entry that
/// closes the SSR loop (the server emits the island via
/// [`RenderedPage::into_fragment`](../../pocopine_ssr/struct.RenderedPage.html);
/// this consumes it). The component tag comes from the root's
/// `data-pp-scope-id`; the state from the adjacent
/// `<script type="application/json" data-pp-state>` sibling (absent ⇒
/// hydrate against default state). Returns the scope id, or `None` if
/// `host` carries no scope-id stamp or the tag isn't registered.
pub fn hydrate_root(host: &web_sys::Element) -> Option<ScopeId> {
    let tag = host.get_attribute("data-pp-scope-id")?;
    let state = read_state_island(host).unwrap_or(Value::Null);
    hydrate_subtree(host, &tag, &state)
}

/// Parse the `<script data-pp-state>` island that follows a server-
/// rendered component root (the [`RenderedPage::into_fragment`] layout:
/// `body` then island, as element siblings). Returns `None` when no
/// island is present (e.g. a stateless component).
fn read_state_island(host: &web_sys::Element) -> Option<Value> {
    let mut sib = host.next_element_sibling();
    while let Some(el) = sib {
        if el.local_name() == "script" && el.has_attribute("data-pp-state") {
            let text = el.text_content().unwrap_or_default();
            return serde_json::from_str(&text).ok();
        }
        sib = el.next_element_sibling();
    }
    None
}

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
