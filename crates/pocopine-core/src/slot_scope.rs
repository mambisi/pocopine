//! `SlotScope` — the per-materialization scope bound to scoped-slot
//! content. Per RFC-011.
//!
//! Exposes a single user-named identifier (the `pp-let` value) whose
//! JS-object shape is `{ prop_1: <owner-scope read of path_1>, ... }`
//! — i.e. the `:prop="path"` bindings the component author declared
//! on the `<slot>` element. Everything else falls through to the
//! owner scope, so a scoped-slot template can still reach `$store`,
//! `$route`, and the component's own fields.
//!
//! Read-only: slot scopes don't write. Mutating the bound values
//! flows through the owner component's handler / `emit` surface.

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;

use crate::reactive::ScopeId;
use crate::scope::ComponentState;

pub struct SlotScope {
    /// The `pp-let` identifier (e.g. `"ctx"`).
    pub ident: String,
    /// Each `:prop="path"` pair declared on the `<slot>`. The prop
    /// name lands under the ident's object; the path resolves on
    /// [`Self::bind_source`] at read time.
    pub bindings: Vec<(String, String)>,
    /// The scope whose template contains the `<slot>` element. Used
    /// to resolve `:prop="path"` binding paths — `path` was
    /// authored in that scope (e.g. `<slot :ctx="current">` reads
    /// `current` from the component that declared the slot).
    /// RFC-096 S2 — held as the OWNER's scope id; publication
    /// expressions resolve through the shared read mirror.
    pub bind_source_scope_id: ScopeId,
    /// Scope id of the caller — `caller`'s proxy alone isn't enough
    /// to dispatch handlers because [`crate::scope::invoke_handler`]
    /// works off ids, not proxies. Without this, `@click="handler"`
    /// inside a scoped slot would hit [`SlotScope::invoke`] (which
    /// only knows the proxy) and the call would silently no-op.
    pub caller_scope_id: ScopeId,
}

impl ComponentState for SlotScope {
    fn get(&self, key: &str) -> JsValue {
        if key == self.ident.as_str() {
            let obj = Object::new();
            for (prop, path) in &self.bindings {
                // RFC-096 S2 — resolve the publication path
                // against the OWNER scope via the read mirror
                // (root segment tracked at the owner; deeper
                // segments walk the plain value).
                let mut segs = path.split('.').filter(|s| !s.is_empty());
                let val = match segs.next() {
                    None => JsValue::UNDEFINED,
                    Some(first) => {
                        let mut cur =
                            crate::scope::read_scope_key(self.bind_source_scope_id, first);
                        for seg in segs {
                            cur = Reflect::get(&cur, &JsValue::from_str(seg))
                                .unwrap_or(JsValue::UNDEFINED);
                        }
                        cur
                    }
                };
                let _ = Reflect::set(&obj, &JsValue::from_str(prop), &val);
            }
            return obj.into();
        }
        // Fall through to the caller for everything else — handlers,
        // parent-scope fields, magics — via the shared read mirror.
        crate::scope::read_scope_key(self.caller_scope_id, key)
    }

    // Slot scopes derive their value from a parent proxy on every
    // call, so caching would freeze the slot at its first-render
    // snapshot. Reactivity rides through the parent's per-key
    // trigger picked up by the inner `resolve_path` / `Reflect::get`
    // calls above.
    fn cacheable_fields(&self) -> bool {
        false
    }

    fn set(&mut self, _key: &str, _value: JsValue) {
        // Slot scopes are read-only.
    }

    fn keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn invoke(&mut self, key: &str, args: &Array) -> JsValue {
        // Delegate to the caller's scope so handler expressions
        // inside scoped slots — `@click="parent_handler"` — reach
        // the component that authored the slot content. The slot
        // scope itself owns no handlers; without this delegation,
        // handler dispatch would silently no-op for every compiled
        // and mount-driven scoped slot listener.
        crate::scope::invoke_handler(self.caller_scope_id, key, args)
    }
}
