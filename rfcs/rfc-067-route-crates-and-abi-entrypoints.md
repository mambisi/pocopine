# RFC 067 — Route crates and ABI entry points

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-02 |
| **Builds on** | [RFC 066](./rfc-066-shared-runtime-route-abi.md) |
| **Related** | [RFC 059](./rfc-059-server-side-rendering-and-hydration.md), [RFC 065](./rfc-065-route-cluster-bundling.md) |

## 1. Summary

Pocopine split applications should use route crates as the default
ownership boundary for non-trivial routes. Each route crate exposes one
macro entry point:

```rust
pocopine::route! {
    id: "story",
    path: "/item/:id",
    component: StoryDetail,
}
```

The route crate is an authoring and dependency boundary. It is not a
miniature Pocopine application. Its compiled artifact must cross into
the shell through the shared route ABI from RFC 066: descriptors,
compiled plans, handler IDs, and explicit host calls.

## 2. Problem Statement

The split experiments proved two facts:

1. Source-level route ownership works. When the shell stops naming route
   component Rust types, route vtables and route template code leave the
   shell.
2. Separately linked route `cdylib` artifacts are the wrong runtime
   default. They duplicate wasm-bindgen glue and Pocopine runtime code,
   and shared-memory variants run into allocator, stack, table, data
   segment, and JS heap boundaries.

React can load a route as a JS module because all modules share one JS
runtime. Rust wasm `cdylib` outputs do not share that model. Each module
wants its own linked world unless the framework provides a real ABI.

## 3. Goals

- Make route crates the recommended structure for split-ready apps.
- Give every route a single explicit `pocopine::route!` entry point.
- Let route-only dependencies live outside shell and unrelated routes.
- Make strict split builds fail when a route artifact would cross the
  boundary using Rust pointers, vtables, or shared allocator state.
- Use the same route entry point for client descriptors, SSR entries,
  hydration manifests, and future server components.
- Keep `src/shell`, `src/shared`, and in-crate `src/routes` supported
  for small apps and migration.

## 4. Non-goals

- This RFC does not make independent route `cdylib` modules share shell
  memory.
- This RFC does not pass `ComponentVTable`, `Scope`, `Rc`, `Box`, or
  Rust function pointers across wasm instances.
- This RFC does not require browser-native WebAssembly Component Model
  support.
- This RFC does not finish SSR or hydration; it reserves the route
  descriptor surface they will consume.

## 5. Project Layout

The preferred split app layout is:

```text
my-app/
  Cargo.toml
  src/
    app.rs
    shell/
    shared/
  routes/
    home/
      Cargo.toml
      src/lib.rs
      src/Home.poco
    story/
      Cargo.toml
      src/lib.rs
      src/StoryDetail.poco
```

The shell crate may depend on shared crates. It must not depend on route
crates in strict split builds.

Route crates may depend on shared crates and route-only external crates.
They must not depend on shell internals.

## 6. Route Entry Macro

The route entry macro declares the stable route identity:

```rust
pocopine::route! {
    id: "story",
    path: "/item/:id",
    component: StoryDetail,
}
```

The macro emits a descriptor constant with:

- route ABI version,
- route id,
- path pattern,
- component tag,
- future SSR/hydration capability flags.

The first implementation only emits the type surface and marker export.
Later phases lower the component template and handlers into RFC 066
artifacts.

## 7. Build Products

A route crate may produce one or more products:

```text
route_story.client.js        descriptor / compiled plan / loader glue
route_story.client.wasm      optional handler module, host-ABI only
route_story.ssr              server render entry
route_story.hydration.json   stable node ids and descriptor hashes
```

The client route artifact must not be a second Pocopine runtime. It must
not register Rust vtables into the shell.

## 8. SSR And Hydration

The route entry point is also the SSR boundary:

- the server selects a route by `id` and `path`,
- server rendering uses the same component/template descriptor,
- hydration uses route id plus descriptor hash,
- server components become explicit route-server handlers instead of
  hidden client imports.

This makes hydration ownership local to the route. The shell hydrates
the app shell once, then asks the matched route descriptor to hydrate
its outlet.

## 9. Strict Mode

`pocopine build --split --strict` must eventually enforce:

- shell does not depend on route crates,
- route crates do not depend on shell internals,
- route artifacts do not export `ComponentVTable` pointers,
- route artifacts do not import shell memory unless using an approved
  host-ABI handler module,
- unsupported route features fail with a diagnostic that explains which
  feature forced fallback.

Example diagnostic:

```text
error: route crate `story` cannot be emitted as an ABI artifact
  reason: handler `render_markdown` captures a non-serializable Rust value
  help: move this handler behind a host ABI call, or mark the route as
        wasm-handler once route handler modules are implemented
```

## 10. CLI

The CLI should make the layout easy:

```bash
pocopine route crate story --pattern "/item/:id" --component StoryDetail
```

This creates a route crate with:

- `Cargo.toml`,
- `src/lib.rs`,
- a `.poco` template,
- a `pocopine::route!` entry point,
- comments explaining the ABI boundary.

The existing `pocopine route add` remains the lightweight in-crate
scaffold.

## 11. Phases

### Phase A — Entry Surface

- Add `pocopine::route!`.
- Add `RouteDescriptor` type surface.
- Add CLI route-crate scaffolding.
- No split build integration yet.

### Phase B — Descriptor Routes From Route Crates

- Build template-only route crates into JS descriptors.
- Verify route artifacts do not contain Pocopine runtime symbols.
- Support browser smoke for direct route loads and client navigation.

### Phase C — Compiled Plan ABI

- Lower text bindings, class/style, child mounts, simple model updates,
  and event descriptors into serializable route plans.

### Phase D — SSR/Hydration

- Emit SSR route entries and hydration manifests from the same route
  descriptor.
- Add descriptor hashes to route manifests.

### Phase E — Route-local Wasm Handlers

- Allow route-local Rust handler modules only through explicit host ABI
  imports.
- Do not share allocators, `Scope`, or Rust pointers across modules.

