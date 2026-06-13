# RFC 059 — Server-Side Rendering and Hydration

| Field | Value |
|---|---|
| **Status** | Superseded by [RFC-099](./rfc-099-ssr-hydration.md) — the shipping SSR design is the simpler "stamp on the server, claim on the client" model. Retained as the reference for the 0.3+ deferred features this RFC researched (streaming, event replay, the mismatch-policy gradient) and for the comment-marker scheme. |
| **Author** | pocopine team |
| **Created** | 2026-04-26 |
| **Revised** | 2026-04-27 |
| **Related** | [RFC 058](./rfc-058-compiled-views-walker-removal.md), [issue #10](https://github.com/mambisi/pocopine/issues/10), [issue #13](https://github.com/mambisi/pocopine/issues/13) |
| **Supersedes** | RFC 058 §6 Phase 5 (Hydration), which deferred specifics to this RFC |

## 1. Summary

Add server-side rendering (SSR) and client-side hydration on top of
the RFC-058 compiled-views architecture. The compiler already
generates a per-component `mount(host)` function; this RFC adds a
peer `hydrate(host)` function and a server-side `render_to_string`
mode.

**Architectural commitments locked in by RFC 058 + Phase 6.5**:

- Reactivity stays Vue-3-style **proxy + effect** (NOT fine-grained
  signals). RFC 058 Phase 6.5 measured that signals would require a
  full architectural rewrite for ~25 KB gzip; the council has
  rejected that trade.
- Animations stay **default-on** (`animate::*` + `transition::*` in
  core, ~15 KB gzip). Hydration must coexist with `<Transition>`-
  style enter/leave without flicker.
- The runtime walker is gone for compiled apps (RFC 058 Phase 6.5);
  hydration must drive the same compiled-mount machinery, not a
  separate hydrate-via-walker path.
- Templates are compiled to per-tag plans (`StaticTemplatePlan`)
  with `child_mounts` / `for_plans` / `if_plans` / `teleport_plans`
  / `slot_outlets` / `interps` / `native_models`. Hydration must
  reuse these plans — no second hydration-only IR.

## 2. Motivation

Same as the original draft (TTI, SEO, slow connections), plus three
forces specific to pocopine:

1. **The compiled-views Phase 6.5 work makes hydration cheap.** The
   per-component plan already records "this `node_path` carries an
   `pp-on:click` listener for `expr_src`" — hydration is just
   "install the listener on the existing element" instead of
   "create the element + install the listener."
2. **Vue 3 + Solid hybrid alignment.** Pocopine's current
   architecture is closer to Vue 3 (proxy reactivity, runtime
   templates, transitions in core) than to Solid (signals) or Yew
   (vdom). The hydration design follows Vue 3 where Vue's choice
   maps onto our model, and Solid where Vue's model doesn't (e.g.
   event replay, which Vue doesn't ship).
3. **Server-functions + stores need a transfer protocol.** RFC
   056's server functions and the `#[store]` macro both need a way
   to ship initial state to the hydrating client. Without an SSR
   contract, server-rendered pages today silently re-fetch
   everything on hydration.

## 3. Goals

- `render_to_string<C: Component>(...)` in `pocopine-server`,
  framework-agnostic; returns the rendered HTML body and a side
  channel for state to transfer.
- `#[component]` macro generates a `hydrate(host, scope)` function
  alongside `mount(host, scope)`. Both consume the same
  `StaticTemplatePlan`; only the per-entry installer differs.
- Non-destructive client hydration: walks the existing DOM via
  `firstChild`/`nextSibling`, attaches listeners + effects without
  recreating nodes.
- A documented mismatch policy (warn + best-effort fix in
  development; warn-once + leave attrs alone in production), with a
  per-element `pp-allow-mismatch` opt-out.
- A `<Transition>`-during-hydration story that doesn't flicker on
  first paint (lift Vue 3's inert-`<template>` `appear` trick).
- Adopt Solid's pre-hydration **event replay** — a tiny inline
  capture script that queues clicks/inputs received before the wasm
  bundle finishes downloading; the runtime drains the queue after
  hydrate completes.
- Per-server-function / per-`use_prepared_state` state serialization
  rather than one global `__POCOPINE_STATE__` blob (Yew's model).
- Streaming SSR for `<Suspense>` boundaries (Solid's
  `renderToStream` + `<template id="pl-N">` placeholder protocol).

## 4. Non-goals

- A full server framework. `render_to_string` is a pure function
  that plugs into Axum / Actix / Cloudflare Workers / etc.
- Islands / partial hydration in v1. Full-page hydration first; the
  RFC-058 Phase 8 cluster work is the right granularity for islands
  later.
- Replacing the SPA router. The SSR path renders the initial route;
  subsequent navigations stay client-side.
- A custom serialization format. We use `serde-json` for the wire,
  but each `use_prepared_state` site picks its own
  `serde::{Serialize, Deserialize}` types (see §5.5).

## 5. Proposal

### 5.1 Server-side rendering — `render_to_string`

```rust
// pocopine-server crate
pub fn render_to_string<C: Component + Default>() -> RenderedPage {
    /* ... */
}

pub struct RenderedPage {
    /// Rendered HTML body (no `<html>` / `<head>` wrapping —
    /// the caller composes the full document).
    pub body: String,
    /// `<script type="application/json" data-pp-state-id="...">`
    /// fragments the caller injects into `<body>` after `body`.
    /// One fragment per `use_prepared_state` / `register_store`
    /// site that emitted a value.
    pub state_fragments: Vec<String>,
    /// The optional pre-hydration event-replay script
    /// (see §5.6). Caller injects into `<head>` if event-replay
    /// is enabled.
    pub event_replay_script: Option<String>,
}
```

Implementation runs the compiler's emitted `mount` against a
**string backend** instead of `web_sys`. The trick that makes this
small: the per-component `StaticTemplatePlan` already records every
binding as `(node_path, BindingKind, expr_src)`. The string backend
walks the cleaned HTML once, stamps interpolated text + attribute
values from the plan, and emits comment markers at every dynamic
boundary (§5.4).

#### 5.1.1 Reactivity is disabled during SSR

Vue 3 (`setupComponent(instance, isSSR=true)` →
`setInSSRSetupState(true)`) disables reactive tracking server-side
because there's nothing to subscribe to. Pocopine adopts the same
pattern:

```rust
// pocopine-core
pub fn with_ssr_disabled<R>(f: impl FnOnce() -> R) -> R { /* ... */ }
```

`Scope::into_proxy()` checks the flag and returns a proxy whose
`get` doesn't `track()`. `effect()` becomes a no-op that runs the
closure once and discards the dep set. `Scope::find` etc. still
work — only the **subscription** half of reactivity is silenced.

This lets the same `mount` code path execute on server and client
without forking the macro emit. The string backend is a
`RenderTarget` trait with a `Dom` impl (today) and a `String` impl
(this RFC).

### 5.2 Compiler changes — `hydrate` peer of `mount`

The `#[component]` macro currently emits per-component:

```rust
pub fn __pp_mount_<COMPONENT>(host: &Element, scope: &Scope) { /* ... */ }
pub static __POC_TEMPLATE_PLAN_<COMPONENT>: StaticTemplatePlan = /* ... */;
```

This RFC adds:

```rust
pub fn __pp_hydrate_<COMPONENT>(host: &Element, scope: &Scope) -> HydrateResult { /* ... */ }
```

`hydrate` and `mount` share the `StaticTemplatePlan`. They differ
only in how they realize each plan entry:

| Plan entry | `mount` action | `hydrate` action |
|---|---|---|
| `StaticBinding` | install effect that writes the attr/text | install effect (server already wrote initial value) — first run is suppressed |
| `StaticListener` | `addEventListener` on the freshly-created element | `addEventListener` on the existing element looked up by `node_path` |
| `StaticChildMount` | mount child component into the host element | call child's `__pp_hydrate_*` against the existing child DOM |
| `StaticIfPlan` (truthy at SSR) | clone body + install effects | walk body via comment markers, install effects, no DOM changes |
| `StaticIfPlan` (falsy at SSR) | install controller, no body | install controller anchored at `<!--pp-if-->` placeholder |
| `StaticForPlan` | clone N rows | walk N row fragments delimited by `<!--pp-for-key:K-->` markers |
| `StaticTeleportPlan` | clone body + insert at target | locate teleported clone via `<!--pp-teleport-out:N-->` back-reference, attach |
| `StaticSlotOutlet` | materialize slot fragment | walk between `<!--pp-slot:NAME-->` markers, run installer |
| `StaticInterp` | parse segments, install per-dynamic effect | install per-dynamic effect; static segments adopt existing text |
| `StaticNativeModel` | install read-effect + write-listener | install read-effect (suppress first run) + write-listener |
| `StaticOpaqueDirective` | dispatch via `directives::lookup` | dispatch via `directives::lookup` (these directives are `<Transition>`-style and idempotent) |

The "first run is suppressed" pattern is critical (see §5.7): an
effect installed during hydration must not fire during the
hydration walk because that would mutate the DOM and risk overwriting
SSR state. Vue 3's solution is to NOT have a global flag; instead,
each render-effect entry checks `el && hydrateNode` and routes the
*first* tick into `hydrate` instead of `patch`
(`runtime-core/renderer.ts:1306-1342`). Pocopine adopts the same
shape: effects' first invocation during hydration is a no-op
(reactive subscription is established, but the side-effect is
skipped).

### 5.3 Client-side hydration entry — `App::hydrate()`

```rust
impl App {
    /// Like `run_compiled` but expects the body to already
    /// contain server-rendered HTML + state fragments.
    pub fn hydrate(self) { /* ... */ }
}
```

The runtime detects hydration mode by querying for any
`<script data-pp-state-id="...">` tag. If absent, falls through to
`run_compiled`'s normal mount.

Steps:

1. **Drain state fragments**: parse every
   `<script type="application/json" data-pp-state-id="...">`,
   index by id, hand them to the per-site loaders (§5.5).
2. **Locate root component tags** via the same querySelectorAll
   the Phase 6.5 `start_compiled` uses.
3. **For each root**, instantiate the scope (priming with
   transferred state if relevant), call `__pp_hydrate_*` instead
   of `__pp_mount_*`.
4. **Drain the event-replay queue** (§5.6) on the next microtask.

Once `hydrate` returns, the app is fully interactive. The wire
contract: from `<script id="__pp_state_index__">` parse-time to
the queue drain takes one synchronous frame (the hydrate walk is
allocation-free and DOM-mutation-free except for state-fragment
removal).

### 5.4 Comment marker scheme

Inspired by Solid's `<!--$-->` / `<!--/-->` pairs and Vue 3's
`<!--[-->` / `<!--]-->`. Pocopine uses a `pp-`-prefixed namespace
to avoid confusing devtools with framework markers:

| Boundary | Open marker | Close marker |
|---|---|---|
| Component fragment (multi-root) | `<!--pp-frag-->` | `<!--/pp-frag-->` |
| `pp-if` (truthy) | `<!--pp-if-->` | `<!--/pp-if-->` |
| `pp-if` (falsy) | `<!--pp-if-->` | (no close, just placeholder) |
| `pp-for` block | `<!--pp-for-->` | `<!--/pp-for-->` |
| `pp-for` keyed item | `<!--pp-for-key:KEY-->` | `<!--/pp-for-key-->` |
| `pp-teleport` source anchor | `<!--pp-teleport-from:N-->` | (none) |
| `pp-teleport` destination | `<!--pp-teleport-to:N-->` | `<!--/pp-teleport-to-->` |
| `<slot>` outlet | `<!--pp-slot:NAME-->` | `<!--/pp-slot-->` |
| `{{expr}}` text segment | `<!--pp-interp-->` | (no close, single text node) |
| Suspense pending | `<template id="pp-pl:N"></template>` | `<!--pp-pl:N-->` |

**Important**: every marker is at most ~25 chars; the gzip
overhead is negligible. The component-level `<!--pp-frag-->`
markers are emitted only for components whose root is a multi-
node fragment; single-root components (the common case) skip them.

The keyed `pp-for` marker carries the key string verbatim so the
client hydration can reuse SSR-computed `LoopScope` instances
without re-evaluating `pp-key`. Keys with special chars
(`-->`, etc.) are URL-encoded.

### 5.5 State serialization — per-prepared-site fragments

Yew's `use_prepared_state` (per-hook bincode + base64) and Vue's
Pinia (per-store JSON) share a key insight: **don't ship one global
state blob**. A global blob couples every store / server-fn / async
component to one serialization point and adds a "load order"
problem on the client.

Pocopine's mechanism:

```rust
// In a #[component] handler or async server function:
let user = use_prepared_state!("user", async {
    server_fn::fetch_user(id).await
}).await;
```

Server side, the macro emits:

```rust
// One <script> per use_prepared_state! site
RenderedPage::state_fragments.push(format!(
    r#"<script type="application/json" data-pp-state-id="{}">{}</script>"#,
    SITE_ID,
    serde_json::to_string(&value).unwrap()
));
```

Client side, `use_prepared_state!` checks the index built during
`App::hydrate`'s step 1 and either takes the SSR value (decoding
via `serde_json::from_str::<T>`) or runs the closure (the CSR
fallback / SPA navigation case).

Each `#[store]` registers itself as a prepared-state site with the
store's struct name as the `SITE_ID`. Stores are
serialized server-side after all `serverPrefetch`-style hooks run.

**Cross-request pollution**. Vue 3's documented warning
(`https://vuejs.org/guide/scaling-up/ssr.html#cross-request-state-pollution`)
applies to pocopine too: module-level singletons shared between
requests leak. Pocopine adopts Pinia's pattern: `App::new()`
builds a per-request scope tree; stores live inside that tree, not
in module statics. Existing `pocopine_core::store::*` thread-locals
need to migrate to a per-request arena before SSR is enabled (§7,
implementation phase 0).

### 5.6 Pre-hydration event replay (Solid model)

Adopted verbatim from Solid (`dom-expressions/src/server.js:541-543`
and `client.js:308-327`). A small inline script runs in the page
`<head>` before any wasm loads:

```javascript
// Approx. 250 bytes minified; emitted by RenderedPage.event_replay_script
window._pp = window._pp || {events: [], done: false};
['click', 'input', 'submit', 'change'].forEach(t => {
  document.addEventListener(t, e => {
    if (window._pp.done) return;
    window._pp.events.push([e.type, e.target, e.timeStamp]);
  }, true);
});
```

After `hydrate` finishes, the runtime drains `_pp.events` in
microtask order, dispatching synthetic events on the now-bound
target chain. Events whose target is no longer in the DOM (the
hydrate completed, but the user clicked something the SSR didn't
include because of a `pp-if` mismatch) are silently dropped.

Event types are configurable per-page via
`render_to_string(opts)` so a server-rendered marketing page that
doesn't expect form submissions can ship a smaller capture script.

### 5.7 Mismatch policy

**Categories** (cribbed from Vue 3 `hydration.ts:964-979`):
TEXT, CHILDREN, CLASS, STYLE, ATTRIBUTE, NODE_TYPE, FRAGMENT_BOUNDS.

**Behaviour**:

| Category | Dev | Production |
|---|---|---|
| TEXT | `console.warn` with both values, overwrite to client value | overwrite to client value, no warn |
| CHILDREN | `console.warn`, mount missing / remove excess | mount missing / remove excess |
| CLASS, STYLE | `console.warn`, write client value | write client value |
| ATTRIBUTE | `console.warn`, write client value | **no-op** (Vue's choice — round-tripping every attr in production is too expensive) |
| NODE_TYPE | `console.error`, replace node | replace node |
| FRAGMENT_BOUNDS | `console.error`, abandon hydration of this subtree, full client mount | same as dev |

**Per-element opt-out**: `pp-allow-mismatch` attribute (Vue
`data-allow-mismatch` rebrand). Comma-separated category list:

```html
<!-- this whole subtree's mismatches are silenced -->
<div pp-allow-mismatch>{{server_only_value}}</div>

<!-- only attribute mismatches silenced -->
<input pp-allow-mismatch="attribute" :value="client_only">
```

Both `mount` and `hydrate` strip the attribute after consuming it
so it doesn't reach the live DOM.

### 5.8 `<Transition>` during hydration — the inert-template trick

This is the single non-obvious Vue trick worth copying verbatim
(`runtime-core/hydration.ts:391-552`). Without it, `<Transition>`
elements with `appear` flicker on first paint: the SSR rendered the
final state, the client adds `enter-from` classes, the browser
paints both states.

Vue's solution: the SSR compiler renders any `<Transition appear>`
child wrapped in `<template>...</template>`. The browser parses
`<template>` as inert `DocumentFragment` content — the inner DOM
exists but is never painted. Hydration calls `transition.beforeEnter`
on the inner content (applies `enter-from` classes), then
`replaceNode` swaps the `<template>` for the real element, then
queues `transition.enter(el)` post-flush. The browser sees the
element appear with `enter-from` already applied → animates in
naturally.

Pocopine's animation runtime (`crate::animate::*`) has the same
contract: `apply_preset(el, in_name, out_name)` stamps
`pp-transition:enter-start` etc. before the element enters the
live tree. The SSR macro detects components with `transition = "..."`
or any `pp-transition:*` attribute on the rendered root and wraps
the element in `<template data-pp-appear="N">`. The hydrate path
unwraps and triggers the enter.

For non-`appear` transitions (the default), enter is suppressed
on first hydrate exactly as Vue does — the element is just adopted
in its SSR-rendered final state.

### 5.9 Streaming SSR (`render_to_stream`)

Adopt Solid's shell-first model (`dom-expressions/src/server.js:75-302`):

1. Render the synchronous shell into the response stream, emitting
   `<template id="pp-pl:N"></template><fallback HTML><!--pp-pl:N-->`
   for each suspended `<Suspense>` boundary.
2. Hold the stream open while suspended futures resolve.
3. As each resolves, emit `<template id="pp-pl:N">...resolved
   HTML...</template>` followed by a tiny `<script>$pp_swap("N")</script>`
   that splices the resolved fragment in over the placeholder.

The runtime swap helper (`$pp_swap`) is ~120 bytes, inlined into
the bootstrap script.

`render_to_stream` returns `impl Stream<Item = Result<Bytes, Err>>`
so it composes naturally with Axum's `body::Body::from_stream`.

For v1, streaming is **opt-in** via
`render_to_stream_with(opts)`; the basic `render_to_string` blocks
until all pending futures resolve. Streaming has subtle
interactions with event replay (events dispatched on a
not-yet-streamed subtree need to land in the queue but not be
dropped), and we want the simple path proven first.

## 6. Lessons from prior art

Concrete findings from research into Solid, Vue 3, and Yew (see
issue #14 for the long-form research notes). Each numbered finding
maps to a design decision above.

### 6.1 Solid (`dom-expressions` runtime)

- **Marker scheme**: `<!--$-->` / `<!--/-->` for dynamic boundaries,
  `data-hk` attribute for hydration keys per element. Tracked via
  `gatherHydratable` (`querySelectorAll('*[data-hk]')` once at
  hydrate). **Pitfall**: `gatherHydratable` snapshots once; DOM
  mutations by third-party scripts (analytics, ad blockers) after
  that point are invisible. → Pocopine uses sequential
  `firstChild`/`nextSibling` walk anchored at component roots, not
  a global registry, so we don't take this snapshot at all.
- **Resource serialization** uses `seroval` (cyclic refs, Promises,
  Maps, FormData). One global `_$HY.r = {}`. → Pocopine prefers
  `serde-json` per site; we don't need cyclic refs in initial
  state, and a single global blob is the load-order hazard Vue
  documented.
- **Event replay** (`_$HY.events`, ~250 bytes) — adopted (§5.6).
- **`createUniqueId` is the dominant mismatch source**
  (solidjs/solid#2452): conditional rendering shifts every
  subsequent ID. → Pocopine should expose `pp_unique_id()` only
  for hydration-stable contexts, and document that it must be
  invariant between SSR and hydrate.
- **Browser HTML repair breaks hydration** (`<p>` in `<p>`,
  `<td>` outside `<tr>`, block elements in `<a>`) silently
  invalidates the registry. → Pocopine's macro should validate
  template HTML at compile time against an HTML5 nesting allowlist
  (RFC 050's `html5ever` is already in the workspace; this is one
  more validation pass).

### 6.2 Vue 3 (`runtime-core/hydration.ts`)

- **Per-render-effect hydration entry** (`renderer.ts:1306-1342`)
  — no global "hydrating" flag, instead the *first* tick of each
  render-effect routes into `hydrateNode` instead of `patch`.
  → Pocopine adopts this exactly: no global flag in `pocopine-core`,
  per-effect first-run suppression.
- **Reactivity disabled during SSR** (`setInSSRSetupState`).
  → Pocopine adopts via `with_ssr_disabled` (§5.1.1).
- **Mismatch policy** is the most thoughtfully-tuned of the three
  frameworks: warn + best-effort fix in dev, warn-once + leave
  attrs alone in production, opt-out via `data-allow-mismatch`.
  → Adopted as `pp-allow-mismatch` (§5.7).
- **`<Transition>` inert-`<template>` trick** for `appear`
  animations (`hydration.ts:391-552`). → Adopted (§5.8). This
  closes the single biggest "looks janky on first paint" hazard
  pocopine inherits from shipping animations in core.
- **Per-request store factory** is the answer to cross-request
  state pollution. → Documented as a pre-SSR migration in §7
  Phase 0.
- **`compiler-ssr` is a separate compiler** (no `hoistStatic`, no
  `cacheHandlers`, emits `_push(\`<div…\`)` template-literal
  builders). → Pocopine's macro emit reuses the same
  `StaticTemplatePlan`; the divergence is the per-entry installer
  (string buffer vs `web_sys` calls), not the plan shape.
- **Pinia's `serverPrefetch` lifecycle + `devalue` for state
  serialization** — pocopine uses `serde-json` instead since we're
  not crossing a JSON-string boundary inside JS (server emits the
  value directly into the script tag).

### 6.3 Yew

- **`Renderer::hydrate` walks via `firstChild`/`nextSibling`**
  on a per-component-tree `Fragment`, asserting the DOM matches
  the VDOM. → Pocopine adopts the walk pattern but trades the
  asserts for the Vue mismatch-policy gradient (panic is too
  aggressive for a framework that ships to user devices).
- **`use_prepared_state` per-hook serialization** (bincode +
  base64 in script tag). → Pocopine adopts the per-site model
  (§5.5) but uses JSON for inspectability — bincode is opaque,
  which makes hydration debugging harder for a marginal size win.
- **Yew panics on mismatch** — closed issues #2664, #2596, #2623,
  #4002, #3913 all stem from this. The Suspense slot-ownership
  bugs (`DomSlot` chain poisoning) in particular are a class
  pocopine should design out by anchoring slots at explicit
  comment markers rather than implicit "next sibling" relationships.
- **No event replay**, no streamed Suspense placeholders, no
  islands. The cumulative effect is that Yew apps with large wasm
  bundles are visually present but inert until hydration finishes
  — issue #3619 ("SSR + hydration results in a stalled request").
  → Pocopine ships event replay in v1 (§5.6).

## 7. Pitfalls catalog

Cross-referenced from the three research reports. Each entry is a
documented bug or footgun in one or more of Solid / Vue / Yew that
pocopine must explicitly handle or document.

| # | Pitfall | Prior art | Pocopine response |
|---|---|---|---|
| P1 | `Math.random` / `Date.now` in render → text mismatch | Vue (docs), Solid (silent) | Documented in §5.7 mismatch categories. `pp-allow-mismatch="text"` for unavoidable cases. |
| P2 | Date/time formatting differences (server vs client locale/timezone) | Vue (docs), Solid (silent) | Same as P1. Recommend `chrono` with explicit timezones in component handlers. |
| P3 | `localStorage` / `sessionStorage` reads in setup | Vue (community) | `with_ssr_disabled` skips effects, but `setup` still runs server-side. Document: gate browser-only reads behind `if cfg!(target_arch = "wasm32")` or move into `on_mount`. |
| P4 | Browser HTML repair (`<p>` in `<p>`, `<td>` outside `<tr>`) breaks marker walk | Solid (#2400, #2274), Yew (#2684) | Compile-time HTML nesting validator (`html5ever`-backed). |
| P5 | Adjacent text-node coalescing | Solid (`<!--!$-->` separator), Yew (BText explicit handling) | Pocopine emits `<!--pp-interp-->` between adjacent dynamic text segments to keep the browser from coalescing. |
| P6 | Browser-extension DOM injection (analytics, ad-blockers, theme toggles) | Solid (snapshot-once registry breaks), Vue (warn) | Sequential walk + per-element mismatch tolerance handles this case; `pp-allow-mismatch` on app root for paranoia. |
| P7 | Form-field state typed by user before hydrate gets stomped | Yew (always re-applies attrs), Solid (`assignProp` short-circuits during hydrate) | Per `directives::model::install_native` (RFC 058 Phase 6.5), suppress the **first** read-side effect during hydrate so user-typed values aren't overwritten. |
| P8 | `<Transition>` enter classes apply post-paint → flicker | Vue (inert-`<template>` trick) | §5.8 adopts the trick. |
| P9 | `createUniqueId` / scope-id-based stable refs shifting between SSR and hydrate | Solid (#2452) | `pp_unique_id()` documented as hydration-unsafe; provide `pp_stable_id(seed)` that's deterministic per call site + render index. |
| P10 | Suspense boundary slot-ownership races | Yew (#4002, #3913) | Slots anchor at explicit `<!--pp-slot:NAME-->` markers — slot ownership is positional, not next-sibling-implicit. |
| P11 | Cross-request state pollution (module singletons) | Vue (docs) | §7 Phase 0 migrates `store::*` thread-locals into per-request arena. |
| P12 | HMR + SSR registry desync | Solid (#2219) | Document as v1 limitation; HMR mode falls back to client mount. |
| P13 | Effects firing mid-hydration mutate the DOM | Vue (per-effect first-run gate) | Each effect installed during hydrate has its first run suppressed — subscription is established, but the side effect doesn't fire until the next reactive trigger. |
| P14 | `web_sys` calls in component handlers crash SSR | Yew (universal pitfall in Rust/wasm SSR) | `pocopine-server`'s `RenderTarget::String` impl panics on any `web_sys::*` access; the panic message points at the offending call site. Encourage moving DOM access into `on_mount`. |
| P15 | Wasm bundle download latency leaves page inert | Yew (#3619) | §5.6 event replay buffers user input across the gap. |

## 8. Implementation phases

### Phase 0 — Pre-SSR migration (must land before Phase 1)

1. Migrate `pocopine_core::store::*` thread-locals to a per-`App`
   arena so SSR can run multiple `render_to_string` calls in
   parallel without cross-pollution. Touch surface: `crates/pocopine-core/src/store.rs`,
   `crates/pocopine-macros/src/lib.rs::store!` macro emit.
2. Add `with_ssr_disabled` scope (§5.1.1). Gate
   `Scope::into_proxy`, `effect`, `effect_with` on the flag.
3. Add `RenderTarget` trait (`Dom` + `String` impls) — refactor
   `walker::mount_component` + `apply_static_plan` to write through
   the trait instead of calling `web_sys` directly. Behaviour
   preserved for the `Dom` impl; `String` impl panics on every
   method until Phase 2.

### Phase 1 — Client-side hydration (no server yet)

1. `#[component]` macro emits `__pp_hydrate_*` alongside `__pp_mount_*`.
2. `App::hydrate()` entry; state-fragment scanner; per-component
   hydrate dispatch.
3. Each plan entry's hydrate installer (per the table in §5.2).
4. Comment marker emission in the macro — both `mount` and
   `hydrate` walk through the same marker positions, so emission
   lives in the cleaned-HTML serializer.
5. Mismatch policy + `pp-allow-mismatch` attribute.
6. **Testing**: hand-write static HTML files mimicking the SSR
   output (markers + state script), call `App::hydrate()`, assert
   listeners fire + reactive updates land. No server needed.

Acceptance: a Counter-shaped component hand-hydrates to fully
interactive state, with `bind_call_count == 0` (Phase 6.5 metric)
verifying nothing leaks back to the legacy walker.

### Phase 2 — Server-side rendering

1. `render_to_string<C>()` — drives the Phase 0 `RenderTarget::String`
   impl through every plan entry.
2. State serialization — `use_prepared_state!` macro,
   `#[store]` per-request hooks, fragment emission.
3. Event-replay script generation.
4. Streaming hooks (placeholder emission for `<Suspense>`; the
   resolution-and-flush half ships in Phase 4).

Acceptance: `cargo run -p hn` (or any example with a server)
returns SSR HTML for the initial route; the page becomes
interactive on hydrate without reissuing the per-route fetch.

### Phase 3 — `<Transition>` integration

1. SSR macro detects components with `transition = "..."` /
   `pp-transition:*` and wraps the SSR root in
   `<template data-pp-appear="N">`.
2. Hydrate path unwraps + invokes `animate::apply_preset` +
   `transition::enter` post-flush.
3. Documentation + a Pine showcase regression test for every
   compound that uses `appear`.

Acceptance: the Pine `<Dialog>` / `<Popover>` showcase pages
render server-side and animate in on hydrate without flicker.

### Phase 4 — Streaming (`render_to_stream`)

1. Build the resolution pipeline for `<Suspense>` boundaries.
2. `<template id="pp-pl:N">` + `$pp_swap("N")` runtime helper.
3. Update event-replay queue to handle targets that arrive
   post-shell.
4. Axum example end-to-end.

Acceptance: a deliberately-slow server-fn renders its placeholder
in the first chunk, the resolved HTML in a later chunk, the user
sees both transitions cleanly.

### Phase 5 — Cluster-aware shell rendering (depends on RFC 058 Phase 8)

When RFC 058 Phase 8 (cluster split delivery) ships, the SSR
output emits the shell-cluster manifest and the per-route cluster
loader runs as the route resolves. v1 ignores cluster boundaries
and ships one wasm.

## 9. Testing requirements

The RFC is not implemented until tests cover:

- Pure hydrate (no SSR) for every directive in `StaticTemplatePlan`:
  `pp-text`, `pp-html`, `pp-bind:*`, `pp-on:*`, `pp-show`,
  `pp-init`, `pp-ref`, `pp-if`, `pp-for`, `pp-teleport`, `pp-model`,
  `<slot>`, `{{interp}}`.
- Mismatch policy: every category triggers the documented
  warn/recover behaviour.
- `pp-allow-mismatch` (whole-element + per-category list) silences
  warnings + skips per-category recovery.
- Event replay: capture script + drain ordering, with at least
  click + input + submit verified end-to-end.
- `with_ssr_disabled`: effects installed within the scope don't
  fire; reactive proxies still get/set without tracking.
- Per-request store isolation: two concurrent `render_to_string`
  calls don't see each other's writes.
- `<Transition>` `appear` doesn't flicker on first hydrate
  (visual regression: pre-paint enter-from class is set).
- Streaming: `render_to_stream` interleaves placeholder + resolved
  + swap-script in the right order.
- Per-pitfall (P1–P15): a regression test per documented
  pitfall confirming the recommended response holds.

## 10. Measurement requirements

Report separately for the SSR + hydrate path:

- TTFB for the hn / website examples (cold + warm).
- TTI: time from server response received to event-replay queue
  fully drained.
- Bundle size of the event-replay script (target ≤ 300 bytes
  inline).
- SSR throughput (renders/sec) for hn home + a Pine showcase
  page.
- Hydration cost: ms from `App::hydrate()` start to first
  user-event-replay drain, for hn (small page) and a 500-row
  table (large page).

Compare against the matching CSR-only `App::run_compiled()` numbers
from RFC 058 Phase 6.5 to quantify what hydration buys vs costs.

## 11. Open questions

1. **Async server functions and reactive subscriptions**.
   `use_prepared_state!` is sync-only as drafted. RFC 056 server
   functions are async. We need a clean rule for which futures
   block the SSR shell vs which suspend (Solid's
   `deferStream: true` is the prior art).
2. **Hydration vs RFC 058 Phase 8 clusters**. When the shell
   cluster hydrates, the route cluster may not have downloaded
   yet. Does hydration block on the route cluster, or do we
   render a placeholder + hydrate that subtree later? Solid /
   Vue don't have a clean answer because they don't split-by-
   default.
3. **Devtools support**. Solid's hydration integrates with the
   devtools panel (showing per-component hydrate timings). RFC
   059 v1 doesn't budget for this; deferred to a follow-up.
4. **Server-side router**. The pocopine SPA router lives entirely
   client-side; the SSR path picks the initial route via the URL
   passed to `render_to_string`. Should the router move to a
   shared crate so server + client see the same matcher?

## 12. Council questions

1. Is the `pp-`-prefixed comment-marker namespace OK, or should
   we adopt the shorter Solid convention (`<!--$-->` /
   `<!--/-->`) and accept the devtools confusion cost for the
   ~10 KB of body savings on large pages?
2. Are we comfortable with **panic on `web_sys` access during
   SSR** (Yew's de-facto behaviour, though more graceful) as the
   default? The alternative is a `Mock<Element>` that absorbs
   calls silently — but that masks real bugs.
3. Do we ship event replay (§5.6) in v1, or defer to v2 with the
   simpler "page is inert during the wasm-load gap" UX?
4. Is `serde-json` the right wire format for state, or do we
   need binary encoding (Yew's bincode + base64) for large
   payloads? Inspectability vs size.
5. Should `pp-allow-mismatch` accept a Rust expression for
   conditional opt-out, or stay as the static comma-list Vue
   uses? Conditional opt-out enables "don't warn for these specific
   reactive states" but adds parser complexity.
