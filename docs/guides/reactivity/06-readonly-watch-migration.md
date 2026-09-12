---
title: "Migrating watchers to snapshots and updates"
description: "Declare watch inputs and writable outputs; return a typed multi-field patch."
---

# Migrating watchers to snapshots and updates

The authoritative contract is
[RFC-125: Typed watcher inputs and restricted state updates](../../../rfcs/rfc-125-watch-inputs-and-updates.md).

A watcher receives named values, borrows, or `Change<T>` snapshots and may
return an `Update` with an explicit output tuple or `updates(...)` shorthand.
It takes no `self` receiver. `#[computed]` remains the right tool for a value
that is always derived; a watcher can coordinate changes to several
independently editable fields.

```rust
#[handlers]
impl Editor {
    #[watch(record)]
    fn start_edit(record: Change<String>)
        -> Update<Self, (Self::Draft, Self::Dirty, Self::Error)> {
        Update::new()
            .draft(record.current)
            .dirty(false)
            .error(None)
    }
}
```

`draft`, `dirty`, and `error` are ordinary fields on `Editor`. The generated
builder exposes setters only for those fields. Returning `Update::new()`
leaves every field unchanged. Omitting `.error(...)` preserves the existing
error; `.error(None)` explicitly clears an `Option` field.

The explicit return type declares the allowed outputs. Its optional shorthand is:

```rust
#[watch(record, updates(draft, dirty, error))]
fn start_edit(record: Change<String>) -> Update<Self> {
    Update::new().draft(record.current).dirty(false).error(None)
}
```

Both forms return the same type. Use one declaration, not both. The earlier
`writes(...)` spelling has been replaced by `updates(...)`.

`Update<Self>` without `updates(...)` is an error. An empty tuple
(`Update<Self, ()>`) or `updates()` emits `pocopine::empty_watch_update`,
suggesting `-> ()` for an observer. It uses Rust's deprecated-use warning,
so `#[allow(deprecated)]` on the method permits an intentional empty set.
A watcher with declared outputs may still return `Update::new()` to skip a
change without triggering this warning. Bare `#[watch]` requires `()`.

The owner generates reusable descriptors such as `EditorField::Draft`.
Within watcher output tuples, `Self::Draft` resolves to that descriptor.
Outside watcher signatures, use `EditorField::Draft` directly and import
`EditorField::setters::{Draft as _, Dirty as _}` for selected fluent setters.
Descriptors preserve field visibility and expose the value type, Rust name, and a borrowed `get`
through `pocopine::Field<Editor>`. `draft_text` becomes `DraftText`; duplicate
marker names are rejected. Output tuples support up to 32 fields, and their
order is part of the Rust type.

## Input contract

Each named parameter chooses its input form:

| Parameter | Current value | Previous value | Requirements |
| --- | --- | --- | --- |
| `T` | Owned clone | Not retained | Field type implements `Clone`. |
| `&T` | Shared borrow for this call | Not retained | No `Clone` requirement; shared coercions such as `String` to `&str` work. |
| `Change<T>` | Owned snapshot | Retained snapshot | Field type implements `Clone`. |

These forms can be mixed in one watcher. `&mut T` is rejected. All inputs
are read under one shared component borrow, which remains active during the
synchronous callback and ends before its returned patch commits. Borrowed
inputs cannot escape into asynchronous work. Only `Change<T>` inputs retain
history between invocations.

```rust
pub struct Change<T> {
    pub current: T,
    pub previous: Option<T>,
}
```

- Every input names a real Rust field on the component or store. Parameter
  order does not matter; names must match the watch list exactly.
- `previous` is the value at the previous invocation of this watcher. All
  `Change<T>` inputs have `previous: None` on its first invocation. If only one input
  changes, the other inputs still contain their current and previous values.
- `changed()` is available when `T: PartialEq`; it is true on the initial
  invocation and when the current value differs from the previous value.
- Watchers are synchronous, safe, non-generic functions. They return `()`
  for observation or an `Update` with declared outputs for a patch.
- Only declared inputs subscribe. Incidental reactive reads inside the
  callback do not add dependencies; include every triggering field in the
  watch list.

For flattened props, watch the Rust container field and compare the relevant
leaf in its snapshots. Flattened template aliases and computed keys are not
Rust fields and cannot be named as watcher inputs or patch outputs.

## Observe every field

Bare `#[watch]` observes all watchable Rust fields on the component or store:

```rust
#[watch]
fn on_state(changes: Changes<Self>) {
    tracing::info!(?changes.record.current, ?changes.record.previous, "record observed");
}
```

The macro expands `Changes<Self>` into a generated bundle with a
`Change<FieldType>` member for each field, accessible in the owner's module.
Members can be moved out, and the
component itself does not need `Clone`. Each included field does need `Clone`
because this form retains its history. Computed keys, flattened aliases, and
`#[serde(skip)]` fields are excluded.

Bare observers must return `()`; even an empty `Update<Self>` is rejected.
There is no `updates(...)` form because every watchable field is an input.
Initial delivery, coalescing, mutation guards, and unmount cleanup are the
same as for named watches. Use an explicit field list with `T` or `&T` when
history is unnecessary.

## Multi-field transitions

```rust
#[watch(first_name, last_name)]
fn check_errors(
    first_name: Change<String>,
    last_name: Change<String>,
) -> Update<Self, (Self::Errors, Self::Valid)> {
    let mut errors = Vec::new();
    if first_name.current.trim().is_empty() {
        errors.push("First name is required".to_string());
    }
    if last_name.current.trim().is_empty() {
        errors.push("Last name is required".to_string());
    }
    let valid = errors.is_empty();
    Update::new().errors(errors).valid(valid)
}
```

Both forms resolve to a tuple of reusable owner field descriptors. Its
patch stores optional values for the declared outputs; generated setter
traits require membership in that tuple. The runtime commits the patch
through one component mutation and model-writeback batch after evaluation.
Reactive observers see the completed batch, and unchanged model values do
not emit another update event. An empty patch skips the mutation entirely.

Same-flush input changes coalesce into one invocation. Initial delivery is
deferred until after `on_ready`, and unmount releases the subscription and
its previous snapshot. The component scope and callback safe point remain
active while the watcher evaluates and its patch commits.

DOM APIs such as `focus()` or `click()` can synchronously dispatch events
into the same component or another component. During watcher evaluation,
named handler invocations (such as `@click="on_click"`) join the callback
FIFO and run after evaluation and patch commit, if their target scope is
still live. Outside evaluation, handlers on other scopes may run
synchronously.

Inline event expressions (such as `@click="count = count + 1"`) retain the
existing same-scope queue: if their target scope already has an active
callback frame, the whole expression waits for the callback frames to
unwind. This includes events targeting the watcher owner's scope. Otherwise
the expression evaluates synchronously, and any field write during watcher
evaluation is rejected. Named-handler deferral does not extend this queue
to inline expressions targeting an inactive scope. Direct handle,
field-handle, signal, and model or prop mirror writes executed during
evaluation also remain rejected.

## Cycle and mutation checks

The macro rejects a field listed in both the inputs and `updates(...)`.
It also checks the declared graph across watchers and computed methods in
the handlers block, including longer cycles. Conditional compilation applies
to the graph nodes, so inactive watchers do not create false cycles.

```text
watch(a) writes b
watch(b) writes a    // compile error: dependency graph contains a cycle
```

During synchronous watcher evaluation, the standard handle, field-handle,
and signal write APIs reject direct mutation before it occurs. Return a
patch instead of calling `this().update(...)`, `store().update(...)`, mutable
handle borrows, or a signal setter. DOM observations and animations can
return `()`.

This is an API boundary, not a Rust sandbox. Raw scope internals and
independently scheduled asynchronous work are outside the patch contract.
The runtime flush-cycle guard remains necessary for dependency paths across
components, model bindings, and lower-level effects. Do not move a prohibited
write into a deferred callback just to bypass the contract.

## Choose the owner of the behavior

| Existing watcher | Migration |
| --- | --- |
| Keeps a purely derived label, percentage, or completion flag synchronized | Remove the stored output and use `#[computed]`. |
| Resets an editable draft, clears errors, or closes a popover | Declare `updates(...)` and return a patch. |
| Updates DOM properties, starts an animation, or logs | Take named snapshots and return `()`. |
| Normalizes the same input that triggered the watcher | Normalize in the action accepting that input; a watcher cannot write its own inputs. |
| Changes another scope or manages request/loading/result state | Give the transition an explicit event/action or editing-session lifecycle, preserving cancellation and stale-response checks. |

The website [date picker](../../../examples/website/src/components/showcase/date_picker/mod.rs)
returns a patch that closes its popover on selection. The
[PIN/card form](../../../examples/website/src/components/showcase/pin_card/mod.rs)
uses a computed completion flag. The animation showcase observes all motion
inputs together without changing component fields.

The file-browser [size control](../../../examples/file-browser/src/components/size_control/mod.rs)
derives its display and accepts native edits in actions. Its watcher uses
current numeric values and a borrowed unit string to synchronize DOM
properties without retaining history. The configuration action normalizes upload
and chunk limits together; incoming bounds never silently write back to the
parent. Dialog opening actions start keyed editing sessions initialized on
mount; retained closed sessions allow exit animations to finish.

## Migration status

The layout, website, and file-browser examples and their Pine, chart, layout,
and icon dependencies use the snapshot contract. Related inputs share one
watcher, so a configuration change produces one restricted patch.

Charts preserve existing selection and animation history while computing on a
detached snapshot. Only declared outputs commit to the live component. Child
chart parts submit configuration through named actions on their owning chart;
the runtime queues those actions until watcher evaluation and patch commit
finish. End events caused by data changes explicitly target the chart host.

Downstream applications need the same migration before upgrading.
Validate consumers on their supported targets and build examples
through the Pocopine CLI; the workspace CI defines the host and wasm package
selections.
