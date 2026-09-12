# RFC-125: Typed watcher inputs and restricted state updates

**Status:** Accepted

**Created:** 2026-09-11

**Crates:** `pocopine-macros`, `pocopine-core`, `pocopine`

**Supersedes:** The generated `#[watch]` handler contracts in
[RFC-026](./rfc-026-post-mount-watch-field.md) and
[RFC-115](./rfc-115-multi-field-watch.md). Their lifecycle and low-level
watch/effect behavior remain applicable except where this RFC explicitly
changes generated watcher dispatch.

**Implementation:** Contract and focused tests implemented on
`fix/readonly-watch`; library and downstream consumer migration remains
pending. This status does not imply a merged or released feature.

## Summary

A watcher declares the fields that trigger it and the fields it may update.
Its callback takes no `self` receiver. Named parameters provide a cloned
current value (`T`), a shared current value (`&T`), or current and previous
values (`Change<T>`). A callback returns either `()` or a restricted
`Update<Self, (Self::FieldName, ...)>` patch:

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

Here `record` and `draft` are `String` fields, `dirty` is a `bool`, and
`error` is an `Option<String>`. A newly selected record starts an editable
draft and clears the previous session's dirty/error state. Subsequent user
edits belong to actions; the draft is not permanently derived from `record`.

Bare `#[watch]` observes all watchable fields through `Changes<Self>` and
must return `()`. The macro checks local dependency cycles, and the runtime
rejects direct writes through standard reactive mutation APIs during
callback evaluation. Patches commit after input borrows end.

## 1. Motivation and scope

Mutable watchers make dependencies difficult to inspect. A method watching
`card_number` can call a helper that modifies `pin`, whose watcher modifies
`card_number` again. Moving the assignment behind a helper does not change
the cycle, but it hides the write from the macro and the reader.

Some existing watchers also maintain redundant state. A completion flag
whose value is always determined by two inputs should be computed:

```rust
#[handlers]
impl PinCardDemo {
    #[computed]
    fn complete(card_number: &str, pin: &str) -> bool {
        card_number.chars().count() == 16 && pin.chars().count() == 4
    }
}
```

The component stores `card_number` and `pin`; it does not store `complete`.
The template can still use `pp-show="complete"`.

Other watchers implement transitions across independently editable fields:
resetting a draft, clearing errors, closing a popover, or synchronizing DOM
properties. Those behaviors need a watcher. This RFC makes their inputs and
permitted outputs explicit without requiring one callback per output.

This is a breaking change to macro-generated component and store watchers.
It does not redesign `#[computed]`, action handlers, the scheduler, or the
public low-level `watch()` and `effect()` primitives. Universal Rust purity,
whole-application static cycle analysis, asynchronous watcher callbacks,
and a general-purpose state patch API are outside its scope.

## 2. Authoring contract

### 2.1 Attribute and signature forms

| Form | Parameters | Return |
| --- | --- | --- |
| `#[watch(a)]` | One named input for `a` | `()` or `Update<Self, (Self::Output, ...)>` |
| `#[watch(a, b)]` | One named input for each listed field | `()` or `Update<Self, (Self::Output, ...)>` |
| `#[watch(a, b, updates(c, d))]` | One named input for each listed field | `Update<Self>` |
| `#[watch]` | One parameter of type `Changes<Self>` | `()` only |

A named watcher lists at least one input. The optional `updates(...)` list
comes last. Duplicate inputs, duplicate outputs, repeated `updates(...)`
lists, stacked watch attributes, and input/output overlap are errors.
`#[watch()]`, `#[watch(*)]`, and a updates-only attribute are not supported.

Watch callbacks are synchronous, safe, non-generic associated functions
inside `#[handlers]`. They accept no receiver, including `&self`, and no
context extractors. Lifecycle method names cannot also be watcher names.
The result is unit (implicit or explicit), or `Update<Self, (Self::Field, ...)>`.
`Update<Self>` requires the `updates(...)` shorthand. `Option`, `Result`, and
asynchronous results are not accepted. Declare outputs once: combining an
explicit tuple with `updates(...)` is an error, even if the lists match.
Return an empty update value to skip a transition.

Every named parameter must match a declared input's Rust field name exactly.
Parameters may appear in a different order from the attribute; each input
appears once. Use the real name even when its value is unused, rather than
renaming it `_new` or `_previous`. Raw identifiers work normally.

Inputs and outputs must be watchable real Rust fields exposed by the owner
macro's field metadata. Computed keys, synthetic flattened aliases, and
`#[serde(skip)]` fields are excluded. For a flattened prop, watch its Rust
container field and inspect the relevant leaf. Output value types come from
the same metadata, so misspelled fields and incorrectly typed setters fail
compilation.

### 2.2 Current values and history

| Input | What the callback receives | Retained history | Requirements |
| --- | --- | --- | --- |
| `T` | A clone of the current field | None | Field implements `Clone` |
| `&T` | A shared borrow for this invocation | None | No `Clone` requirement |
| `Change<T>` | Owned current and previous values | One previous delivered value | Field implements `Clone` |

Forms may be mixed in the same callback. Shared reference coercions work,
including a `String` field passed as `&str`. Mutable references are rejected.
All inputs are evaluated under one shared borrow of the component or store;
that borrow remains active during the callback. Borrowed values cannot
escape into the returned patch or a task requiring an owned, `'static`
value. A callback can explicitly clone a value when it needs ownership.

`Change<T>` is a value describing two observations:

```rust
pub struct Change<T> {
    pub current: T,
    pub previous: Option<T>,
}

impl<T: PartialEq> Change<T> {
    pub fn changed(&self) -> bool {
        self.previous.as_ref() != Some(&self.current)
    }
}
```

The type also implements `Clone`, `Debug`, `PartialEq`, and `Eq` when its
contents do. It is neither a signal handle nor a subscription: reading it
does not track anything, and it has no setter.

`previous` means the value delivered at this watcher's previous invocation,
not the value immediately before the most recent assignment. For example:

| Event | Current value delivered | Previous value delivered |
| --- | --- | --- |
| Initial delivery with `count = 1` | `1` | `None` |
| Change to `2`, then `3`, before the next flush | `3` | `Some(1)` |
| Another input triggers the watcher, with `count` still `3` | `3` | `Some(3)` |

Every `Change<T>` input has `previous: None` initially. `changed()` returns
true initially, then compares the two delivered values. It is a convenience
for callback logic, not an additional scheduler gate, and `PartialEq` is not
required merely to receive `Change<T>`.

Only `Change<T>` parameters retain history. The implementation clones a
current value for delivery and another for the next history entry, plus the
previous entry when present. Choose `T` or `&T` when history is unnecessary.

### 2.3 Restricted patches

The return type declares the permitted output fields. The field list uses
Rust tuple syntax, including a trailing comma for a one-field tuple:

```rust
#[watch(record)]
fn start_edit(record: String) -> Update<Self, (Self::Draft, Self::Dirty)> {
    Update::new().draft(record).dirty(false)
}
```

`updates(...)` is optional sugar for that tuple:

```rust
#[watch(record, updates(draft, dirty))]
fn start_edit(record: String) -> Update<Self> {
    Update::new().draft(record).dirty(false)
}
```

Both forms resolve to exactly `Update<Editor, (EditorField::Draft,
EditorField::Dirty)>`. There is no watcher-specific patch policy. The old
`writes(...)` spelling is rejected with a migration diagnostic.

The owner macro generates the `<Owner>Field` module once per component or
store. It contains field descriptors such as `EditorField::Draft`, each
implementing `Field<Editor>` with an associated `Value` type, a `NAME`
constant, and a borrowed `get(&Editor)` operation. `Self::Draft` is shorthand
resolved by `#[handlers]` in watcher output tuples; it is not an inherent
Rust associated type. Outside watcher signatures, use the ordinary marker
path:

```rust
fn reset_draft(record: &str) -> Update<Editor, (EditorField::Draft, EditorField::Dirty)> {
    use EditorField::setters::{Draft as _, Dirty as _};
    Update::new().draft(record.to_owned()).dirty(false)
}
```

The module has the owner's visibility, and each marker and setter trait
preserves its field's visibility. The marker name capitalizes each
underscore-separated word (`draft_text` becomes `DraftText`; `r#type`
becomes `Type`). A leading digit retains an underscore (`_1` becomes `_1`),
and the reserved `Self` spelling gains a trailing underscore (`self_` becomes
`Self_`). Fields that normalize to the same marker name are rejected.
Original field types resolve in the owner's declaration module, preserving
private types and relative module paths. Descriptor reads themselves do not
subscribe or expose a reactive mutation handle.

Tuple order is part of the Rust type. Sugar preserves the declared order;
helpers sharing a return type must use that same order. Tuples support up
to 32 output fields. Duplicate outputs, unknown or skipped fields, and
input/output overlap are compile errors. The same graph checks apply to
both forms. Fluent setters are generated per owner field, with a tuple
membership bound that permits calls only for declared outputs.

Each tuple patch slot is `Option<FieldType>`. Omission means preserve the
field; setting an optional field to `None` means explicitly clear it:

```rust
Update::new()                         // Preserve every field.
Update::new().dirty(false)            // Set only dirty.
Update::new().error(None)             // Clear error.
Update::new().error(Some(message))    // Replace error.
```

Calling a setter twice replaces its pending value. A watcher may set any
subset of its declared outputs. `Update<Self>` without `updates(...)` is an
error, with a suggestion to declare outputs or return `()` for observation.

`Update<Self, ()>` and `updates()` are valid empty output sets, but emit the
`pocopine::empty_watch_update` warning: "this watcher declares no outputs;
return () instead of Update<Self, ()> or updates()". Watcher callbacks are
non-generic, so an empty capability usually obscures an observer. The lint
uses the same stable-Rust deprecated-constant mechanism as existing Pocopine
warnings: `#[deny(warnings)]` or `#[deny(deprecated)]` makes it an error;
`#[allow(deprecated)]` on the method permits an intentional empty set.
Inactive `#[cfg]` watchers do not warn. A nonempty output tuple returning
`Update::new()` never triggers this lint. Bare observers cannot return even
an empty patch.

The runtime commits a nonempty patch through one owner `Handle::update`
and model-writeback batch. Reactive observers see the completed batch;
unchanged model values do not emit another update event. An empty patch
skips mutation entirely. This is batching, with no rollback transaction or
guarantee of ordering between distinct watchers. Multiple watchers may
declare the same output if the graph remains acyclic; applications needing
a deterministic resolution should give that transition one owner.

### 2.4 Observing every field

```rust
#[watch]
fn on_state(changes: Changes<Self>) {
    if changes.record.changed() {
        tracing::debug!(?changes.record.current, "record changed");
    }
}
```

The macro supplies a concrete bundle with one `Change<FieldType>` member
per watchable field. Members are accessible within the owner's module and
may be moved out. Each included field needs `Clone`; the owner struct itself
does not. Skipped fields, computed keys, and flattened aliases are excluded.
A type with no watchable fields has an empty bundle and an initial delivery.

Bare observers must return `()`, including when every branch would return
an empty patch. Watching every field leaves no local watchable output to
write. Use a named watch with a smaller input set for a state transition.

All-field history is generated only when an active bare observer needs it.
A component with only borrowed named inputs does not acquire a `Clone`
requirement because the macro also supports bare observers. An inactive
`#[cfg]` observer likewise imposes no history requirement.

## 3. Expansion and runtime ownership

The owner macro (`#[component]` or `#[store]`) knows field types and emits
reusable field descriptors. The handlers macro resolves the explicit tuple
or `updates(...)` into those descriptors and records their field names in
the local dependency graph. It does not search callback or helper bodies
for assignments.

For example, `Editor` emits the following field metadata (abbreviated):

```rust
pub mod EditorField {
    pub enum Draft {}
    pub enum Dirty {}
    pub enum Error {}
    // Generated setter traits, with UpdateField membership bounds.
}

impl pocopine::Field<Editor> for EditorField::Draft {
    type Value = String;
    const NAME: &'static str = "draft";
    fn get(state: &Editor) -> &String { &state.draft }
    fn set(state: &mut Editor, value: String) { state.draft = value; }
}
// Dirty and Error have corresponding metadata implementations.
```

Both authoring forms expand the callback to the same signature and import
the same owner-level setter traits:

```rust
impl Editor {
    fn start_edit(record: Change<String>)
        -> Update<Self, (EditorField::Draft, EditorField::Dirty, EditorField::Error)>
    {
        use EditorField::setters::{Draft as _, Dirty as _, Error as _};
        Update::new().draft(record.current).dirty(false).error(None)
    }
}
```

The core crate implements `WatchSpec<C>` for tuples of descriptors that
implement `Field<C>`. Its patch storage is a tuple of optional field values:
for this example, `(Option<String>, Option<bool>, Option<Option<String>>)`.
The generated `.draft(...)` setter requires
`W: UpdateField<Editor, EditorField::Draft, INDEX>`, with the slot index
inferred from the tuple. A setter for an undeclared field cannot satisfy
that bound. The runtime owns the final mutable access.

The generated installation supplies a shared-state callback and history
containing only the inputs that requested `Change<T>`:

```rust
pocopine::__private::install_snapshot_watch::<Editor, (String,), _>(
    scope,
    &["record"],
    "Editor::start_edit",
    |state, previous| {
        let record = Change {
            current: state.record.clone(),
            previous: previous.map(|p| p.0.clone()),
        };
        let result = Editor::start_edit(record);
        (result, (state.record.clone(),))
    },
);
```

An owned current input instead uses `state.field.clone()`, a borrowed input
uses `&state.field`, and neither adds a history tuple entry. A watcher with
no history uses `()` as its history type.

For a bare observer, the owner macro emits a helper that lazily generates
`Inputs`, `History`, and a `WatchAllSpec<Owner>` policy. The handlers macro
rewrites `Changes<Self>` to `Changes<Self, Policy>` and installs the policy's
complete field list. `WatchAllSpec::read` builds the owned bundle and next
history under the same shared state borrow. This works when the handlers
block precedes the owner declaration.

### 3.1 Delivery sequence

1. Install one scope-bound effect tracking the declared field list. Defer
   initial delivery until after `on_ready` and its mutable borrow finish.
2. On delivery, resolve the live owner, enter its callback safe point and
   current-scope context, and suspend incidental dependency tracking.
3. Borrow owner state and previous history immutably. Enable the synchronous
   evaluation guard, construct inputs, call the watcher, and construct next
   history. Restore the guard even when the call unwinds.
4. Release both shared borrows and retain the next history. If the scope is
   still live, commit the returned patch through one `Handle::update`.
5. Leave the callback context. Release the subscription and retained history
   on unmount. A pending initial delivery cannot call an unmounted owner.

Evaluation can call browser APIs that remove the owner, so liveness is
checked again before patch commit. The input borrow ends before commit;
borrowed parameters do not require cloning the entire component.

Named handler invocations (such as `@click="on_click"`) triggered
synchronously by DOM APIs during evaluation join the callback FIFO,
including events targeting another component. They run after evaluation and
patch commit complete, provided their target scope is still live. Outside
watcher evaluation, the existing same-scope reentry deferral applies;
handlers on other scopes may execute synchronously.

Changes to several inputs in the same flush pass coalesce into one
invocation. Further changes during a cascade may cause another pass;
delivery is not an event log of every intermediate assignment. Existing
field probes skip a run when every input is provably unchanged. When a
probe cannot prove equality, delivery stays conservative.

Generated watchers record probes from before the callback and preserve
queued input changes. They do not use the legacy low-level callback's
post-callback probe and self-dequeue behavior: an event queued at a browser
callback safe point must still trigger the next invocation.

## 4. Mutation and cycle boundaries

### 4.1 Compile-time checks

An output cannot also be an input of the same watcher, even if the callback
would write only sometimes or converge to a fixed point. Normalize an
incoming value in the action accepting it, before committing it as state.

The handlers macro also emits a const dependency graph. Each named watcher
is a node with declared reads and writes; each computed method contributes
its declared inputs and computed output. An edge exists when one node
writes a field another reads. A cycle is a compile error:

```text
watch(a) -> updates(b)
watch(b) -> updates(c)
watch(c) -> updates(a)    // Rejected, even if each callback is conditional.
```

The check covers the declarations visible in that handlers block, including
longer cycles. `cfg` and `cfg_attr` apply to generated nodes so inactive
callbacks do not create false cycles. Bare observers have no outputs and
cannot close a cycle; the implementation omits their terminal read edges
from the cycle graph while subscribing to all their fields at runtime.

The graph describes declared potential writes, not executed branches. It
does not analyze cross-component model bindings, external stores, hidden
interior mutations, or lower-level effects.

### 4.2 Synchronous mutation guard

Removing the receiver alone cannot stop `this().update(...)` or an imported
signal setter. During evaluation, standard `Handle` mutation and mutable
borrow APIs, `FieldHandle` writes, scope/model mutation helpers, and signal
writes reject direct mutation before it occurs. This includes trying to
enqueue a write with `Handle::defer_update` during evaluation and writing a
different component or store. The diagnostic directs the author to return
`Update<Self>`.

The guard covers the synchronous evaluation window; it ends before the
runtime applies the returned patch. Only declared inputs subscribe:
incidental reactive reads in the callback or model writeback do not add
dependencies. External values that should trigger a transition need an
explicit owning action or a declared local input.

Inline event expressions (such as `@click="count = count + 1"`) retain the
existing same-scope queue: if their target scope already has an active
callback frame, the whole expression waits for the callback frames to
unwind. This includes events targeting the watcher owner's scope. Otherwise
the expression evaluates synchronously, and any field write during watcher
evaluation is rejected. Named-handler deferral does not extend this queue
to inline expressions targeting an inactive scope.

Direct `Handle`, `FieldHandle`, signal, and model or prop mirror writes do
not use named-handler dispatch. Any such write executed during evaluation
still fails immediately; queued callbacks execute after the guard has ended.

These are framework API guarantees. Shared references and cloned values
can contain interior mutability, and hidden macro support APIs are not a
security boundary. Raw scope access, independently scheduled asynchronous
work, and low-level effects remain outside the checked local graph. Moving
a forbidden write into an unrelated deferred callback is not a supported
migration strategy.

The existing runtime flush-cascade guard remains necessary for cycles
through those other paths. This RFC prevents declared local cycles and
standard direct writes during evaluation; it does not claim every Rust
callback is pure or every application graph is statically acyclic.

## 5. Compatibility and migration

Existing receiver-based single- and multi-field watchers fail compilation.
There is no mutable compatibility mode: maintaining two contracts would
leave the source of hidden writes in place. Migration is staged by behavior:

| Existing behavior | New owner or form |
| --- | --- |
| Maintain a purely derived label, percentage, or completion flag | Remove the stored mirror and use `#[computed]`. |
| Reset editable fields or close a popover after an input changes | Declare inputs and `updates(...)`; return a patch. |
| Observe DOM, animate, or log | Named inputs and a unit return. |
| Normalize the same field being watched | Normalize in the action that accepts the input. |
| Observe the complete local state | Bare `#[watch]` with `Changes<Self>` and unit return. |
| Start requests or change another scope | An explicit action or lifecycle-owned session with cancellation and stale-result checks. |

Parameter history is opt-in. Use `T` for an owned current value, `&T` for a
synchronous borrow, and `Change<T>` when comparing or animating from the
previous delivered value. Bare observers deliberately retain every included
field; use a named list to control that cost.

The [migration guide](../docs/guides/reactivity/06-readonly-watch-migration.md)
provides concrete consumer examples. The implementation rollout is:

1. **Contract and tests — implemented locally.** Add types, macro validation,
   restricted patch generation, dispatch, history, and mutation guards.
2. **Examples and documentation — migrated locally.** Layout, website, and
   file-browser examples use computed values, patches, observation, or
   explicit actions according to their ownership needs.
3. **Libraries and downstream consumers — pending.** Migrate Pine, chart,
   layout, and icon libraries, then downstream applications. Example builds
   still encounter the old watcher signatures in those dependencies.
4. **Integration — pending.** Complete workspace host and wasm validation
   and build the affected applications through the Pocopine CLI before
   integrating the combined migration.

The framework patch is reviewable separately. Focused contract tests do not
establish that all existing consumers compile with the new API.

## 6. Validation and implementation references

Acceptance coverage must include:

- Valid owned, borrowed, history, mixed, and all-field callbacks; borrowing
  a non-`Clone` field; no unused history clones; raw names and conditional
  callbacks; skipped fields and declaration ordering.
- Explicit/sugar type equivalence, reusable field descriptors and visibility,
  tuple limits, duplicate marker diagnostics, and empty-output lint controls.
- Rejection of receivers, mutable inputs, incorrect signatures, unknown
  fields, undeclared setters, self-writes, local cycles, and bare patches.
- Deferred initial delivery, coalescing, previous-delivery semantics,
  no incidental subscriptions, unmount cleanup, queued browser reentry,
  and releasing input borrows before patch application.
- Empty patches, explicit clearing of optional fields, batched multi-field
  model writeback, direct mutation rejection before a write, and restoration
  of the evaluation guard after unwind.

These behaviors have focused coverage in the implementation branch. Broad
consumer integration remains subject to the rollout gates above. The
implementation is organized as follows:

| Responsibility | Source |
| --- | --- |
| Public values, tuple patch storage, field membership, evaluation guard, patch commit, graph validation | [`watch_update.rs`](../crates/pocopine-core/src/watch_update.rs) |
| Attribute/signature parsing, field descriptors and setters, empty-output lint, history generation | [`watchers.rs`](../crates/pocopine-macros/src/watchers.rs) |
| Handler lifecycle installation and computed graph nodes | [`pocopine-macros/src/lib.rs`](../crates/pocopine-macros/src/lib.rs) |
| Field subscriptions, probes, deferred seed, coalesced dispatch | [`watch.rs`](../crates/pocopine-core/src/watch.rs) |
| Signature and diagnostic fixtures | [`handlers_contract_ui.rs`](../crates/pocopine/tests/handlers_contract_ui.rs) |
| Browser delivery and patch tests | [`watch_updates.rs`](../crates/pocopine/tests/watch_updates.rs), [`watch_readonly.rs`](../crates/pocopine/tests/watch_readonly.rs) |

## 7. Alternatives and naming

**Keep `&self`.** This prevents direct field assignment but still exposes
undeclared reads and handle-based writes. Named inputs plus a synchronous
mutation guard make the dependency declaration useful for cycle checking.

**Use only computed values.** Computed values suit permanent derivations.
Editable drafts and state transitions need explicit writes to ordinary
fields, sometimes several at once.

**Pass a signal or `Ref<T>`.** Those names imply a live handle or borrow.
`Change<T>` owns two observations. `Delta<T>` suggests a difference or edit
operation, whereas this type holds the current and previous values. `T` and
`&T` cover callbacks that do not need history without a second wrapper name.

**Rename the attribute to `effect`.** Keep `watch`: its triggers are declared
explicitly. The existing low-level `effect()` automatically tracks reactive
reads, a different contract that should remain distinguishable.

**Allow an unrestricted patch excluding only current inputs.** An explicit
`updates(...)` list gives the macro the output edges required to reject
cycles spanning several callbacks. It also bounds which transitions a
watcher can perform as its implementation changes.

**Retain the old `(new, previous)` arguments.** They work for one field but
do not scale to heterogeneous coalesced inputs. A named `Change<T>` keeps
each pair together, while optional history avoids unnecessary cloning.

**Permit self-writes that converge.** Determining convergence through
arbitrary Rust helpers is outside the macro's reach. The uniform rule is
to normalize in actions and reject input/output overlap.

**Spell all-field observation `#[watch(*)]`.** Bare `#[watch]` expresses the
same contract with no wildcard syntax. It remains observation-only.
