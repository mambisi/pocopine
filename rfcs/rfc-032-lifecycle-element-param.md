# RFC 032 — Pipe a `HookCtx` to `on_mount` / `on_ready`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 026](./rfc-026-post-mount-watch-field.md), [RFC 029](./rfc-029-on-ready-rename.md) |

## 1. Summary

Let `on_mount` and `on_ready` receive a **`HookCtx`** — a small
copy-able struct carrying the component's rendered root element
and its scope id. Today those hooks get `&self` / `&mut self`
only; to touch the DOM they re-derive both values:
`current_scope_id().unwrap()` + `refs::get_on(scope, "…")` —
four lines of boilerplate in 20+ of Pine's ~38 hook sites.

```rust
// Current — opt-in DOM access via pp-ref + registry lookups.
pub fn on_ready(&self) {
    let Some(scope) = current_scope_id() else { return };
    let Some(content) = refs::get_on(scope, "content") else { return };
    overlay::activate(scope, &content, /* ... */);
}

// Proposed — one `ctx` carries both.
pub fn on_ready(&self, ctx: HookCtx) {
    overlay::activate(ctx.scope_id, ctx.el, /* ... */);
}
```

The parameter is **optional at the call site** — `#[handlers]`
detects which signature the author wrote and generates the right
forwarder. Old code keeps working unchanged; new code can opt in.

The `HookCtx` bet is deliberate: elements + scope id travel
together in almost every handler, and bundling them in one
struct lets the hook surface grow (parent scope id, timing info,
typed refs map) without another RFC-style signature churn.

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

### 4.1 `HookCtx` shape

```rust
/// Context handed to `on_mount` and `on_ready`. Copy — the
/// element is a JS handle (cheap to clone) and the scope id is
/// a `u64` wrapper. Authors can pass it through by value, no
/// lifetime dance.
#[derive(Clone, Copy)]
pub struct HookCtx<'a> {
    /// The component's rendered root element — what
    /// `SCOPE_ID_KEY` is pinned on. Template root on the normal
    /// path; the hoisted user element under `pp-as`.
    pub el: &'a web_sys::Element,
    /// This component's scope id.
    pub scope_id: ScopeId,
}
```

v1 ships exactly these two fields. Future fields (parent scope
id, a pre-walked refs map, the mount epoch) can be added
additively — `HookCtx` is `#[non_exhaustive]` so downstream code
never destructures by ordered fields.

### 4.2 Supported handler signatures

The `#[handlers]` macro accepts both shapes for each of
`on_mount` and `on_ready`:

```rust
// No ctx (unchanged — existing code).
fn on_mount(&mut self) { … }
fn on_ready(&self) { … }

// With ctx — RFC-032 opt-in.
fn on_mount(&mut self, ctx: HookCtx) { … }
fn on_ready(&self, ctx: HookCtx) { … }
```

The parameter name is author-chosen; convention is `ctx`. Type
is exactly `HookCtx` (taken by value — it's `Copy`). An
incorrect type is a compile error (rustc); an extra parameter
past the ctx is a macro error with the message *"on_ready
takes at most one `HookCtx` parameter (RFC-032)"*.

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
the rendered root it walked. Build a `HookCtx` and pass it into
`mount` and the subsequent `on_ready` schedule:

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

`tick::next` fires after the microtask queue drains; we clone
`el` into the closure so the call site owns it, then re-borrow
through a fresh `HookCtx` at invoke time. Cheap — `Element` is
a wasm-bindgen JsValue handle, so clone is a ref bump.

### 5.2 `#[handlers]` macro

Introspect each user method's signature. For `on_ready`:

```rust
match user_method.sig.inputs.len() {
    1 => quote! { fn on_ready(&self, _ctx: HookCtx) { Self::on_ready(self); } },
    2 => quote! { fn on_ready(&self, ctx: HookCtx)  { Self::on_ready(self, ctx); } },
    _ => compile_error!(…),
}
```

Same shape for `on_mount`. The trait definition always takes
`HookCtx`; the forwarder bridges to whichever the author wrote.

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
pub fn on_ready(&self, ctx: HookCtx) {
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(ctx.scope_id, ctx.el, modal);
    // … watch_scope_field …
}
```

Dialog's `PineDialogContent.poco` would drop the redundant
`pp-ref="content"` (the ref was there only to enable the
`get_on` lookup). Three lines collapse into one `ctx.` prefix.

> **Caveat when `pp-ref` names disagree with "the root."** A
> component whose template is `<ul><slot/></ul>` but whose
> handlers need the inner user button (somewhere below `<slot>`)
> would still use `refs::get_on(ctx.scope_id, "user_btn")` —
> RFC-032 only surfaces the rendered root. The pp-ref mechanism
> isn't going anywhere.

## 7. Edge cases

- **Scope id recovery inside the hook.** `ctx.scope_id`
  replaces most `current_scope_id()` calls inside hooks. The
  thread-local still exists for code that runs outside a hook
  (async tasks, timer callbacks, watch-scope-field observers).
- **Pointer stability across `on_ready` calls.** `on_ready`
  fires once per scope (via `tick::next`). `ctx.el` is stable
  across that single invocation. No promise is made across hook
  types — `on_mount` may see a different element from
  `on_ready` in principle if the subtree was replaced between
  them, though today the walker guarantees they're the same.
- **`&mut self` vs `&self`.** `on_mount(&mut self, ctx: HookCtx)`
  — mutable borrow of state, copy of ctx. No aliasing issue
  since `ctx.el` is a `&Element` that doesn't reborrow `self`.
- **Element cloning for async moves.** Authors spawning async
  tasks that need the element clone it: `let el = ctx.el.clone();`
  — cheap because `Element` wraps a JS handle.
- **`#[non_exhaustive]` ergonomics.** Constructing `HookCtx`
  outside pocopine-core is impossible (by design — only the
  walker mints them), so the attribute costs authors nothing.
  It costs *us* if we ever want a public constructor: we'd
  then need `HookCtx::new(el, scope_id)`. Worth the lock-in.

## 8. Alternatives considered

### 8.1 Bare `&Element` parameter (no struct)

```rust
fn on_ready(&self, el: &web_sys::Element) { … }
```

Simpler type. But the scope id is needed alongside the element
in ~40% of sites (overlay::activate, watch_scope_field's owner,
emit_from's dispatch target), and bundling them means one
less `current_scope_id().unwrap()` per hook. More importantly:
once the struct is there, future hook data (typed refs map,
parent scope id, mount epoch) adds fields instead of arguments —
no more RFC-style signature churn.

### 8.2 Always break, no detection

Force every `on_mount` / `on_ready` to take the ctx parameter.
Simpler macro, noisier per-site (authors who don't care write
`_ctx: HookCtx`). RFC-031 did take the breaking route; this
RFC leans additive because the boilerplate burden is genuinely
asymmetric — about half of today's hooks don't need DOM
access.

### 8.3 Thread-local current element

Set a `CURRENT_EL` thread-local for the duration of the hook,
reachable via `pocopine::current_el()`. Already exists for
directives; could be extended. Downside: implicit-argument
smell, magic to explain, vs a visible parameter.

### 8.4 Opt-in via trait method override

Have two trait methods — `on_ready(&self)` and
`on_ready_with_ctx(&self, ctx: HookCtx)` — and let the user
implement whichever. Clutters the trait surface.

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

- **Should `on_mount` always take `HookCtx` while `on_ready`
  stays optional?** `on_mount` is where most DOM work happens
  (install listeners, stamp attributes); `on_ready` is more
  about "wait for subtree" than "touch my own element."
  Counter-argument: consistency is cheap, and some `on_ready`
  sites (install focus trap, anchor positioning after teleport
  commits) genuinely need the root.
- **What goes in v1.1.** Obvious candidates for later
  additions to `HookCtx`:
  - `parent_scope_id: Option<ScopeId>` — avoids a
    `context::parent_of` lookup.
  - `refs: &RefMap` — pre-walked `pp-ref` collection, so
    authors could write `ctx.refs["menu"]` instead of
    `refs::get_on(ctx.scope_id, "menu")`.
  - `is_teleported: bool` — for overlays that branch on it.
  Land them as fields; existing code keeps compiling because
  `HookCtx` is `#[non_exhaustive]` and `Copy`.
- **Naming.** `HookCtx` vs `LifecycleCtx` vs `MountCtx`. Single
  `HookCtx` for both hooks keeps the type count down;
  per-hook structs would let us add fields that only make
  sense in one (e.g. a "walk epoch" on mount but not ready).
  v1 ships one `HookCtx` for simplicity.
