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
`Update<Self>` patch:

```rust
#[handlers]
impl Editor {
    #[watch(record, writes(draft, dirty, error))]
    fn start_edit(record: Change<String>) -> Update<Self> {
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
| `#[watch(a)]` | One named input for `a` | `()` or `Update<Self>` |
| `#[watch(a, b)]` | One named input for each listed field | `()` or `Update<Self>` |
| `#[watch(a, b, writes(c, d))]` | One named input for each listed field | `Update<Self>` |
| `#[watch]` | One parameter of type `Changes<Self>` | `()` only |

A named watcher lists at least one input. The optional `writes(...)` list
comes last. Duplicate inputs, duplicate outputs, repeated `writes(...)`
lists, stacked watch attributes, and input/output overlap are errors.
`#[watch()]`, `#[watch(*)]`, and a writes-only attribute are not supported.

Watch callbacks are synchronous, safe, non-generic associated functions
inside `#[handlers]`. They accept no receiver, including `&self`, and no
context extractors. Lifecycle method names cannot also be watcher names.
The only result shapes are unit (implicit or explicit) and `Update<Self>`;
`Option<Update<Self>>`, `Result`, and asynchronous results are not accepted.
Return an empty update to skip a transition.

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

`Update<Self>` is authoring shorthand. The handlers macro adds a generated
policy type identifying this particular watcher. Only its declared outputs
receive fluent setters; declaring `writes(draft, dirty, error)` does not
provide a `.record(...)` setter or general mutable access to the owner.

Each patch slot is `Option<FieldType>`. Omission means preserve the field;
setting an optional field to `None` means explicitly clear it:

```rust
Update::new()                         // Preserve every field.
Update::new().dirty(false)            // Set only dirty.
Update::new().error(None)             // Clear error.
Update::new().error(Some(message))    // Replace error.
```

Calling a setter twice replaces its pending value. A watcher may set a
subset of its declared outputs. `writes(...)` requires a patch return type;
a named watcher returning `Update<Self>` without declared outputs can only
return an empty patch. Bare observers cannot return a patch at all.

The runtime commits a nonempty patch through one owner `Handle::update`
and model-writeback batch. Reactive observers see the completed batch;
unchanged model values do not emit another update event. An empty patch
skips mutation entirely. This is batching, with no rollback transaction or
guarantee of ordering between distinct watchers. Multiple watchers may
declare the same output if the graph remains acyclic; applications needing
a deterministic resolution should give that transition one owner.

The generated policy and builder traits are implementation details.
`Update<Self>` and `Changes<Self>` are macro shorthand within watcher
signatures, not one-parameter aliases usable in arbitrary Rust signatures.
Helpers can return ordinary domain values for the watcher to put in a patch.

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

The owner macro (`#[component]` or `#[store]`) knows field types; the
handlers macro knows declared inputs and outputs. Generated `WatchField`
metadata connects them. There is no source-text search for assignments in
the callback or its helper methods.

The following expansion illustrates `Editor::start_edit` from the summary.
It abbreviates generated names, substitutes concrete field types and direct
assignments for the `WatchField` bridge, and omits lifecycle boilerplate:

```rust
mod __start_edit {
    use super::*;
    use pocopine::__private::WatchSpec;

    pub struct Policy;

    #[derive(Default)]
    pub struct Patch {
        draft: Option<String>,
        dirty: Option<bool>,
        error: Option<Option<String>>,
    }

    impl WatchSpec<Editor> for Policy {
        type Patch = Patch;

        fn is_empty(patch: &Patch) -> bool {
            patch.draft.is_none() && patch.dirty.is_none() && patch.error.is_none()
        }

        fn apply(patch: Patch, state: &mut Editor) {
            if let Some(value) = patch.draft { state.draft = value; }
            if let Some(value) = patch.dirty { state.dirty = value; }
            if let Some(value) = patch.error { state.error = value; }
        }
    }

    pub trait Setters: Sized {
        fn draft(self, value: String) -> Self;
        fn dirty(self, value: bool) -> Self;
        fn error(self, value: Option<String>) -> Self;
    }

    impl Setters for Update<Editor, Policy> {
        fn draft(mut self, value: String) -> Self {
            self.__patch_mut().draft = Some(value);
            self
        }
        fn dirty(mut self, value: bool) -> Self {
            self.__patch_mut().dirty = Some(value);
            self
        }
        fn error(mut self, value: Option<String>) -> Self {
            self.__patch_mut().error = Some(value);
            self
        }
    }
}

impl Editor {
    fn start_edit(record: Change<String>) -> Update<Self, __start_edit::Policy> {
        use __start_edit::Setters as _;
        Update::new().draft(record.current).dirty(false).error(None)
    }
}
```

The setters are a generated extension trait implemented for one specialized
`Update` type. The runtime owns the final mutable access; author code only
constructs values in the restricted patch.

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
watch(a) -> writes(b)
watch(b) -> writes(c)
watch(c) -> writes(a)    // Rejected, even if each callback is conditional.
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
| Reset editable fields or close a popover after an input changes | Declare inputs and `writes(...)`; return a patch. |
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
| Public values, generated policy traits, evaluation guard, patch commit, graph validation | [`watch_update.rs`](../crates/pocopine-core/src/watch_update.rs) |
| Attribute/signature parsing, field metadata, policies, history generation | [`watchers.rs`](../crates/pocopine-macros/src/watchers.rs) |
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
`writes(...)` list gives the macro the output edges required to reject
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
