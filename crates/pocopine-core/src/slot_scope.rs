//! `SlotScope` — the per-materialization scope bound to scoped-slot
//! content. Per RFC-011.
//!
//! Exposes a single user-named identifier (the `pp-let` value) whose
//! JS-object shape is `{ prop_1: resolve_path(owner, path_1), ... }`
//! — i.e. the `:prop="path"` bindings the component author declared
//! on the `<slot>` element. Everything else falls through to the
//! owner scope, so a scoped-slot template can still reach `$store`,
//! `$route`, and the component's own fields.
//!
//! Read-only: slot scopes don't write. Mutating the bound values
//! flows through the owner component's `$dispatch` / handler surface.

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::JsValue;

use crate::path::resolve_path;
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
    pub bind_source: JsValue,
    /// The scope whose template *authored* the slot content. Used
    /// for fall-through identifier reads — `@click="parent_handler"`
    /// inside the slot has to resolve against the caller, not the
    /// component that declared the slot.
    pub caller: JsValue,
}

impl ComponentState for SlotScope {
    fn get(&self, key: &str) -> JsValue {
        if key == self.ident.as_str() {
            let obj = Object::new();
            for (prop, path) in &self.bindings {
                let val = resolve_path(&self.bind_source, path);
                let _ = Reflect::set(&obj, &JsValue::from_str(prop), &val);
            }
            return obj.into();
        }
        // Fall through to the caller for everything else — handlers,
        // parent-scope fields, magics. This is what makes
        // `@click="parent_handler"` work inside a slot's template.
        Reflect::get(&self.caller, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
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

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}
