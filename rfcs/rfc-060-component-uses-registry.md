# RFC 060 — `uses` as the authoritative component registry

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-28 |
| **Supersedes** | — (extends RFC 049; resolves RFC 051's deferral) |
| **Related** | [RFC 049](./rfc-049-typed-slot-contracts.md), [RFC 051](./rfc-051-component-registry-safety.md), [RFC 056](./rfc-056-component-interaction-safety-batch.md), [RFC 058 §5.10](./rfc-058-compiled-views-walker-removal.md) |

## 1. Summary

Promote `#[component(uses = [...])]` from a *local slot-contract
validator* (RFC 049) into the **authoritative source of which
components exist in a pocopine application**. Every custom tag a
template references must resolve through the consumer's `uses`
list (or a bundle re-exported via it); the transitive closure of
`uses` from each `App::route::<C>(...)` entry is the entire
component registry the runtime knows about.

Concrete consequences:

- **Compile-time tag validation.** `<pine-dialog-content>` in any
  `template_inline` (or `.poco`) without a matching `uses` entry is
  a hard `cargo check` error, not a silent runtime no-op.
- **No more `T::register()` calls in user code.** The macro emits
  registration via the closure walk; tests, examples, and libraries
  stop calling per-component register helpers.
- **Bundle re-exports** let compounds expose one stable name. A
  consumer types `uses = [pine::Dialog]`; the macro flattens the
  bundle to the six concrete dialog tags.
- **Tree-shaking falls out for free.** A component never referenced
  through any route's `uses` closure is dead code — `wasm-opt`
  drops it.
- **Cluster graph reuse.** The same `uses` graph drives the
  route-boundary clustering committed in RFC 058 §5.10.1; one
  declaration, two consumers (compile-time validation today,
  split-by-default delivery in Phase 7-8).
- **The adopted-DOM bridge boundary stays exactly where Phase 6.5
  drew it.** The bridge mounts only tags reachable from the route
  closure (i.e. registered via `uses`); unknown tags in
  user-authored runtime HTML are flagged in dev and silently
  ignored in release, matching the existing per-element-directive
  policy.

## 2. Motivation

Three forces converge on this:

### 2.1 RFC 049 already proved the pattern

`uses = [...]` works today for slot-contract validation: the
consumer declares which child types its template can host; the
macro checks slot-`accepts` constraints at compile time. Authors
already type `uses = [PineDialogTrigger, PineDialogContent, ...]`
when they compose Pine compounds. The data is there; we're just
not consuming it for registration.

### 2.2 Phase 6.5 left the registry pluggable but lazy

Post-RFC-058 Phase 6.5 the runtime walker is gone, but
registration is still per-component runtime-side-effect: each
`#[component]` macro emits a `Foo::register()` method that mutates
the thread-local `TEMPLATES` map when called. Forgetting to call
it is a silent failure — case 1 of `adopted_dom_contract.rs` hit
this: `AdtChild::register()` was missing and the leaf component
silently failed to mount. Discovery via `querySelectorAll` over a
*lazily-populated* registry has the same shape as the walker's
attribute-scanning loop in miniature: invisible if you forget to
opt in.

### 2.3 The cluster algorithm needs a graph anyway

RFC 058 §5.10.1 commits to **route-boundary clustering** as
the v1 delivery model. That algorithm needs a complete component
dependency graph — exactly what `uses` already records, but
without the transitive-closure entry point. Defining the entry
point (`App::route::<C>`) and the closure rule unlocks the cluster
algorithm without inventing a second declaration mechanism.

### 2.4 RFC 051 was deferred for the right reason

RFC 051 wanted runtime registry safety (collision detection, boot
verification, error screens) without addressing *where the registry
comes from in the first place*. With this RFC, the registry comes
from the typed `uses` graph at compile time — collisions are
either ident clashes (Rust catches them) or duplicate tag-string
entries (RFC 049 already rejects). The runtime fail-fast story
RFC 051 wanted reduces to: assert at boot that every entry in the
generated registry has a matching `register_template` entry, which
is now compiler-guaranteed.

## 3. Non-goals

- **Not a replacement for the macro's `template_inline` parser.**
  Tag validation runs *after* the existing template AST extraction;
  this RFC adds a closure step, not a new parser.
- **Not a runtime API.** No new `register_with_uses(...)` function.
  Everything is compile-time-emitted.
- **Not a hot-reload protocol.** `uses` lists change at compile
  time; dev rebuilds re-emit the registry. Out-of-band hot-replace
  of a single component's `uses` is out of scope.
- **Not a styling/CSS dependency mechanism.** `uses` is for
  custom-element tag resolution only; CSS-side import graphs are
  unchanged.
- **Not a server-function dependency declaration.** RFC 056
  handles `#[server]` boundaries; `uses` is purely about template
  custom-element tags.

## 4. Design

### 4.1 `uses` becomes mandatory for templates that reference custom tags

Today `uses` is optional — omit it and slot-contract validation
silently skips. With this RFC, **a template that references any
custom tag (i.e. any tag containing `-`) must declare every such
tag via the consumer's `uses` list**. The macro fails compilation
otherwise:

```text
error: tag `<pine-dialog-content>` is not declared in this component's `uses` list
  --> src/pages/home.rs:14:5
   |
14 |     <pine-dialog-content>...</pine-dialog-content>
   |     ^^^^^^^^^^^^^^^^^^^^
   = help: add `uses = [PineDialogContent]` to the #[component(...)] attribute, or
           re-export via a bundle (e.g. `uses = [pine::Dialog]`).
```

Native HTML5 elements are exempt — the same allowlist
`#[component]` already uses to reject native-name struct idents
(RFC 001) governs which tags skip the check.

### 4.2 Bundle re-exports for compounds

Pine compounds (Dialog, Popover, Combobox, ContextMenu) each
expose 5-10 sub-components. Forcing every consumer to type the
full list defeats the API ergonomics. Solution: a marker type's
`#[component(extends = [...])]` declares a bundle of additional
tags that flatten into any `uses` list referencing the marker:

```rust
// In `pine::dialog` module:
#[component(
  template_inline = "<root></root>",
  role = "scope",
  extends = [PineDialogTrigger, PineDialogPortal, PineDialogContent,
             PineDialogTitle, PineDialogDescription, PineDialogClose],
)]
pub struct Dialog;
```

A consumer's `uses = [pine::Dialog]` resolves at macro time to:
`[Dialog, PineDialogTrigger, PineDialogPortal, PineDialogContent,
PineDialogTitle, PineDialogDescription, PineDialogClose]`. The
flattening is recursive (a bundle can `extends` other bundles).

`extends` is mutually exclusive with `template_inline`/`template`
having any compiled body — bundles are pure type-level markers,
so the macro emits a no-op `register()` for the marker itself
and skips template-plan emission.

### 4.3 Closure entry points: `App::route::<C>(...)`

The `App` builder API (RFC 002 / RFC 003) already names every
top-level component a route can mount. Each `App::route::<HomePage>(...)`
call is one entry point into the `uses` graph. The macro-generated
registry walk visits:

```
seeds   = { every C in App::route::<C>(...) calls in the consumer crate }
visited = {}
queue   = seeds.clone()

while let Some(c) = queue.pop():
    if visited.contains(c): continue
    visited.insert(c)
    for u in c.uses_table().entries():
        queue.push(u.type_path)

# `visited` is the complete registry the runtime needs to know about.
```

The walk runs once per `App` build via a `pocopine-codegen` step
that's invoked from `App::route` itself (not a `build.rs`). The
closure result is materialized as a `const REGISTRY: &[(&str,
fn() -> Box<dyn ComponentScope>)]` slice the runtime iterates
once at app startup.

### 4.4 What replaces `T::register()`

The macro keeps emitting per-component `register()` methods (no
breaking change for downstream code that calls them explicitly,
e.g. test fixtures). What changes is **app startup**: `App::run()`
walks the closure-derived `REGISTRY` slice and calls each entry's
`register()` exactly once before mounting any route. Manual
`T::register()` calls remain valid but become unnecessary.

Tests that mount components without an `App` context (e.g. the
adopted-DOM contract tests' `register_all()` helper) keep working
unchanged — the closure walk is an `App`-level convenience, not a
replacement for direct registration.

### 4.5 Adopted-DOM bridge interaction

The Phase 6.5 bridge contract is unchanged. The bridge already
queries `templates::registered_template_names()` for tag
discovery; with this RFC, that registry is fully populated at
startup from the closure walk, so the bridge sees every component
the app could possibly use. Unknown tags in user-authored runtime
HTML (not from a `#[component]` template) are still silently
ignored at runtime — the framework can't validate strings the
compiler never saw. **Dev mode addition**: a debug-build-only
`MutationObserver` warning when an unknown tag is inserted into
the DOM and never matched by the registry, so authors notice the
typo. Off in release.

### 4.6 Cluster-graph hookup (RFC 058 §5.10.1)

The closure walk in §4.3 produces exactly the graph RFC 058
§5.10.1 calls a "route cluster": components reachable from one
route. RFC 058 §5.10's "shell cluster" is the intersection of all
route closures. RFC 058 Phase 7 (always-on cluster decision)
becomes a pure re-projection of this RFC's data — no new
declaration, no new analysis pass.

### 4.7 Diagnostics

Compile-time errors the macro must produce, ranked by frequency
of expected hit:

1. **Unknown tag in template** — §4.1's example.
2. **Tag declared in `uses` but unused** — warning, not error
   (mirrors `unused_imports` in tone).
3. **Bundle cycle** — `A extends [B]; B extends [A]` is a hard
   error with the cycle path.
4. **Duplicate tag after flattening** — RFC 049 §4.7 already
   rejects this; this RFC inherits the rule.
5. **`uses` entry not annotated `#[component]`** — hard error
   with hint.

## 5. Migration

### 5.1 Phase 1 — opt-in strict mode

Add `#![pocopine_strict_uses]` crate attribute. Crates that opt in
get §4.1's hard errors; crates that don't get the existing
permissive behavior. Pine and the workspace examples flip the
attribute first.

### 5.2 Phase 2 — `App::run()` registry walk

`App::run()` starts walking the closure-derived registry in
addition to running explicit `T::register()` calls in user code.
Both paths populate the same registry; double-registration is a
no-op. Tests that work today keep working.

### 5.3 Phase 3 — strict by default

Crate attribute removed; §4.1 errors are unconditional. Workspace
audit deletes leftover `T::register()` calls from `App::run()`
sites. The `pocopine_core::templates::register_template` public
API stays (test fixtures still use it directly), but the
macro-emitted `App::run()` no longer needs explicit user calls.

### 5.4 Phase 4 — cluster algorithm hookup

RFC 058 Phase 7 lands. The cluster decision is published as build
metadata. No author code changes; the decision is informational
until Phase 8 (split-by-default delivery) flips the linker.

## 6. Testing requirements

The RFC is not implemented until tests cover:

- `uses` flattening through one bundle + two bundles deep;
- bundle cycle detection;
- transitive closure from a single-route `App` matches a hand-rolled
  registry list;
- transitive closure from a multi-route `App` produces the union;
- adopted-DOM bridge mounts a tag reachable through `uses`;
- adopted-DOM bridge silently ignores a tag not reachable through
  `uses` in release; warns in dev;
- compile-error messages in §4.7 each have a UI-test fixture
  (`tests/ui/uses/`).

## 7. Measurement requirements

Compare against the post-Phase-6.5 baseline:

- counter raw + gzip wasm size after Phase 3 (expected: small
  shrinkage from dead-code elimination of unreached components);
- website example wasm size (Pine + custom components);
- `cargo check` time delta from the closure walk;
- `cargo build --release` time delta;
- mount time for counter, website showcase, jsbench (expected:
  unchanged — the registry walk is pure startup, not per-mount).

## 8. Open questions

1. Does the closure walk live in `pocopine-macros` (per-`App::route`
   call) or in a `pocopine-codegen` step invoked from `App::run()`?
   The latter scales to multi-crate workspaces; the former is
   simpler.
2. Should `App::route::<C>()` also fail at compile time if `C`
   isn't `#[component]`? Today it's a trait bound on
   `Component`; this RFC could tighten it to a marker trait the
   macro implements (`RegisteredComponent`) so unregistered types
   can't slip through.
3. Bundle re-exports cross crate boundaries (Pine → user). Is the
   bundle's transitive `extends` resolved in Pine's macro
   expansion (eager) or in the consumer's expansion (lazy)? Eager
   is faster but couples Pine's bundle changes to consumer
   recompiles; lazy lets each consumer pull only what it
   referenced.
4. Should `extends` accept arbitrary type paths, or only direct
   `#[component]` types? Allowing nested bundles is convenient
   but complicates cycle detection.

## 9. Council questions

1. Is **§4.1's hard-error-on-unknown-tag** the right authoring
   contract, given Vue 3's `components: { Foo, Bar }` requires the
   same explicit declaration and React's JSX requires an `import`?
2. Does the council endorse **bundle re-exports** as the
   compound-component composition pattern, or should each consumer
   list the full child set?
3. Are **`App::route::<C>()` calls the right closure entry**, or
   should the entry set include `<pp-outlet>` placeholders, popover
   roots, and other dynamic mount points the runtime can reach into?
4. Should the closure-derived registry **disable** the public
   `pocopine_core::templates::register_template` API in release
   builds (defense-in-depth against forgotten call sites
   re-introducing silent no-mounts)?
