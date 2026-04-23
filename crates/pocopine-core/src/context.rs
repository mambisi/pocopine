//! Parent-scope context — RFC-027, typed-key revision per RFC-030.
//!
//! A component can `provide(&KEY, value)` on its own scope; any
//! descendant can `inject(&KEY)` and walk the scope-parent chain
//! to find the first matching entry. The key is an `InjectKey<T>`
//! (a Rust cousin of `Symbol("name")` + Vue 3's `InjectionKey<T>`):
//! unique by construction, typed in the value it carries, with a
//! debug name that shows up in devtools.
//!
//! Parent relationships are tracked here explicitly (not through
//! the DOM), so teleported children and slot-materialised content
//! still resolve to their *authoring* parent — regardless of where
//! they physically render.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::reactive::ScopeId;
use crate::scope::current_scope_id;

/// Process-local counter for minting fresh `InjectKey` ids. Starts
/// at 1 so 0 is reserved for "unset"-style sentinels if we ever
/// need one. Monotonic, never reused — matches Symbol identity
/// semantics (two `Symbol("foo")` calls return distinct symbols).
static NEXT_KEY_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque, typed, unique injection key. Created once per logical
/// slot (module-scope static via [`inject_key!`] or runtime via
/// `InjectKey::new`). The `T` type parameter pins the value type
/// so `inject` returns `Option<T>` with no turbofish at the
/// callsite.
///
/// `PhantomData<fn() -> T>` (contravariant in `T`) keeps the type
/// parameter in the signature without requiring `T: 'static` in
/// unrelated positions; `T: 'static` is enforced on use via
/// [`provide`] / [`inject`].
pub struct InjectKey<T: 'static> {
    id: u64,
    name: &'static str,
    _t: PhantomData<fn() -> T>,
}

impl<T: 'static> InjectKey<T> {
    /// Mint a fresh unique key. Two calls — even with the same
    /// `name` — yield keys that never collide. `name` is a debug
    /// label only.
    pub fn new(name: &'static str) -> Self {
        Self {
            id: NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed),
            name,
            _t: PhantomData,
        }
    }

    /// Debug label, surfaces in devtools + error messages.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Unique process-local id; stable for the key's lifetime.
    /// Used as the HashMap key inside the provides table.
    pub fn id(&self) -> u64 {
        self.id
    }
}

// `Copy` is the canonical form for an opaque token; `Clone` follows
// automatically via `{ *self }` (per clippy's non-canonical-clone
// lint — don't spell out field-by-field when `Copy` is available).
impl<T: 'static> Copy for InjectKey<T> {}
impl<T: 'static> Clone for InjectKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> std::fmt::Debug for InjectKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectKey")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

/// Entries are keyed by `InjectKey::id()` — a `u64`. Debug names
/// aren't part of the key; they only drive diagnostics.
type ProvideMap = HashMap<u64, Box<dyn Any>>;

thread_local! {
    /// Child → parent map, populated by the walker when a new
    /// scope is minted. Cleared on `Scope::remove`.
    static PARENTS: RefCell<HashMap<ScopeId, ScopeId>> =
        RefCell::new(HashMap::new());

    /// Scope → (key.id → boxed value). Populated by `provide`,
    /// queried by `inject` along the parent chain.
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

/// Store `value` under `key` on the current scope.
///
/// Panics outside a handler / lifecycle context — a provide call
/// that couldn't identify its scope is always a programming error
/// and we'd rather surface it loudly than silently drop.
pub fn provide<T: Any + 'static>(key: &InjectKey<T>, value: T) {
    let scope =
        current_scope_id().expect("pocopine::provide called outside a handler / lifecycle context");
    PROVIDES.with(|p| {
        p.borrow_mut()
            .entry(scope)
            .or_default()
            .insert(key.id(), Box::new(value));
    });
}

/// Walk up the scope chain starting at the current scope and
/// return a clone of the first provided value whose key matches.
/// Type is inferred from the key — no turbofish.
///
/// Returns `None` when no ancestor provided this key, or when the
/// stored value's type doesn't match the key's `T` (which should
/// be impossible through the public API — the key's type guards
/// the provide side — but stays as a belt-and-braces guard against
/// `Any::downcast_ref` inconsistencies across crate boundaries).
///
/// Panics outside a handler / lifecycle context.
pub fn inject<T: Clone + Any + 'static>(key: &InjectKey<T>) -> Option<T> {
    let mut scope =
        current_scope_id().expect("pocopine::inject called outside a handler / lifecycle context");
    loop {
        let hit = PROVIDES.with(|p| {
            let map = p.borrow();
            map.get(&scope)
                .and_then(|entries| entries.get(&key.id()))
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

/// Devtools-only accessor: every (key-id, provider-scope) pair
/// resolvable from `scope`. Walks the same parent chain as
/// [`inject`] but collects instead of returning on the first hit,
/// so the panel can show the full chain. The key's debug `name`
/// isn't recoverable from its id alone — pair it with a separate
/// key-id → name registry if needed; for now the panel shows the
/// numeric id + the provider scope id.
///
/// Note: this is a best-effort introspection. Keys minted at
/// runtime via [`InjectKey::new`] have module-independent debug
/// names that aren't registered anywhere; panels using this helper
/// should treat names as optional.
#[cfg(feature = "devtools")]
pub fn inject_chain(scope: ScopeId) -> Vec<(u64, ScopeId)> {
    let mut out: Vec<(u64, ScopeId)> = Vec::new();
    let mut cur = scope;
    loop {
        PROVIDES.with(|p| {
            if let Some(entries) = p.borrow().get(&cur) {
                for key_id in entries.keys() {
                    out.push((*key_id, cur));
                }
            }
        });
        match parent_of(cur) {
            Some(parent) => cur = parent,
            None => break,
        }
    }
    out
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

/// Define a module-scope `InjectKey<T>` bound lazily on first use.
/// The key's debug name is derived from `module_path!()` plus the
/// identifier so collisions across crates stay impossible even if
/// two crates pick the same local identifier.
///
/// ```ignore
/// pocopine_core::inject_key!(pub(crate) ROOT: Handle<PineDialogRoot>);
/// // later:
/// provide(&ROOT, this::<PineDialogRoot>());
/// let root = inject(&ROOT);
/// ```
#[macro_export]
macro_rules! inject_key {
    ($vis:vis $name:ident : $ty:ty) => {
        $vis static $name: ::std::sync::LazyLock<$crate::context::InjectKey<$ty>> =
            ::std::sync::LazyLock::new(|| {
                $crate::context::InjectKey::new(
                    concat!(module_path!(), "::", stringify!($name))
                )
            });
    };
}
