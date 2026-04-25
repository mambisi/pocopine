# RFC 056 - Component interaction safety batch

Status: Implemented (phases 1–4 + extractor surface; phase 5 repo migration of Pine primitives outstanding)

Author: Codex

Created: 2026-04-25

Supersedes: [RFC 051](./rfc-051-component-registry-safety.md),
[RFC 052](./rfc-052-parent-extractors.md),
[RFC 053](./rfc-053-typed-component-interaction.md),
[RFC 055](./rfc-055-typed-context.md)

Related: [RFC 027](./rfc-027-provide-inject.md),
[RFC 028](./rfc-028-emit.md),
[RFC 030](./rfc-030-inject-key-symbols.md),
[RFC 032](./rfc-032-lifecycle-element-param.md),
[RFC 049](./rfc-049-typed-slot-contracts.md),
[RFC 050](./rfc-050-html5ever-compile-time-parser.md)

## 1. Summary

Pocopine should batch the component interaction cleanup into one
coherent rollout instead of implementing several overlapping draft RFCs
piece by piece.

This RFC is the authoritative target for:

- component registry collision safety,
- typed structural parent extractors,
- keyed context cleanup,
- handler and lifecycle context extractors,
- typed emitted events,
- migration away from weaker stringly or ambient APIs.

Where the superseded RFCs disagree, this RFC follows the stricter
direction from RFC 053:

- `create_context!` replaces `inject_key!` as the author-facing
  declaration macro,
- keyed context remains the runtime model,
- `Inject<KEY, T>` is the extractor form,
- `Parent<T>` / `NearestParent<T>` express structural ownership,
- typed event enums become the preferred emit surface,
- compatibility aliases are temporary migration aids, not permanent
  parallel APIs.

## 2. Motivation

RFC 051, 052, 053, and 055 all point at the same underlying problem:
component interaction is correct in many places, but the public surface
still exposes too much low-level runtime machinery.

Current code can require authors to juggle:

- string component tags with silent registry overwrites,
- `inject_key!` plus free `provide(&KEY, ...)` / `inject(&KEY)` calls,
- raw `ScopeId` ancestry plumbing,
- `watch_scope_field(scope, "field", ...)`,
- string event names passed to `emit(...)`.

Each individual RFC improves one slice, but implementing them
independently would leave awkward seams:

- RFC 055 keeps `inject_key!` as stable naming while RFC 053 prefers
  `create_context!`.
- RFC 052 defines parent extractors, while RFC 053 places them in a
  broader typed interaction surface.
- RFC 051 strengthens global registry behavior but does not connect that
  work to the broader "do not let users shoot themselves in the foot"
  direction.

This RFC batches the work so the runtime, macros, docs, tests, and Pine
primitive migrations land around one coherent contract.

## 3. Goals

- Make component registration deterministic and fail-fast.
- Preserve explicit keyed context identity while improving call sites.
- Replace raw structural ancestry plumbing with typed extractors.
- Let handler and lifecycle signatures carry their dependencies.
- Provide a typed event emission path.
- Give app authors one clear migration path instead of several
  competing draft surfaces.
- Keep low-level escape hatches available for runtime internals.

## 4. Non-goals

- Replacing keyed context with type-only injection.
- Auto-discovering components through linker sections.
- Making aliases hide component registry conflicts.
- Replacing RFC 049's local compile-time `uses = [...]` slot contract
  model.
- Rewriting every Pine primitive in the first implementation PR.
- Removing all low-level runtime helpers immediately.

## 5. Precedence

This RFC supersedes RFC 051, 052, 053, and 055.

The intended carry-forward rules are:

- RFC 051's registry safety design is included here with minor naming
  consolidation.
- RFC 052's `Parent<T>` / `NearestParent<T>` design is included here.
- RFC 053's stricter interaction direction is the primary basis for this
  RFC.
- RFC 055 remains useful background, but RFC 053 wins where they
  conflict.

Concretely:

- `create_context!` is the target public macro.
- `inject_key!` may exist as a temporary compatibility alias during
  migration, but it is not the long-term documented surface.
- keyed context remains authoritative; bare `Inject<T>` is rejected.
- `Inject<KEY, T>` is the public extractor form.

## 6. Design

### 6.1 Registry safety

The component registry must stop using last-write-wins semantics.

The runtime registry stores canonical entries, aliases, ownership
metadata, and accumulated errors:

```rust
pub type ComponentCtor = fn() -> Scope;

pub struct RegisteredComponent {
    pub canonical: &'static str,
    pub owner: &'static str,
    pub ctor: ComponentCtor,
}

pub enum RegistryErrorKind {
    DuplicateCanonicalTag,
    DuplicateAlias,
    AliasConflictsWithCanonical,
    CanonicalConflictsWithAlias,
}

pub struct RegistryError {
    pub kind: RegistryErrorKind,
    pub tag: &'static str,
    pub first_owner: &'static str,
    pub second_owner: &'static str,
}
```

Public registration APIs:

```rust
pub fn register_component(
    canonical: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
);

pub fn register_component_as(
    alias: &'static str,
    canonical: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
);

pub fn register_component_prefixed(
    prefix: &'static str,
    short: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
);
```

Collision policy:

- canonical vs canonical: error,
- alias vs alias: error,
- alias vs canonical: error,
- canonical vs alias: error,
- exact same owner and same tag re-registration: no-op.

There is no fallback where the last call silently wins.

### 6.2 Boot verification

`pocopine::run()` and `App::run()` verify the registry before the first
mount.

If registry errors exist:

- normal mounting does not start,
- the error is logged to the console,
- the app root renders a permanent client-side boot error surface,
- the surface lists conflicting tags and owners.

The runtime also exposes test helpers:

```rust
pub fn registry_errors() -> Vec<RegistryError>;
pub fn assert_registry_clean();
pub fn registered_component_names() -> Vec<String>;
pub fn verify_registry() -> Result<(), Vec<RegistryError>>;
```

### 6.3 Context declaration

The author-facing context declaration macro is:

```rust
pocopine::create_context!(ROOT: Handle<PineDialogRoot>);
```

It expands to a keyed context channel. The runtime still uses explicit
identity, not type-only lookup.

The low-level primitive remains:

```rust
pub struct ContextKey<T: 'static> { /* private */ }
```

`InjectKey<T>` may remain as a deprecated alias during migration:

```rust
#[deprecated(note = "use create_context! / ContextKey instead")]
pub type InjectKey<T> = ContextKey<T>;
```

The public direction is:

- docs use `create_context!`,
- examples use `create_context!`,
- new Pine primitives use `create_context!`,
- old `inject_key!` call sites migrate gradually.

### 6.4 Context use

Both method-style and free-function forms are allowed during the
migration window:

```rust
ROOT.provide(this::<Self>());
let root = ROOT.inject();
```

Equivalent low-level forms:

```rust
provide(&ROOT, this::<Self>());
let root = inject(&ROOT);
```

The dominant documented style is method-style once this RFC lands.

### 6.5 Keyed context extractor

Handlers and lifecycle hooks support keyed context extraction:

```rust
pub fn on_click(&self, root: Inject<ROOT, Handle<PineDialogRoot>>) {
    root.update(|dialog| dialog.close());
}
```

Optional context uses `Option`:

```rust
pub fn on_mount(
    &mut self,
    root: Option<Inject<ROOT, Handle<PineDialogRoot>>>,
) {
    self.inside_dialog = root.is_some();
}
```

Rejected forms:

- `Inject<T>` by type,
- string keys,
- implicit "nearest value of this type" lookup.

### 6.6 Structural parent extractors

The structural parent API is:

```rust
pub struct Parent<T>(/* private */);
pub struct NearestParent<T>(/* private */);
```

Semantics:

- `Parent<T>` requires the immediate parent scope to be component `T`.
- `NearestParent<T>` walks upward and returns the first ancestor
  component of type `T`.

Required relationships use the bare extractor:

```rust
pub fn on_mount(&mut self, parent: Parent<PineDialogRoot>) {
    parent.update(|dialog| dialog.close());
}
```

Optional relationships use `Option`:

```rust
pub fn on_mount(
    &mut self,
    parent: Option<NearestParent<PineDialogRoot>>,
) {
    self.inside_dialog = parent.is_some();
}
```

The extractor API mirrors `Handle<T>`:

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
```

`NearestParent<T>` exposes the same methods.

Use structural parent extractors when the relationship is part of a
compound primitive's ownership contract. Use context when the dependency
is semantic and should not care about tree shape.

### 6.7 Handler and lifecycle extractor set

Handlers and lifecycle hooks should share a small explicit extractor
model.

Supported context-style extractors:

- `Handle<Self>`,
- `ScopeId`,
- `Refs`,
- `El`,
- `Parent<T>`,
- `NearestParent<T>`,
- `Inject<KEY, T>`,
- `Option<...>` of the above.

Event handlers also continue to accept event payload types from the
existing `FromHandlerArg` model:

- `web_sys::*Event`,
- primitive payloads,
- `JsValue`,
- user types implementing the handler argument conversion trait.

This remains a whitelist, not a general "anything can be extracted"
mechanism.

### 6.8 Typed emitted events

Pocopine adds typed event emission:

```rust
#[derive(Emit)]
pub enum DialogEvent {
    Close,
    Confirm { value: String },
}
```

Usage:

```rust
emit(DialogEvent::Close);
emit(DialogEvent::Confirm { value });
```

The derive maps variants to event names:

- `Close` -> `close`,
- `Confirm` -> `confirm`.

Struct variants serialize their fields as the event payload. At the DOM
boundary, that payload is carried in `CustomEvent.detail`; `detail` is
the browser transport field, not the pocopine author-facing term.

The generated trait shape is:

```rust
pub trait Emit {
    const NAME: &'static str;
    type Payload: serde::Serialize;

    fn payload(&self) -> Self::Payload;
}
```

The final implementation can choose a different internal trait if macro
ergonomics require it, but public usage should remain `emit(Event::...)`.

String emission remains available as `emit_raw(...)` or equivalent for
runtime internals and dynamic cases, but typed events become the primary
documented API.

### 6.9 Migration policy

This is a breaking cleanup direction, but the implementation should
still land in phases:

1. Add new APIs alongside old APIs.
2. Migrate framework examples and Pine primitives.
3. Mark old names as deprecated once the repo has moved.
4. Remove deprecated aliases in a later breaking release.

Expected migrations:

```rust
// old
pocopine::inject_key!(ROOT: Handle<MyRoot>);
provide(&ROOT, this::<Self>());
let root = inject(&ROOT);

// new
pocopine::create_context!(ROOT: Handle<MyRoot>);
ROOT.provide(this::<Self>());
let root = ROOT.inject();
```

```rust
// old
emit("close", ());

// new
emit(DialogEvent::Close);
```

```rust
// old
let root = inject(&ROOT).expect("root context");
watch_scope_field::<bool, _>(root.scope_id(), "open", ...);

// new
pub fn on_ready(&self, parent: NearestParent<PineDialogRoot>) {
    parent.observe(|root| root.open, ...);
}
```

## 7. Implementation Plan

### Phase 1 - Registry safety

1. Replace the component registry `HashMap<&'static str, ComponentCtor>`
   with canonical entries, aliases, owner metadata, and accumulated
   errors.
2. Update `#[component]` registration output to pass owner metadata.
3. Add registry query, verification, and assertion helpers.
4. Add boot verification and the permanent DOM error renderer.
5. Add tests for duplicate canonical tags, duplicate aliases,
   alias/canonical conflicts, idempotent re-registration, and boot
   failure.

### Phase 2 - Context key cleanup

1. Introduce `ContextKey<T>` and `create_context!`.
2. Keep `InjectKey<T>` and `inject_key!` as deprecated compatibility
   names.
3. Add key methods: `KEY.provide(value)` and `KEY.inject()`.
4. Add `Inject<KEY, T>` and optional `Option<Inject<KEY, T>>`
   extraction for lifecycle hooks.
5. Extend handler macro support for the same keyed extractor form.

### Phase 3 - Structural parent extractors

1. Add `Parent<T>` and `NearestParent<T>` runtime types.
2. Resolve them from the existing scope parent chain and typed scope
   storage.
3. Add `with`, `update`, `observe`, and `handle` methods.
4. Add lifecycle and handler macro extraction support.
5. Add tests for immediate success, wrong-parent failure, nearest
   ancestor success, optional absence, and wrapper scenarios.

### Phase 4 - Typed emit

1. Add an `Emit` derive macro.
2. Add typed `emit(event)` and typed explicit-target variants.
3. Rename or split the existing string API so dynamic emission remains
   available without being the primary surface.
4. Add tests for unit variants, struct variants, event names, payload
   serialization into `CustomEvent.detail`, bubbling, and cancelable
   variants if supported.

### Phase 5 - Repo migration

1. Migrate Pine primitive context declarations.
2. Replace raw `ScopeId` + `watch_scope_field` compound plumbing where
   `Parent<T>` / `NearestParent<T>` is a clear improvement.
3. Migrate docs and examples.
4. Add deprecation warnings for old APIs after the repo no longer uses
   them in normal examples.

## 8. Tests

The batch is not complete until these categories have coverage:

- component registry conflict tests,
- app boot failure rendering tests,
- context key method tests,
- keyed extractor tests in lifecycle hooks,
- keyed extractor tests in event handlers,
- parent extractor tests,
- optional extractor tests,
- parent observation tests,
- typed emit tests,
- compile-fail tests for invalid extractor shapes where feasible.

## 9. Open Questions

1. Should `create_context!` generate a `ContextKey<T>` directly or a
   macro-specific wrapper type that carries the identifier as a type-level
   marker for `Inject<KEY, T>`?
2. Should `Inject<KEY, T>` use const generics, generated marker types, or
   macro-generated helper aliases to represent `KEY` on stable Rust?
3. Should typed `Emit` support bubbles/cancelable configuration on the
   enum, variant, or explicit call site?
4. Should registry aliases be documented for app authors, or treated as a
   framework/internal migration tool?
5. How long should `inject_key!` remain as a compatibility alias after
   `create_context!` lands?

## 10. Recommendation

Implement this RFC in dependency order:

1. registry safety,
2. context key cleanup,
3. parent extractors,
4. typed emit,
5. repo migration and deprecations.

That order avoids migrating primitives onto new interaction APIs before
the runtime can fail fast on invalid component registration, and it keeps
the context API settled before parent extractors and typed events start
appearing in public examples.
