//! `$id` — monotonic, per-component-instance unique identifiers
//! for a11y wiring. Per RFC-018.
//!
//! Reading `$id` in a template returns a short, stable string of the
//! form `pp-<N>`. The first access on a scope mints a fresh value;
//! every subsequent access on the same scope returns the cached one,
//! so two bindings can safely reference the same id (e.g. `label[for]`
//! and the corresponding `input[id]`).
//!
//! Cache is cleared when the scope is dropped (`Scope::remove` calls
//! [`clear_scope`]), so long-lived pages that mint + drop many
//! components don't leak counter-keyed strings.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

thread_local! {
    /// Global counter. Monotonic across the whole document lifetime
    /// — no recycling — so IDs are unique forever (until page
    /// reload).
    static NEXT_ID: Cell<u64> = const { Cell::new(0) };

    /// Per-scope cache. A scope that has never called [`generate`]
    /// is absent from the map.
    static CACHE: RefCell<HashMap<ScopeId, String>> =
        RefCell::new(HashMap::new());
}

/// Return the stable `$id` for `scope`. First call mints a fresh
/// one; subsequent calls return the cached string.
pub fn generate(scope: ScopeId) -> String {
    CACHE.with(|c| {
        let mut map = c.borrow_mut();
        if let Some(existing) = map.get(&scope) {
            return existing.clone();
        }
        let id = NEXT_ID.with(|n| {
            let v = n.get() + 1;
            n.set(v);
            v
        });
        let s = format!("pp-{id}");
        map.insert(scope, s.clone());
        s
    })
}

/// Return the `$id` of the scope whose handler is currently on the
/// stack, if any. Useful inside `#[handlers]` methods that need to
/// reach back into their own id for e.g. focusing a labelled input.
pub fn current() -> Option<String> {
    current_scope_id().map(generate)
}

/// Drop any cached id for `scope`. Called from `Scope::remove`
/// alongside `refs::clear_scope`.
pub fn clear_scope(scope: ScopeId) {
    CACHE.with(|c| {
        c.borrow_mut().remove(&scope);
    });
}
