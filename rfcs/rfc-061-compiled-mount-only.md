# RFC 061 — Compiled-mount-only architecture

| Field | Value |
|---|---|
| **Status** | Draft (open questions resolved 2026-04-28) |
| **Author** | pocopine team |
| **Created** | 2026-04-28 |
| **Supersedes** | RFC 058 Phase 6.5 adopted-DOM bridge (deletion target) |
| **Related** | [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 059](./rfc-059-server-side-rendering-and-hydration.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 062](./rfc-062-per-component-mount-specialization.md) |
| **Depends on** | RFC 060 implemented (registry must be exhaustive at compile time before the bridge can go) |

## 1. Summary

Commit to **compiled-mount-only** as pocopine's v2 architectural rule:
every mount entry is a typed `#[component]` reachable from
`App::route::<C>(...)`. Delete the adopted-DOM bridge entirely
(`mount_adopted_components`, `install_adopted_controllers`,
`materialize_adopted_slot`, the runtime slot store, the
`querySelectorAll` discovery passes). Mount, hydrate, and reconcile
become **pure compiled-plan applications** — no DOM scans, no
attribute parsing, no string lookups, no runtime registry walks.

This is the architectural step from "Vue-3-style with a small
adopted-DOM escape hatch" to **Vue-3-vapor / Svelte / Solid
performance class, in a Rust-native shape**.

## 2. Motivation

### 2.1 The bridge is the last attribute-shaped runtime cost

Post-RFC-058 Phase 6.5 the walker is gone, but every app mount
still pays for:

- **`start_compiled` `querySelectorAll`** over the union of every
  registered tag (one DOM scan per app boot, plus per-subtree on
  `pp-for` body materialisation when `body_fn = None`).
- **`install_adopted_controllers` `querySelectorAll`** over every
  `<template>` in adopted subtrees (per slot replay, per body clone).
- **Runtime slot store HashMap** allocated and consulted on every
  component mount (`capture_slots` → `slots::put` → `slots::lookup`).
- **String-keyed registry lookups** (`templates_plan::template_plan_for`,
  `templates::template_for`, `is_registered`) on every mount.

These are bounded but they're shaped wrong: each one is a runtime
question with a compile-time answer. Vue 3 vapor and Svelte have
**zero** runtime tag discovery — the compiler emits direct calls.
Pocopine can match that.

### 2.2 Users are starting to benchmark us against Vue 3 + Svelte

The counter benchmark sits at 433KB raw / 179KB gzip. Vue 3 vapor
and Svelte ship counters in 6-12KB gzip; Solid is ~7KB. Pocopine's
gap is dominated by reactivity + animations (architecturally locked
by RFC 058 Phase 6.5 + RFC 059), but the bridge contributes
measurable bytes (~3-4KB gzip estimate, to be confirmed via twiggy)
*and* runtime cost (~50-200µs per mount on a mid-tier device,
dominated by `querySelectorAll`'s string-matching cost on the union
selector). Deleting it is one of the few remaining step-function
wins.

### 2.3 RFC 060 makes deletion safe

Without RFC 060, deleting the bridge silently breaks any app that
relies on raw HTML containing custom tags. With RFC 060, the
compiler **proves** every custom tag in any compiled template
resolves to a registered component, the closure walk **proves**
the registry is exhaustive, and the only thing that can go wrong
at runtime is a user typing custom-element markup directly into
`document.body.innerHTML` — which we explicitly deprecate.

### 2.4 The "rusty direction" — what changes structurally

Vue 3 vapor and Solid achieve their wins through generated render
functions: the compiler emits direct DOM-construction code, no
template parsing at runtime. Pocopine's compiled-plan model
(`StaticTemplatePlan` + `apply_static_plan`) is structurally
equivalent — a typed plan walked by a generic applier. Going
strict-compiled lets us specialize the applier per-plan-shape:

- **Const-eval the applier per template** — instead of one
  generic `apply_static_plan` consuming any plan, the macro emits
  a per-component `mount(host, scope)` function with the plan
  inlined and unrolled. No iteration, no bounds checks, no
  variant matching. This is what Solid does in JS; Rust + monomorphisation makes it free.
- **Trait-driven component composition** — `Component` becomes a
  trait with associated `MOUNT_FN: fn(...)`, `HYDRATE_FN: fn(...)`,
  `PLAN: &'static StaticTemplatePlan` items. Composing components
  is type-level; the registry is a `phf` perfect-hash table from
  tag string to function pointer, populated at build time by RFC
  060's closure walk.
- **Zero-cost slot fragments** — every slot that today might fall
  back to runtime capture is provably handled by a
  compile-time-emitted fragment closure. The runtime slot store
  goes away entirely.
- **Zero registry mutability after boot** — the registry is
  `&'static [(&'static str, &'static ComponentVTable)]`, sorted at
  build time, looked up by binary search or `phf`. No `RefCell`,
  no thread-local mutability, no boot-time `register()` calls
  from user code.

Each of these is a Rust-leverage move: the type system + monomorphisation + const eval do the work that Vue 3 vapor's codegen does in JS, with no runtime cost.

## 3. Non-goals

- **Not a reactivity rewrite.** The Vue-3-style proxy + effect
  system stays exactly as RFC 058 Phase 6.5 + RFC 059 committed.
  Signals are still rejected; this RFC's perf wins come from
  removing runtime DOM discovery, not from changing the reactive
  primitive.
- **Not deleting `template_inline`.** That was a test-shorthand
  affordance; it's still valid. The compiled plan it produces is
  the same shape as a `.poco`-file plan.
- **Not a new template syntax.** Authors keep writing the same
  `.poco` (or inline) templates.
- **Not a vDOM.** Pocopine remains a fine-grained DOM updater;
  this RFC just removes the parts that scan the DOM looking for
  work.
- **Not a hard kill of `set_inner_html`.** That's a browser
  primitive. Pocopine just stops *processing* custom-element
  markup the user injects via it. Plain HTML (no pp-* attrs, no
  registered custom tags) keeps working.

## 4. Design

### 4.1 The new mount contract

Two entries, both typed:

**Primary path** — `App::run()` discovers the single fixed root
element matched by the `[pp-app]` attribute selector and calls the
route's compiled `mount(host, scope)` function. The selector is
**not configurable** (resolved 2026-04-28): one canonical
convention, no per-app divergence, no two-pocopine-instances-on-a-
page footgun. Apps that need multiple roots use the secondary path
below.

**Secondary path** — `App::mount_subtree::<C>(target_element, props)
-> SubtreeHandle` lets tooling code mount a typed pocopine
component into an arbitrary DOM element pocopine doesn't otherwise
own. Returned handle has a symmetric `unmount()` for cleanup.
Resolved 2026-04-28: ship as a public API but framed primarily for
**devtools, test harnesses, Storybook-style component galleries,
and embedded widgets** — not as a primary user-facing pattern.
The default app shape is still `App::run()`.

The strict-compiled invariant is preserved on the secondary path
because `C: Component` is a typed parameter — the runtime knows
exactly which compiled plan to apply, no tag scanning, no
discovery. This is structurally different from the deleted bridge,
which worked off attribute scanning.

Beyond these two, no entry points. No `start_compiled(host)`
taking a user-supplied subtree. No `mount_component` callable from
user code. No registry inspection. The runtime exposes:

```rust
// pocopine-core
pub mod app {
    /// Discover the `[pp-app]` root and mount the active route.
    pub fn run();

    /// Tooling-oriented escape hatch — mount a typed component
    /// into an arbitrary host element. See module docs for the
    /// supported use cases (devtools, test harness, Storybook,
    /// embedded widgets).
    pub fn mount_subtree<C: Component>(host: &Element, props: C::Props)
        -> SubtreeHandle;
}

/// Handle for a `mount_subtree` call. Drop or call `.unmount()`
/// to tear down the subtree's scope tree + lifecycle.
pub struct SubtreeHandle { /* private */ }
impl SubtreeHandle {
    pub fn unmount(self);
}
```

Cross-subtree communication is intentionally not supported — each
subtree is its own scope tree. Apps that need shared state across
multiple subtrees use a `#[store]` (RFC 056) which lives outside
the scope tree and is reachable from any mount.

### 4.2 What gets deleted

- `pocopine_core::walker::start_compiled` — replaced by
  `app::run` with the compiled-only mount.
- `pocopine_core::walker::mount_adopted_components` — gone.
- `pocopine_core::walker::install_adopted_controllers` — gone.
- `pocopine_core::walker::materialize_adopted_slot` — gone.
- `pocopine_core::walker::capture_slots` + the entire
  `pocopine_core::slots` runtime store — gone. Slot content is
  always emitted by the parent's compiled plan.
- `pocopine_core::templates::registered_template_names` —
  replaced by the RFC 060 build-time `&'static REGISTRY` slice.
- `pocopine_core::templates_plan::registered_template_tags` —
  same.
- `mount_component`'s slot-capture branch (lines ~410-411 in
  current `walker.rs`).

The `walker.rs` file shrinks from ~1300 lines to ~200 — pure
"apply a compiled plan" helpers (`finalize_compiled_subtree`,
`bind_scope_to`, `enclosing_scope`, the lifecycle dispatch
helpers). Probably renamed `mount.rs` to retire the historical
"walker" name.

### 4.3 What changes for users

| Pattern | Before | After |
|---|---|---|
| App entry | `<body>...pocopine markup...</body>` + `App::run()` discovers | `<body><my-app pp-app></my-app></body>` + `App::run()` mounts the route |
| Slot content | Authored as children of host tag in any HTML | Authored inside a `#[component]` template; macro lifts it |
| Dynamic content | `set_inner_html` with custom tags + walker discovers | Use `pp-if` / `pp-for` / dynamic `<component :is="...">` (RFC TBD) |
| Third-party HTML inject | Mostly worked via walker discovery | Explicit `App::mount_subtree::<C>(...)` with a typed root |

The breaking changes are surgical: most pocopine apps never
relied on the bridge in the first place. Pine compounds always
authored their slot content inside `#[component]` templates.
The breaking case is "drop pocopine into an existing HTML page,
sprinkle `pine-*` tags in the markup, and have it work" — that
goes away.

### 4.4 SSR hydration interaction (RFC 059)

RFC 059's `hydrate(host, scope)` already commits to walking the
compiled plan against existing DOM nodes by node-path. Strict-
compiled is the same architectural shape applied to first-paint
mount. Both consume `StaticTemplatePlan`; the difference is that
mount creates DOM nodes while hydrate adopts existing ones. The
bridge has no role in either, and RFC 059's hydration design
gets simpler: it doesn't have to reason about a bridge it could
fall through to.

### 4.5 Performance specialization

The "rusty direction" — emitting per-component `mount()` functions
with the plan inlined and unrolled, replacing the generic
`apply_static_plan` walker — moved to its **own RFC (RFC 062)**
(resolved 2026-04-28). RFC 061 stays focused on the architectural
commitment (delete the bridge); RFC 062 owns the codegen-shape
question (how the macro emits the mount path). The two RFCs land
in sequence: 061 first (commit + delete), 062 second (specialize
what's left).

This RFC's perf wins come from removing runtime DOM scans + the
slot store + registry walks. Specialization adds another step
function on top in 062.

### 4.6 The dev-mode safety net

Strict mode means a raw `<pine-foo>` in `index.html` silently
becomes inert (just an unknown HTML element). To catch this,
debug builds install a `MutationObserver` on `document.body` that
warns once per unique tag name when an unrecognised custom tag
(matching `[a-z]+-[a-z-]+`) appears in the DOM and isn't claimed
by a compiled mount within one microtask. Off in release.

## 5. Migration

### 5.1 Phase 0 — RFC 060 implemented + accepted

Hard precondition. Cannot start until the registry is exhaustive
at compile time.

### 5.2 Phase 1 — deprecate the bridge entry points

Add `#[deprecated(note = "compiled-mount-only — see RFC 061")]`
to `start_compiled`, `mount_adopted_components`,
`install_adopted_controllers`, the `slots` module. Compiled apps
still work; users see warnings during their next build.

### 5.3 Phase 2 — flip `App::run()` to compiled-only

`App::run()` no longer calls `start_compiled` on the body. It
discovers the `[pp-app]` root and calls the route's compiled
`mount(host, scope)`. Old apps that relied on raw-HTML mounting
get a clear error message ("no `[pp-app]` root found —
pocopine v2 is compiled-mount-only; see RFC 061 migration
guide").

### 5.4 Phase 3 — delete the bridge code

Drop `mount_adopted_components`, `install_adopted_controllers`,
`materialize_adopted_slot`, `capture_slots`, the `slots` module,
the runtime registry mutators. Rename `walker.rs` → `mount.rs`.
Re-run twiggy.

### 5.5 Phase 4 — registry as `phf` table

Build-time generation of a perfect-hash registry from the RFC 060
closure walk. Replaces the runtime HashMap; lookups become
constant-time with no hash computation collision.

### 5.6 Phase 5 — RFC 062 (per-component mount specialization)

Out of scope for this RFC. RFC 062 owns the codegen-shape change
that emits per-component `__pocopine_mount_<name>` functions. It
lands after Phase 4 here.

## 6. Testing requirements

The RFC is not implemented until tests cover:

- every existing pocopine + pine test passes after each phase
  (the contract tests in `adopted_dom_contract.rs` get *deleted*
  in Phase 3 since the bridge they pin no longer exists; that's
  intentional);
- Phase 2 boot path: `App::run()` errors loudly when no
  `[pp-app]` root exists;
- Phase 2 boot path: `App::run()` mounts cleanly when `[pp-app]`
  is present and the route is registered;
- `App::mount_subtree::<C>(target, props)` mounts cleanly into a
  detached element + an in-document element;
- `SubtreeHandle::unmount()` releases scopes, removes effects,
  and clears DOM cleanly (no leaked listeners, no orphaned
  scopes — assert via the existing scope/effect counters);
- dev-mode `MutationObserver` warns exactly once per unknown
  custom tag.

(Specialization-related cross-validation moved to RFC 062.)

## 7. Measurement requirements

Report after each phase:

- counter raw + gzip wasm size;
- website example raw + gzip wasm size;
- mount time for counter, website showcase, jsbench (per-component
  mount cost is the headline number — that's where the bridge tax
  lives);
- `runLots(10000)` from jsbench (proxies the per-row mount cost
  the specialization Phase 4 should improve);
- twiggy report on the mount path;
- comparison row in a benchmarks table against Vue 3 vapor, Solid,
  Svelte, Yew (counter only — these frameworks don't all ship
  comparable jsbench harnesses).

## 8. Performance target

This RFC's standalone targets (Phase 4 complete, *before* RFC 062
specialization layers on top):

- counter raw: **≤390 KB** (currently 433 KB);
- counter gzip: **≤160 KB** (currently 179 KB);
- counter mount time: **≤3.5 ms** on a 2024 mid-tier laptop
  (currently ~5 ms);
- jsbench `runLots(10000)`: within 1.9× Solid (currently ~2.2×).

The bigger jump (≤300 KB raw / ≤130 KB gzip / ≤2 ms / within 1.5×
Solid) is the RFC 062 target — specialization is where the
step-function lives. Splitting the targets keeps each RFC's
acceptance criterion measurable on its own.

Not a competitive parity claim — pocopine carries reactivity +
animations + transitions in the core, while Solid/Svelte ship
those as opt-in. The target is "the gap is dominated by the
features pocopine intentionally bundles, not by runtime tax we
can delete."

## 9. Open questions — resolved 2026-04-28

1. **Discovery selector for `[pp-app]`** — **Resolved: one fixed
   `[pp-app]` attribute selector, no configuration.** One canonical
   convention; multiple pocopine instances on a page use
   `App::mount_subtree::<C>` instead.
2. **Dynamic `<component :is="...">`** — **Resolved: deferred.**
   File a follow-on issue when this RFC ships. Strict-compiled
   doesn't preclude it; the design just isn't urgent for v2.
3. **Multi-root mounts (`App::mount_subtree::<C>`)** —
   **Resolved: ship as a public API**, framed primarily for
   tooling use cases (devtools, test harnesses, Storybook,
   embedded widgets). Default app shape stays `App::run()`.
   Folded into §4.1.
4. **Per-component specialization** — **Resolved: split into its
   own RFC (RFC 062).** Phase 4 of this RFC removed; Phase 4 here
   is now `phf` registry. Specialization is RFC 062.

### Follow-on issues to file when RFC 061 lands

- Dynamic `<component :is="...">` — design + RFC.
- `<Suspense>` / async children — Vue 3 + Solid both ship
  versions; design TBD, probably its own RFC.

## 10. Council questions

1. Does the council accept that **"drop pocopine into existing HTML"
   dies in v2**? It's the headline breaking change; everything
   else flows from it.
2. Are the **§8 performance targets** the right commitments, or
   should we hold them looser until Phase 5 measurements come in?
3. Should the `[pp-app]` root convention be **`pp-app` attribute**
   or **`<pp-app>` tag**? (Configurability is off the table per
   §9 Q1 resolution.) The attribute form lets the root be any
   semantic element (`<main pp-app>`, `<body pp-app>`); the tag
   form is more visually distinct but adds a non-semantic element.
4. Per-component specialization moved to RFC 062 (resolved §9 Q4).
