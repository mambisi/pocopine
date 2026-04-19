//! Slot content captured at component mount time, keyed by the
//! owning component's `ScopeId`. Per RFC-011 §5.1.
//!
//! Each entry is a `DocumentFragment` cloned from the user's
//! `<template pp-slot="name" pp-let="ident">` plus the identifier
//! the user bound. The walker's slot materialiser pulls entries
//! out of here by name when it hits a `<slot>` element, cloning the
//! fragment per materialisation so the same named slot inside a
//! `pp-for` renders once per iteration.

use std::cell::RefCell;
use std::collections::HashMap;

use web_sys::DocumentFragment;

use crate::reactive::ScopeId;

pub struct UserSlot {
    pub source: DocumentFragment,
    /// `pp-let` identifier the user declared on the slot template.
    /// Empty when the user didn't write `pp-let`.
    pub ident: String,
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
/// out into the DOM).
pub fn lookup(scope_id: ScopeId, name: &str) -> Option<(DocumentFragment, String)> {
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
        Some((clone, slot.ident.clone()))
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
