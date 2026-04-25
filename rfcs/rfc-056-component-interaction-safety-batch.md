# RFC 056 - Component interaction safety batch

Status: Implemented (all phases landed + follow-on infrastructure: scope-bound listener / timer / reactive helpers, unified lifecycle, Handle method-style watches)

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

## 7.5 Follow-on infrastructure (landed alongside Phase 5)

Implementing Phase 5 surfaced gaps the original RFC didn't anticipate.
The migration would have stayed verbose without this supporting
surface, so it landed in the same rollout window:

### 7.5.1 `pocopine::events`

Typed DOM event listeners with scope-bound auto-cleanup. The
pre-existing `Closure::wrap` + `add_event_listener_with_callback` +
`closure.forget()` triplet showed up dozens of times across pine and
leaked a listener on every consumer's unmount.

- `events::on(target, ev::name, handler) -> ListenerHandle` —
  RAII-cancelled on drop.
- `events::on_scoped(target, ev::name, handler)` — registers the
  cleanup against the current scope's unmount; no handle to manage.
- `events::ev::*` — compile-time catalog of ~70 standard DOM event
  names paired with their web-sys payload types via
  `DomEventName`. Removes both the stringly event name and the
  turbofish on the closure parameter.
- `events::on_named(_scoped)` — escape hatch for custom-element /
  vendor / dynamic event names.
- `events::on_emit(_scoped)` — receives `#[derive(Emit)]` enums by
  registering one DOM listener per variant name and reconstructing
  the enum from `CustomEvent.detail` via `Emit::from_event`.
- `events::on_scope_unmount(f)` — generic per-scope teardown hook,
  used by `timers` and the scope-bound reactive helpers.

### 7.5.2 `pocopine::timers`

Scope-bound `setTimeout` / `setInterval` helpers with the same
RAII-handle / `_scoped` shape:

- `timers::after(_scoped)` / `timers::every(_scoped)` — single-fire
  and repeating, with `TimeoutHandle` / `IntervalHandle` returns.
- `timers::Debounced` — reusable cancel-and-replace slot. The
  workhorse for hover / scroll-fade / autosave debouncing.
- `timers::sleep(ms).await` / `next_frame().await` /
  `next_tick().await` — awaitable helpers for use inside
  `pocopine::spawn_scoped` async flows.

### 7.5.3 Scope-bound reactive helpers

`reactive::effect_scoped`, `watch::watch_scoped`,
`watch_field_scoped`, `watch_scope_field_scoped`. Same shape as the
existing `effect` / `watch` / `watch_field` etc., but the EffectId
is `release`d at the consumer's scope unmount instead of leaking
for the page lifetime — every original `watch_scope_field` call
site in pine was a leak.

### 7.5.4 `Handle::watch_field` / `Handle::observe`

Method-style watches on the universal `Handle<T>`:

- `handle.watch_field::<V>("name", cb)` — sugar for
  `watch_scope_field_scoped::<V, _>(handle.scope_id(), "name", cb)`.
  Stringly-named for tight per-field tracking.
- `handle.observe(|s| selector(s), cb)` — fully-typed alternative.
  Selector reads through `&T` (no string), re-runs on any field
  change, PartialEq-gates the callback. The right default for
  one-field selectors and derived expressions.

### 7.5.5 `pocopine::dom`

`dom::window()` / `dom::document()` / `dom::body()` /
`dom::document_element()` shortcuts for the
`web_sys::window().and_then(|w| w.document())` chain that appeared
~30 times across pine.

### 7.5.6 Sugar macros

`on!(target, name, |e| body)` and `on_emit!(target, EnumTy, |e|
match e { … })` — declarative sugar over `events::on_scoped` /
`events::on_emit_scoped` with implicit `move` and bare event-name
identifier resolved against `events::ev`.

### 7.5.7 Lifecycle unification

All four lifecycle hooks (`on_setup`, `on_mount`, `on_ready`,
`on_unmount`) now take a `LifecycleContext<'_>` and the
`#[handlers]` macro projects extractors uniformly into the user
signature. `LifecycleContext` carries a `LifecyclePhase` tag;
element-dependent extractors (`El`, `Refs`, `HostEl`, …) panic at
runtime with a precise message when used in `Setup` / `Unmount`
where the rendered template doesn't exist or may be detaching.
Phase-agnostic extractors (`Handle`, `Inject`, `Parent`,
`NearestParent`, `ScopeId`, `Doc`, `Win`, `Body`, …) work in every
phase. Author code that wrote zero-arg `fn on_setup(&mut self)` /
`fn on_unmount(&mut self)` is unaffected.

### 7.5.8 Pine handler visibility tightening

Lifecycle methods inside `#[handlers]` impls dropped from `pub fn`
to `fn`. They're dispatched by macro-generated forwarders in the
same impl block, never called directly from outside the module.
Codifies the architectural rule that cross-component coordination
flows through events (child → parent) and props/context (parent →
child), not direct method calls. Removes incidental visibility
cascades when an `Inject<KEY, T>` extractor appears in a handler
signature — the marker stays default-visibility.

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

## 9. Resolved Open Questions

1. **Should `create_context!` generate a `ContextKey<T>` directly or a
   macro-specific wrapper type?** — *Both.* The macro emits both the
   `LazyLock<ContextKey<T>>` static (value namespace) AND a same-name
   marker struct (type namespace) implementing `ContextMarker`. Method
   calls like `ROOT.provide(x)` resolve the static; type-level uses
   like `Inject<ROOT, T>` resolve the marker. They share an
   identifier across the two namespaces.

2. **Should `Inject<KEY, T>` use const generics, generated marker
   types, or macro-generated helper aliases?** — *Generated marker
   types* via the `ContextMarker` trait. `Inject<KEY, T>` requires
   `KEY: ContextMarker<Value = T>`; the bound carries the wire
   identity via `KEY::key()`.

3. **Should typed `Emit` support bubbles/cancelable on the enum,
   variant, or call site?** — *Call site for now*: `emit_event` is
   bubbling; `emit_cancelable` / `emit_cancelable_from` stay as
   separate stringly fns for the cancelable case until a concrete
   primitive needs the typed shape. Per-variant attributes
   (`#[emit(cancelable)]`) deferred.

4. **Should registry aliases be documented for app authors?** —
   *Framework/internal tool.* `register_component_as` /
   `register_component_prefixed` are public for plugin/wrapper
   crates that need to alias an existing component into a new tag
   namespace, but app authors should reach for plain
   `register_component`.

5. **How long should `inject_key!` remain as a compatibility alias?** —
   *Two minor releases minimum, then evaluate.* Pine has fully
   migrated to `create_context!`; first-party app code should follow
   in the next release window. The alias stays as a
   non-`#[deprecated]` alias initially (Rust limits `#[deprecated]`
   on `macro_rules!`), with the docstring noting the preferred
   form.

## 10. Recommendation

Implemented in dependency order:

1. registry safety,
2. context key cleanup,
3. parent extractors,
4. typed emit,
5. repo migration and deprecations.

That order avoided migrating primitives onto new interaction APIs
before the runtime could fail fast on invalid component registration,
and it kept the context API settled before parent extractors and
typed events appeared in public examples. The follow-on infrastructure
in §7.5 (events / timers / scope-bound reactive helpers / Handle
methods / lifecycle unification) was added during phase 5 once
migrating real consumers surfaced the patterns that warranted
abstraction.
