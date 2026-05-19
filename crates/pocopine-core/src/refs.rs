//! Template refs — `pp-ref="name"` pins the decorated element under
//! its enclosing scope so Rust handlers can reach it imperatively.
//!
//! Two resolution flavours are available on the same `pp-ref` name:
//!
//! 1. **DOM element** ([`get`] / [`get_as`]). The element decorated
//!    with `pp-ref="search"` returns from `refs::get("search")` as an
//!    `Element`; `refs::get_as::<HtmlInputElement>("search")` downcasts.
//! 2. **Typed child-component handle** ([`get_component`]). When the
//!    decorated element happens to be a child *component*'s host
//!    (RFC 081), `refs::get_component::<Child>("body")` resolves a
//!    [`Handle<Child>`](crate::handle::Handle) for the child's Rust
//!    state — same primitive used by `Parent<T>` / `this::<T>()`,
//!    mirror direction.
//!
//! ```ignore
//! use pocopine::prelude::*;
//! use pocopine::refs;
//! use web_sys::HtmlInputElement;
//!
//! #[handlers]
//! impl SearchBar {
//!     pub fn init(&mut self) {
//!         // DOM element flavour
//!         if let Some(input) = refs::get_as::<HtmlInputElement>("search") {
//!             let _ = input.focus();
//!         }
//!     }
//!
//!     pub fn save(&mut self) {
//!         // Child-component handle flavour
//!         if let Some(body) = refs::get_component::<KeepNoteBody>("body") {
//!             let md = body.with(|b| b.editor()?.get::<Markdown>().ok())?;
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

use crate::handle::Handle;
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

/// Resolve a typed [`Handle<T>`] for the child component whose host
/// element was tagged `pp-ref="name"` in the current handler's scope.
///
/// Returns `None` when:
/// - no `pp-ref` of that name exists in the current scope (also
///   covered by [`get`] returning `None`),
/// - the tagged element is a plain DOM element rather than a child
///   component host,
/// - the registered child component's Rust type doesn't match `T`,
/// - the call site has no live scope context (e.g. fired from a
///   `tick::next` continuation after the parent's handler returned).
///
/// Implementation: the host element of every mounted child component
/// carries the child's `SCOPE_ID_KEY` (set in
/// [`crate::mount::mount_component`]); this helper reads it via
/// [`crate::mount::scope_id_of_element`] and looks up the typed
/// component state via [`crate::scope::Scope::typed`]. No DOM walk.
pub fn get_component<T: 'static>(name: &str) -> Option<Handle<T>> {
    let id = current_scope_id()?;
    get_component_on::<T>(id, name)
}

/// Explicit-scope variant of [`get_component`]. Useful when the
/// calling code already holds a [`ScopeId`] (e.g. cached at on_ready
/// time for use inside a `tick::next` continuation, mirroring
/// [`get_on`]).
pub fn get_component_on<T: 'static>(parent_scope: ScopeId, name: &str) -> Option<Handle<T>> {
    let el = get_on(parent_scope, name)?;
    let child_scope = crate::mount::scope_id_of_element(&el)?;
    // Reject plain DOM elements pinned in the same scope as the
    // caller — those carry no SCOPE_ID_KEY, but a template root
    // happens to share its component's scope id with its parent's
    // ref entry in rare layouts; this guard keeps "ref to self"
    // from resolving as "child of self".
    if child_scope == parent_scope {
        return None;
    }
    let scope = crate::scope::Scope::find(child_scope)?;
    let rc = scope.typed::<T>()?;
    Some(Handle::new(rc, child_scope))
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
