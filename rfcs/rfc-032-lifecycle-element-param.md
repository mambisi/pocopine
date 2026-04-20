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
from a common `LifecycleContext` carrier via the `FromLifecycleContext` trait.

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

The `FromLifecycleContext` trait is open — authors define their own
extractors (typed `<ul>` ref, `Handle<ParentRoot>`, …) the same
way axum lets you extend `FromRequest`. Each parameter is a
zero-cost projection of the walker-supplied `LifecycleContext`; handlers
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
- **Replacing `pp-ref` with typed positional params.** Authors
  still reach specific named refs through the built-in
  `Refs<'a>` extractor (tier 3) or a per-ref newtype extractor
  they define themselves (§4.6). This RFC doesn't try to
  inject refs by their template-side *name* alone — that
  would need const-string generics.
- **Changing the template's "root element" semantics.** The
  rendered root is still whatever the macro pins
  `SCOPE_ID_KEY` / `SCOPE_PROXY_KEY` onto — `first_element_child`
  of the custom-element tag on the normal path, `user_root`
  under `pp-as`. This RFC surfaces that element; it doesn't
  introduce a new concept.

## 4. Surface

### 4.1 The `LifecycleContext` carrier

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
pub struct LifecycleContext<'a> {
    pub el: &'a web_sys::Element,
    pub scope_id: ScopeId,
}
```

### 4.2 The `FromLifecycleContext` trait — the extractor shape

```rust
pub trait FromLifecycleContext<'a>: Sized {
    fn from_lifecycle_context(ctx: LifecycleContext<'a>) -> Self;
}
```

Built-in impls (live in `pocopine-core`):

| Type | What it gives you |
|---|---|
| `El<'a>` (newtype over `&'a Element`) | the rendered root |
| `ScopeId` | the component's scope id |
| `LifecycleContext<'a>` | the full carrier (for the escape hatch) |

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

impl<'a> FromLifecycleContext<'a> for El<'a> {
    fn from_lifecycle_context(ctx: LifecycleContext<'a>) -> Self { El(ctx.el) }
}

impl<'a> FromLifecycleContext<'a> for ScopeId {
    fn from_lifecycle_context(ctx: LifecycleContext<'a>) -> Self { ctx.scope_id }
}

impl<'a> FromLifecycleContext<'a> for LifecycleContext<'a> {
    fn from_lifecycle_context(ctx: LifecycleContext<'a>) -> Self { ctx }
}
```

### 4.3 Proposed built-in extractors — shopping list

Aggressive brainstorm. Council prunes. Each entry says what it
gives, why it's worth it, and what it'd cost to ship.

#### Tier 1 — obvious yes, ships in v1

| Extractor | Type | What it gives | Cost |
|---|---|---|---|
| `El<'a>` | newtype over `&'a Element` | rendered root | ~0; already in `ctx` |
| `ScopeId` | `u64` wrapper | own scope id | ~0 |
| `LifecycleContext<'a>` | the carrier itself | escape hatch | ~0 |

#### Tier 2 — strongly useful, ships in v1

| Extractor | Type | What it gives | Cost |
|---|---|---|---|
| `Me<T>` | typed handle to self | `Handle<T>` to own scope — replaces `this::<Self>()` in hooks | macro substitutes `T = SelfType` |
| `ParentId` | `Option<ScopeId>` | RFC-027 parent scope id via `context::parent_of` | one HashMap lookup |
| `Document` | `web_sys::Document` | shared shortcut (pocopine never exposes it as a singleton) | `web_sys::window().unwrap().document().unwrap()` |
| `Window` | `web_sys::Window` | same, at window level | `web_sys::window().unwrap()` |
| `Body<'a>` | newtype over `&'a HtmlElement` | document body — useful for teleport-adjacent listener install | cached per call |
| `TagName` | `&'static str` | the component's registered custom-element tag (`"pine-dialog-root"`) | read from a macro-generated const |

#### Tier 3 — domain-useful, ship in v1 if cheap

| Extractor | Type | What it gives | Cost |
|---|---|---|---|
| `Refs<'a>` | map-like accessor | `refs.get("menu")` / `refs.get_as::<HtmlButtonElement>("trigger")` — wraps `refs::get_on(scope, name)` with typed casting | tiny view struct over existing refs registry |
| `TypedEl<'a, T: JsCast>` | `&'a T` | rendered root pre-cast via `dyn_into::<T>()` (panics on mismatch — author's contract) | one `dyn_ref` call |
| `HostEl<'a>` | newtype over `&'a Element` | the custom-element tag (parent of `El`) — useful when you need to dispatch events from it | one `parent_element()` call |
| `IsTeleported` | `bool` | whether `El` is inside a teleported subtree (via `TELEPORT_ORIGIN_KEY` ancestry walk) | tree-walk once per hook |

#### Tier 4 — speculative but shipping in v1

| Extractor | Type | What it gives | Cost |
|---|---|---|---|
| `ScopePath` | `Vec<ScopeId>` | full chain from current scope to root | linear walk; rarely needed — injection already handles the common cases |
| `TeleportHost<'a>` | `Option<&'a Element>` | original host of a teleport (via `teleport::host_of`) | ancestry walk; only useful in teleported code |
| `MountEpoch` | `u64` | monotonic counter bumped each walk — authors can distinguish re-mounts from re-renders | counter state in walker |
| `Provider<K>` / `Injected<T, K>` | typed inject result | resolves `inject(&KEY)` automatically | needs named const-generic keys, or const-string generics (unstable) |
| `Slots<'a>` | map of slot names → fragments | rarely needed directly; templates already handle slots | walker-side exposure |
| `Elapsed` | `Duration` | time since the scope was created | two `performance.now()` calls cached |

### 4.4 Rough shapes for each

Enough to see how they compose with `FromLifecycleContext`:

```rust
// Tier 1
pub struct El<'a>(pub &'a web_sys::Element);
impl<'a> Deref for El<'a> { type Target = Element; fn deref(&self) -> &Element { self.0 } }
impl<'a> FromLifecycleContext<'a> for El<'a> { fn from_lifecycle_context(c: LifecycleContext<'a>) -> Self { El(c.el) } }

// Tier 2
pub struct Me<T: 'static>(pub Handle<T>);
// Generated via the `#[handlers]` macro — it knows the current
// component type, so `Me<Self>` at the hook-param site gets
// substituted with the component's concrete type.
// Not a blanket FromLifecycleContext impl; see §5.2 for how the macro
// wires this one.

pub struct ParentId(pub Option<ScopeId>);
impl<'a> FromLifecycleContext<'a> for ParentId {
    fn from_lifecycle_context(c: LifecycleContext<'a>) -> Self { ParentId(context::parent_of(c.scope_id)) }
}

pub struct Document(pub web_sys::Document);
impl<'a> FromLifecycleContext<'a> for Document {
    fn from_lifecycle_context(_c: LifecycleContext<'a>) -> Self {
        Document(web_sys::window().unwrap().document().unwrap())
    }
}

pub struct Window(pub web_sys::Window);
// similar

pub struct Body<'a>(pub &'a web_sys::HtmlElement);
// cache in a thread-local per hook call; see §5.3

pub struct TagName(pub &'static str);
// macro-generated — each component knows its own tag at compile time,
// so Me and TagName are macro-synthesized for the component in
// question rather than read from ctx

// Tier 3
pub struct Refs<'a> {
    scope_id: ScopeId,
    _m: PhantomData<&'a ()>,
}
impl<'a> Refs<'a> {
    pub fn get(&self, name: &str) -> Option<web_sys::Element> {
        pocopine_core::refs::get_on(self.scope_id, name)
    }
    pub fn get_as<T: JsCast>(&self, name: &str) -> Option<T> {
        self.get(name).and_then(|el| el.dyn_into().ok())
    }
}
impl<'a> FromLifecycleContext<'a> for Refs<'a> {
    fn from_lifecycle_context(c: LifecycleContext<'a>) -> Self {
        Refs { scope_id: c.scope_id, _m: PhantomData }
    }
}

pub struct TypedEl<'a, T: JsCast>(pub &'a T, PhantomData<T>);
// panic on mismatch — author's contract

pub struct HostEl<'a>(pub &'a web_sys::Element);
impl<'a> FromLifecycleContext<'a> for HostEl<'a> {
    fn from_lifecycle_context(c: LifecycleContext<'a>) -> Self {
        HostEl(c.el.parent_element().as_ref().unwrap_or(c.el))
    }
}

pub struct IsTeleported(pub bool);
// walks ancestors checking TELEPORT_ORIGIN_KEY

// Tier 4 — sketches only, not for v1.
```

### 4.5 Worked handler signatures — how the tiers read

```rust
// Dialog Content today — four lines of lookup + mutation guard.
pub fn on_ready(&self) {
    let Some(scope) = current_scope_id() else { return };
    let Some(content) = refs::get_on(scope, "content") else { return };
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(scope, &content, modal);
}

// With Tier 1 alone — already half the size.
pub fn on_ready(&self, el: El, scope: ScopeId) {
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(scope, &el, modal);
}

// With Tier 2 adding Me<Self> — watch_scope_field chains get
// their Handle from the extractor, not a `this::<Self>()` line.
pub fn on_ready(&self, el: El, scope: ScopeId, me: Me<Self>) {
    // … use me.0 inside closures instead of cloning `this::<Self>()`.
}

// Author-defined typed ref — one impl per "ref I use a lot."
pub fn on_ready(&self, MenuRef(menu): MenuRef, scope: ScopeId) {
    init_roving_tabindex(&menu);
    focus::auto_focus_first(&menu);
}

// Tier 3 Refs — typed access without a per-ref newtype.
pub fn on_ready(&self, scope: ScopeId, refs: Refs) {
    if let Some(menu) = refs.get_as::<HtmlUListElement>("menu") {
        init_roving_tabindex(&menu);
    }
}

// Tier 3 TypedEl — the rendered root already typed.
pub fn on_ready(&self, el: TypedEl<HtmlButtonElement>) {
    let _ = el.0.focus();
}

// Tier 2 Document — no `web_sys::window().unwrap().document().unwrap()`.
pub fn on_mount(&mut self, doc: Document) {
    let _ = doc.0.add_event_listener_with_callback(/* … */);
}
```

### 4.6 Author-defined extractors

Because `FromLifecycleContext` is open, authors add their own when a
pattern repeats. Example — a typed menu ref that looks up
`pp-ref="menu"`:

```rust
pub struct MenuRef(pub web_sys::HtmlElement);

impl<'a> FromLifecycleContext<'a> for MenuRef {
    fn from_lifecycle_context(ctx: LifecycleContext<'a>) -> Self {
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

With the Tier 3 `Refs<'a>` built-in, most typed-ref author
extractors collapse to a single line at the call site —
`refs.get_as::<HtmlUListElement>("menu")` — so authors only
define their own when they want the type hint to show up in
the handler signature (e.g. `MenuRef(menu): MenuRef` is more
self-documenting than `refs.get_as("menu").unwrap()`).

### 4.7 Supported handler signatures

The `#[handlers]` macro accepts any of these for `on_mount` /
`on_ready`:

```rust
// No extractors (unchanged — existing code).
fn on_mount(&mut self) { … }
fn on_ready(&self) { … }

// Any number, any order — each param implements FromLifecycleContext.
fn on_ready(&self, el: El) { … }
fn on_ready(&self, el: El, scope: ScopeId) { … }
fn on_ready(&self, scope: ScopeId, ctx: LifecycleContext) { … }
fn on_ready(&self, MenuRef(menu): MenuRef) { … }
```

The macro doesn't constrain order or count. The only
constraint is that `&self` / `&mut self` is the first
receiver; every subsequent parameter must implement
`FromLifecycleContext<'_>`. rustc enforces the latter via the generated
forwarder.

Non-`FromLifecycleContext` types in the signature produce a normal
rustc trait-bound error: *"the trait `FromLifecycleContext<'_>` is not
implemented for …"* — same shape as axum's "not a valid
extractor" compile error.

### 4.8 `on_setup` / `on_unmount` — unchanged

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
    fn mount(&mut self, ctx: LifecycleContext) {}
    fn on_ready(&self, ctx: LifecycleContext) {}
    // …
}
```

Default impls ignore the ctx, so non-component
`ComponentState` impls (if any ever appear) keep compiling.

## 5. Implementation

### 5.1 Walker call sites

`walker::fire_mount_hook(el)` already has `el` in scope — it's
the rendered root it walked. Build a `LifecycleContext` and hand it to
the trait method:

```rust
// In fire_mount_hook (walker.rs ~line 165):
let ctx = LifecycleContext { el, scope_id: id };
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
                sc.state.borrow().on_ready(LifecycleContext { el: &el_owned, scope_id: id });
            });
        }
    });
}
```

The ComponentState trait methods receive a single `LifecycleContext`
argument; the `#[handlers]`-generated forwarder destructures it
through `FromLifecycleContext::from_lifecycle_context` per parameter.

`tick::next` fires after the microtask queue drains; we clone
`el` into the closure so the call site owns it, then re-borrow
through a fresh `LifecycleContext` at invoke time. Cheap — `Element` is
a wasm-bindgen JsValue handle, so clone is a ref bump.

### 5.2 `#[handlers]` macro

For each handler with extractor parameters, generate a forwarder
that extracts each one via `FromLifecycleContext`:

```rust
// User writes:
fn on_ready(&self, el: El, scope: ScopeId) { … }

// Macro generates (inside the HandlerDispatch impl):
fn on_ready(&self, __ctx: LifecycleContext) {
    Self::on_ready(
        self,
        <El as FromLifecycleContext>::from_lifecycle_context(__ctx),
        <ScopeId as FromLifecycleContext>::from_lifecycle_context(__ctx),
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
        quote! { <#ty as ::pocopine::FromLifecycleContext>::from_lifecycle_context(__ctx) }
    })
    .collect();

quote! {
    fn on_ready(&self, __ctx: ::pocopine::LifecycleContext) {
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
pub fn on_ready(&self, MenuRef(menu): MenuRef, ctx: LifecycleContext) {
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
  controls call order — each `FromLifecycleContext::from_lifecycle_context` runs
  left to right. For stateless built-ins that's irrelevant;
  author extractors that mutate side tables (rare) should
  document their ordering expectations.
- **`#[non_exhaustive]` on `LifecycleContext`.** Constructing `LifecycleContext`
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

### 8.2 Single `LifecycleContext` struct

```rust
fn on_ready(&self, ctx: LifecycleContext) { … }
```

Bundles element + scope id. Better than 8.1 because adding
`LifecycleContext` fields is non-breaking. Worse than the extractor
approach because:

- Every site reads `ctx.el` / `ctx.scope_id` — more noise at
  the callsite than named typed params.
- Author-defined types aren't addressable directly (a typed
  `MenuRef` would need to be wrapped in an inherent method on
  `LifecycleContext`, or resolved inline from `ctx`).
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
`on_ready_with_ctx(&self, ctx: LifecycleContext)` — and let the user
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
- **What built-in extractors ship in v1.** All four tiers per
  §4.3 — pay-for-what-you-use means unused extractors cost
  authors nothing. Tier 4 entries that require new walker
  state (`MountEpoch`, `Slots<'a>`) still need their runtime
  piece plumbed; `Provider<K>` / `Injected<T, K>` is blocked on
  const-string generics and lands as a placeholder that errors
  at compile time until stable. See §4.3 for the full list.
- **Naming — settled on `LifecycleContext`** +
  `FromLifecycleContext`. Rejected alternatives: `HookCtx`
  (overloaded with pocopine's other "hook" uses), `MountCtx`
  (misleads — `on_ready` shares the type), `ScopeCtx`
  (collides with the `Scope` type), `ComponentCtx` (verbose,
  and the carrier is more transient than a component), bare
  `El` (closes off future fields), `HostCtx` (misreads — the
  carrier's `el` is the rendered root, not the host tag),
  `Env` (too generic, loses the lifecycle signal). Spelling it
  out over `LifecycleCtx` trades a handful of keystrokes at
  the rare site where authors name the carrier for immediate
  readability everywhere — and since ~all callsites use
  extractors directly (`fn on_ready(&self, el: El, scope:
  ScopeId)`), the carrier name shows up in docs and the
  escape-hatch `ctx: LifecycleContext` only.
- **Panic policy for custom extractors.** Do we recommend
  `Option<T>` returning impls, or panic-on-missing? The
  built-ins can't fail; author extractors might want either.
  Probably leave it to convention — both are valid depending
  on whether a missing value is a template bug or an expected
  absence.
