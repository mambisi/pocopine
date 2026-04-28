//! Slot content captured at component mount time, keyed by the
//! owning component's `ScopeId`. Per RFC-011 §5.1.
//!
//! Each entry is a `DocumentFragment` cloned from the user's
//! `<template pp-slot="name" pp-let="ident">` plus the identifier
//! the user bound. The walker's slot materialiser pulls entries
//! out of here by name when it hits a `<slot>` element, cloning the
//! fragment per materialisation so the same named slot inside a
//! `pp-for` renders once per iteration.
//!
//! **RFC 061 — deprecated, deletion in Phase 3.** Slot content
//! flows through `slot_fragment::*` (compiled, parent-emitted)
//! once the adopted-DOM bridge retires. The `pub mod slots`
//! declaration in `lib.rs` carries the `#[deprecated]`
//! attribute.

use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::JsValue;
use web_sys::DocumentFragment;

use crate::reactive::ScopeId;

pub struct UserSlot {
    pub source: DocumentFragment,
    /// `pp-let` identifier the user declared on the slot template.
    /// Empty when the user didn't write `pp-let`.
    pub ident: String,
    /// Scope that *authored* this slot content (the parent
    /// template, not the child component that declared the slot).
    /// The walker's slot materialiser pins this as the borrowed
    /// scope on inserted elements so `@click` / `pp-text` / etc.
    /// inside the slot resolve against the caller — even when the
    /// slot lives inside a teleported subtree whose DOM ancestors
    /// point at a different scope.
    pub owner_scope_id: ScopeId,
    pub owner_proxy: JsValue,
}

pub struct SlotStore {
    pub by_name: HashMap<String, UserSlot>,
}

thread_local! {
    static STORES: RefCell<HashMap<ScopeId, SlotStore>> = RefCell::new(HashMap::new());
}

/// Register the map of named slots captured for a component
/// instance. Called once by `walker::mount_component`.
pub fn put(scope_id: ScopeId, store: SlotStore) {
    if store.by_name.is_empty() {
        return;
    }
    STORES.with(|s| s.borrow_mut().insert(scope_id, store));
}

/// Look up a user-provided slot by name on `scope_id`. Returns a
/// clone of the source fragment (ready to have its children moved
/// out into the DOM), the `pp-let` ident, and the owner scope id
/// + proxy (for borrowed-scope binding on the inserted content).
pub fn lookup(
    scope_id: ScopeId,
    name: &str,
) -> Option<(DocumentFragment, String, ScopeId, JsValue)> {
    STORES.with(|s| {
        let stores = s.borrow();
        let store = stores.get(&scope_id)?;
        let slot = store.by_name.get(name)?;
        let clone = slot
            .source
            .clone_node_with_deep(true)
            .ok()?
            .dyn_into::<DocumentFragment>()
            .ok()?;
        Some((
            clone,
            slot.ident.clone(),
            slot.owner_scope_id,
            slot.owner_proxy.clone(),
        ))
    })
}

/// RFC-032 — return the names of all slots captured for `scope_id`,
/// in insertion order. Used by the `Slots` extractor. Returns an
/// empty vec when the scope has no slot store.
pub fn names_for(scope_id: ScopeId) -> Vec<String> {
    STORES.with(|s| {
        s.borrow()
            .get(&scope_id)
            .map(|store| store.by_name.keys().cloned().collect())
            .unwrap_or_default()
    })
}

/// Drop any stored slots for a component whose scope has been
/// removed. Hooked from `Scope::remove` so the map doesn't outlive
/// the component.
pub fn clear(scope_id: ScopeId) {
    STORES.with(|s| {
        s.borrow_mut().remove(&scope_id);
    });
}

// `DocumentFragment` needs `JsCast` in scope for the `dyn_into` above.
use wasm_bindgen::JsCast;
