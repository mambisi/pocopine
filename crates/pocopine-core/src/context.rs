//! Parent-scope context — RFC-027.
//!
//! A component can `provide("key", value)` on its own scope; any
//! descendant can `inject("key")` and walk the scope-parent chain
//! to find the first matching entry. Mirrors Vue 3's
//! provide / inject shape.
//!
//! Parent relationships are tracked here explicitly (not through
//! the DOM), so teleported children and slot-materialised content
//! still resolve to their *authoring* parent — regardless of where
//! they physically render.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

type ProvideMap = HashMap<String, Box<dyn Any>>;

thread_local! {
    /// Child → parent map, populated by the walker when a new
    /// scope is minted. Cleared on `Scope::remove`.
    static PARENTS: RefCell<HashMap<ScopeId, ScopeId>> =
        RefCell::new(HashMap::new());

    /// Scope → (key → boxed value). Populated by `provide`, queried
    /// by `inject` along the parent chain.
    static PROVIDES: RefCell<HashMap<ScopeId, ProvideMap>> =
        RefCell::new(HashMap::new());
}

/// Record that `parent` is the scope that enclosed `child` at
/// mount time. Called by the walker right after minting the
/// child's scope.
pub fn set_parent(child: ScopeId, parent: ScopeId) {
    PARENTS.with(|p| {
        p.borrow_mut().insert(child, parent);
    });
}

/// Return the parent scope id for `scope`, if one was recorded.
pub fn parent_of(scope: ScopeId) -> Option<ScopeId> {
    PARENTS.with(|p| p.borrow().get(&scope).copied())
}

/// Store `value` under `key` on the current scope. RFC-027.
///
/// Panics outside a handler / lifecycle context — a provide call
/// that couldn't identify its scope is always a programming error
/// and we'd rather surface it loudly than silently drop.
pub fn provide<T: Any + 'static>(key: &str, value: T) {
    let scope = current_scope_id()
        .expect("pocopine::provide called outside a handler / lifecycle context");
    PROVIDES.with(|p| {
        p.borrow_mut()
            .entry(scope)
            .or_default()
            .insert(key.to_string(), Box::new(value));
    });
}

/// Walk up the scope chain starting at the current scope and
/// return a clone of the first provided value matching `key` and
/// type `T`. Returns `None` when no ancestor provided the key or
/// the stored value is of a different type.
///
/// Panics outside a handler / lifecycle context.
pub fn inject<T: Clone + Any + 'static>(key: &str) -> Option<T> {
    let mut scope = current_scope_id()
        .expect("pocopine::inject called outside a handler / lifecycle context");
    loop {
        let hit = PROVIDES.with(|p| {
            let map = p.borrow();
            map.get(&scope)
                .and_then(|entries| entries.get(key))
                .and_then(|any| any.downcast_ref::<T>())
                .cloned()
        });
        if let Some(v) = hit {
            return Some(v);
        }
        match parent_of(scope) {
            Some(parent) => scope = parent,
            None => return None,
        }
    }
}

/// Drop all provide entries + the parent pointer for `scope`.
/// Called from `Scope::remove` alongside the other per-scope
/// side-table cleaners.
pub fn clear_scope(scope: ScopeId) {
    PARENTS.with(|p| {
        p.borrow_mut().remove(&scope);
    });
    PROVIDES.with(|p| {
        p.borrow_mut().remove(&scope);
    });
}
