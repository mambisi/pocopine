# RFC 030 — Typed `InjectKey` (Symbol-style provide/inject)

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | Extends [RFC 027](./rfc-027-provide-inject.md) §2 (non-goal: "TypeId-keyed context") |
| **Related** | [Vue 3 `InjectionKey<T>`](https://vuejs.org/api/options-composition.html#provide-inject), [MDN `Symbol`](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Symbol), [`rfc-027-provide-inject.md`](./rfc-027-provide-inject.md) |

## 1. Summary

Replace RFC-027's string-keyed `provide("menu", …)` / `inject("menu")`
with **typed, unique `InjectKey<T>`** — a Rust equivalent of Vue 3's
`InjectionKey<T>` / the JS `Symbol("name")` idiom. Each key is a
compile-time-typed, runtime-unique token with an optional debug
name. Misuse — wrong type, wrong key, accidental collision across
crates — becomes impossible by construction.

```rust
// Define once, module-scope.
pocopine::inject_key!(ROOT: Handle<PineDialogRoot>);

// Provide.
provide(&ROOT, this::<PineDialogRoot>());

// Inject — return type inferred from the key.
let root: Option<Handle<PineDialogRoot>> = inject(&ROOT);
```

## 2. Problem

RFC-027 uses `&str` keys:

```rust
const ROOT_KEY: &str = "pine-dialog-root";
provide(ROOT_KEY, handle);
let h = inject::<Handle<PineDialogRoot>>(ROOT_KEY);
```

Three pain points surface as the Pine catalog grows:

1. **Silent collisions across crates.** Two independent libraries
   both providing `"root"` shadow each other with no compile
   error. The deeper provider wins; the shallower one's
   descendants get the wrong handle.
2. **Type argument written at every callsite.** `inject::<Handle<…>>("key")`
   restates the type on every read — easy to drift from the
   provider's type, especially after a refactor. Mismatch
   silently returns `None`.
3. **String-typing.** Typos in the key string don't compile-fail.
   `inject("pine-dialog-rot")` returns `None` at runtime, with
   no diagnostic, no grep hit against the provider.

The JS equivalent of this problem was solved long ago: `Symbol()`
creates a unique opaque token that can't collide with another
symbol even with the same description. Vue 3 uses typed symbol
keys (`InjectionKey<T>`) for exactly this reason.

## 3. Surface

### 3.1 `InjectKey<T>`

```rust
pub struct InjectKey<T: 'static> {
    id: u64,
    name: &'static str,
    _t: PhantomData<fn() -> T>,
}

impl<T: 'static> InjectKey<T> {
    /// Mint a new unique key. `name` is a debug label only —
    /// two calls with the same name produce distinct keys.
    pub fn new(name: &'static str) -> Self;

    /// Debug name. Surfaces in devtools + panic messages.
    pub fn name(&self) -> &'static str;
}
```

Unique id comes from a process-local atomic counter (`AtomicU64`).
Every `InjectKey::new` call returns a distinct key even with
identical `name` arguments — same semantic as `Symbol("name")`.

### 3.2 `provide` / `inject`

```rust
pub fn provide<T: 'static>(key: &InjectKey<T>, value: T);

/// Return type drives the lookup — no turbofish on `inject`.
pub fn inject<T: Clone + 'static>(key: &InjectKey<T>) -> Option<T>;
```

Resolution walks the scope-parent chain exactly as RFC-027
specifies; only the key space changes from `String` to
`InjectKey.id`.

### 3.3 `inject_key!` macro

Ergonomic definition, one per compound:

```rust
inject_key!(ROOT: Handle<PineDialogRoot>);
// expands to roughly:
// pub(crate) static ROOT: pocopine::InjectKey<Handle<PineDialogRoot>> =
//     pocopine::InjectKey::__const_new("pine-dialog-root::ROOT");
```

Bound at program start via a `ctor`-style one-shot so the id is
stable for the process lifetime. Name is derived from `module_path! + "::" + ident`.

### 3.4 Interaction with JS `Symbol`

The underlying id is a Rust `u64` — we deliberately don't mirror
it into a `js_sys::Symbol` for two reasons:

1. **Hot-path cost.** provide/inject fires during mount; an
   extra wasm↔JS hop on every call taxes the scope chain walk.
2. **No JS-side consumer today.** Templates don't call inject
   directly; all provide/inject happens in Rust. The moment a
   JS-side consumer appears (a dev extension, an interop API),
   we can expose `InjectKey::as_symbol() -> js_sys::Symbol` that
   lazily mints one per key and caches it — additive, no
   breaking change.

For debugging, `InjectKey.name()` shows up in the `__pp_devtools`
panel's "provides on scope N" list the way Vue 3 shows symbols.

## 4. Migration

### 4.1 Back-compat shim

`provide(&str, T)` and `inject::<T>(&str)` stay for one release as
`#[deprecated]` forwards to an internal "legacy" key table. Strings
and InjectKeys share no namespace — mixing them in the same
compound is a bug, flagged by the shim at runtime.

### 4.2 Per-compound migration

Mechanical change per compound in `crates/pine/src/*/mod.rs`:

```diff
-const ROOT_KEY: &str = "pine-dialog-root";
+inject_key!(ROOT_KEY: Handle<PineDialogRoot>);

-provide(ROOT_KEY, this::<Self>());
+provide(&ROOT_KEY, this::<Self>());

-if let Some(root) = inject::<Handle<PineDialogRoot>>(ROOT_KEY) { … }
+if let Some(root) = inject(&ROOT_KEY) { … }
```

Type-argument noise drops at every call site. Every compound in
the catalog migrates in one pass — roughly ten modules, straight
find-and-replace.

### 4.3 Removed

After migration settles:

- `provide(&str, …)` / `inject::<T>(&str)` surface.
- All `const KEY: &str = "..."` declarations in `crates/pine/`.
- Per-site type turbofishes on `inject`.

## 5. Semantics (unchanged from RFC-027)

- **Scope chain** — same `set_parent` / `parent_of` walk.
- **Type matching** — `Any::downcast_ref::<T>` still gates the
  return. With typed keys this becomes a redundant check (the
  key's `T` already guarantees the stored type), but we keep it
  as a belt-and-braces guard against the unsafe path of mixing
  an InjectKey across incompatible type instantiations.
- **Cleanup** — `clear_scope(id)` drops the inner map whole; key
  identity doesn't matter.

## 6. Implementation sketch

### 6.1 `crates/pocopine-core/src/context.rs`

```rust
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_KEY_ID: AtomicU64 = AtomicU64::new(1);

pub struct InjectKey<T: 'static> {
    id: u64,
    name: &'static str,
    _t: PhantomData<fn() -> T>,
}

impl<T: 'static> InjectKey<T> {
    pub fn new(name: &'static str) -> Self {
        Self {
            id: NEXT_KEY_ID.fetch_add(1, Ordering::Relaxed),
            name,
            _t: PhantomData,
        }
    }
    pub fn name(&self) -> &'static str { self.name }
    pub fn id(&self) -> u64 { self.id }
}

type ProvideMap = HashMap<u64, Box<dyn Any>>;

thread_local! {
    static PARENTS: RefCell<HashMap<ScopeId, ScopeId>> = RefCell::new(HashMap::new());
    static PROVIDES: RefCell<HashMap<ScopeId, ProvideMap>> = RefCell::new(HashMap::new());
}

pub fn provide<T: 'static>(key: &InjectKey<T>, value: T) {
    let scope = current_scope_id()
        .expect("pocopine::provide called outside a handler");
    PROVIDES.with(|p| {
        p.borrow_mut()
            .entry(scope)
            .or_default()
            .insert(key.id(), Box::new(value));
    });
}

pub fn inject<T: Clone + 'static>(key: &InjectKey<T>) -> Option<T> {
    let mut scope = current_scope_id()?;
    loop {
        let hit = PROVIDES.with(|p| {
            p.borrow()
                .get(&scope)
                .and_then(|entries| entries.get(&key.id()))
                .and_then(|any| any.downcast_ref::<T>())
                .cloned()
        });
        if let Some(v) = hit { return Some(v); }
        match parent_of(scope) {
            Some(parent) => scope = parent,
            None => return None,
        }
    }
}
```

### 6.2 `inject_key!` macro

```rust
#[macro_export]
macro_rules! inject_key {
    ($vis:vis $name:ident : $ty:ty) => {
        $vis static $name: ::std::sync::LazyLock<$crate::InjectKey<$ty>> =
            ::std::sync::LazyLock::new(|| {
                $crate::InjectKey::new(concat!(module_path!(), "::", stringify!($name)))
            });
    };
}
```

Callers deref via `&*ROOT_KEY`. The `LazyLock` init is one-shot;
the key id is assigned on first deref and stays stable for the
process lifetime.

### 6.3 `pocopine_core::scope::Scope::remove` already calls
`context::clear_scope(id)` — no change.

## 7. Worked migration — Dialog

Before:

```rust
const ROOT_KEY: &str = "pine-dialog-root";
const TITLE_ID_KEY: &str = "pine-dialog-title-id";

provide(ROOT_KEY, this::<Self>());
provide(TITLE_ID_KEY, self.title_id.clone());

if let Some(root) = inject::<Handle<PineDialogRoot>>(ROOT_KEY) { … }
if let Some(id) = inject::<String>(TITLE_ID_KEY) { … }
```

After:

```rust
inject_key!(ROOT: Handle<PineDialogRoot>);
inject_key!(TITLE_ID: String);

provide(&ROOT, this::<Self>());
provide(&TITLE_ID, self.title_id.clone());

if let Some(root) = inject(&ROOT) { … }
if let Some(id) = inject(&TITLE_ID) { … }
```

Reads stay one line; no type noise at the callsite.

## 8. Edge cases

- **Cross-crate same-name keys** — different `module_path!()`
  means different debug names, and different runtime ids. Two
  crates each defining `ROOT` in `my_lib::ROOT` vs
  `other_lib::ROOT` are cleanly isolated.
- **Key defined inside a generic function** — `InjectKey::new`
  runs once per template instantiation. Distinct keys per
  monomorphisation, which is correct (each instantiation is
  logically a different key type anyway).
- **Dynamic keys** — `InjectKey::new` at runtime still works for
  cases that need a keyed family (e.g. a dynamic list of
  compounds). The macro form is optional sugar, not required.
- **Feature-gated code** — same `InjectKey::new` call in
  different cfg bodies yields different ids at different
  compilations — expected, matches string keys today.

## 9. Non-goals

- **JS Symbol mirroring** — deferred (§3.4). Add `as_symbol()`
  when a JS-side consumer needs it.
- **Runtime introspection of all provides** — devtools can
  iterate the `PROVIDES` map and resolve id → name via a
  process-local `BTreeMap<u64, &'static str>` registry. Nice
  follow-up, not part of v1.
- **Typed defaults** — Vue's `inject(key, defaultValue)`. Stick
  with `Option<T>` + `unwrap_or(…)` at the callsite.

## 10. Rollout

1. Land `InjectKey` + `inject_key!` in pocopine-core, alongside
   the deprecated string-keyed path.
2. Migrate Pine compounds one per commit (Dialog, AlertDialog,
   Popover, DropdownMenu, Tabs, Tooltip, Collapsible, Accordion,
   Avatar, RadioGroup). ~10 commits, mechanical.
3. Remove the string-keyed surface in the release after that.
