//! `LoopScope` — the per-item scope bound to each `pp-for` clone.
//!
//! A loop scope exposes the loop variable (e.g. `story` in
//! `pp-for="story in stories"`) plus `$index`, `$first`, and
//! `$last`. Every other key falls through to the enclosing scope's
//! proxy, so `$store.cart`, `$route.path`, and component fields work
//! inside a loop without any special plumbing.
//!
//! See `rfcs/rfc-004-pp-for.md` §5.3 for the resolution order.

use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::JsValue;

use crate::reactive::ScopeId;
use crate::scope::ComponentState;

pub struct LoopScope {
    /// The loop variable's identifier (e.g. `"story"`). Shared as
    /// `Rc<str>` across every loop iteration so new clones only
    /// bump the refcount instead of cloning the underlying bytes.
    pub item_name: Rc<str>,
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
    /// Scope id of the enclosing component / outer loop. Used for
    /// `invoke()` fall-through so `@click="bump"` inside a `pp-for`
    /// calls the enclosing component's `bump` handler instead of
    /// silently no-op'ing.
    pub parent_scope_id: ScopeId,
}

impl ComponentState for LoopScope {
    fn get(&self, key: &str) -> JsValue {
        if key == &*self.item_name {
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
                Reflect::get(&self.parent, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
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

    fn invoke(&mut self, key: &str, args: &Array) -> JsValue {
        // Delegate handler calls to the enclosing component scope —
        // mirrors the `get` fall-through so `@click="bump"` or
        // `@input="type(slot.index)"` inside `pp-for` reaches the
        // outer component's handler table. Without this, bindings
        // inside a loop body silently no-op.
        crate::scope::invoke_handler(self.parent_scope_id, key, args)
    }
}
