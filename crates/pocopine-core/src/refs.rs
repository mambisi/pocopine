//! Template refs — `pp-ref="name"` pins the decorated element under
//! its enclosing scope so Rust handlers can reach it imperatively.
//!
//! ```ignore
//! use pocopine::prelude::*;
//! use pocopine::refs;
//! use web_sys::HtmlInputElement;
//!
//! #[handlers]
//! impl SearchBar {
//!     pub fn init(&mut self) {
//!         if let Some(input) = refs::get_as::<HtmlInputElement>("search") {
//!             let _ = input.focus();
//!         }
//!     }
//! }
//! ```
//!
//! Refs resolve against the current handler's scope (the one on the
//! call stack via [`crate::scope::current_scope_id`]) so the same
//! name used in two sibling components doesn't collide. Scope eviction
//! ([`crate::scope::Scope::remove`]) also clears the scope's refs.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsCast;
use web_sys::Element;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

thread_local! {
    static REFS: RefCell<HashMap<ScopeId, HashMap<String, Element>>> =
        RefCell::new(HashMap::new());
}

/// Register `el` as the ref named `name` on `scope_id`. Called by the
/// `pp-ref` directive during walk.
pub fn register(scope_id: ScopeId, name: &str, el: &Element) {
    REFS.with(|r| {
        r.borrow_mut()
            .entry(scope_id)
            .or_default()
            .insert(name.to_string(), el.clone());
    });
}

/// Look up a ref on the current handler's scope. Returns `None`
/// outside of a handler invocation or when no ref with that name is
/// registered.
pub fn get(name: &str) -> Option<Element> {
    let id = current_scope_id()?;
    get_on(id, name)
}

/// Typed convenience — downcasts the looked-up `Element` to `T`. Fails
/// silently (returns `None`) on downcast mismatch.
pub fn get_as<T: JsCast>(name: &str) -> Option<T> {
    get(name)?.dyn_into::<T>().ok()
}

/// Look up a ref on an explicit scope. Useful for code that knows the
/// scope id (e.g. stored inside an async task).
pub fn get_on(scope_id: ScopeId, name: &str) -> Option<Element> {
    REFS.with(|r| r.borrow().get(&scope_id)?.get(name).cloned())
}

/// Drop every ref registered on `scope_id`. Called by scope teardown
/// so evicted components don't leak their element handles.
pub fn clear_scope(scope_id: ScopeId) {
    REFS.with(|r| {
        r.borrow_mut().remove(&scope_id);
    });
}

/// Build a plain JS object snapshot of every ref registered on
/// `scope_id`. Used to resolve the `$refs` magic — templates can read
/// `$refs.search` without importing anything.
pub fn as_object(scope_id: ScopeId) -> wasm_bindgen::JsValue {
    let obj = js_sys::Object::new();
    REFS.with(|r| {
        if let Some(map) = r.borrow().get(&scope_id) {
            for (name, el) in map.iter() {
                let _ =
                    js_sys::Reflect::set(&obj, &wasm_bindgen::JsValue::from_str(name), el.as_ref());
            }
        }
    });
    obj.into()
}
