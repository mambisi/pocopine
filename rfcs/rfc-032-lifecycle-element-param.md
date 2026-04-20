# RFC 032 — Extractor-style params for `on_mount` / `on_ready`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 026](./rfc-026-post-mount-watch-field.md), [RFC 029](./rfc-029-on-ready-rename.md) |

## 1. Summary

Let `on_mount` and `on_ready` accept a **variable list of
extractor parameters** — axum / bevy-system style. Authors
declare the types they want and `#[handlers]` wires each one
from a common `HookCtx` carrier via the `FromHookCtx` trait.

Today those hooks get `&self` / `&mut self` only; to touch the
DOM they re-derive the scope id + element by hand —
`current_scope_id().unwrap()` + `refs::get_on(scope, "…")` —
four lines of boilerplate in 20+ of Pine's ~38 hook sites.

```rust
// Current — opt-in DOM access via pp-ref + registry lookups.
pub fn on_ready(&self) {
    let Some(scope) = current_scope_id() else { return };
    let Some(content) = refs::get_on(scope, "content") else { return };
    overlay::activate(scope, &content, /* ... */);
}

// Proposed — params are typed extractors; pick whichever you need.
pub fn on_ready(&self, el: El, scope: ScopeId) {
    overlay::activate(scope, &el, /* ... */);
}

// Another handler, same component, different shape:
pub fn on_mount(&mut self, _el: El) {
    // Just the element; don't care about scope id.
}
```

The `FromHookCtx` trait is open — authors define their own
extractors (typed `<ul>` ref, `Handle<ParentRoot>`, …) the same
way axum lets you extend `FromRequest`. Each parameter is a
zero-cost projection of the walker-supplied `HookCtx`; handlers
pay only for what they use.

Parameters are **optional at the call site** — `#[handlers]`
reads the signature and generates the right forwarder. Old code
(`fn on_ready(&self)`) keeps working unchanged; new code picks
any subset of extractors in any order.

## 2. Motivation

Surveying `crates/pine/src/**/mod.rs`:

- 38 `on_ready` / `on_mount` handlers total.
- 20 of them (~53%) call `refs::get_on(scope, "…")` as their
  first or second line.
- Most pull a single ref by a predictable name: `"root"`,
  `"content"`, `"menu"`, or `"trigger"`. Many of those refs
  point at the component's template root — the same element
  the macro already knows about at mount time.

Three costs of the status quo:

1. **Boilerplate repetition.** Every overlay-ish compound
   (Dialog, AlertDialog, Popover, DropdownMenu, HoverCard,
   Tooltip, ContextMenu) opens with the same four-line dance.
2. **Template-driven plumbing.** Authors sprinkle
   `pp-ref="root"` / `pp-ref="content"` purely so the handler
   can look the element back up — the ref isn't read from the
   template itself.
3. **Teaching surface.** The first thing new component authors
   learn is "to touch your own DOM, pop scope id through
   `current_scope_id`, then ask `refs::get_on`." Passing
   `&Element` matches what every other reactive framework does
   (Vue's `onMounted` getter on template refs, React's `ref`
   callback, Solid's `ref` function) and collapses the teaching
   step to one sentence.

## 3. Non-goals

- **Passing it to `on_setup`.** Runs before the template
  clones; the rendered root doesn't exist yet. Keep
  `on_setup(&mut self)` as today.
- **Passing it to `on_unmount`.** The element is *being* torn
  down; the hook's job is to release side-table entries, not
  poke DOM. `on_unmount(&mut self)` stays unchanged — if an
  author really needs the element, `refs::get_on` still works.
- **Passing named refs by position.** This RFC adds *one*
  parameter: the rendered root. Additional refs (`content`,
  `menu`, …) stay on the `pp-ref` + `refs::get_on` path. A
  future RFC could expose a map/struct of refs to handlers;
  out of scope here.
- **Changing the template's "root element" semantics.** The
  rendered root is still whatever the macro pins
  `SCOPE_ID_KEY` / `SCOPE_PROXY_KEY` onto — `first_element_child`
  of the custom-element tag on the normal path, `user_root`
  under `pp-as`. This RFC surfaces that element; it doesn't
  introduce a new concept.

## 4. Surface

### 4.1 The `HookCtx` carrier

Private-ish — authors rarely name it directly. It's the blob
the walker hands to the macro-generated forwarder; extractors
read from it.

```rust
/// Context handed by the walker to each lifecycle-hook call.
/// `Copy` — the element is a JS handle and the scope id is a
/// `u64` wrapper. `#[non_exhaustive]` so later versions can
/// add fields without breaking existing extractor impls.
#[non_exhaustive]
#[derive(Clone, Copy)]
pub struct HookCtx<'a> {
    pub el: &'a web_sys::Element,
    pub scope_id: ScopeId,
}
```

### 4.2 The `FromHookCtx` trait — the extractor shape

```rust
pub trait FromHookCtx<'a>: Sized {
    fn from_hook_ctx(ctx: HookCtx<'a>) -> Self;
}
```

Built-in impls (live in `pocopine-core`):

| Type | What it gives you |
|---|---|
| `El<'a>` (newtype over `&'a Element`) | the rendered root |
| `ScopeId` | the component's scope id |
| `HookCtx<'a>` | the full carrier (for the escape hatch) |

`El<'a>` is a thin newtype instead of a bare `&Element` because
rustc can't infer `&'a Element` as a generic extractor type
slot cleanly — wrapping gives the macro a concrete, named
type to match against.

```rust
pub struct El<'a>(pub &'a web_sys::Element);

impl<'a> std::ops::Deref for El<'a> {
    type Target = web_sys::Element;
    fn deref(&self) -> &Self::Target { self.0 }
}

impl<'a> FromHookCtx<'a> for El<'a> {
    fn from_hook_ctx(ctx: HookCtx<'a>) -> Self { El(ctx.el) }
}

impl<'a> FromHookCtx<'a> for ScopeId {
    fn from_hook_ctx(ctx: HookCtx<'a>) -> Self { ctx.scope_id }
}

impl<'a> FromHookCtx<'a> for HookCtx<'a> {
    fn from_hook_ctx(ctx: HookCtx<'a>) -> Self { ctx }
}
```

### 4.3 Author-defined extractors

Because `FromHookCtx` is open, authors add their own when a
pattern repeats. Example — a typed menu ref that looks up
`pp-ref="menu"`:

```rust
pub struct MenuRef(pub web_sys::HtmlElement);

impl<'a> FromHookCtx<'a> for MenuRef {
    fn from_hook_ctx(ctx: HookCtx<'a>) -> Self {
        let el = pocopine::refs::get_on(ctx.scope_id, "menu")
            .expect("pp-ref=\"menu\" on template root");
        MenuRef(el.dyn_into().unwrap())
    }
}

// And then:
fn on_ready(&self, MenuRef(menu): MenuRef) {
    init_roving_tabindex(&menu);
    focus::auto_focus_first(&menu);
}
```

A panicking extractor is an author choice — the built-in ones
don't panic (the element is guaranteed by the walker).

### 4.4 Supported handler signatures

The `#[handlers]` macro accepts any of these for `on_mount` /
`on_ready`:

```rust
// No extractors (unchanged — existing code).
fn on_mount(&mut self) { … }
fn on_ready(&self) { … }

// Any number, any order — each param implements FromHookCtx.
fn on_ready(&self, el: El) { … }
fn on_ready(&self, el: El, scope: ScopeId) { … }
fn on_ready(&self, scope: ScopeId, ctx: HookCtx) { … }
fn on_ready(&self, MenuRef(menu): MenuRef) { … }
```

The macro doesn't constrain order or count. The only
constraint is that `&self` / `&mut self` is the first
receiver; every subsequent parameter must implement
`FromHookCtx<'_>`. rustc enforces the latter via the generated
forwarder.

Non-`FromHookCtx` types in the signature produce a normal
rustc trait-bound error: *"the trait `FromHookCtx<'_>` is not
implemented for …"* — same shape as axum's "not a valid
extractor" compile error.

### 4.2 `on_setup` / `on_unmount` — unchanged

```rust
fn on_setup(&mut self) { … }
fn on_unmount(&mut self) { … }
```

No alternate signatures. As §3 notes, `on_setup` runs before
the element exists; `on_unmount` runs during teardown where DOM
poking is a footgun.

### 4.3 What element `ctx.el` points at

The **rendered root** — the element that `SCOPE_ID_KEY` is
pinned on. Concretely:

- **Normal mount path:** the first element child of the
  custom-element tag (the template root).
- **`pp-as` path:** the hoisted user element (`user_root` —
  RFC-019).
- **`pp-for` / `pp-if` clones:** the cloned root.

Same element `refs::get_on(ctx.scope_id, "root")` would return
when the template defines `pp-ref="root"` on its root. RFC-032
makes it the default rather than opt-in.

### 4.4 `ComponentState` trait

```rust
pub trait ComponentState: 'static {
    // …
    fn mount(&mut self, ctx: HookCtx) {}
    fn on_ready(&self, ctx: HookCtx) {}
    // …
}
```

Default impls ignore the ctx, so non-component
`ComponentState` impls (if any ever appear) keep compiling.

## 5. Implementation

### 5.1 Walker call sites

`walker::fire_mount_hook(el)` already has `el` in scope — it's
the rendered root it walked. Build a `HookCtx` and hand it to
the trait method:

```rust
// In fire_mount_hook (walker.rs ~line 165):
let ctx = HookCtx { el, scope_id: id };
if has_mount {
    scope::with_current_scope_id(id, || {
        scope.state.borrow_mut().mount(ctx);
    });
}
if has_ready {
    let el_owned = el.clone();
    tick::next(move || {
        if let Some(sc) = Scope::find(id) {
            scope::with_current_scope_id(id, || {
                sc.state.borrow().on_ready(HookCtx { el: &el_owned, scope_id: id });
            });
        }
    });
}
```

The ComponentState trait methods receive a single `HookCtx`
argument; the `#[handlers]`-generated forwarder destructures it
through `FromHookCtx::from_hook_ctx` per parameter.

`tick::next` fires after the microtask queue drains; we clone
`el` into the closure so the call site owns it, then re-borrow
through a fresh `HookCtx` at invoke time. Cheap — `Element` is
a wasm-bindgen JsValue handle, so clone is a ref bump.

### 5.2 `#[handlers]` macro

For each handler with extractor parameters, generate a forwarder
that extracts each one via `FromHookCtx`:

```rust
// User writes:
fn on_ready(&self, el: El, scope: ScopeId) { … }

// Macro generates (inside the HandlerDispatch impl):
fn on_ready(&self, __ctx: HookCtx) {
    Self::on_ready(
        self,
        <El as FromHookCtx>::from_hook_ctx(__ctx),
        <ScopeId as FromHookCtx>::from_hook_ctx(__ctx),
    );
}
```

Implementation sketch — reuse the signature-walk that already
powers the handler dispatch:

```rust
let params: Vec<_> = user_method.sig.inputs.iter()
    .skip(1) // drop &self / &mut self
    .map(|arg| {
        let PatType { ty, .. } = match arg {
            FnArg::Typed(p) => p,
            FnArg::Receiver(_) => unreachable!(),
        };
        quote! { <#ty as ::pocopine::FromHookCtx>::from_hook_ctx(__ctx) }
    })
    .collect();

quote! {
    fn on_ready(&self, __ctx: ::pocopine::HookCtx) {
        Self::on_ready(self, #(#params),*);
    }
}
```

`on_mount` gets the same treatment with `&mut self`.

The zero-extractor case (`fn on_ready(&self)`) short-circuits:
the generated forwarder ignores `__ctx` and calls straight
through. No change for existing code.

### 5.3 Migration

None required for existing code. The no-element signature keeps
working. New compounds can opt into the element parameter on a
per-hook basis. Pine's own migration is discretionary — do it
as sites are touched, not all at once.

## 6. Worked example — Dialog Content

Before:

```rust
pub fn on_ready(&self) {
    let Some(scope) = current_scope_id() else { return };
    let Some(content) = refs::get_on(scope, "content") else { return };
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(scope, &content, modal);
    // … watch_scope_field …
}
```

After:

```rust
pub fn on_ready(&self, el: El, scope: ScopeId) {
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(scope, &el, modal);
    // … watch_scope_field …
}
```

Dialog's `PineDialogContent.poco` would drop the redundant
`pp-ref="content"` (the ref was there only to enable the
`get_on` lookup). Three lines collapse into two typed params.

**Richer example** — DropdownMenu Content that also wants its
`<ul>` ref:

```rust
pub fn on_ready(&self, MenuRef(menu): MenuRef, ctx: HookCtx) {
    init_roving_tabindex(&menu);
    focus::auto_focus_first(&menu);
    if let Some(anchor) = resolve_anchor(&self.anchor) {
        pocopine_core::directives::anchor::install(
            /* … */,
            &anchor,
            /* placement from self */,
            self.side_offset,
            true,
        );
    }
}
```

The `MenuRef` extractor absorbs the `refs::get_on(scope, "menu")
+ dyn_into::<HtmlElement>()` pair into a typed value — reusable
across every compound that ships a menu.

> **Caveat when the rendered root isn't what you want.** A
> component whose handler needs a user-provided inner button
> (somewhere below `<slot>`) still writes a custom extractor
> that calls `refs::get_on(…)`. RFC-032 surfaces the rendered
> root by default and makes the refs lookup one impl away.

## 7. Edge cases

- **Scope id recovery inside the hook.** The `ScopeId`
  extractor replaces most `current_scope_id()` calls inside
  hooks. The thread-local still exists for code that runs
  outside a hook (async tasks, timer callbacks, watch-scope-
  field observers).
- **Pointer stability across `on_ready` calls.** `on_ready`
  fires once per scope (via `tick::next`). The element is
  stable across that single invocation. No promise is made
  across hook types — `on_mount` may see a different element
  from `on_ready` in principle if the subtree was replaced
  between them, though today the walker guarantees they're the
  same.
- **`&mut self` vs `&self`.** `on_mount(&mut self, el: El)` —
  mutable borrow of state, copy of `El` (which newtype-wraps
  `&Element`). No aliasing issue: `El` doesn't reborrow from
  `self`.
- **Element cloning for async moves.** Authors spawning async
  tasks that need the element clone it:
  `let el_owned = (*el).clone();` — cheap because `Element`
  wraps a JS handle.
- **Panicking extractors.** Author-defined extractors can panic
  (e.g. a `MenuRef` that unwraps `refs::get_on`). That's the
  author's contract, not the framework's. The built-ins never
  panic.
- **Extractor ordering.** Order in the handler signature
  controls call order — each `FromHookCtx::from_hook_ctx` runs
  left to right. For stateless built-ins that's irrelevant;
  author extractors that mutate side tables (rare) should
  document their ordering expectations.
- **`#[non_exhaustive]` on `HookCtx`.** Constructing `HookCtx`
  outside pocopine-core is impossible (by design — only the
  walker mints them), so the attribute costs authors nothing.
  New fields are purely additive.

## 8. Alternatives considered

### 8.1 Bare `&Element` parameter

```rust
fn on_ready(&self, el: &web_sys::Element) { … }
```

Simplest type. Loses the extensibility of extractors —
authors who want scope id or a typed ref write
`current_scope_id().unwrap()` + `refs::get_on(…)` again, same
as today. Also locks the surface: adding a "parent scope id"
later would need another RFC.

### 8.2 Single `HookCtx` struct

```rust
fn on_ready(&self, ctx: HookCtx) { … }
```

Bundles element + scope id. Better than 8.1 because adding
`HookCtx` fields is non-breaking. Worse than the extractor
approach because:

- Every site reads `ctx.el` / `ctx.scope_id` — more noise at
  the callsite than named typed params.
- Author-defined types aren't addressable directly (a typed
  `MenuRef` would need to be wrapped in an inherent method on
  `HookCtx`, or resolved inline from `ctx`).
- Doesn't scale to future additions like a refs map without
  authors plumbing `ctx.refs.get("menu").unwrap().dyn_into::
  <HtmlButtonElement>().unwrap()` every time.

Axum had the same choice and landed on extractors for the same
reasons.

### 8.3 Always break, no detection

Force every `on_mount` / `on_ready` to take at least one
extractor. Simpler macro. Noisier per-site for the ~47% of
hooks that don't need DOM access (authors write `_: El`). We
take the additive route because the burden is asymmetric —
half of today's hooks don't need any extractor at all.

### 8.4 Thread-local current element

Set a `CURRENT_EL` thread-local for the duration of the hook,
reachable via `pocopine::current_el()`. Already exists for
directives; could be extended. Downside: implicit-argument
smell, magic to explain, vs visible typed params.

### 8.5 Opt-in via trait method override

Have two trait methods — `on_ready(&self)` and
`on_ready_with_ctx(&self, ctx: HookCtx)` — and let the user
implement whichever. Clutters the trait surface and doesn't
give the typed-extractor benefits of §4.3.

## 9. Rollout

1. Land the macro signature introspection + trait-default
   widening. Every existing `on_mount` / `on_ready` keeps
   compiling unchanged.
2. Update one example (Dialog Content) as a reference.
3. Migrate opportunistically — when a Pine compound gets a
   touch for other reasons, drop its redundant
   `let Some(el) = refs::get_on(...)` dance.
4. Document in `docs/` and update RFC-001's lifecycle table.

## 10. Open questions

- **Should `on_mount` always take at least one extractor while
  `on_ready` stays optional?** `on_mount` is where most DOM
  work happens (install listeners, stamp attributes); `on_ready`
  is more about "wait for subtree" than "touch my own
  element." Counter-argument: consistency is cheap.
- **What built-in extractors ship in v1.** Candidates:
  - `El<'a>` — rendered root. (Obvious yes.)
  - `ScopeId` — own scope. (Obvious yes.)
  - `HookCtx<'a>` — escape hatch. (Obvious yes.)
  - `ParentId(ScopeId)` — injected parent. Maybe — most
    compounds want `Handle<Root>` via `inject::<>`, not the
    raw parent.
  - `Handle<T>` — resolves `inject(&KEY)` automatically.
    Tempting but needs a key to know *which* handle; probably
    a custom extractor thing.
  - `Refs<'a>` — map-like accessor over `pp-ref` entries:
    `refs.get("menu")`. Nice but adds a walker-side data
    structure.
  v1 ships the three "obvious yes" extractors only.
- **Naming.** `HookCtx` vs `LifecycleCtx` vs `MountCtx`. Single
  `HookCtx` for both hooks keeps the type count down;
  per-hook structs would let us add fields that only make
  sense in one (e.g. a "walk epoch" on mount but not ready).
  v1 ships one `HookCtx` for simplicity. `El` vs `Elem` vs
  `Element` for the rendered-root newtype — go with `El` for
  brevity since the template is a typed param (`el: El`
  reads naturally).
- **Panic policy for custom extractors.** Do we recommend
  `Option<T>` returning impls, or panic-on-missing? The
  built-ins can't fail; author extractors might want either.
  Probably leave it to convention — both are valid depending
  on whether a missing value is a template bug or an expected
  absence.
