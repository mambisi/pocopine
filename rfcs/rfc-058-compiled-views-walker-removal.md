# RFC 058 - Compiled views and walker removal

| Field | Value |
|---|---|
| **Status** | Draft - council review |
| **Author** | pocopine team |
| **Created** | 2026-04-26 |
| **Supersedes** | Draft direction of [RFC 057](./rfc-057-compile-time-template-plans.md) for the main optimization path |
| **Related** | [RFC 011](./rfc-011-scoped-slots.md), [RFC 050](./rfc-050-html5ever-compile-time-parser.md), [RFC 054](./rfc-054-compiled-pp-for-row-plans.md), [RFC 057](./rfc-057-compile-time-template-plans.md), [issue #10](https://github.com/mambisi/pocopine/issues/10) |

## 1. Summary

Compile `.poco` templates into generated Rust view code instead of
mounting raw HTML and asking the runtime walker to discover
directives from DOM attributes.

For first-party compiled `.poco` components, the normal mount path
becomes:

1. instantiate the component scope,
2. call a macro-generated view mount function,
3. explicitly mount child components,
4. pass parent-owned compiled slot fragments to those children,
5. install directives, refs, lifecycle hooks, and dynamic block
   controllers from generated code.

The runtime walker stops being the authority for compiled `.poco`
templates. It may remain temporarily as a legacy/adopted-DOM path, but
compiled component mounts must not depend on attribute scanning.

This RFC deliberately pivots beyond RFC 057. Static template plans are
a useful stepping stone, but they still preserve the walker as the
semantic fallback and struggle around component/slot ownership. The
new target is Vue-style compiled views: templates become executable
mount/hydrate code, and the runtime traverses known structure rather
than discovering meaning from `pp-*` attributes.

## 2. Motivation

The walker currently does too much:

- discovers every directive by scanning DOM attributes,
- mounts child components by noticing custom tags,
- captures and materializes slots,
- binds scopes to roots,
- defers `pp-init`,
- fires mount/ready lifecycle hooks,
- scans text interpolation,
- tracks effects/listeners for cleanup,
- observes dynamically inserted DOM.

That is flexible, but it makes core behavior implicit and expensive.
Every compiled component already passed through `#[component]`, and
the macro already has a parsed `TemplateAst` from RFC 050. We should
use that compile-time knowledge to generate the mount work directly.

The strongest reason is not just performance. It is ownership.

In a template like:

```html
<pine-dialog-root>
  <button @click="close">Close</button>
</pine-dialog-root>
```

the inner `<button>` is authored by the parent, but it is inserted
through the child component's `<slot>`. A static directive plan can
avoid planning the subtree, but then the walker remains load-bearing.
A compiled-view model can represent the ownership directly: the parent
generates a slot-fragment function, and the child invokes that fragment
when it reaches its `<slot>`.

## 3. Goals

- Remove the walker from the normal mount path for compiled `.poco`
  components.
- Generate Rust mount code for native elements, static attributes,
  refs, text interpolation, bindings, listeners, `pp-init`, and
  lifecycle ordering.
- Generate explicit child-component mount calls instead of discovering
  custom tags at runtime.
- Compile parent-authored slot content into parent-owned slot fragment
  functions.
- Generate controllers for `pp-if`, `pp-for`, and `pp-teleport`
  rather than falling back to subtree scanning.
- Preserve existing author-facing `.poco` syntax.
- Preserve `.poco` rebuild behavior through the existing
  `include_str!` dependency pin.
- Provide a walker-free hydration path for SSR owned by pocopine's
  compiler.

## 4. Non-goals

- Deleting every walker-related function in the first implementation
  phase. A legacy walker may remain for dynamic/adopted DOM until that
  surface is retired or feature-gated.
- State-preserving browser HMR. `pocopine dev` can keep rebuilding the
  wasm bundle on save; automatic browser reload and component-state
  HMR are separate work.
- Changing `.poco` syntax or requiring authors to opt into compiled
  mode.
- Replacing pocopine with a virtual-DOM architecture. Generated views
  still mount and patch DOM directly.
- Solving server-side rendering for large pages in this RFC. This RFC
  defines the hydration shape needed for walker-free SSR, but the
  full server-rendering pipeline can land separately.

## 5. Proposal

### 5.1 Generated view artifact

For each `#[component]`, the macro emits a compiled view module in
tokens. A physical `*.view.rs` debug artifact may be added later, but
the shipping path is macro-generated Rust so Cargo's existing macro
pipeline remains enough.

Conceptual shape:

```rust
#[doc(hidden)]
pub struct CompiledView {
    pub mount: fn(&web_sys::Element, ScopeId, SlotSet),
    pub hydrate: fn(&web_sys::Element, ScopeId, SlotSet),
}

#[doc(hidden)]
pub fn register_compiled_view(name: &'static str, view: &'static CompiledView);
```

The macro still emits:

```rust
const _: &str = include_str!(#template_path);
```

That line is a build-graph dependency pin. It is not the runtime source
of truth.

### 5.2 Mount pipeline

The compiled mount pipeline replaces:

```text
set_inner_html -> walk(root) -> discover directives
```

with:

```text
instantiate scope -> generated_mount(host, scope, slots)
```

Generated code owns the work the walker used to do:

```rust
fn mount_counter(host: &Element, scope_id: ScopeId, slots: SlotSet) {
    let root = create_element("div");
    bind_scope_to(&root, scope_id);

    let dec = create_element("button");
    set_static_text(&dec, "-");
    directives::on::install(&dec, scope_id, "click", &[], expr!("decrement()"));

    let value = create_element("span");
    directives::text::install(&value, scope_id, expr!("count"));

    let inc = create_element("button");
    set_static_text(&inc, "+");
    directives::on::install(&inc, scope_id, "click", &[], expr!("increment()"));

    append(&root, &dec);
    append(&root, &value);
    append(&root, &inc);
    append(host, &root);

    drain_deferred_init_post_order(scope_id);
    fire_mount_ready(scope_id, &root);
}
```

This example uses DOM builders. A first implementation may still stamp
clean HTML with `set_inner_html` and hold generated node handles by
path, but the committed semantic target is generated mount code, not
runtime directive discovery.

### 5.3 Native DOM codegen

For native HTML elements, the macro emits direct code for:

- element creation,
- static attributes,
- static text nodes,
- text interpolation segments,
- `pp-text`,
- `pp-html`,
- `pp-show`,
- native `pp-bind:<attr>` / `:<attr>`,
- `pp-on:<event>` / `@event`,
- `pp-ref`,
- deferred `pp-init`,
- transition attributes and animation hooks.

Directive modules expose cleanup-safe install helpers. The generated
code must reuse the same machinery as today's walker-backed directives:

- `with_current_el`,
- `track_effect_on`,
- `track_listener_on_with_opts`,
- `refs::register`,
- scope unmount cleanup,
- current event magic for listeners,
- current scope tracking.

Generated code is a caller of shared directive helpers, not a second
copy of directive semantics.

### 5.4 Child components

Custom component tags compile to explicit mount calls. The generated
code must preserve the order currently provided implicitly by
`mount_component`:

1. instantiate child scope,
2. set the RFC 027 parent context chain,
3. apply static props before the proxy is built,
4. run `on_setup`,
5. build or hydrate the child view,
6. apply parent-driven reactive prop bindings,
7. materialize child slots,
8. run post-order init/mount/ready scheduling.

Conceptual generated call:

```rust
mount_child_component(
    "pine-dialog-root",
    ChildMount {
        static_props: &[("open", expr!("dialog_open"))],
        dynamic_props: &[...],
        slots,
        parent_scope_id,
    },
);
```

The parent no longer relies on the walker to notice a custom tag. The
mount call is part of the parent's generated view.

### 5.5 Slot fragments

Slot content is parent-owned. This is the central contract for walker
removal.

For:

```html
<pine-dialog-root>
  <button @click="close">Close</button>
</pine-dialog-root>
```

the parent component emits a fragment function:

```rust
fn parent_default_slot(ctx: SlotMountCtx) {
    let button = create_element("button");
    set_static_text(&button, "Close");
    directives::on::install(
        &button,
        ctx.parent_scope_id,
        "click",
        &[],
        expr!("close"),
    );
    append(ctx.host, &button);
}
```

and passes it to the child:

```rust
mount_child_component(
    "pine-dialog-root",
    ChildMount {
        slots: SlotSet::new().default(parent_default_slot),
        parent_scope_id,
        ..ChildMount::default()
    },
);
```

When the child view reaches `<slot>`, it invokes the provided parent
fragment in the correct slot host. If no parent fragment exists, the
child invokes its compiled default slot fragment.

This replaces the old runtime flow:

```text
capture child nodes -> store by slot name -> materialize <slot> -> walk inserted DOM
```

with:

```text
compile parent fragments -> pass fragment functions -> invoke at slot site
```

### 5.6 Dynamic block controllers

The walker cannot be removed until dynamic block directives have
generated controllers.

`pp-if` compiles to a conditional controller:

- holds anchors,
- mounts branch content when truthy,
- releases branch effects/listeners/refs on unmount,
- drives enter/leave transitions.

`pp-for` compiles to a list controller:

- reuses RFC 054 row-plan learnings,
- owns keyed reconciliation,
- mounts rows through generated row fragment functions,
- tracks row cleanup directly.

`pp-teleport` compiles to a portal controller:

- resolves the target,
- mounts generated child fragments into that target,
- preserves context and cleanup ownership,
- restores or releases teleported nodes on unmount.

Until each controller exists, the compiler may keep a narrow legacy
fallback for that directive, but the fallback must be tracked as a
temporary migration state.

### 5.7 Lifecycle and post-order scheduling

Generated code must preserve the walker's observable ordering:

1. create/mount child DOM,
2. bind scopes,
3. register refs,
4. install effects and listeners,
5. mount child components,
6. materialize slots,
7. invoke `pp-init` after descendants are ready,
8. fire `on_mount` post-order,
9. schedule `on_ready` on the existing next-tick path.

The implementation should expose explicit runtime helpers for these
steps instead of copying private walker keys. For example:

```rust
defer_init_on(el, scope_id, expr);
fire_mount_post_order(el, scope_id);
release_compiled_subtree(el);
```

These helpers are transitional: they pull lifecycle semantics out of
the walker so generated views can call them directly.

### 5.8 Hydration

Walker-free SSR requires generated hydrate functions, not a directive
scanner.

Server render and client hydrate must share the same compiled template
shape. The server emits HTML plus any anchors needed for ambiguous
dynamic boundaries. The client runs generated `hydrate_*` code that
resolves existing DOM nodes by structure/anchor and attaches behavior:

```rust
fn hydrate_counter(root: &Element, scope_id: ScopeId, slots: SlotSet) {
    let dec = child(root, 0);
    directives::on::install(&dec, scope_id, "click", &[], expr!("decrement()"));

    let value = child(root, 1);
    directives::text::hydrate(&value, scope_id, expr!("count"));
}
```

Hydration traverses DOM to match generated structure. It does not scan
for `pp-*` attributes to discover behavior.

Arbitrary server/adopted DOM that was not produced by pocopine's
compiler may keep using a legacy walker while that surface exists.

### 5.9 Hot reload and rebuilds

This RFC does not require a new dev server.

The macro continues to emit `include_str!(#template_path)` as a
dependency pin, so Cargo rebuilds when `.poco` changes. `pocopine dev`
already watches `src/` and reruns `wasm-pack build`; generated view
tokens participate in that normal rebuild.

Browser auto-refresh and state-preserving HMR are future work. They
are not required for compiled views.

### 5.10 Split-by-default via WIT-bindgen contracts

**Architectural commitment from v1; delivery is gated on toolchain
maturity but the design is locked in now.** Authors opt OUT per
component, not in.

Two real walls show up at scale in any Rust/wasm framework that
ships everything inline — both observed in production Leptos
codebases the size of a real SaaS app:

1. **Bundle growth from generated view code.** Every `#[component]`
   adds a generated `mount_*` function (§5.2), and every reactive
   binding adds an install closure. A 200-component site with 50
   server functions ships every one of those into a single wasm
   module. wasm doesn't tree-shake across the module boundary the
   way ESM does — unused branches in the generated mount code stay
   in the binary, and once a server-function client stub is
   compiled in, it ships even if the current route never calls it.
2. **Per-export overhead.** Each `#[wasm_bindgen]` function and
   each `#[server]` function lands an import/export entry plus a
   small JS shim in the wasm-bindgen-generated glue. The wall isn't
   a hard browser cap (modern browsers accept very large function
   tables), it's the cumulative parse + start-up tax: a single
   multi-megabyte wasm with thousands of exports parses slower than
   several smaller modules loaded as needed.

Vue's release builds dodge both walls by code-splitting on route
boundaries via dynamic ES imports — and the dynamic imports are the
*default* in any non-trivial app, not a per-component opt-in. The
wasm equivalent is the **WebAssembly Component Model + WIT
interface contracts** ([wit-bindgen](https://github.com/bytecodealliance/wit-bindgen)):
each cluster of views becomes a separate component with its surface
defined in a `.wit` file, instantiated on demand via
`WebAssembly.instantiate`. The host shell stays small; the
per-route view code only ships when the route is visited.

#### 5.10.1 Default policy: route-boundary clustering

The compiler treats split as the **default** for compiled views.
The split-decision algorithm:

1. Start from each `App::route::<C>("...")` registration. The
   route's root component `C` and the transitive closure of
   components reachable from it (via template `<custom-tag>`
   children and `<slot>` accept types) form a **route cluster**.
2. Components reachable from **every** registered route — or from
   the always-on `App` shell directly — go into the **shell
   cluster**. These ship in the always-loaded core.
3. Each non-shell cluster compiles to its own wasm Component
   exporting the §5.10.2 view interface.
4. Pine primitives and other "infrastructure" components inherit
   the cluster of every consumer. In practice they end up in the
   shell because they're used everywhere; the algorithm doesn't
   special-case them.

Authors don't think about clustering. The compiler infers it from
the existing `App::route` graph the project already has. Adding a
route automatically creates a split point; removing one folds the
cluster back into the shell at next build.

**Opt-out:** `#[component(inline)]` forces a component into the
shell cluster regardless of where it's reachable from. Use case:
something route-scoped that's hot-pathed at first paint and would
hurt to load lazily (e.g. a route's loading skeleton). Authors
should rarely need this — the algorithm gets the right answer for
the same reason Vue's route splits don't need per-component
overrides.

When Component Model toolchain matures further, a future RFC may
add finer-grained controls (`#[component(cluster = "checkout")]`,
component-level prefetch hints, manual cluster boundaries). v1
of split delivery only commits to the route-boundary default and
the `inline` opt-out.

#### 5.10.2 The view interface

Each cluster exports the same versioned WIT world:

```wit
// crates/pocopine-runtime/runtime.wit
package pocopine:runtime

interface scope {
    type scope-id = u64
    /// Opaque handle — the host owns the registry; clusters borrow.
}

interface dom {
    resource element { /* host-implemented */ }
}

interface slots {
    type slot-set = list<slot-fragment>
    type slot-fragment = func(ctx: slot-mount-ctx)
}

interface view {
    use scope.{scope-id}
    use dom.{element}
    use slots.{slot-set}

    /// Fresh mount for client-side navigation.
    mount: func(host: borrow<element>, scope: scope-id, slots: slot-set)

    /// Attach to existing SSR DOM.
    hydrate: func(host: borrow<element>, scope: scope-id, slots: slot-set)

    /// Tear down. Used by `pp-if` / `pp-for` / route swap.
    release: func(host: borrow<element>)
}

world cluster {
    import pocopine:runtime/scope
    import pocopine:runtime/dom
    import pocopine:runtime/slots
    import pocopine:runtime/reactivity
    /// One export entry per `#[component]` in the cluster:
    export view as <component-name-1>
    export view as <component-name-2>
    /* ... */
}
```

Why WIT specifically over hand-rolled `WebAssembly.instantiate`
shims:

* **Single source of truth** — `wit-bindgen` derives the host
  loader and the guest skeleton from the same `.wit`, so the
  contract can't drift across versions of either side.
* **Versioned runtime ABI** — clusters target a runtime *version*,
  not whatever lib symbols happen to be in the host. A `pocopine`
  upgrade that breaks the cluster ABI is a visible WIT version
  bump.
* **Boundary marshaling for free** — Component Model interface
  types handle `borrow<element>` and the `slot-set` shape without
  a per-call serde round-trip, which a naive split would force.

#### 5.10.3 Loader

The shell carries a small async loader keyed by route:

```rust
// host side (in the always-loaded shell)
pub async fn mount_route_cluster(
    cluster_name: &str,
    component_name: &str,
    host: &Element,
    scope_id: ScopeId,
    slots: SlotSet,
) -> Result<(), ClusterLoadError> {
    let component = cluster_loader::fetch_or_cached(cluster_name).await?;
    let view = component.lookup_view(component_name)?;
    view.mount(host, scope_id, slots);
    Ok(())
}
```

Cluster fetch is cached after first load — re-entering a route
after the first visit is synchronous instantiation, no second
network round-trip.

#### 5.10.4 Toolchain dependency

The Component Model is **not** natively supported in any browser
yet. Until it is, clusters ship through
[`jco`](https://github.com/bytecodealliance/jco), which lowers
each component to ESM + a JS shim — the same shape `wasm-pack`
already produces, just per-cluster.

This RFC commits to split-by-default as the **architecture**.
Delivery sequencing:

* While the toolchain isn't ready, `pocopine build` still emits the
  cluster-decision metadata (every component knows which cluster
  it belongs to) but the linker emits a single inline wasm. No
  behavioural change vs the inline-only world; the data is forward-
  compatible.
* When `wit-bindgen`'s rust-runtime is stable enough and `jco`
  ships a Cargo-friendly transpile pipeline, `pocopine build`
  flips the linker's default to actually emit per-cluster
  components. No author code changes.
* Server-function clustering follows the same boundaries (each
  cluster's `#[server]` stubs ship with that cluster's component)
  in the same delivery flip.

The §5.1 `mount` / `hydrate` / `release` triple is designed to be
the projection of the WIT `view` interface from day 1. v1 of the
ABI uses opaque handles for `Element` and `ScopeId`, marshaling-
friendly `SlotSet` shape, no raw pointer types — so the inline
version of the artifact is byte-for-byte the same shape as the
eventual cross-Component version, just linked together rather
than fetched on demand.

#### 5.10.5 Bundle shape (post-delivery)

```
public/
├── pocopine-shell.wasm       ~ runtime + always-on shell cluster
├── pocopine-runtime.wit      ~ versioned ABI definition
├── clusters/
│   ├── checkout.wasm         ~ checkout route cluster
│   ├── dashboard.wasm        ~ dashboard route cluster
│   └── settings.wasm         ~ settings route cluster
└── server/
    ├── shell-fns.wasm        ~ always-on server-fn cluster
    └── checkout-fns.wasm     ~ checkout server-fn cluster
```

Routes load clusters on demand; cold start ships only the shell.
This is the wasm-native equivalent of Vue's route-level dynamic
imports — and like Vue, it's the *default*, not a per-component
choice.

## 6. Implementation phases

### Phase 1 - Runtime helper extraction

Extract walker-owned semantics into public hidden runtime helpers:

- scope/root binding,
- effect/listener tracking,
- `pp-init` deferral,
- mount/ready hook scheduling,
- subtree release,
- slot invocation scaffolding,
- child component mount entry points.

Acceptance condition: existing walker path calls the extracted helpers
and remains behaviorally unchanged.

### Phase 2 - Generated native views

Generate mount code for templates that contain only native HTML
elements and v1 native directives.

No child components, slots, `pp-if`, `pp-for`, or `pp-teleport` yet.
This phase proves code generation, hot rebuilds, cleanup, lifecycle
ordering, and bundle/performance measurement.

### Phase 3 - Compiled child components and slots

Generate explicit child component mount calls and parent-owned slot
fragment functions.

Acceptance condition: a component tree with default slots, named slots,
nested slots, fallthrough attributes, and parent-scope event handlers
mounts without walker discovery.

### Phase 4 - Generated dynamic controllers

Generate controllers for:

- `pp-if`,
- keyed and unkeyed `pp-for`,
- `pp-teleport`,
- transition integration.

Acceptance condition: website and Pine showcase components no longer
need walker discovery for first-party `.poco` templates.

### Phase 5 - Hydration

Add generated hydrate functions and SSR anchors for dynamic boundaries.

Acceptance condition: compiler-owned SSR output hydrates without
attribute scanning.

### Phase 6 - Walker quarantine

Move the walker behind a narrow legacy/adopted-DOM API or feature flag.
Compiled `.poco` mounts do not call it.

Acceptance condition: examples and Pine primitives run with compiled
views only. Any remaining walker use is explicit and documented.

### Phase 6.5 - Walker deletion + adopted-DOM bridge contract

Phase 6 ended with the runtime walker fully removed, not feature-gated.
What survives in `pocopine-core::walker` is a deliberately narrow
**adopted-DOM bridge** — three functions, one entry point, no
attribute-dispatch loop:

- [`start_compiled`](../crates/pocopine-core/src/walker.rs) — the only
  mount entry. Discovers registered custom-element tags in the host
  subtree and calls `mount_component` per tag. Pure compiled-mount
  driver; no `pp-*` scanning.
- [`mount_adopted_components`](../crates/pocopine-core/src/walker.rs) —
  recursive discovery used by the bridge: finds every registered
  custom tag inside a runtime-injected subtree (e.g. captured slot
  content, cloned `pp-for` row body) and mounts each.
- [`install_adopted_controllers`](../crates/pocopine-core/src/walker.rs) —
  finds `<template pp-for>` / `<template pp-if>` / `<template pp-teleport>`
  in adopted DOM and installs the matching controller. The three
  structural directives are the only ones the bridge knows.
- [`materialize_adopted_slot`](../crates/pocopine-core/src/walker.rs) —
  splices `slots::lookup`-captured slot content back when
  `slot_fragment::lookup` (the compile-time emitter path) misses,
  then runs `mount_adopted_components` over the spliced subtree.

The bridge supports three job classes the macro can never own
because they only exist at runtime:

| Allowed | Disallowed |
|---|---|
| Discover registered custom-component tags + mount them | Bind `pp-*` / `:prop` / `@event` on plain or custom-tag hosts |
| Install `<template pp-for>` / `<template pp-if>` / `<template pp-teleport>` controllers | Per-element `pp-text` / `pp-bind` / `pp-show` / `pp-init` / `pp-model` / `pp-html` on adopted DOM |
| Materialise runtime-captured slot content (`slots::lookup` consume path) | Anything that requires the deleted directive registry / `dispatch` / `parse_attr` |

**Authoring rule that falls out**: per-element `pp-*` / `:prop` /
`@event` directives only bind when the macro processes them at
compile time inside a `#[component]` template. Author-supplied
adopted DOM gets structural directives + custom-tag mount and
nothing else. Authors who need per-element directives on dynamic
content wrap that content in a `#[component]` (or the
`template_inline = "..."` macro arg, the test shorthand that lifts
a verbatim string through the same compiler path).

Contract is locked by
[`crates/pocopine/tests/adopted_dom_contract.rs`](../crates/pocopine/tests/adopted_dom_contract.rs):

1. Custom component tag inside runtime slot content mounts.
2. `<template pp-for>` inside runtime slot content materialises rows.
3. `pp-text` / `:prop` / `@event` on plain HTML inside runtime slot
   content does **not** bind — the literal attributes remain on the
   element as proof the bridge never processed them.
4. Same authoring shape inside `#[component(template_inline = ...)]`
   does bind — the macro-processed escape hatch.

A future regression that reintroduces a runtime directive
dispatch loop fails case 3.

Acceptance condition (achieved): no `pocopine-core` runtime call
parses `pp-*` attributes off arbitrary author DOM. The four
contract tests pass. Counter benchmark: 433KB raw / 179KB gzip
(down from 599KB / 219KB at the Phase 6 endpoint).

### Phase 7 - Cluster-decision algorithm (always-on)

Implement §5.10.1 in the macro / compiler: every `#[component]`
gets a cluster assignment derived from the `App::route::<C>(...)`
graph. The decision is recorded in build metadata even though
the linker still emits a single inline wasm. Adds `#[component(inline)]`
opt-out support.

This phase is **always on** — every project gets the metadata
regardless of toolchain readiness. It's the forward-compatibility
anchor for Phase 8.

Acceptance condition: a `cargo pocopine clusters` debug command
prints the cluster assignment for every component in an example
app; flipping a route registration changes the output predictably;
`#[component(inline)]` overrides land in the shell cluster.

### Phase 8 - WIT-bindgen split delivery (deferred on toolchain)

Flip the linker default: each non-shell cluster emits a separate
wasm Component satisfying the §5.10.2 view interface; the shell
ships the `cluster_loader` and registers a fetch handler per
cluster. No author code changes — the §5.1 ABI was designed for
this from day 1.

Gated on:

- `wit-bindgen` rust-runtime stable enough to embed in
  `pocopine-macros`' generated output,
- `jco` (or native Component Model browser support) producing
  drop-in ESM for the per-cluster components,
- a stable §5.1 `mount` / `hydrate` / `release` ABI in production
  (which §5.10.4 commits to making split-shape from day 1),
- benchmark evidence on a representative app: cold-start size
  drop, route-load latency under the inline baseline,
  cluster-cache hit rate on second visit.

Server-function clustering follows the same boundaries in the
same flip — each cluster's `#[server]` stubs ship with that
cluster's wasm.

Acceptance condition: the `examples/website` site (multi-route,
mixes Pine primitives with route-specific components) builds and
serves via per-cluster wasm; jsbench harness running in shell-only
mode still benchmarks at the same numbers as the inline build.

## 7. Testing requirements

The RFC is not implemented until tests cover:

- native directive parity (`pp-text`, interpolation, `pp-bind`,
  `pp-show`, `pp-html`, `pp-on`, `pp-ref`, `pp-init`),
- listener cleanup, including window/document/outside listeners,
- effect cleanup on dynamic unmount,
- child component mount ordering and parent prop writes,
- default and named slot fragments,
- nested slots and slot fallback content,
- `pp-if` mount/unmount with transitions,
- `pp-for` keyed reuse, reorder, clear, and row cleanup,
- `pp-teleport` mount/unmount cleanup,
- lifecycle ordering (`on_setup`, refs, `pp-init`, `on_mount`,
  `on_ready`),
- SSR hydrate parity for compiler-owned output,
- dev rebuild after editing a `.poco` file.

## 8. Measurement requirements

Report separately:

- final raw and gzip wasm size,
- generated view code size,
- removed raw template source bytes,
- mount time for counter, website showcase, and jsbench,
- `run(1000)`, `runLots(10000)`, append, update, clear, swap from
  the existing jsbench harness,
- compile time impact for `cargo check` and `wasm-pack build`.

Measurements must compare against a stable pre-RFC-058 baseline. While
RFC 057 remains unaccepted, do not use a debug/prototype static-plan
branch as the baseline.

## 9. Council questions

The council should decide:

1. Is the project willing to make generated views the primary template
   architecture, with the walker demoted to legacy/adopted DOM?
2. Should the first generated view implementation build DOM manually,
   or stamp clean HTML and install generated handles by node path as a
   transitional step?
3. Should non-compiler-owned SSR/adopted DOM remain supported, and if
   so should it live behind a `legacy-dom` feature? *(Resolved at
   Phase 6.5: a narrow adopted-DOM bridge stays in core — three
   functions covering custom-tag discovery, structural-controller
   install, runtime-captured slot replay. Per-element directive
   binding on adopted DOM was deleted along with the walker; the
   `legacy-dom` branch preserves the old surface as a snapshot.)*
4. Are compiled slot fragments the right public-internal ABI for
   parent-owned slot content?
5. Should RFC 057 be marked "Deferred to RFC 058" now, or remain as a
   fallback design until RFC 058 reaches Accepted?
6. Does the council endorse **split-by-default** as the v1
   architectural commitment (§5.10), with `#[component(inline)]` as
   the only opt-out for v1, even though full Component Model /
   WIT-bindgen delivery (Phase 8) ships later? The choice has
   knock-on consequences:

   * The §5.1 ABI must use opaque handles for `Element` /
     `ScopeId` and a marshaling-friendly `SlotSet` from day 1, so
     the inline artifact is byte-for-byte the same shape as the
     eventual cross-Component version (no migration when Phase 8
     lands).
   * The cluster-decision algorithm (§5.10.1) ships in Phase 7 and
     is always on, even when the linker still emits inline. Every
     project pays the metadata cost (small) and gains
     forward-compatibility automatically.
   * The alternative — "ship inline-only, revisit when toolchain
     matures" — keeps v1 simpler but locks in either an ABI
     redesign or a parallel split-shape ABI when delivery lands.

## 10. Migration notes

Author code does not change. `.poco` syntax stays the same.

Framework contributors should treat the walker as a compatibility
layer during the migration. New directive semantics should be written
as reusable install/controller helpers first, then called by both the
legacy walker and generated views until the walker is removed from the
compiled path.

