# RFC 052 — Typed structural parent extractors

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 027](./rfc-027-provide-inject.md), [RFC 032](./rfc-032-lifecycle-element-param.md), [RFC 046](./rfc-046-children-extractor.md), [RFC 048](./rfc-048-hooks.md), [RFC 049](./rfc-049-typed-slot-contracts.md) |

## 1. Summary

Add two typed structural extractors:

- `Parent<T>` — the immediate parent component must be `T`
- `NearestParent<T>` — walk upward and return the first ancestor
  component of type `T`

These extractors are for **compound-internal structural coupling**,
not for general app dependency injection. They replace raw
`ScopeId` + string-field plumbing in places where children already
know they belong to a specific parent primitive.

```rust
#[handlers]
impl PineComboboxItem {
    pub fn on_ready(&self, parent: Parent<PineComboboxRoot>) {
        parent.update(|root| {
            // ...
        });
    }
}
```

## 2. Motivation

Today the framework already has code that "knows its parent" but has
to express that knowledge through low-level primitives:

- `inject(ROOT)` to recover a typed handle when an inject key exists
- raw `scope_id()` lookups
- `watch_scope_field::<T, _>(scope_id, "field", ...)`
- manual ancestor walking in helper code

Combobox is a concrete example. Components like
`PineComboboxEmpty` and `PineComboboxItem` reach into the root with
`watch_scope_field(root.scope_id(), "has_matches", ...)`,
`watch_scope_field(root.scope_id(), "value", ...)`, and
`watch_scope_field(root.scope_id(), "query", ...)`. The code works,
but the API shape is too low-level for normal authoring:

- stringly-typed field names
- raw scope ids in app/component code
- weak refactor safety
- structural intent hidden behind runtime plumbing

The framework needs a middle layer:

- higher-level than raw scope ids and scope-watch helpers
- lower-level than `provide` / `inject`
- explicit about the fact that this is **structural coupling**

## 3. Non-goals

* **Not a replacement for `provide` / `inject`.** Context remains the
  preferred API for semantic dependencies and shared services.
* **Not arbitrary ancestor queries.** v1 is only `Parent<T>` and
  `NearestParent<T>`.
* **Not sibling traversal in v1.** `Siblings<T>` is discussed in
  §6.3, but not part of the initial surface.
* **Not raw scope graph exposure.** `ScopeId` remains runtime
  plumbing, not the primary user-facing API.

## 4. Design

### 4.1 Extractors

Two extractors are added:

```rust
pub struct Parent<T>(/* private */);
pub struct NearestParent<T>(/* private */);
```

Semantics:

- `Parent<T>` succeeds only when the **immediate parent scope**
  belongs to component `T`
- `NearestParent<T>` walks the parent chain upward and returns the
  first matching ancestor of type `T`

These names are intentionally precise:

- `Parent<T>` means exactly one hop
- `NearestParent<T>` means upward search

Names like `Root<T>`, `First<T>`, or `Last<T>` are rejected because
they are ambiguous once pocopine already has "root element", "app
root", and "component root" concepts.

### 4.2 Extraction model

The extractors are valid anywhere normal lifecycle/handler extractors
are valid:

- `on_setup`
- `on_mount`
- `on_ready`
- event handlers

They are not a new context system. Under the hood they are resolved
from the current scope id plus the runtime parent chain that pocopine
already maintains.

Example:

```rust
#[handlers]
impl PineDialogClose {
    pub fn on_click(&mut self, parent: Parent<PineDialogRoot>) {
        parent.update(|dialog| dialog.close());
    }
}
```

And when wrappers may sit in between:

```rust
#[handlers]
impl PineComboboxEmpty {
    pub fn on_ready(&self, parent: NearestParent<PineComboboxRoot>) {
        // ...
    }
}
```

### 4.3 Methods

The extractors expose a small typed-handle-like API:

```rust
impl<T: 'static> Parent<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R;
    pub fn update(&self, f: impl FnOnce(&mut T));
    pub fn observe<V>(
        &self,
        selector: impl Fn(&T) -> V + 'static,
        f: impl Fn(V, Option<V>) + 'static,
    )
    where
        V: Clone + PartialEq + 'static;
    pub fn handle(&self) -> Handle<T>;
}

impl<T: 'static> NearestParent<T> {
    pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R;
    pub fn update(&self, f: impl FnOnce(&mut T));
    pub fn observe<V>(
        &self,
        selector: impl Fn(&T) -> V + 'static,
        f: impl Fn(V, Option<V>) + 'static,
    )
    where
        V: Clone + PartialEq + 'static;
    pub fn handle(&self) -> Handle<T>;
}
```

These are intentionally close to `Handle<T>` so the mental model is
simple: an extractor yields a typed parent handle with controlled
access to the underlying component.

Typed parent observation is part of the extractor surface because the
common author intent is "this child observes its parent":

```rust
parent.observe(|p| p.value.clone(), move |value, prev| {
    // ...
});
```

Under the hood the extractor may still forward to `Handle<T>::observe(...)`;
that is an implementation detail. The user-facing API stays on the
structural relationship they are expressing.

### 4.4 Failure semantics

Required extractor:

```rust
fn on_mount(parent: Parent<PineDialogRoot>) { ... }
```

If extraction fails, the handler/lifecycle call errors immediately
with a framework panic or structured extraction error, same as other
required extractors.

Optional extractor:

```rust
fn on_mount(parent: Option<NearestParent<PineDialogRoot>>) { ... }
```

This is the escape hatch for components that can be used both inside
and outside a given parent compound.

This RFC deliberately prefers **type-level optionality** over
`.get()`-style probing APIs:

- `Parent<T>` / `NearestParent<T>` means the parent relationship is
  required by the contract
- `Option<Parent<T>>` / `Option<NearestParent<T>>` means the
  relationship is optional

That keeps the requirement visible in the function signature instead
of hiding it behind imperative checks in the body.

APIs like `get()`, `try_get()`, or `get_unchecked()` are explicitly
not the primary design:

- `get()` invites "maybe there is a parent, maybe not" logic to leak
  into every call site
- `get_unchecked()` is the wrong default for a framework extraction
  surface and encourages skipping explicit absence handling

An `expect(...)` helper on the extractor wrapper is acceptable as
convenience later, but the canonical model is:

```rust
Parent<T>                 // required
Option<Parent<T>>         // optional
NearestParent<T>          // required nearest ancestor
Option<NearestParent<T>>  // optional nearest ancestor
```

### 4.5 Position relative to `provide` / `inject`

This RFC explicitly does **not** bless structural ancestry as the new
default dependency mechanism.

Use `provide` / `inject` when:

- the dependency is semantic, not structural
- wrappers should not matter
- the child should not care where in the tree the value came from
- the relationship is app/service-like rather than compound-local

Use `Parent<T>` / `NearestParent<T>` when:

- the child is part of a tightly-coupled compound primitive
- the parent/owner relationship is part of the component contract
- the code already relies on structural parentage, but currently
  expresses it through low-level scope plumbing

In short:

- `inject` is preferred for **context**
- `Parent<T>` is acceptable for **ownership**
- `NearestParent<T>` is acceptable for **nearest structural owner**

### 4.6 Interaction with `#[observe]` and hooks

These extractors do not replace `#[observe]`.

Recommended layering:

1. `#[observe(KEY)]`
   Best when a child only needs to mirror a parent/root field into its
   own local state.
2. `Parent<T>`
   Best when a child needs typed parent reads/updates/observation and
   the parent is immediate.
3. `NearestParent<T>`
   Best when wrappers may exist between child and structural owner.
4. `hooks::use_watch(...)`
   Best for reacting to local or observed state once it is already in
   the component.
5. raw `watch_scope_field(...)`
   Runtime/internal escape hatch, not the preferred public API.

This is the intended improvement path for code like:

```rust
watch_scope_field::<String, _>(root_scope, "value", move |v, _| {
    let is_selected = v == &my_value;
    h.update(|s| s.selected = is_selected);
});
```

### 4.7 Combobox sketch

Current low-level shape:

```rust
watch_scope_field::<bool, _>(root.scope_id(), "has_matches", move |&v, _| {
    handle.update(|s| s.empty = !v);
});
```

Cleaner structural shape:

```rust
#[handlers]
impl PineComboboxEmpty {
    pub fn on_ready(
        &self,
        parent: NearestParent<PineComboboxRoot>,
        handle: Handle<Self>,
    ) {
        parent.with(|root| {
            handle.update(|s| s.empty = !root.has_matches);
        });
    }
}
```

Or, with typed parent observation:

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

The pieces fit cleanly together:

- extractor finds the typed structural owner
- extractor-level `observe(...)` expresses the parent-child reactive
  relationship directly
- runtime implementation may reuse `Handle<T>::observe(...)` internally

## 5. Implementation

1. Add typed parent extraction on top of the existing parent-scope
   links maintained by the runtime.
2. Add extractor parsing/expansion support in `pocopine-macros` for
   `Parent<T>` and `NearestParent<T>`.
3. Reuse existing typed-handle downcast machinery so extraction yields
   a real `Handle<T>` under the hood.
4. Add tests for:
   - immediate parent success/failure
   - nearest-parent success with wrappers in between
   - optional extraction
   - wrong-type extraction failure

## 6. Alternatives considered

### 6.1 Raw `ParentId`

`ParentId` is useful internally, but weaker as the main public API:

- it leaks runtime plumbing
- it pushes users back toward stringly low-level watchers
- most authors do not actually want an id; they want a typed parent

Rejected as the primary surface.

### 6.2 `Ancestor<T>`

Too ambiguous for v1:

- first ancestor?
- any ancestor?
- all ancestors?

`NearestParent<T>` is longer but precise.

### 6.3 `Siblings<T>`

This is interesting, but not a v1 requirement.

Potential semantics:

```rust
fn on_mount(siblings: Siblings<PineTabsTrigger>) { ... }
```

It could be useful for:

- roving/focus compounds
- item count and index calculations
- ARIA setsize/posinset helpers

But it is also more dangerous:

- sibling ordering semantics need to be defined
- filtered vs raw siblings needs a contract
- wrappers and slots complicate the query shape

The recommendation is to defer `Siblings<T>` to a follow-up RFC once
`Parent<T>` / `NearestParent<T>` have proven their ergonomics.

## 7. Open questions

* Should extractor-level `observe(...)` land in the same rollout as
  parent extractors, or as a follow-up once the typed-watch surface
  settles?
* Should `NearestParent<T>` be documented as "advanced" more strongly
  than `Parent<T>` because it couples harder to ancestry shape?
* Should there also be an internal `ParentId` extractor for runtime
  code and devtools, even if it is not part of the main public story?
