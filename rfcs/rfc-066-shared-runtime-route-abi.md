# RFC 066 — Shared runtime route ABI

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-01 |
| **Supersedes** | RFC 065 Option B as originally described |
| **Related** | [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 062](./rfc-062-per-component-mount-specialization.md), [RFC 065](./rfc-065-route-cluster-bundling.md) |

## 1. Summary

RFC 065 Phase 2 proved route artifact generation, but the
size result was wrong: each route artifact is still a standalone
Rust/wasm-bindgen application and therefore duplicates the
pocopine runtime. This RFC replaces the old "fetch a route wasm
and call its Rust `register()` functions" model with a shared
runtime ABI.

The shell owns the runtime. Route artifacts are data/code modules
that describe components through a stable host-call interface.
They do not export Rust function pointers, Rust constructors, or
`ComponentVTable` addresses to the shell.

## 2. Problem Statement

The current component runtime is intentionally intra-wasm:

```rust
pub struct ComponentVTable {
    pub register: fn(),
    pub mount_template: Option<ComponentMountFn>,
    // ...
}

pub fn register_component_with_mount(
    canonical: &'static str,
    owner: &'static str,
    ctor: ComponentCtor,
    mount_template: Option<ComponentMountFn>,
)
```

This is valid inside one wasm instance. It is not a browser
module ABI. A separately instantiated route wasm cannot safely
hand the shell a Rust `fn()` pointer or a `Scope` constructor
from its own linear memory and expect the shell's Rust runtime
to call it as if both lived in the same module.

The measured failure mode is exactly what the ABI predicts:

```text
monolith HN wasm:      ~708 KiB raw
split shell wasm:      ~477 KiB raw
split home route wasm: ~486 KiB raw
```

`shell + home` is larger than monolith because both are complete
standalone wasm modules with duplicated runtime code.

## 3. Goals

- Make route artifacts thin enough that `shell + initial route`
  can beat the monolith for route-heavy apps.
- Keep the shell as the owner of DOM mounting, lifecycle,
  reactivity, registry state, cleanup, and router semantics.
- Let route artifacts provide component definitions without
  importing the full pocopine runtime.
- Preserve strict route ownership (`shell`, `routes`, `shared`)
  and named route artifacts from RFC 065.
- Establish a proof target that catches false splitting early.

## 4. Non-goals

- This RFC does not require browser-native wasm Component Model
  support.
- This RFC does not require inner route crates, though inner
  crates become more useful once the ABI exists.
- This RFC does not solve SSR/hydration delivery.
- This RFC does not make arbitrary Rust component state portable
  across wasm instances. State must cross the ABI explicitly.

## 5. Corrected Architecture

### 5.1 Two Products

Split builds produce two kinds of artifacts:

```text
shell/runtime artifact:
  - pocopine runtime
  - router
  - registry
  - shell components
  - host ABI implementation

route ABI artifact:
  - route component descriptors
  - static templates/styles
  - compiled binding plans or bytecode
  - route-local handlers expressed through ABI calls
```

Route artifacts are not miniature pocopine apps.

### 5.2 Host ABI

The shell exposes a small JS/wasm host object to each route
artifact. The first version should use JS as the portable bridge:

```js
const host = {
  registerComponent(desc) {},
  registerTemplate(tag, html) {},
  registerStyle(tag, css) {},
  mountComponent(tag, outlet, params) {},
  getProp(scope, path) {},
  setText(nodeId, value) {},
  addEvent(nodeId, event, handlerId) {},
  dispatch(handlerId, payload) {},
};
```

The exact names are internal. The important rule is that handles
are opaque IDs or JS objects, not Rust pointers.

### 5.3 Route Artifact Surface

A route artifact exports ordinary JS functions:

```js
export function register(host) {
  host.registerComponent({
    tag: "story-detail",
    template: "...",
    style: "...",
    plan: [...],
    handlers: [...]
  });
}

export function mount(host, outlet, path) {
  host.mountComponent("story-detail", outlet, paramsFrom(path));
}

export function unmount(host) {
  host.unmountRoute();
}
```

For Rust-generated route artifacts, wasm-bindgen may still
produce the JS wrapper, but the wasm body must not link the
full pocopine runtime.

### 5.4 Route Descriptor, Not VTable

The cross-artifact unit is a descriptor:

```rust
pub struct RouteComponentDescriptor {
    pub tag: &'static str,
    pub template: &'static str,
    pub style: Option<&'static str>,
    pub plan: &'static [AbiPlanOp],
    pub handlers: &'static [AbiHandler],
}
```

This is conceptually Rust, but the actual browser transport is
JSON/JS arrays or a compact binary section. The descriptor must
be serializable. It must not contain:

- Rust `fn` pointers
- `Rc<RefCell<T>>` constructors
- `Scope`
- `JsValue` references owned by another wasm instance
- pointers into a wasm module's linear memory after registration

### 5.5 Handlers

Handlers are the hard boundary. There are three allowed tiers:

1. **Template-only route components**: no Rust handler code in
   the route artifact. This is the first proof target.
2. **ABI bytecode handlers**: generated plans describe simple
   assignments, event payload reads, server-function dispatch,
   and model updates. The shell interpreter executes them.
3. **Route wasm handler module**: route-local Rust logic remains
   in wasm, but it talks to the shell through explicit imported
   host functions. It still must not link `pocopine-core`.

Tier 1 proves the delivery architecture. Tier 2 makes common UI
useful. Tier 3 is the escape hatch for complex route-local Rust.

## 6. Implementation Plan

### Phase A — ABI Proof With Template-only Routes

- Add a split mode that emits route JS descriptors for components
  with no handlers and no dynamic bindings.
- The shell registers descriptor components through a new runtime
  descriptor registry.
- Route artifacts are plain JS first, generated by the CLI/macro.
- Proof target: a `not_found` route artifact should be far smaller
  than the current `hn_route_not_found_bg.wasm` and should not
  contain `pocopine-core`, `wasm-bindgen` runtime, or the mount
  runtime.

### Phase B — Compiled Plan ABI

- Lower a subset of compiled template plans to serializable ABI
  operations.
- Shell executes those operations against its own scopes/proxies.
- Add support for text bindings, static props, class/style,
  event listener installation, and child component mounts.

### Phase C — Handler ABI

- Split handler code into:
  - shell-interpretable generated operations, or
  - route-local wasm functions importing host calls.
- Server functions are invoked through shell-owned transport
  handles so route modules do not pull the full app runtime unless
  they truly need it.

### Phase D — Inner Route Crates

- Route crates own route-only dependencies.
- The ABI remains the delivery boundary.
- External crates used only by `routes::story` are absent from
  `routes::home` and the shell unless explicitly shared.

## 7. Proof Gates

A change does not count as successful shared-runtime splitting
unless all gates pass:

- `shell + initial route` raw and gzip are lower than monolith
  for a route-heavy example.
- `strings route_artifact` does not show unrelated route tags.
- `strings thin_route_artifact` does not show `pocopine-core`
  runtime markers.
- Route artifacts use stable route IDs and content hashes before
  production caching is enabled.
- Browser smoke tests prove direct route load and client-side
  navigation.

## 8. Impact on RFC 065

RFC 065 remains valid for:

- strict layout
- route ownership
- named route artifacts
- loader/manifest mechanics
- inner route crate direction

RFC 065's old Phase 2 assumption that a fetched wasm route module
can register Rust `ComponentVTable`s into the shell is superseded
by this RFC.
