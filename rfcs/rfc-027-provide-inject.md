# RFC 027 — Parent-scope context (`provide` / `inject`)

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-011-scoped-slots.md`](./rfc-011-scoped-slots.md), [`rfc-023-pine-mvp.md`](./rfc-023-pine-mvp.md), [Vue 3 `provide` / `inject`](https://vuejs.org/guide/components/provide-inject.html) |

## 1. Summary

Let a component **provide** a value under a string key, and any
descendant component **inject** it by looking up the key along the
scope-parent chain. Unblocks every Radix-style composite layout:
`<pine-dropdown-menu-item>` reading its `<pine-dropdown-menu>`
root, `<pine-radio-item>` writing to its `<pine-radio-group>`,
`<pine-tab-panel>` reading the selected value from
`<pine-tabs>`, etc.

```rust
// Parent — provides a handle to itself under "menu".
#[handlers]
impl PineDropdownMenuRoot {
    pub fn on_mount(&mut self) {
        pocopine::provide("menu", pocopine::this::<Self>());
    }
}
```

```rust
// Child — injects the handle + calls methods on the root.
#[handlers]
impl PineDropdownMenuItem {
    pub fn on_click(&mut self) {
        if let Some(menu) = pocopine::inject::<Handle<PineDropdownMenuRoot>>("menu") {
            menu.update(|m| m.close());
        }
    }
}
```

## 2. Non-goals

- **Computed-value providers that auto-rerun** (Vue's
  `provide('k', computed(() => …))` shape). For v0, providers
  store a one-shot value — usually a `Handle<T>` or a
  `Signal<T>`. Reactivity flows through whatever was provided
  (handle-update, signal-set); the `inject` call itself isn't
  a reactive subscription.
- **TypeId-keyed context.** Vue 3 supports Symbol keys that give
  compile-time distinctness across unrelated libraries. For
  pocopine v0 we use string keys. A future RFC can add typed
  context via a `ContextKey<T>` shape.
- **Traversing arbitrary DOM ancestry across iframes or shadow
  roots.** Scope chain only.
- **Automatic cleanup on re-provide.** Calling `provide("k", v1)`
  then `provide("k", v2)` from the same scope replaces v1 in
  place. No stack, no history.

## 3. Surface

```rust
pub mod pocopine {
    /// Store `value` under `key` on the current component's
    /// scope. Injections from descendant scopes that query the
    /// same key walk up the scope chain and find this entry.
    ///
    /// Panics outside a handler / lifecycle context.
    pub fn provide<T: 'static>(key: &str, value: T);

    /// Walk up the scope chain starting at the current scope,
    /// returning a clone of the first matching provided value
    /// of type `T`. Returns `None` when no ancestor provided
    /// the key, or when the type doesn't match.
    ///
    /// Panics outside a handler / lifecycle context.
    pub fn inject<T: Clone + 'static>(key: &str) -> Option<T>;
}
```

### 3.1 Typical shapes of `T`

- **`Handle<ParentStruct>`** — the most common. Child calls
  methods on the parent, reads fields via `handle.with()`.
- **`Signal<V>` / `RwSignal<V>`** — fine-grained reactivity without
  exposing the whole parent.
- **Plain values** (`String`, `u32`) — static configuration.
- **`Rc<SomeSharedState>`** — author-defined shared state.

## 4. Semantics

### 4.1 Scope parent tracking

Every scope remembers its *parent* — the scope that enclosed the
component tag at mount time. The walker records this relationship
in a thread-local `HashMap<ScopeId, ScopeId>` side-table at the
point the new scope is created (inside `mount_component`,
`pp-for`'s `LoopScope`, `<slot>`'s `SlotScope`, and `pp-teleport`'s
borrowed-scope bind).

Resolution at `inject` time walks the chain:

```
current → parent(current) → parent(parent(current)) → …
```

Stopping at the first scope whose provides map contains the key
(with a matching type) or when the chain hits `None`.

Parent is recorded **once** at scope birth. If the parent scope
is evicted mid-life (rare — parent outlives child by
construction), the child's inject still walks the cached id; if
it's missing from the registry, walk stops there.

### 4.2 Teleport + slot preserving context

The `parent_of` relationship tracks the **authoring** parent, not
the rendered DOM ancestor. Teleported content keeps its logical
parent, so a `<pine-dropdown-menu-item>` inside a teleported menu
still injects its root from the right scope.

This matches the RFC-011 slot-owner fix we made earlier: slot
content resolves directives against the caller's scope. The
parent chain for injection reuses the same "caller's scope"
attribution.

### 4.3 Type matching

`inject::<T>("key")` uses `Any::downcast_ref::<T>()` on the
stored `Box<dyn Any>`. Exact type match — `Handle<Foo>` and
`Handle<Bar>` don't alias. On mismatch, `inject` returns `None`
(same as "no provider found"). We intentionally don't panic on
type mismatch: it's legitimate for two unrelated contexts to
share a string key in some app, and silent None is the right
failure mode (like HashMap get).

### 4.4 Cleanup

When a scope evicts (`Scope::remove`), its provides entry AND
its parent-pointer entry drop. Mirrors the existing
`refs::clear_scope` + `slots::clear` + `id::clear_scope` trio.

## 5. Implementation

Two files:

### `crates/pocopine-core/src/context.rs` (new, ~100 lines)

```rust
thread_local! {
    static PARENTS: RefCell<HashMap<ScopeId, ScopeId>> = …;
    static PROVIDES: RefCell<HashMap<ScopeId, HashMap<String, Box<dyn Any>>>> = …;
}

pub fn set_parent(child: ScopeId, parent: ScopeId);
pub fn parent_of(scope: ScopeId) -> Option<ScopeId>;
pub fn provide<T: 'static>(key: &str, value: T);
pub fn inject<T: Clone + 'static>(key: &str) -> Option<T>;
pub fn clear_scope(scope: ScopeId);
```

`inject` walks parents via `parent_of`; stops at first hit or
missing parent.

### `crates/pocopine-core/src/walker.rs` + `scope.rs`

- `mount_component` calls `context::set_parent(child.id,
  parent.id)` right after `instantiate`, sourcing the parent via
  `enclosing_scope(el)`.
- `materialize_slot` calls `set_parent` on the `SlotScope`'s id
  → the caller's scope (for scoped slots). Plain-slot content
  has no inner scope; it binds directly to the caller's scope,
  so injection already works via `current_scope_id`.
- `for_.rs` calls `set_parent` on each `LoopScope` → the
  enclosing scope.
- `Scope::remove` calls `context::clear_scope(id)`.

Re-exported from `pocopine::{provide, inject}`.

## 6. Worked example — Radix-shaped Dropdown skeleton

```rust
// Root — owns state, provides a handle to itself.
#[component(template = "DropdownMenuRoot.poco")]
pub struct DropdownMenuRoot {
    pub open: bool,
}

#[handlers]
impl DropdownMenuRoot {
    pub fn on_mount(&mut self) {
        pocopine::provide("dropdown-menu", pocopine::this::<Self>());
    }
    pub fn close(&mut self) {
        self.open = false;
    }
}

// Item — reads the parent's handle and calls close() on click.
#[component(template = "DropdownMenuItem.poco")]
pub struct DropdownMenuItem {}

#[handlers]
impl DropdownMenuItem {
    pub fn on_click(&mut self) {
        if let Some(menu) =
            pocopine::inject::<Handle<DropdownMenuRoot>>("dropdown-menu")
        {
            menu.update(|m| m.close());
        }
    }
}
```

Usage:

```html
<dropdown-menu-root pp-model:open="open">
  <dropdown-menu-item>Copy</dropdown-menu-item>
  <dropdown-menu-item>Paste</dropdown-menu-item>
</dropdown-menu-root>
```

Regardless of teleport / pp-if / pp-for between root and items,
items find the root via the scope chain.

## 7. Edge cases

- **Inject before parent has provided.** `on_mount` on a child
  can fire before its parent's `on_mount` (children mount first
  in pocopine's pre-order walk). Inject returns `None`.
  Workaround: call inject from `on_ready` (RFC-026/029) — fires
  post-walk, parent's `on_mount` has run.
- **Circular provide/inject.** Not possible by construction:
  parent chain is a tree (each scope has at most one parent).
- **Provide from a non-component scope** (LoopScope / SlotScope).
  Works — those scopes are real scope ids in the registry.
  Useful for a `pp-for` over radio options to provide per-item
  context to its children.
- **Inject the wrong type.** Returns `None`. No panic, no
  warning. A typed-key future RFC can upgrade this.
- **Reactive reads inside inject.** Currently the `inject` call
  is NOT a reactive subscription. To get reactive updates on
  provided state, provide a `Signal<T>` / `RwSignal<T>` / a
  `Handle` whose fields the child reads reactively through the
  proxy.
