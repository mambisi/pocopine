---
title: "Migrating mutable watchers"
description: "Move derived values and state transitions out of #[watch] handlers."
---

# Migrating mutable watchers

`#[watch]` requires a shared receiver. This is a breaking change for both
forms:

```rust
#[watch(value)]
fn on_value(&self, next: String, previous: Option<String>) {
    // Observe the change, for example by updating a DOM property.
}

#[watch(width, height)]
fn on_size(&self) {
    // Read self.width and self.height; no value arguments in this form.
}
```

The macro rejects `&mut self` with a diagnostic directing authors to
computed values or event/action handlers. Ordinary event and lifecycle
handlers can still use `&mut self`.

## Choose the owner of the behavior

| Existing watcher | Migration |
| --- | --- |
| Computes a label, percentage, validity flag, or view model | Remove the stored output field and derive it with `#[computed]`. |
| Updates DOM properties, starts an animation, or logs a change without writing component state | Change the receiver and any read-only helpers to `&self`. |
| Normalizes another input, resets selection, closes a popover, or publishes a model change | Move the transition into the action accepting the input, preserving its model/event contract. |
| Starts a request and changes loading/result state | Separate request lifecycle and result commits from read-only observation; preserve cancellation and stale-response checks. |

For example, replace two watchers that assign `self.complete` with:

```rust
#[computed]
fn complete(card_number: &str, pin: &str) -> bool {
    card_number.chars().count() == 16 && pin.chars().count() == 4
}
```

Remove `complete` from the component struct and its constructors. The
template can still use `pp-show="complete"`. Declared computed dependencies
are checked for cycles at compile time.

Migrations involving child `pp-model` inputs need a deliberate event/action
design. Replacing the receiver alone cannot preserve a date control that
normalizes other inputs and writes its outputs back to a parent. Do not
replace these watchers with `handle.update(...)` or a free-function watcher
just to get them compiling: that retains the same implicit mutation chain.

## Dispatch behavior and limits

Generated watchers acquire a shared state borrow and preserve the current
component scope, callback safe point, deferred initial notification,
single-field previous values, multi-field coalescing, and unmount cleanup.
They no longer run a dirty sweep or model writeback around the callback.

This first patch restricts the receiver. It does not introduce a runtime
write prohibition, restrict existing handle/signal APIs, or propagate
read-only permissions through asynchronous work. Low-level `watch()` and
`watch_scope_fields()` retain their existing callback contracts and cycle
guards. Arbitrary Rust code is not made pure by `&self`.

The consumer migration is a separate patch. Existing mutable watchers in
Pine, charts, layout, icons, examples, and downstream applications will fail
compilation until migrated. A full workspace build is therefore not a
passing gate for the isolated contract patch; it is required after the
consumer migration before integrating the combined change.
