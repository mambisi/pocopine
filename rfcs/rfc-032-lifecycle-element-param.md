# RFC 032 — Pipe `&Element` to `on_mount` / `on_ready`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [RFC 001](./rfc-001-components.md), [RFC 026](./rfc-026-post-mount-watch-field.md), [RFC 029](./rfc-029-on-ready-rename.md) |

## 1. Summary

Let `on_mount` and `on_ready` receive the component's **rendered
root element** as a parameter. Today those hooks get `&self` /
`&mut self` only; to touch the DOM they re-derive the element
via `refs::get_on(current_scope_id().unwrap(), "…")` — four
lines of boilerplate in 20+ of Pine's ~38 hook sites.

```rust
// Current — opt-in DOM access via pp-ref + registry lookup.
pub fn on_ready(&self) {
    let Some(scope) = current_scope_id() else { return };
    let Some(content) = refs::get_on(scope, "content") else { return };
    overlay::activate(scope, &content, /* ... */);
}

// Proposed — element is a first-class hook parameter.
pub fn on_ready(&self, root: &web_sys::Element) {
    if let Some(scope) = current_scope_id() {
        overlay::activate(scope, root, /* ... */);
    }
}
```

The parameter is **optional at the call site** — `#[handlers]`
detects which signature the author wrote and generates the right
forwarder. Old code keeps working unchanged; new code can opt in.

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

### 4.1 Supported handler signatures

The `#[handlers]` macro accepts both shapes for each of
`on_mount` and `on_ready`:

```rust
// No element (unchanged — existing code).
fn on_mount(&mut self) { … }
fn on_ready(&self) { … }

// With element — RFC-032 opt-in.
fn on_mount(&mut self, el: &web_sys::Element) { … }
fn on_ready(&self, el: &web_sys::Element) { … }
```

The parameter name is author-chosen; convention is `el` or
`root`. Type is exactly `&web_sys::Element`. An incorrect type
is a compile error (rustc); an extra parameter past the element
is a macro error with the message *"on_ready takes at most one
element parameter (RFC-032)"*.

### 4.2 `on_setup` / `on_unmount` — unchanged

```rust
fn on_setup(&mut self) { … }
fn on_unmount(&mut self) { … }
```

No alternate signatures. As §3 notes, `on_setup` runs before
the element exists; `on_unmount` runs during teardown where DOM
poking is a footgun.

### 4.3 What element is passed

The **rendered root** — the element that `SCOPE_ID_KEY` is
pinned on. Concretely:

- **Normal mount path:** the first element child of the
  custom-element tag (the template root).
- **`pp-as` path:** the hoisted user element (`user_root` —
  RFC-019).
- **`pp-for` / `pp-if` clones:** the cloned root.

Same element `refs::get_on(scope, "root")` would return when
the template defines `pp-ref="root"` on its root. RFC-032 makes
this the default rather than opt-in.

### 4.4 `ComponentState` trait

```rust
pub trait ComponentState: 'static {
    // …
    fn mount(&mut self, el: &Element) {}
    fn on_ready(&self, el: &Element) {}
    // …
}
```

Default impls ignore the element, so non-component
`ComponentState` impls (if any ever appear) keep compiling.

## 5. Implementation

### 5.1 Walker call sites

`walker::fire_mount_hook(el)` already has `el` in scope — it's
the rendered root it walked. Pass it into `mount` and the
subsequent `on_ready` schedule:

```rust
// In fire_mount_hook (walker.rs ~line 165):
if has_mount {
    scope::with_current_scope_id(id, || {
        scope.state.borrow_mut().mount(el);          // NEW: &el
    });
}
if has_ready {
    let el_owned = el.clone();
    tick::next(move || {
        if let Some(sc) = Scope::find(id) {
            scope::with_current_scope_id(id, || {
                sc.state.borrow().on_ready(&el_owned);  // NEW: &el_owned
            });
        }
    });
}
```

`tick::next` fires after the microtask queue drains; we clone
`el` into the closure so the call site owns it. Cheap —
`Element` is a wasm-bindgen JsValue handle, so clone is a ref
bump.

### 5.2 `#[handlers]` macro

Introspect each user method's signature. For `on_ready`:

```rust
match user_method.sig.inputs.len() {
    1 => quote! { fn on_ready(&self, _el: &::web_sys::Element) { Self::on_ready(self); } },
    2 => quote! { fn on_ready(&self, el: &::web_sys::Element)  { Self::on_ready(self, el); } },
    _ => compile_error!(…),
}
```

Same shape for `on_mount`. The trait definition always has two
arguments; the forwarder bridges to whichever the author wrote.

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
pub fn on_ready(&self, content: &web_sys::Element) {
    let Some(scope) = current_scope_id() else { return };
    let modal = inject(&ROOT).map(|r| r.with(|root| root.modal)).unwrap_or(true);
    overlay::activate(scope, content, modal);
    // … watch_scope_field …
}
```

Dialog's `PineDialogContent.poco` would drop the redundant
`pp-ref="content"` (the ref was there only to enable the
`get_on` lookup).

> **Caveat when `pp-ref` names disagree with "the root."** A
> component whose template is `<ul><slot/></ul>` but whose
> handlers need the inner user button (somewhere below `<slot>`)
> would still use `refs::get_on(scope, "user_btn")` — RFC-032
> only surfaces the rendered root. The pp-ref mechanism isn't
> going anywhere.

## 7. Edge cases

- **Scope id recovery inside the hook.** Handlers that still
  want `current_scope_id()` (for injecting, dispatching, or
  passing to `watch_scope_field`) keep using it — the element
  parameter doesn't replace that lookup.
- **Pointer stability across `on_ready` calls.** `on_ready`
  fires once per scope (via `tick::next`). The element is stable
  across that single invocation. No promise is made across hook
  types — `on_mount` may see a different element from
  `on_ready` in principle if the subtree was replaced between
  them, though today the walker guarantees they're the same.
- **`&mut self` vs `&self`.** `on_mount(&mut self, el: &Element)`
  — mutable borrow of state, shared borrow of element.
  Standard Rust multi-borrow; no aliasing issue since `el` is a
  JsValue wrapper, not a reborrow of `self`.
- **Element cloning for async moves.** Authors spawning async
  tasks that need the element clone it: `let el = el.clone();`
  — cheap because `Element` wraps a JS handle.

## 8. Alternatives considered

### 8.1 Always break, no detection

Force every `on_mount` / `on_ready` to take the element
parameter. Simpler macro, noisier per-site (authors who don't
care write `_el: &Element`). RFC-031 did take the breaking
route; this RFC leans additive because the boilerplate burden
is genuinely asymmetric: ~53% of hooks benefit, ~47% don't.

### 8.2 Context struct

Pass `&HookCtx { el, scope_id }`. Bundles scope access too,
but authors who want `current_scope_id` already have it without
the bundle. Extra type for marginal value.

### 8.3 Thread-local current element

Set a `CURRENT_EL` thread-local for the duration of the hook,
reachable via `pocopine::current_el()`. Already exists for
directives; could be extended. Downside: implicit-argument
smell, magic to explain, vs a visible parameter.

### 8.4 Opt-in via trait method override

Have two trait methods — `on_ready(&self)` and
`on_ready_with_el(&self, el: &Element)` — and let the user
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

- **Should `on_mount` always get `&Element` and `on_ready`
  stay optional?** `on_mount` is where most DOM work happens
  (install listeners, stamp attributes); `on_ready` is more
  about "wait for subtree" than "touch my own element."
  Counter-argument: consistency is cheap, and some `on_ready`
  sites (install focus trap, anchor positioning after teleport
  commits) genuinely need the root.
- **Name the parameter in docs.** Settle on `el`, `root`, or
  `element`. Pine's code already reads `el` everywhere —
  recommend `el` for brevity, and `root` when the author wants
  to emphasize role in templates where `pp-ref="root"` would
  otherwise resolve to it.
