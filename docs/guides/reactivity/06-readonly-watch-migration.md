---
title: "Migrating watchers to snapshots and updates"
description: "Declare watch inputs and writable outputs; return a typed multi-field patch."
---

# Migrating watchers to snapshots and updates

A watcher receives `FieldUpdate<T>` snapshots and may return an `Update<Self>`
patch. It takes no `self` receiver. `#[computed]` remains the right tool for a
value that is always derived; a watcher can coordinate changes to several
independently editable fields.

```rust
#[handlers]
impl Editor {
    #[watch(record, writes(draft, dirty, error))]
    fn start_edit(record: FieldUpdate<String>) -> Update<Self> {
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

## Input contract

```rust
pub struct FieldUpdate<T> {
    pub current: T,
    pub previous: Option<T>,
}
```

- Every input names a real Rust field on the component or store and uses
  `FieldUpdate<ThatFieldType>`. Parameter order does not matter; names must
  match the watch list exactly.
- Inputs are cloned together under one shared borrow, which ends before
  author code runs. Snapshot types must implement `Clone`.
- `previous` is the value at the previous invocation of this watcher. All
  inputs have `previous: None` on its first invocation. If only one input
  changes, the other inputs still contain their current and previous values.
- `changed()` is available when `T: PartialEq`; it is true on the initial
  invocation and when the current value differs from the previous value.
- Watchers are synchronous, safe, non-generic functions. They return `()`
  for observation or `Update<Self>` for a patch.
- Only declared inputs subscribe. Incidental reactive reads inside the
  callback do not add dependencies; include every triggering field in the
  watch list.

For flattened props, watch the Rust container field and compare the relevant
leaf in its snapshots. Flattened template aliases and computed keys are not
Rust fields and cannot be named as watcher inputs or patch outputs.

## Multi-field transitions

```rust
#[watch(first_name, last_name, writes(errors, valid))]
fn check_errors(
    first_name: FieldUpdate<String>,
    last_name: FieldUpdate<String>,
) -> Update<Self> {
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

The macro expands `Update<Self>` into a policy specific to that watcher.
Its patch stores optional values for the declared outputs and its generated
extension trait supplies the fluent setters. The runtime commits the patch
through one component mutation and model-writeback batch after evaluation.
Reactive observers see the completed batch, and unchanged model values do
not emit another update event. An empty patch skips the mutation entirely.

Same-flush input changes coalesce into one invocation. Initial delivery is
deferred until after `on_ready`, and unmount releases the subscription and
its previous snapshot. The component scope and callback safe point remain
active while the watcher evaluates and its patch commits.

## Cycle and mutation checks

The macro rejects a field listed in both the inputs and `writes(...)`.
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
| Resets an editable draft, clears errors, or closes a popover | Declare `writes(...)` and return a patch. |
| Updates DOM properties, starts an animation, or logs | Take named snapshots and return `()`. |
| Normalizes the same input that triggered the watcher | Normalize in the action accepting that input; a watcher cannot write its own inputs. |
| Changes another scope or manages request/loading/result state | Give the transition an explicit event/action or editing-session lifecycle, preserving cancellation and stale-response checks. |

The website [date picker](../../../examples/website/src/components/showcase/date_picker/mod.rs)
returns a patch that closes its popover on selection. The
[PIN/card form](../../../examples/website/src/components/showcase/pin_card/mod.rs)
uses a computed completion flag. The animation showcase observes all motion
inputs together without changing component fields.

The file-browser [size control](../../../examples/file-browser/src/components/size_control/mod.rs)
derives its display and accepts native edits in actions. Its snapshot watcher
only synchronizes DOM properties. The configuration action normalizes upload
and chunk limits together; incoming bounds never silently write back to the
parent. Dialog opening actions start keyed editing sessions initialized on
mount; retained closed sessions allow exit animations to finish.

## Migration status

The layout, website, and file-browser examples use the snapshot contract.
The Pine, chart, layout, icon libraries and downstream applications require
separate consumer migration. Their old watchers fail compilation, so example
builds depending on those libraries remain blocked until they are migrated.
A full workspace build is required before integrating the combined change.
