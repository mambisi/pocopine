//! `LoopScope` — the per-item scope bound to each `pp-for` clone.
//!
//! A loop scope exposes the loop variable (e.g. `story` in
//! `pp-for="story in stories"`) plus `$index`, `$first`, and
//! `$last`. Every other key falls through to the enclosing scope's
//! proxy, so `$store.cart`, `$route.path`, and component fields work
//! inside a loop without any special plumbing.
//!
//! See `rfcs/rfc-004-pp-for.md` §5.3 for the resolution order.

use js_sys::{Array, Reflect};
use wasm_bindgen::JsValue;

use crate::scope::ComponentState;

pub struct LoopScope {
    /// The loop variable's identifier (e.g. `"story"`).
    pub item_name: String,
    /// The current item — a `JsValue` (typically a JS object from
    /// serialized Rust data, or a primitive).
    pub item: JsValue,
    /// Zero-based position of `item` in the iteration.
    pub index: usize,
    /// Total length of the collection being iterated.
    pub total: usize,
    /// The parent (enclosing component or outer loop) proxy. Used for
    /// fall-through reads of keys that aren't loop-local.
    pub parent: JsValue,
}

impl ComponentState for LoopScope {
    fn get(&self, key: &str) -> JsValue {
        if key == self.item_name.as_str() {
            return self.item.clone();
        }
        match key {
            "$index" => JsValue::from_f64(self.index as f64),
            "$first" => JsValue::from_bool(self.index == 0),
            "$last" => JsValue::from_bool(self.index + 1 == self.total),
            _ => {
                // Fall through — `$store`, `$route`, magics, and any
                // parent-scope field resolve via the parent proxy's
                // get trap (which also tracks the dep at that scope).
                Reflect::get(&self.parent, &JsValue::from_str(key))
                    .unwrap_or(JsValue::UNDEFINED)
            }
        }
    }

    fn set(&mut self, _key: &str, _value: JsValue) {
        // Loop scopes are read-only. The loop variable is a snapshot
        // of an array element, not a live reference — mutating it
        // wouldn't round-trip to the Vec. Handlers that need to mutate
        // should dispatch to the parent scope with the index or id.
    }

    fn keys(&self) -> &'static [&'static str] {
        // Dynamic — no static key list. Sweep-triggers by the parent's
        // scope cover any effects reading loop-local keys because the
        // effects subscribe to `(parent_scope, items_key)` first.
        &[]
    }

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}
