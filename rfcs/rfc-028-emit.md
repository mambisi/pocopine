# RFC 028 — `emit` / `emit_from` helpers

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-009-pp-model-components.md`](./rfc-009-pp-model-components.md), [`rfc-023-pine-mvp.md`](./rfc-023-pine-mvp.md) |

## 1. Summary

Every Pine overlay that talks to `pp-model` currently ships the
same ~13-line helper:

```rust
fn emit_open_changed(open: bool) {
    let Some(el) = pocopine_core::scope::current_el() else { return };
    tick::next(move || {
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_bool(open));
        if let Ok(ev) =
            web_sys::CustomEvent::new_with_event_init_dict(
                "pp:update:model", &init,
            )
        {
            let _ = el.dispatch_event(&ev);
        }
    });
}
```

Six overlays × thirteen lines = ~80 lines of identical ceremony.
RFC-028 replaces them with:

```rust
pocopine::emit("pp:update:model", self.checked);
```

One-line Vue-parity emission, same shape users learn for their
own components.

## 2. Non-goals

- **Replacing `dispatch_event` / `$dispatch`.** Those fire
  *synchronously* from `current_el()` and are exposed to the
  template expression evaluator as the `$dispatch(name, detail)`
  magic. `emit` is for Rust handlers whose `&mut self` borrow
  would re-enter if we dispatched synchronously; different
  semantics, different name.
- **Typed event buses.** No compile-time check that a given
  event name actually matches a shape the listener expects. An
  event is a string; authors who want stronger typing can wrap
  `emit` in their own helper with a concrete payload struct.
- **Auto-inferring the host tag for teleported content.** When
  a handler runs inside a teleported subtree (Dialog, Popover,
  DropdownMenu), it needs `emit_from(&host, …)` with the host
  it walks to itself — because "which element is the logical
  host" is an overlay-specific question. `emit()` does the
  simple thing (`current_el()`) and `emit_from` is the escape
  hatch.

## 3. Surface

### 3.1 Function API

```rust
use serde::Serialize;
use web_sys::Element;

/// Fire a bubbling `CustomEvent` from the current directive
/// element with `detail` serialized via `serde_wasm_bindgen`.
/// Deferred to the next microtask so the caller's `&mut self`
/// borrow has released before any listener re-enters this
/// scope.
pub fn emit<T: Serialize>(name: &str, detail: T);

/// Like [`emit`] but dispatches from an explicit element. Used
/// by teleport-backed overlays (Dialog, Popover, DropdownMenu)
/// where bubbling from the teleported subtree would miss the
/// host tag's listener.
pub fn emit_from<T: Serialize>(el: &Element, name: &str, detail: T);
```

Both are re-exported from `pocopine::prelude::*`.

### 3.2 Serialization

`detail` is passed through `serde_wasm_bindgen::to_value`. That
covers every shape Pine emits today (`bool`, `String`, `u32`),
plus any user-defined `#[derive(Serialize)]` struct. Passing
`()` produces `JsValue::UNDEFINED` — use it when the event
name itself is the payload:

```rust
emit("close", ());
emit("pp:update:model", self.checked);
emit("change", Selected { id, label });
```

If serialization fails (only possible for non-`Serialize`-safe
edge cases like cycles), `emit` silently drops the event rather
than panicking. The handler path is never allowed to blow up
the scope mid-borrow-release.

### 3.3 Deferral

Both functions schedule the dispatch via `tick::next`. The
reason is structural: pp-model's parent → child mirror effect
calls `Handle::update(|s| s.field = v)`, which takes a fresh
`borrow_mut`. If `emit` fired synchronously from inside a
handler still holding the scope's mutable borrow, re-entry
would panic at `RefCell::borrow_mut`. The microtask delay lets
the current borrow release first.

Consequence: `emit` is fire-and-forget — there's no way to
observe the event's return value or cancellation. Vue's `emit`
has the same property. Authors who need synchronous dispatch
reach for `dispatch_event` (for template-expression use) or
`el.dispatch_event(&ev)` directly.

## 4. Migration

Six Pine overlays collapse:

| Before | After |
|---|---|
| `fn emit_checked_changed(checked: bool)` + helper body | `emit("pp:update:model", self.checked);` |
| `fn emit_state_changed(state: String)` + helper body | `emit("pp:update:model", self.state.clone());` |
| `fn emit_value_changed(value: String)` + helper body | `emit("pp:update:model", value);` |
| Dialog / Popover / DropdownMenu: `emit_open_changed(open)` + host-walk + helper body | Keep host-walk; call `emit_from(&host, "pp:update:model", open);` |

Authors writing their own components get the same shape from
day one — no need to recreate the boilerplate.

## 5. Implementation notes

The helpers live in a new module
`crates/pocopine-core/src/emit.rs` re-exported from the crate
root. Placing them alongside `dispatch_event` (currently in
`magics.rs`) would muddle the distinction between the
magic-expression bridge and the Rust-handler helper; a
separate file makes the contract visible to readers.

Cargo.toml already depends on `serde_wasm_bindgen` via
`pocopine-macros`; the core crate picks up the dependency
directly for this helper.

Cost is negligible: one `to_value` call per emit, one
`CustomEvent` construction per microtask fire. The existing
Pine tests run unchanged and their timing didn't measurably
shift.
