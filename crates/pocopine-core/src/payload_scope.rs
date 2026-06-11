//! RFC-094 — the per-mount scope a matched `pp-case`'s body binds
//! against when `pp-let` names the payload. `get(ident)` serves
//! the extracted payload; everything else falls through to the
//! match owner via the shared read mirror (so component fields,
//! handlers, and magics keep working inside a case body).

use js_sys::Array;
use wasm_bindgen::JsValue;

use crate::reactive::ScopeId;
use crate::scope::ComponentState;

pub struct PayloadScope {
    pub ident: String,
    pub value: JsValue,
    pub parent_scope_id: ScopeId,
}

impl ComponentState for PayloadScope {
    fn get(&self, key: &str) -> JsValue {
        if key == self.ident.as_str() {
            return self.value.clone();
        }
        crate::scope::read_scope_key(self.parent_scope_id, key)
    }

    fn set(&mut self, _key: &str, _value: JsValue) {
        // Payload bindings are read-only.
    }

    fn keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn invoke(&mut self, key: &str, args: &Array) -> JsValue {
        crate::scope::invoke_handler(self.parent_scope_id, key, args)
    }

    // The payload changes on same-tag updates; the ident is
    // triggered directly — never cache it.
    fn cacheable_fields(&self) -> bool {
        false
    }
}
