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

/// Opaque, typed, unique context key. Created once per logical
/// slot (module-scope static via [`create_context!`] / the
/// deprecated [`inject_key!`], or runtime via `ContextKey::new`).
/// The `T` type parameter pins the value type so [`inject`] returns
/// `Option<T>` with no turbofish at the callsite.
///
/// `PhantomData<fn() -> T>` (contravariant in `T`) keeps the type
/// parameter in the signature without requiring `T: 'static` in
/// unrelated positions; `T: 'static` is enforced on use via
/// [`provide`] / [`inject`].
pub struct ContextKey<T: 'static> {
    id: u64,
    name: &'static str,
    _t: PhantomData<fn() -> T>,
}

/// Deprecated alias kept for migration. New code should use
/// [`ContextKey`] (declared via [`create_context!`]).
#[deprecated(note = "use create_context! / ContextKey instead (RFC 056 §6.3)")]
pub type InjectKey<T> = ContextKey<T>;

impl<T: 'static> ContextKey<T> {
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

    /// Method-style provide. Equivalent to [`provide(&self, value)`](provide)
    /// but reads naturally on a typed key declared with
    /// [`create_context!`] (RFC 056 §6.4):
    ///
    /// ```ignore
    /// ROOT.provide(this::<Self>());
    /// ```
    pub fn provide(&self, value: T)
    where
        T: Any + 'static,
    {
        provide(self, value);
    }

    /// Method-style inject. Equivalent to [`inject(&self)`](inject).
    pub fn inject(&self) -> Option<T>
    where
        T: Clone + Any + 'static,
    {
        inject(self)
    }
}

// `Copy` is the canonical form for an opaque token; `Clone` follows
// automatically via `{ *self }` (per clippy's non-canonical-clone
// lint — don't spell out field-by-field when `Copy` is available).
impl<T: 'static> Copy for ContextKey<T> {}
impl<T: 'static> Clone for ContextKey<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> std::fmt::Debug for ContextKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextKey")
            .field("name", &self.name)
            .field("id", &self.id)
            .finish()
    }
}

/// Entries are keyed by `InjectKey::id()` — a `u64`. Debug names
/// aren't part of the key; they only drive diagnostics.
type ProvideMap = HashMap<u64, Box<dyn Any>>;

thread_local! {
    /// Child → parent map, populated by the mount when a new
    /// scope is minted. Cleared on `Scope::remove`.
    static PARENTS: RefCell<HashMap<ScopeId, ScopeId>> =
        RefCell::new(HashMap::new());

    /// Scope → (key.id → boxed value). Populated by `provide`,
    /// queried by `inject` along the parent chain.
    static PROVIDES: RefCell<HashMap<ScopeId, ProvideMap>> =
        RefCell::new(HashMap::new());
}

/// Record that `parent` is the scope that enclosed `child` at
/// mount time. Called by the mount right after minting the
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
pub fn provide<T: Any + 'static>(key: &ContextKey<T>, value: T) {
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
pub fn inject<T: Clone + Any + 'static>(key: &ContextKey<T>) -> Option<T> {
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

/// Bulk counterpart to [`clear_scope`] for compiled `pp-for` row scopes.
/// Row scopes normally own only a parent edge, but clearing both tables keeps
/// this correct if a future row feature adds a provided value.
pub(crate) fn clear_scopes(scopes: &[ScopeId]) {
    if scopes.is_empty() {
        return;
    }
    PARENTS.with(|parents| {
        let mut parents = parents.borrow_mut();
        for scope in scopes {
            parents.remove(scope);
        }
    });
    PROVIDES.with(|provides| {
        let mut provides = provides.borrow_mut();
        for scope in scopes {
            provides.remove(scope);
        }
    });
}

/// Marker trait paired with each [`ContextKey<T>`] declared via
/// [`create_context!`]. Lets `Inject<KEY, T>` (RFC 056 §6.5) name a
/// key at type level on stable Rust without const generics.
///
/// The marker type lives in the *type* namespace and shares its
/// identifier with the value-namespace static, e.g. `ROOT` resolves
/// to the marker type in `Inject<ROOT, …>` and to the `LazyLock`
/// static everywhere else (`ROOT.provide(...)`, `ROOT.inject()`).
pub trait ContextMarker: 'static {
    type Value: Clone + Any + 'static;
    fn key() -> &'static ContextKey<Self::Value>;
}

/// Define a module-scope [`ContextKey<T>`] plus the matching
/// [`ContextMarker`] type. The key's debug name is derived from
/// `module_path!()` plus the identifier so collisions across crates
/// stay impossible even if two crates pick the same local identifier
/// (RFC 056 §6.3 — the successor to `inject_key!`).
///
/// Expands to two items:
/// * `static <name>: LazyLock<ContextKey<T>>` (value namespace) so
///   `<name>.provide(...)` / `<name>.inject()` work.
/// * `struct <name> {}` (type namespace) so `Inject<<name>, T>` can
///   name the key at the type level via [`ContextMarker`].
///
/// ```ignore
/// pocopine::create_context!(pub(crate) ROOT: Handle<PineDialogRoot>);
/// // later:
/// ROOT.provide(this::<PineDialogRoot>());
/// let root = ROOT.inject();
///
/// fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) {
///     root.update(|dialog| dialog.close());
/// }
/// ```
#[macro_export]
macro_rules! create_context {
    ($vis:vis $name:ident : $ty:ty) => {
        $vis static $name: ::std::sync::LazyLock<$crate::context::ContextKey<$ty>> =
            ::std::sync::LazyLock::new(|| {
                $crate::context::ContextKey::new(
                    concat!(module_path!(), "::", stringify!($name))
                )
            });

        #[allow(non_camel_case_types)]
        $vis struct $name {}

        impl $crate::context::ContextMarker for $name {
            type Value = $ty;
            fn key() -> &'static $crate::context::ContextKey<$ty> {
                &*$name
            }
        }
    };
}

/// Deprecated alias for [`create_context!`]. Kept so existing call
/// sites keep building during the RFC 056 migration window. New code
/// should reach for [`create_context!`].
#[macro_export]
macro_rules! inject_key {
    ($vis:vis $name:ident : $ty:ty) => {
        $crate::create_context!($vis $name : $ty);
    };
}
