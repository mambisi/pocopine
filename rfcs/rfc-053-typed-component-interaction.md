# RFC 053 — Typed component interaction surface

Status: Draft

Author: Codex

Created: 2026-04-25

## 1. Summary

Pocopine should tighten its component-interaction surface around typed,
explicit APIs and stop treating legacy stringly or overly ambient
surfaces as compatibility obligations.

This RFC proposes one coherent direction:

- rename `inject_key!` to `create_context!`
- prefer keyed context everywhere:
  `Inject<ROOT, Handle<MyRoot>>`
- standardize structural parent access:
  `Parent<T>` and `NearestParent<T>`
- support a small handler-context extractor set
- standardize parent observation:
  `parent.observe(|p| p.has_matches, ...)`
- add typed emitted events:
  `#[derive(Emit)] enum DialogEvent { ... }`
- avoid preserving weaker legacy APIs when the replacement is
  substantially safer and clearer

The project motto for this surface is simple:

> Do not let users shoot themselves in the foot.

## 2. Motivation

Pocopine is converging on a strong model:

- RFC 049 gives compile-time structural contracts for children
- RFC 050 gives compile-time template analysis
- RFC 051 tightens global component registry safety
- keyed `provide` / `inject` already beats string-key context models

What remains uneven is the runtime authoring surface around compound
components and local context:

- `inject_key!` is more low-level than it needs to be
- raw scope watchers are too easy to misuse
- event emission is still stringly
- event handlers and lifecycle hooks do not yet feel like one coherent
  extractor model

This RFC groups the obvious follow-up cleanup into one direction instead
of leaving each piece to drift independently.

## 3. Goals

- Prefer typed, explicit, signature-level APIs over stringly ones.
- Clearly separate structural relations from contextual relations.
- Make compound components easier to author without raw scope plumbing.
- Remove weak legacy naming when the replacement is clearly better.
- Keep the public surface small and composable.

## 4. Non-goals

- Replacing keyed context with type-only injection.
- Turning handlers into a fully generic extractor kitchen sink.
- Preserving every legacy helper for compatibility.
- Replacing native DOM event types with framework-specific wrapper traits.

## 5. Proposal

### 5.1 Context creation becomes `create_context!`

The public macro for declaring keyed context channels should be:

```rust
pocopine::create_context!(ROOT: Handle<PineDialogRoot>);
```

The old `inject_key!` name should be removed rather than kept as a
permanent alias.

Rationale:

- the macro creates a full context channel, not merely an “inject key”
- the declared item is used for both `provide` and `inject`
- the new name reads better at callsites and in documentation

### 5.2 Preferred keyed context shape

Runtime use:

```rust
pocopine::create_context!(ROOT: Handle<PineDialogRoot>);

ROOT.provide(this::<Self>());
let root = ROOT.inject();
```

Extractor use:

```rust
pub fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) {
    root.update(|dialog| dialog.close());
}
```

Optional contextual dependency:

```rust
pub fn on_mount(
    &mut self,
    root: Option<Inject<ROOT, Handle<PineDialogRoot>>>,
) {
    self.inside_dialog = root.is_some();
}
```

`Inject<T>` by type remains out of scope. Keys stay explicit.

### 5.3 Future `#[context]` sugar stays keyed underneath

Pocopine may later add:

```rust
#[context]
pub struct DialogContext<T> {
    pub handle: T,
}
```

but that sugar must compile down to generated keyed context. It does not
authorize type-only lookup.

### 5.4 Structural parent APIs

Pocopine should expose:

- `Parent<T>` for “immediate parent must be `T`”
- `NearestParent<T>` for “walk upward and return the first ancestor of
  type `T`”

Required relationships are explicit in the function signature:

```rust
pub fn on_mount(&mut self, parent: Parent<PineComboboxRoot>) { ... }
```

Optional relationships use `Option`:

```rust
pub fn on_mount(
    &mut self,
    parent: Option<NearestParent<PineDialogRoot>>,
) { ... }
```

This RFC rejects `.get()` / `get_unchecked()` as the primary public
model. Presence requirements should be visible in the signature.

### 5.5 Parent observation

Once a child has a typed structural owner, the preferred observation
shape is:

```rust
parent.observe(|p| p.has_matches, move |has_matches, prev| {
    ...
});
```

This is the typed replacement for raw scope watchers in compound
components.

### 5.6 Small handler-context extractor set

Ordinary event handlers should support a small explicit context extractor
set alongside event payload arguments:

- `Handle<Self>`
- `ScopeId`
- `Refs`
- `El`
- `Parent<T>`
- `NearestParent<T>`
- `Inject<KEY, T>`
- `Option<...>` of the above

This should remain a whitelist, not a general “extract anything”
mechanism.

### 5.7 Typed emitted events

Pocopine should add typed event emission:

```rust
#[derive(Emit)]
pub enum DialogEvent {
    Close,
    Confirm { value: String },
}
```

and:

```rust
emit(DialogEvent::Close);
emit(DialogEvent::Confirm { value });
```

The derive maps variant names to event names:

- `Close` -> `close`
- `Confirm` -> `confirm`

Payload variants serialize into event `detail`.

This should become the preferred event surface over raw string event
names.

## 6. Design principles

### 6.1 Structural vs contextual relations stay separate

These APIs solve different problems:

- `Parent<T>` / `NearestParent<T>` are structural
- `Inject<KEY, T>` is contextual

Pocopine should not blur them into one generic “context” abstraction.

### 6.2 Breaking cleanup is acceptable here

If a new API is materially safer and more coherent, pocopine should not
keep the older weaker surface merely for compatibility.

Concretely:

- `create_context!` is better than `inject_key!`
- keyed `Inject<KEY, T>` is better than `Inject<T>`
- typed emit enums are better than string-only emit as the primary
  documented path

### 6.3 Signatures should carry the contract

This RFC prefers:

- explicit key parameters
- explicit `Option<...>`
- explicit typed parent relations

over APIs that defer failure or ambiguity into the method body.

## 7. Examples

### 7.1 Context

```rust
pocopine::create_context!(ROOT: Handle<PineDialogRoot>);

#[handlers]
impl PineDialogRoot {
    pub fn on_mount(&mut self) {
        ROOT.provide(this::<Self>());
    }
}
```

### 7.2 Parent observation

```rust
#[handlers]
impl PineComboboxEmpty {
    pub fn on_ready(
        &self,
        parent: NearestParent<PineComboboxRoot>,
        handle: Handle<Self>,
    ) {
        parent.observe(|root| root.has_matches, move |has_matches, _| {
            handle.update(|s| s.empty = !has_matches);
        });
    }
}
```

### 7.3 Event handler mixing event payload and context

```rust
#[handlers]
impl PineDialogCloseButton {
    pub fn on_click(
        &mut self,
        ev: web_sys::MouseEvent,
        root: Inject<ROOT, Handle<PineDialogRoot>>,
        el: El,
        refs: Refs,
    ) {
        ev.stop_propagation();
        root.update(|dialog| dialog.close());
    }
}
```

### 7.4 Typed emit

```rust
#[derive(Emit)]
pub enum DialogEvent {
    Close,
    Confirm { value: String },
}

#[handlers]
impl PineDialogRoot {
    pub fn confirm(&self) {
        emit(DialogEvent::Close);
    }
}
```

## 8. Migration guidance

This RFC deliberately prefers clean migration over indefinite alias
support.

Expected migration examples:

```rust
// old
pocopine::inject_key!(ROOT: Handle<MyRoot>);

// new
pocopine::create_context!(ROOT: Handle<MyRoot>);
```

and:

```rust
// old
emit("close");

// new
emit(DialogEvent::Close);
```

## 9. Drawbacks

- This is a breaking cleanup direction, not a compatibility-first one.
- It adds more typed surface area that must remain coherent.
- Typed emit requires derive and serialization design work.

## 10. Alternatives considered

### 10.1 Keep all legacy names as aliases

Rejected. The resulting docs and code would remain split between old and
new styles for little real value.

### 10.2 Type-only injection

Rejected. It is more elegant at first glance but too easy to misuse in
large trees.

### 10.3 Keep string-only emit as the main event surface

Rejected. It is weaker than the rest of the typed direction and does not
fit the framework’s safety goals.

## 11. Unresolved questions

1. Should `#[context]` support tuple newtypes in addition to named-field
   structs, or should it optimize for future growth and only support
   named fields at first?
2. Should typed emit support cancelable/bubbles configuration at the
   enum level, the variant level, or only through explicit low-level
   helpers?
