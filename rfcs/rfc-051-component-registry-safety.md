# RFC 051 — Component registry safety: aliases, prefixes, boot verification

| Field | Value |
|---|---|
| **Status** | Deferred to [RFC 056](./rfc-056-component-interaction-safety-batch.md) |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 045](./rfc-045-single-root-templates.md), [RFC 049](./rfc-049-typed-slot-contracts.md), [RFC 050](./rfc-050-html5ever-compile-time-parser.md) |

## 1. Summary

The runtime component registry stops being "last write wins."
Registration becomes collision-aware and boot verification
becomes explicit:

- components register under a canonical tag,
- optional aliases and prefix helpers are first-class APIs,
- duplicate canonical tags or aliases are recorded as registry
  errors instead of silently overwriting earlier entries,
- app boot checks the registry and surfaces a **permanent
  client-side error screen** when registration is invalid,
- test helpers let CI fail before production if a registry
  collision slips in.

This RFC does **not** replace RFC 049's local `uses = [...]`
model for compile-time slot checks. It strengthens the runtime
registry so app-wide component discovery is deterministic and
fail-fast.

## 2. Motivation

Today `register_component(name, ctor)` is a plain
`HashMap::insert` into a thread-local map. If two components
register the same tag, the later registration silently
replaces the former. The app boots, but the wrong component
instantiates for that tag and the author gets a runtime bug
far away from the registration site.

That is the worst kind of failure:

- no compile error,
- no boot error,
- no warning in devtools,
- last-call-wins depending on registration order.

At the same time, pocopine is moving toward richer compile-time
checks:

- RFC 045 validates template shape,
- RFC 049 validates slot-child contracts using a local `uses`
  registry,
- future work will likely warn or error on unknown custom tags.

Those checks work better if the runtime registry itself has a
coherent public API and a fail-fast story.

## 3. Non-goals

* **Not auto-discovery via linker sections.** `wasm32-unknown-unknown`
  still has no portable `inventory` / `linkme` story we want to
  rely on for core runtime boot.
* **Not replacing RFC 049's local `uses`.** Consumer-local
  compile-time registries stay local; this RFC is about runtime
  registration and boot safety.
* **Not making aliases visible to RFC 049 automatically.**
  Compile-time checks still use syntax the macro can see.
* **Not silently namespacing conflicts away.** Prefix helpers
  improve ergonomics; they do not excuse ambiguous tags.

## 4. Design

### 4.1 Canonical registration and aliases

The registry grows explicit APIs:

```rust
pub fn register_component(name: &'static str, ctor: ComponentCtor);
pub fn register_component_as(
    alias: &'static str,
    canonical: &'static str,
    ctor: ComponentCtor,
);
pub fn register_component_prefixed(
    prefix: &'static str,
    short: &'static str,
    ctor: ComponentCtor,
);
```

Semantics:

- `register_component("pine-dialog", ctor)` registers the
  canonical tag.
- `register_component_as("dialog", "pine-dialog", ctor)`
  registers an alias that resolves to the same constructor.
- `register_component_prefixed("pine", "dialog", ctor)` is
  shorthand for canonical registration as `"pine-dialog"`.

The precise public naming can still be bikeshedded (`register_as`
vs `register_component_as`), but the shape matters:

- canonical tag,
- optional alias,
- explicit prefix helper.

### 4.2 Collision policy

The registry no longer overwrites silently.

Conflicts become recorded `RegistryError`s:

```rust
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

Rules:

- canonical tag vs canonical tag: error
- alias vs alias: error
- alias vs existing canonical: error
- canonical vs existing alias: error
- re-registering the exact same owner/tag pair: no-op

No "pick the last one" fallback exists.

### 4.3 Registry ownership metadata

To make collision errors readable, macro-generated registration
includes owner metadata. The registry stores:

```rust
pub struct RegisteredComponent {
    pub canonical: &'static str,
    pub owner: &'static str, // e.g. "pine::dialog::PineDialog"
    pub ctor: ComponentCtor,
}
```

The macro already knows the Rust type path at expansion time,
so threading a readable owner string through registration is
cheap and makes errors actionable.

### 4.4 Boot verification

`pocopine::run()` and `App::run()` verify the registry before the
first mount.

If registry errors exist:

- the app does **not** proceed with normal mount,
- a permanent client-side error surface is rendered,
- the error lists the conflicting tags and owners,
- the error is also logged to the console.

This is a deliberate "stop the world" behavior. Component-tag
ambiguity is not recoverable; trying to boot anyway would make
the app nondeterministic.

### 4.5 Permanent client-side error surface

The browser-visible failure should be obvious and sticky:

```text
pocopine failed to boot: invalid component registry

- duplicate component tag `pine-dialog`
  first registered by `pine::dialog::PineDialog`
  then registered by `my_app::dialog::Dialog`

- alias `dialog` conflicts with canonical component tag `dialog`
  ...
```

Requirements:

- fills the app root,
- persists until reload,
- visually distinct from normal app UI,
- works without any app component registration beyond core
  pocopine boot code.

This is not a devtools-only panel. It must be visible in a
production build if a bad registration makes it that far.

### 4.6 Test and CI helpers

To keep collisions out of production, pocopine adds test helpers:

```rust
pub fn registry_errors() -> Vec<RegistryError>;
pub fn assert_registry_clean();
pub fn registered_component_names() -> Vec<String>;
```

Recommended usage:

- app smoke tests call `assert_registry_clean()`,
- example apps call it in a browser-start test,
- CI can add one dedicated "registration sanity" test that just
  registers the whole app and asserts the registry is clean.

This does not replace boot-time verification. It complements it.

### 4.7 Interaction with RFC 049

RFC 049's `uses = [...]` remains a **local compile-time
registry**. It does not read from the runtime registry.

The split is intentional:

- RFC 049 answers: "what child component tags are visible to
  this consumer's slot-contract check?"
- RFC 051 answers: "when the app boots, is the global component
  registry internally consistent?"

They reinforce each other without sharing a single source of
truth.

## 5. Implementation

1. Replace the thread-local `HashMap<&'static str, ComponentCtor>`
   with a richer table that stores canonical entries, aliases,
   and accumulated `RegistryError`s.
2. Extend macro-generated `register()` to pass owner metadata.
3. Add alias/prefix registration helpers to `pocopine-core` and
   re-export them from `pocopine`.
4. Add boot verification to `run()` / `App::run()`.
5. Add a built-in DOM error renderer for invalid-registry boot
   failures.
6. Add tests for:
   - duplicate canonical tags,
   - duplicate aliases,
   - alias/canonical conflicts,
   - idempotent same-owner registration,
   - boot failure path,
   - registry-clean assertion helper.

## 6. Alternatives considered

* **Keep last-write-wins.** Rejected. Silent overwrite is too
  dangerous for a core registry.
* **Panic immediately on second registration call.** Better than
  silent overwrite, but weaker than recording all conflicts and
  surfacing them together at boot.
* **Compile-time global registry generation.** Useful for future
  unknown-tag checks, but orthogonal to runtime boot safety.
* **Linker-based auto-discovery (`inventory`, `linkme`).**
  Rejected for core wasm boot semantics.

## 7. Open questions

* Should aliases be instantiable everywhere the canonical tag is,
  or should aliases be dev-only compatibility shims?
* Should `register_component_prefixed` register only the
  prefixed canonical tag, or both canonical and short alias?
  V1 recommendation: canonical only. Aliases should be explicit.
* Should boot verification be exposed as a separate API
  (`verify_registry()`) for hosts embedding pocopine outside
  the normal `run()` path? Probably yes.
