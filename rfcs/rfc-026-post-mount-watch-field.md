# RFC 026 — `post_mount` lifecycle + `#[watch(field)]` sugar

| Field | Value |
|---|---|
| **Status** | Implemented — the `post_mount` hook shipped but was renamed `on_ready` by RFC-029; `#[watch(field)]` shipped as specified. |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md) |

## 1. Summary

Two closely-coupled ergonomic fixes for a pattern every Pine
overlay hit:

1. **`post_mount(&mut self)`** — a lifecycle hook that fires
   *after* `on_mount` returns and its `&mut self` borrow releases,
   and after the surrounding template (including `pp-if` /
   `pp-teleport` children and their slot materialisations) has
   been walked. Eliminates the `tick::next` wrapper every overlay
   component currently writes inside `on_mount` just to escape
   the borrow.

2. **`watch_field::<V>("name", cb)`** — a typed watcher that reads
   the named scope field **through the proxy**, so the effect
   actually subscribes to the right dep. Replaces the
   `me.with(|s| s.open)` idiom that silently never fires because
   `Handle::with` bypasses the proxy's `get` trap.

Both land in pocopine-core; the `#[handlers]` macro gains a
`post_mount` recognition rule.

## 2. Non-goals

- **`pre_mount` / `pre_unmount`.** Not requested yet; the current
  `on_mount` already fires before children are walked (useful for
  synchronous state setup) and on_unmount fires while state is
  still valid. Adding `pre_*` hooks without a concrete need.
- **Fine-grained dep tracking inside `watch_field`.** One field =
  one subscription. Watching `user.profile.name` falls under a
  future "path watcher" RFC. For now, Pine's needs are single-
  field (`open`, `value`, `checked`, `state`).
- **Async lifecycle hooks** (`async fn post_mount`). Spawn from
  inside via `dispatch!` / `spawn_local` as usual.
- **Replacing `watch`.** `watch(source, cb)` stays — it's the
  right shape for "reactive expression → callback". `watch_field`
  is sugar for the single-field-on-self case, which is
  overwhelmingly common.

## 3. Surface

### 3.0 `#[watch(field)]` — the ergonomic front door

```rust
#[handlers]
impl PineDialog {
    #[watch(open)]
    fn on_open_change(&mut self, is_open: bool, prev: Option<bool>) {
        match (prev, is_open) {
            (None, true) | (Some(false), true) => activate(),
            (Some(true), false) => deactivate(),
            _ => {}
        }
    }
}
```

The `#[handlers]` macro scans for methods tagged `#[watch(field)]`,
strips the attribute, and auto-generates a `post_mount` that
registers a `watch_field` per tag. The value type `V` is inferred
from the first typed argument; the callback receives `(new_v: V,
prev: Option<V>)` by value. Multiple `#[watch]`es stack into a
single auto-generated `post_mount`.

Under the hood each registration expands to:

```rust
let __scope = current_scope_id().expect(…);
pocopine::watch_field::<V, _>("field", move |new, prev| {
    let new_v  = new.clone();
    let prev_v = prev.cloned();
    if let Some(scope) = Scope::find(__scope) {
        if let Some(inner) = scope.typed::<Self>() {
            Handle::new(inner, __scope).update(|s| {
                s.<method>(new_v, prev_v);
            });
        }
    }
});
```

Capturing `__scope` + building the `Handle` from the id (instead
of `this::<Self>()`) matters because watch callbacks fire from the
parent's effect chain — `CURRENT_SCOPE_ID` isn't reliably set
during the fire, so `this::<Self>()` would panic.

### 3.1 `post_mount`

```rust
#[handlers]
impl PineDialog {
    pub fn on_mount(&mut self) {
        // setup-side state only; no calls that need to subscribe
        // to `self.open` or read a freshly-mounted child element.
    }

    pub fn post_mount(&mut self) {
        // `&mut self` is a fresh borrow here. pp-if / pp-teleport
        // have committed. refs::get_on("content") works. Install
        // watches here.
        watch_field::<bool>("open", |&is_open, prev| match (prev, is_open) {
            (None, true) | (Some(false), true) => activate(),
            (Some(true), false) => deactivate(),
            _ => {}
        });
    }
}
```

Fires exactly once per mount, scheduled via `tick::next` so it
runs in the microtask after `walker::walk` returns to the caller.
If the component is unmounted before the microtask fires (rare —
only happens if a parent `pp-if` flips off synchronously in the
same task), `post_mount` is skipped.

### 3.2 `watch_field`

```rust
use pocopine::watch_field;

pub fn post_mount(&mut self) {
    watch_field::<bool>("open", |&is_open, prev| {
        // fires once on initial read (prev == None), then on every
        // distinct change to `self.open`.
    });
}
```

Generic over `V: Clone + PartialEq + Default + DeserializeOwned`.
The source closure:

1. Finds the current scope via `current_scope_id()`.
2. Grabs the scope's proxy via `Scope::into_proxy` (so reads
   subscribe through the `get` trap).
3. `Reflect::get(proxy, field)` → `JsValue`.
4. `serde_wasm_bindgen::from_value::<V>(v).unwrap_or_default()`.

`unwrap_or_default` keeps the callback surface clean — the
alternative (`Option<V>` or `Result`) pollutes every callsite.
Deserialization realistically fails only when the scope shape is
wrong, which is a bug the user wants to see immediately; the
first `cb(default, None)` call will do that in practice.

Returns the backing `EffectId` so callers can `release()` it
early if they want (rare).

### 3.3 Handler-name reservation

`post_mount` joins `on_mount`, `on_unmount` as a reserved
lifecycle name. Authoring a regular handler called `post_mount`
(invokable via `@click="post_mount"`) no longer works — the
`#[handlers]` macro emits it as a lifecycle hook only.

## 4. Implementation

### `crates/pocopine-core/src/handler.rs`

Extend the `HandlerDispatch` trait:

```rust
pub trait HandlerDispatch {
    // existing…
    fn post_mount(&mut self) {}
    fn has_post_mount(&self) -> bool { false }
}
```

### `crates/pocopine-macros/src/lib.rs`

- `#[handlers]` scans for a method named `post_mount` alongside
  `on_mount` / `on_unmount`. When present, emit an override:
  ```rust
  fn post_mount(&mut self) { Self::post_mount(self); }
  fn has_post_mount(&self) -> bool { true }
  ```
- `#[component]`'s `ComponentState` blanket impl forwards to the
  HandlerDispatch method (same shape as `fn mount`).

### `crates/pocopine-core/src/walker.rs`

- In `fire_mount_hook`, after the existing `mount()` + optional
  `trigger_scope` sweep, if `has_post_mount()` returns true,
  schedule `post_mount` on a `tick::next`:
  ```rust
  if state.has_post_mount() {
      let id = scope_id;
      tick::next(move || {
          let Some(s) = Scope::find(id) else { return };
          with_current_scope_id(id, || {
              s.state.borrow_mut().post_mount();
          });
      });
  }
  ```
- Crucially, this uses `with_current_scope_id` so `current_scope_id()`
  resolves correctly inside the hook (so `watch_field` can find
  its own scope without the user threading it through).

### `crates/pocopine-core/src/watch.rs`

Add alongside existing `watch`:

```rust
pub fn watch_field<V>(
    field: &'static str,
    cb: impl Fn(&V, Option<&V>) + 'static,
) -> EffectId
where
    V: Clone + PartialEq + Default + serde::de::DeserializeOwned + 'static,
{
    let scope_id = current_scope_id()
        .expect("watch_field called outside a handler / post_mount context");
    watch(
        move || {
            let Some(scope) = Scope::find(scope_id) else { return V::default() };
            let proxy = scope.into_proxy();
            let v = Reflect::get(&proxy, &JsValue::from_str(field))
                .unwrap_or(JsValue::UNDEFINED);
            serde_wasm_bindgen::from_value::<V>(v).unwrap_or_default()
        },
        cb,
    )
}
```

Re-exported from `pocopine::watch_field`.

## 5. Migration

All four overlay components (Dialog, Popover, DropdownMenu,
Tooltip) rewrite their `on_mount` → `post_mount`:

```diff
-pub fn on_mount(&mut self) {
-    let scope = current_scope_id().expect(...);
-    tick::next(move || {
-        watch(
-            move || read_open(scope),
-            move |is_open, prev| match (prev, *is_open) { ... },
-        );
-    });
-}
+pub fn post_mount(&mut self) {
+    watch_field::<bool>("open", |&is_open, prev| match (prev, is_open) {
+        (None, true) | (Some(false), true) => activate_current_scope(),
+        (Some(true), false) => deactivate_current_scope(),
+        _ => {}
+    });
+}
```

The `read_open` helper goes away (no proxy-access dance needed).
`activate` / `deactivate` still take `ScopeId` inside but the
lookup moves to helper functions that resolve through
`current_scope_id()`.

## 6. Edge cases

- **post_mount panics.** Panics propagate up through the
  `tick::next` microtask closure — wasm_bindgen's panic handler
  catches + logs. Subsequent hooks on other components still
  fire.
- **Component unmounts before post_mount fires.** `Scope::find`
  returns `None`; hook is silently skipped. Intentional — the
  component's teardown cleared the scope, so the hook has nothing
  meaningful to do.
- **post_mount reads `$el` / `$refs`.** Works:
  `with_current_scope_id` sets the scope, and refs are
  scope-keyed. `current_el()` may be `None` inside post_mount
  since no directive is currently running — use `$refs` or
  `refs::get_on(scope, "name")` explicitly.
- **`watch_field` called outside a handler / lifecycle**. Panics
  with a clear message — this is a programmer error and we'd
  rather catch it loudly than silently never subscribe.
- **Deeply nested field** (`watch_field::<Vec<TabDef>>("tabs", ...)`).
  Works, but V must implement `Default` + `DeserializeOwned`. The
  cb fires once on initial + on every distinct read (distinctness
  via PartialEq). For Vec-of-struct fields the distinctness check
  does a full compare — fine for Pine's sizes (menu items,
  options), revisit if someone hits a hot loop.
