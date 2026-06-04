---
title: "Events"
description: "How components talk across boundaries — the emit one-liner, typed #[derive(Emit)] events, cancelable events, and the on / on_emit listeners."
---

# Events

Components talk to each other through DOM events — the standard ones
(`click`, `input`) and **custom** ones a component emits. pocopine gives
you a stringly one-liner for the common case and a fully typed enum
surface when you want compile-checked events.

This is the channel behind [child → parent](./02-state.md#3-child-parent):
the child emits, an ancestor listens. No pub/sub bus, no parent reference —
events bubble up the DOM.

## Emitting

### The one-liner

`emit(name, detail)` from a handler fires a bubbling `CustomEvent` on the
next microtask (deferred so your `&mut self` borrow releases first):

```rust
#[handlers]
impl TodoItem {
    pub fn complete(&mut self) {
        self.done = true;
        emit("todo-completed", self.id);   // detail: any Serialize value
    }
}
```

`emit_from(&el, name, detail)` dispatches from a specific element, and
`emit_from_host(name, detail)` from the host tag of a teleported subtree —
the one-liner overlays (Dialog, Popover) use it because their content is
moved to `<body>`, outside the host's bubbling path.

### Typed events — `#[derive(Emit)]`

When a component has several events, derive `Emit` on an enum. Each variant
becomes one event: the **name** is the kebab-case of the variant ident, and
the **fields** become the `CustomEvent.detail`.

```rust
#[derive(Emit)]
pub enum DialogEvent {
    Close,                       // → "close",       detail: null
    Confirm { value: String },   // → "confirm",     detail: { value }
    OpenChange(bool),            // → "open-change", detail: [bool]
}

#[handlers]
impl Dialog {
    pub fn confirm(&mut self) {
        emit_event(DialogEvent::Confirm { value: self.value.clone() });
    }
}
```

`emit_event` derives the name and serializes the payload for you, so the
emit side and the listen side can never drift out of sync — rename a
variant and both ends move together. `emit_event_from(&el, event)` is the
explicit-element form.

### Cancelable events

`emit_cancelable(name, detail) -> bool` fires **synchronously** and returns
whether a listener called `preventDefault()` — for "may I proceed?" flows:

```rust
pub fn try_close(&mut self) {
    if emit_cancelable("before-close", ()) {
        return;            // a listener vetoed it
    }
    self.open = false;
}
```

(Synchronous means a listener re-enters your scope's borrow — fine for
fire-and-observe, but use the deferred `emit` for `pp-model` mirror flows.)

### Model channels

`pp-model` is built on the same primitive: a primitive emits
`emit_model(value)` (or `emit_model_field("value", v)` for a named
`pp-model:value` channel) and the binding mirrors it back. See the model
guide for the binding side.

## Listening

### In a template

`@event="handler"` (shorthand for `pp-on:event`) binds DOM and custom
events alike. The handler receives the event through the
[extractors](./05-extractors.md):

```poco
<button @click="save">Save</button>

<!-- listen for the child's custom event -->
<todo-item @todo-completed="record_completed"></todo-item>
```

### In Rust — the `ev` catalog

For listeners you wire by hand (a `pp-ref` element, `document`, `window`),
the `on!` macro and `events::on_scoped` go through a compile-time catalog
of event names paired with their `web_sys` payload type — a typo in either
the name or the payload type is a compile error:

```rust
pub fn on_ready(&self, refs: Refs) {
    let input = refs.get("input").unwrap();
    on!(input, keydown, |e| {                 // e: KeyboardEvent
        if e.key() == "Escape" { /* … */ }
    });
}
```

`on_scoped` / `on!` release the listener automatically when the scope
unmounts; `on` / `events::on` return a `ListenerHandle` you cancel yourself.

### In Rust — typed enum with `on_emit`

The receiving counterpart to `#[derive(Emit)]`: `on_emit!` subscribes to
**every** variant of the enum at once and reconstructs it from the event's
detail, so you `match` on a typed value instead of stringly names:

```rust
pub fn on_ready(&self, host: HostEl) {
    on_emit!(host.0, DialogEvent, |e| match e {
        DialogEvent::Close             => { /* … */ }
        DialogEvent::Confirm { value } => { /* … */ }
        DialogEvent::OpenChange(open)  => { /* … */ }
    });
}
```

## The typed round-trip

`#[derive(Emit)]` generates both ends, so the name/payload mapping is
defined once:

```text
  emit_event(DialogEvent::Confirm { value: "ok".into() })
        │   event_name()  → "confirm"
        │   to_detail()   → { value: "ok" }
        ▼
  CustomEvent "confirm"  (bubbles up the DOM)
        │
        ▼
  on_emit!(host, DialogEvent, …)
        │   from_event("confirm", { value: "ok" })
        ▼
  DialogEvent::Confirm { value: "ok" }   ← matched, typed
```

## Cheat-sheet

| Need | Reach for |
|---|---|
| Fire one custom event | `emit(name, detail)` |
| Fire from a specific / host element | `emit_from(&el, …)` · `emit_from_host(…)` |
| A component's whole event set, typed | `#[derive(Emit)]` + `emit_event(E)` |
| Ask "may I proceed?" | `emit_cancelable(name, detail) -> bool` |
| Back a `pp-model` channel | `emit_model(v)` · `emit_model_field("f", v)` |
| Listen in a template | `@event="handler"` |
| Listen by hand, type-checked | `on!(el, event, \|e\| …)` / `events::on_scoped` |
| Listen to a typed enum | `on_emit!(host, EventEnum, \|e\| match e { … })` |

## Related

- [Extractors](./05-extractors.md) — how a handler's parameters are filled
  from the event.
- [State management → Child → parent](./02-state.md#3-child-parent) — the
  emit / `@event` convention in context.
- [Lifecycle](./04-lifecycle.md) — where to install hand-wired listeners
  (`on_mount` / `on_ready`).
