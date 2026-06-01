# RFC 062 — Per-component mount specialization

| Field | Value |
|---|---|
| **Status** | Accepted (per-component mount specialization + const node-path walk + no-fallback-lane landed; hydrate emit + §4.4 size gate pending) |
| **Author** | pocopine team |
| **Created** | 2026-04-28 |
| **Supersedes** | — |
| **Related** | [RFC 058](./rfc-058-compiled-views-walker-removal.md), [RFC 060](./rfc-060-component-uses-registry.md), [RFC 061](./rfc-061-compiled-mount-only.md) |
| **Depends on** | RFC 061 implemented (compiled-mount-only architecture must be locked before specialization is meaningful) |

## 1. Summary

Replace the generic `apply_static_plan(root, scope_id, proxy,
plan, tag)` walker with a **macro-emitted, per-component
`mount(host, scope)` function** whose body has the component's
template plan inlined and unrolled. Same architectural shape as
Solid's compiled output and Vue 3 vapor's render functions; in
Rust the result is monomorphised and inlined by LLVM, so it's
free at runtime.

This is the codegen-shape question that RFC 061 deliberately
deferred. RFC 061 commits to "no runtime DOM scans"; this RFC
commits to "no runtime plan iteration either."

## 2. Motivation

After RFC 061 lands, every mount is `apply_static_plan(...)`
walking a `&'static StaticTemplatePlan`:

```rust
pub fn apply_static_plan(root: &Element, scope_id: ScopeId,
                         proxy: &JsValue, plan: &StaticTemplatePlan,
                         tag: &str) {
    for entry in plan.bindings { /* match on entry kind */ }
    for entry in plan.listeners { /* ... */ }
    for entry in plan.child_mounts { /* ... */ }
    // ... 8 more entry classes, each a Vec walk + variant match
}
```

Cost per mount: ~11 Vec iterations, ~30-80 variant matches, all
the bounds checks, all the indirect calls through function
pointers stored in plan entries. Dominated by predictable branches
and inlinable functions, but it adds up across 10K-row mounts in
jsbench.

Solid's compiled `template()` output is a flat sequence of direct
DOM ops:

```js
function _tmpl$_my_component(props) {
  const _el$ = _tmpl$_my_component_template.cloneNode(true);
  const _el$2 = _el$.firstChild;
  insert(_el$2, () => props.message);                  // direct call
  _el$2.nextSibling.addEventListener("click", props.bump);
  return _el$;
}
```

Pocopine can do the same in Rust:

```rust
fn __pocopine_mount_my_component(host: &Element, scope: &Scope) {
    let n0 = host.first_element_child().unwrap_throw();
    install_text_binding(&n0, scope, my_component_text_0);
    let n1 = next_element(&n0);
    install_listener(&n1, "click", scope, my_component_click_0);
    // ... unrolled, monomorphised, no Vec walk, no variant match
}
```

The function pointers (`my_component_text_0`, `my_component_click_0`)
are macro-emitted closures that capture nothing — they're const
function pointers that LLVM inlines at the call site. The mount
becomes a flat sequence of direct calls. No iteration, no
indirection, no plan struct in the binary at all.

## 3. Non-goals

- **Not deleting every internal plan literal in this RFC.**
  Lifted slot / `pp-if` / row fragments may still use
  `StaticTemplatePlan` as a macro/runtime IR until they get
  their own fragment-level specialization. Component mount must
  not use the generic applier as a parallel runtime path.
- **Not changing template syntax** or the `#[component]` macro
  surface authors see.
- **Not rewriting reactivity.** Per-binding install helpers
  (`install_text_binding`, `install_listener`, etc.) stay as
  the public ABI between codegen and runtime.
- **Not touching SSR hydration.** RFC 059's `hydrate(host)` gets
  the same specialization treatment as `mount(host)` — same plan,
  different installer call — but the design lives in RFC 059.

## 4. Design

### 4.1 The emit shape

Per `#[component]`, the macro emits two functions:

```rust
// Generated alongside the existing register() etc.
impl MyComponent {
    #[doc(hidden)]
    pub fn __pocopine_mount(host: &Element, scope: ScopeId) {
        // Flat, unrolled, direct-call body.
        // Node-path walk replaced with `firstChild` / `nextSibling`
        // sequences computed at macro expansion time.
        // Each binding is a direct call to an installer + a
        // const fn-pointer the installer consumes.
    }

    #[doc(hidden)]
    pub fn __pocopine_hydrate(host: &Element, scope: ScopeId) {
        // Same shape, but installers attach to existing nodes
        // instead of creating them. RFC 059 owns the hydrate
        // installer ABI.
    }
}
```

The runtime's `Component` trait grows two associated function
pointers:

```rust
pub trait Component: 'static {
    const MOUNT_FN: fn(&Element, ScopeId);
    const HYDRATE_FN: fn(&Element, ScopeId);
    // ... existing items ...
}
```

`App::run()` and `App::mount_subtree::<C>` invoke `C::MOUNT_FN`
directly instead of the generic applier. Pure const dispatch;
LLVM inlines.

### 4.2 Const node-path walk

The plan today identifies each binding by `node_path: Vec<u16>`
(child indices from the template root). The macro can resolve
each path at expansion time into a flat `firstChild` /
`nextSibling` sequence:

```rust
// Plan entry: node_path = [0, 1, 2]   (third grandchild's third sibling)
// Macro emits:
let __n = host.first_element_child().unwrap_throw();           // [0]
let __n = __n.first_element_child().unwrap_throw();            // [0, 0] → walk to [0, 1]
let __n = __n.next_element_sibling().unwrap_throw();           // [0, 1]
let __n = __n.first_element_child().unwrap_throw();            // [0, 1, 0] → walk to [0, 1, 2]
let __n = __n.next_element_sibling().unwrap_throw();
let __n = __n.next_element_sibling().unwrap_throw();           // [0, 1, 2]
```

A peephole optimizer in the macro coalesces shared prefixes
across multiple bindings — e.g. three bindings on `[0, 1, *]`
share the walk to `[0, 1]` and only diverge for the last index.
Same trick Solid does with `_el$ = ..., _el$2 = _el$.firstChild,
_el$3 = _el$2.nextSibling`.

### 4.3 No component-mount fallback lane

RFC 062 is a cleanup, not a second runtime. Component mount has
one compiled path: the macro-emitted mount body. There is no
author-facing `specialize` knob, no size threshold that flips
large components back to `apply_static_plan`, and no default
`Component::mount_template` shim that consults the static-plan
registry.

The generic applier may remain temporarily for lifted fragments
that still use `StaticTemplatePlan` as an internal IR, but it is
not part of component mounting. Once fragment-level codegen
exists, that remaining applier can be removed too.

### 4.4 Build size impact

Per-component specialization produces ~40-200 bytes of unrolled
Rust per binding (post-LLVM-opt). For a typical pocopine app
(~50 components averaging ~6 bindings each), that's ~12-60 KB
of generated code — vs the current generic applier's ~3 KB
shared cost. Net: **specialization grows total binary** unless
LLVM inlines the per-binding installers (which it does in
practice, because the installers are small `#[inline]` functions
and the call sites are unique per binding).

Empirical validation requires Phase 1 measurement before
committing to broader rollout. If the size win doesn't
materialize, RFC 062 is rejected and RFC 061's targets are the
final state.

### 4.5 The Rust-leverage moves

Why this is more powerful in Rust than in JS codegen:

1. **`const fn` everywhere** — installer fn pointers are `const`
   items, so the linker dedupes them across mount sites that
   bind the same expression shape.
2. **Monomorphisation** — generics in installers (e.g.
   `install_text_binding<F: Fn(&Scope) -> String>`) get
   instantiated per binding-expression closure, allowing LLVM
   to inline both the installer and the closure at the call
   site. JS can't do this because all closures are heap-allocated.
3. **Trait associated `const fn`** — `Component::MOUNT_FN`
   resolves at compile time per type; no vtable lookup, no
   indirect call.
4. **Dead-code elimination** — components in the registry
   closure (RFC 060) but never actually mounted on a code path
   the linker traces have their `__pocopine_mount` functions
   stripped. Solid can't do this because the registry is
   runtime; pocopine can because RFC 060 makes it static.

## 5. Migration

### 5.1 Phase 1 — add the mount dispatch ABI

The runtime registry carries one compiled mount function pointer
per component. Macro-emitted components populate it; manual
components get a no-op default because there is no static-plan
component fallback.

### 5.2 Phase 2 — generate component mount bodies

`#[component]` emits an unrolled `__pocopine_mount_template`
body for every planned component. The generated body resolves
node paths via `first_element_child` / `next_element_sibling`
and calls typed install helpers directly.

### 5.3 Phase 3 — quarantine and retire fragment static plans

Any remaining `apply_static_plan` use is limited to lifted
fragment internals (`pp-if`, dynamic slot fragments, row bodies)
where the fragment ABI still passes a plan literal. Those uses
are tracked as follow-up cleanup, not a component-mount fallback
or author-visible mode.

## 6. Testing requirements

- Generated `__pocopine_mount_template` produces the same DOM and
  lifecycle behaviour as the prior compiled mount fixtures.
- Small and large planned components both mount through the same
  generated component path; there is no threshold fallback.
- Slot outlets, interpolation, opaque runtime directives, native
  `pp-model`, child mounts, and structural controllers are covered
  by generated component mount tests.
- Remaining fragment-level `apply_static_plan` use is documented
  and has focused coverage until fragment specialization removes it.

## 7. Performance target

Public commitment for Phase 3 completion (RFC 062 fully landed):

- counter raw: **≤300 KB** (RFC 061 baseline ≤390 KB);
- counter gzip: **≤130 KB** (RFC 061 baseline ≤160 KB);
- counter mount time: **≤2 ms** on a 2024 mid-tier laptop
  (RFC 061 baseline ≤3.5 ms);
- jsbench `runLots(10000)`: within 1.5× Solid (RFC 061 baseline
  within 1.9×).

## 8. Open questions

1. **Threshold tuning** — is 32 entries the right specialize/fall-back
   cutoff? Should it depend on per-binding cost (a `pp-for` is much
   bigger than a `pp-text`), not raw entry count?
2. **Hydrate codegen** lives here or in RFC 059? The natural answer
   is "RFC 059 owns the hydrate installer ABI; RFC 062 owns the
   per-component emit shape," but the line is blurry.
3. **`Component` trait associated `const fn`** vs **non-trait
   `inventory`-style registration** — both work; the former couples
   `Component` more tightly, the latter requires an extra crate.
4. **Per-binding installer dedup** — when two components bind
   `pp-text="message"` against the same closure shape, LLVM
   dedupes if the closures are identical const items. Should the
   macro hoist binding closures to crate-level `const`s
   explicitly (improves dedup, hurts macro hygiene), or trust
   LLVM (clean codegen, sometimes-missed dedup)?
