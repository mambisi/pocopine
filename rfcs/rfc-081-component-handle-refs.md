# RFC 081 - Component handle refs

| Field | Value |
|---|---|
| **Status** | Draft (Phase 1 landed) |
| **Author** | pocopine team |
| **Created** | 2026-05-19 |
| **Related** | [`rfc-002-app-stores-servers.md`](./rfc-002-app-stores-servers.md), [`rfc-060-component-uses-registry.md`](./rfc-060-component-uses-registry.md), [`rfc-079-pine-richtext-tables-extension.md`](./rfc-079-pine-richtext-tables-extension.md) |
| **Supersedes** | - |

## 1. Summary

Today `pp-ref="name"` registers the tagged DOM element under
its enclosing scope's ref table. When the tagged element
happens to be a child *component*'s host, the parent gets
the DOM element but **not** a typed handle to the child
component's Rust state. Reaching across a component
boundary therefore means a DOM query under the parent's
own ref (`form_root.query_selector("pine-rich-text-root")`,
then `Editor::find(&form_root)`).

Extend `pp-ref` so that — when the tagged element IS the
host of a child component — the parent can also resolve a
typed `Handle<ChildComponent>`. Adds one method on
`Refs` / one matching free function. No new macro, no new
visibility attribute: Rust `pub` is already the "expose
this method to outside callers" mechanism.

```rust
// Child component owns a typed surface handle.
impl KeepNoteBody {
    pub fn editor(&self) -> Option<pine_richtext::view::Editor> {
        self.editor.clone()
    }
}

// Parent's template — pp-ref on the child component tag.
<keep-note-body pp-ref="body" />

// Parent's handler — typed reach across the boundary.
fn save(&mut self) {
    let body = refs.get_component::<KeepNoteBody>("body")?;
    let md = body.with(|b| b.editor()?.get::<Markdown>().ok())?;
    // …
}
```

## 2. Motivation

**Current keep flow.** `KeepNoteForm` mounts a
`<keep-note-body>` child which in turn mounts a
`<pine-rich-text-root>`. The parent form needs to read the
surface's markdown at save time. Today's options are all
unsatisfying:

1. `pp-model:markdown` two-way binding — every keystroke
   emits, parent re-seeds, feedback-loops, full re-render
   per character. Rejected.
2. `pp-ref="form_root"` on the form's wrapper +
   `Editor::find(&form_root)` which `querySelector`s down
   to the inner `pine-rich-text-root`. Works but is a DOM
   drill, sensitive to template structure, and inert to
   the child's TYPED API — the parent treats the child as
   an opaque DOM subtree.
3. Cache the surface's element in a thread-local keyed by
   form mode. Hidden global state, opaque lifetimes,
   needs `on_scope_unmount` cleanup. Already tried and
   removed.

The DOM-drill option (option 2) is currently shipping but
it leaks pine-richtext's selector knowledge into keep, and
the keep refactor cycle had to add the
`Editor::find(&form_root)` plumbing on every read.

**The same shape recurs.** Anywhere a parent component
hosts a child that owns nontrivial imperative state —
`<keep-note-body>` (rich-text), `<pine-popover-root>`
(open-state, anchor element), `<pine-select-root>`
(selected value, listbox id), upcoming
`<pine-virtual-list>` (scroll position, item bounds) — the
parent benefits from a typed reach across the boundary.
Today the contract is either "child mirrors state through
`pp-model`" or "parent does its own DOM walk." Neither
generalizes.

**Vue's model.** Vue 3 puts `ref` on a child component
tag and gets back the child's exposed instance —
`defineExpose({ editor })` in the child, `myChild.value
.editor.getJSON()` in the parent. TipTap-Vue uses this
exact pattern: `useEditor` returns `shallowRef<Editor>`,
the parent reaches it through the component ref.

Pocopine is closer to this than it looks — `pp-ref` and
Vue's template refs are otherwise interchangeable — so the
gap is small.

## 3. Goals

- Let a parent reach a typed `Handle<ChildComponent>` for a
  child component tagged with `pp-ref="name"`, without
  introducing new template syntax.
- Reuse the existing `Handle<T>` shape and its
  `with` / `update` semantics — no new owned-reference type
  or borrow rules.
- Keep the API surface minimal: one new method on
  [`Refs`](../crates/pocopine-core/src/lifecycle.rs#L249)
  and one free-fn counterpart in
  [`pocopine::refs`](../crates/pocopine-core/src/refs.rs#L52),
  no other public-API additions.
- Avoid a new `#[expose]` macro. Rust `pub` already
  decides what crosses the boundary; `Handle<T>::with` /
  `update` already gate access through a borrow.
- Resolve in O(1) at handler time — no DOM walk, no
  re-query per call.
- Keep the existing `pp-ref` → `Element` lookup unchanged:
  a parent that needs the DOM element keeps using
  `refs.get` / `refs.get_as`.
- Mirror the cross-scope rules of `pp-ref`: refs
  registered in scope A are reachable only from handlers
  running in scope A (no implicit cross-tree access).

## 4. Non-goals

- No `defineExpose`-style selective visibility — Rust's
  `pub` / `pub(crate)` already gates which methods cross
  the boundary; we don't add a parallel declarative system.
- No reactive subscription to a child handle's internals
  (subscribe via the child's own update channels, e.g.
  `Editor::on_update`).
- No DOM-element resolution change. `refs.get("name")`
  keeps returning the Element. Parents that want both can
  call both — the lookups are independent.
- No async-handle resolution. Like `refs::get`, the lookup
  is synchronous from inside scope context and returns
  `None` outside it.
- No new `pp-ref` syntax. The directive shape stays
  identical; the *resolution* gains a typed variant.

## 5. Design

### 5.1 Public API

Add one method on the [`Refs`](../crates/pocopine-core/src/lifecycle.rs#L249)
extractor and the matching free functions in
[`pocopine::refs`](../crates/pocopine-core/src/refs.rs):

```rust
// In pocopine_core::lifecycle::Refs.
impl<'a> Refs<'a> {
    /// Resolve a typed `Handle<T>` for the child component
    /// whose host element was tagged `pp-ref="name"` in this
    /// scope's template. Returns `None` when the named ref
    /// isn't a child-component mount point or the registered
    /// child's Rust type doesn't match `T`.
    pub fn get_component<T: 'static>(&self, name: &str) -> Option<Handle<T>>;
}

// In pocopine_core::refs (free functions, current-scope variant).
pub fn get_component<T: 'static>(name: &str) -> Option<Handle<T>>;
pub fn get_component_on<T: 'static>(scope_id: ScopeId, name: &str)
    -> Option<Handle<T>>;
```

`Handle<T>` is the existing [`pocopine_core::handle::Handle`](../crates/pocopine-core/src/handle.rs#L42).
No new type. Authors call `body.with(|b| b.field)` / `body.update(|b| ...)`
exactly as they do for `Parent<T>::handle()`.

The name `get_component` distinguishes from `get_as`
(DOM-type downcast); the two are orthogonal lookups that
happen to share the same ref-name keyspace.

### 5.2 Worked example (keep)

Before:

```html
<!-- KeepNoteForm.poco -->
<div pp-ref="form_root">
  <keep-note-body ...></keep-note-body>
</div>
```

```rust
// Save side — DOM drill.
let combined = Self::form_root_in_scope()
    .as_ref()
    .and_then(Editor::find)
    .and_then(|e| e.get::<Markdown>().ok())
    .unwrap_or_else(|| self.body.clone());
```

After:

```html
<!-- KeepNoteForm.poco -->
<div>
  <keep-note-body pp-ref="body" ...></keep-note-body>
</div>
```

```rust
// KeepNoteBody.rs — opt in by exposing a `pub` method.
impl KeepNoteBody {
    pub fn editor(&self) -> Option<pine_richtext::view::Editor> {
        self.editor.clone()
    }
}

// KeepNoteForm.rs — typed cross-boundary reach.
let combined = pocopine::refs::get_component::<KeepNoteBody>("body")
    .and_then(|body| body.with(|b| b.editor()?.get::<Markdown>().ok()))
    .unwrap_or_else(|| self.body.clone());
```

No DOM query. No selector knowledge. The parent depends on
the child's typed surface, not its template structure.

### 5.3 Implementation sketch

The mechanism is smaller than a first read suggests: the
existing `pp-ref` registers an [`Element`], and the mount
already stamps the inner template root with a private
`SCOPE_ID_KEY`. The only missing piece is stamping the
**custom-element host** with the same key — every other
hop in the resolution composes existing primitives.

#### 5.3.1 Stamp the host with the child's scope id

In [`crate::mount::mount_component`](../crates/pocopine-core/src/mount.rs#L286)
(and `try_mount_component_as` for the `pp-as` path), set
`SCOPE_ID_KEY` on `el` (the custom-element host) right
after the template root is bound:

```rust
// Existing: bind the inner template root.
set_private(&root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
set_private(&root, SCOPE_PROXY_KEY, &proxy);

// New: also stamp the outer custom-element host with the
// child's scope id. The host doesn't carry SCOPE_PROXY_KEY
// — proxy reads still go through the inner template root.
set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
```

The pp-ref directive is unchanged. When a parent template
contains `<child-tag pp-ref="body">`, the host element is
both (a) registered in the parent scope's ref table via
the existing `refs::register` and (b) stamped with the
child's scope id by the mount above.

#### 5.3.2 Resolution

```rust
pub fn get_component_on<T: 'static>(
    parent_scope: ScopeId,
    name: &str,
) -> Option<Handle<T>> {
    let el = get_on(parent_scope, name)?;
    let child_scope = crate::mount::scope_id_of_element(&el)?;
    if child_scope == parent_scope {
        // Plain DOM ref or self-ref — reject.
        return None;
    }
    let scope = crate::scope::Scope::find(child_scope)?;
    let rc = scope.typed::<T>()?;
    Some(Handle::new(rc, child_scope))
}

pub fn get_component<T: 'static>(name: &str) -> Option<Handle<T>> {
    let scope = current_scope_id()?;
    get_component_on(scope, name)
}
```

`Scope::typed::<T>()` and `Handle::new` are the same primitives the
existing [`Parent<T>` / `NearestParent<T>`](../crates/pocopine-core/src/extractors.rs#L143)
extractors use; this lookup is the mirror direction (parent →
named child) of those (child → ancestor T).

The `child_scope == parent_scope` guard catches two cases
without extra plumbing: a `pp-ref` on a plain DOM element
in the same scope (no `SCOPE_ID_KEY` ⇒ `scope_id_of_element`
returns `None`, lookup misses), and a `pp-ref` on the
component's own template root (`SCOPE_ID_KEY` matches the
caller's scope ⇒ guarded against).

#### 5.3.3 Eviction

Component scopes evict via
[`Scope::remove`](../crates/pocopine-core/src/scope.rs#L322).
When the parent's scope is removed (or its sub-tree is torn
down by `pp-if` / `pp-for`), the existing
`refs::clear_scope(parent_id)` already drops the parent's
ref entries — including the entry pointing to the now-
removed child's host. No new teardown plumbing needed: the
resolution walks element → child scope → typed handle, and
each link breaks on its own when the underlying piece
unmounts.

#### 5.3.4 `Refs` extractor wiring

`Refs` already carries `scope_id`. Adding
`get_component::<T>` is one line:

```rust
impl<'a> Refs<'a> {
    pub fn get_component<T: 'static>(&self, name: &str) -> Option<Handle<T>> {
        crate::refs::get_component_on::<T>(self.scope_id, name)
    }
}
```

#### 5.3.5 Why not a parallel `COMPONENT_REFS` thread-local

An earlier draft of this RFC proposed registering child
scope ids in a separate `HashMap<ScopeId, HashMap<String,
ScopeId>>` keyed by `(parent_scope, name)`. The simpler
"stamp the host with `SCOPE_ID_KEY`" approach dropped:

- A new thread-local with its own lifecycle.
- A separate `clear_scope` call to evict component-ref
  entries.
- A walker hook to register entries at mount time.

…and reuses two primitives already present in `mount.rs`
([`scope_id_of_element`](../crates/pocopine-core/src/mount.rs#L127),
[`bind_scope_id_only`](../crates/pocopine-core/src/mount.rs#L119)).
Total Phase 1 delta: ~10 lines across mount.rs + refs.rs +
lifecycle.rs.

### 5.4 Why no `#[expose]` attribute

Vue needs `defineExpose` because the default — exposing
everything on the component instance — would leak template
locals, computed signals, and lifecycle hook state to the
parent. Rust has `pub` / `pub(crate)` / `pub(super)`; a
field or method is reachable iff its declared visibility
permits it. `Handle<T>::with(|t: &T| ...)` only sees what
the calling crate's visibility rules allow.

A child that wants to expose just one method writes:

```rust
impl ChildComponent {
    pub fn focus(&self) { ... }      // exposed
    fn private_helper(&self) { ... } // not exposed
}
```

…and parents in any crate can call `body.with(|c| c.focus())` but
cannot reach `private_helper`. No new attribute needed.

### 5.5 Why `Handle<T>`, not a custom wrapper

`Handle<T>` is already the canonical "carry a typed
component reference across handler boundaries" primitive
([`Handle::with`](../crates/pocopine-core/src/handle.rs#L100),
[`Handle::update`](../crates/pocopine-core/src/handle.rs#L116)).
Authors already use it for `this::<Self>()`, `Parent<T>`,
`NearestParent<T>`, context injection. Reusing it avoids
a new mental model and inherits the existing borrow-safety
guarantees.

A custom `ChildRef<T>` wrapper would either be a Handle
re-export (pure noise) or weaken the borrow rules (lose
the borrow-safety check). Neither is worth it.

## 6. Implementation phasing

### Phase 1 — Host stamp + resolution API ✅

- Stamp `SCOPE_ID_KEY` on the custom-element host in
  [`mount_component`](../crates/pocopine-core/src/mount.rs#L286)
  and `try_mount_component_as` (currently only the inner
  template root receives it).
- Add free-fn `refs::get_component` /
  `refs::get_component_on` returning `Option<Handle<T>>`.
- Add `Refs::get_component` extractor method.
- Browser tests
  (`crates/pocopine-core/tests/component_refs.rs`):
  matching-type returns `Some`, mismatched type returns
  `None`, plain DOM ref returns `None`, unknown ref name
  returns `None`, self-scope ref returns `None`,
  `clear_scope` evicts.

### Phase 2 — Macro + walker integration tests

- End-to-end test: a parent `#[component]` template with
  `<child-tag pp-ref="body">` resolves the typed handle in
  a handler. Confirms the macro-emitted plan reaches the
  host stamp in the right order.
- Edge case: `pp-if` swap. The parent's pp-ref points to
  the swapped-out child's element; after remount, the
  ref points to the new child's host (with the new
  scope id) → `get_component` resolves the post-swap
  child.
- Edge case: `pp-for` rows. Each row's pp-ref entry
  resolves to that row's child scope (one ref per row).

### Phase 3 — Documentation + migration

- Add a `ref_handles` example under
  `examples/` (or extend the existing pine examples) that
  shows the worked pattern end-to-end.
- Update keep's `save` / `auto_save` / `schedule_*` paths
  to drop `Editor::find(&form_root)` in favor of
  `refs::get_component::<KeepNoteBody>("body")?`.
- Add a memory note pointing future Claude sessions at the
  new API for the cross-component-handle case.

### Phase 4 (optional follow-up) — `Parent<T>` symmetry

`Parent<T>` extracts a typed handle to the immediate
parent component. After this RFC lands, the parent-side
mirror is `Refs::get_component::<Child>("name")` —
together they form a complete bi-directional reach. A
later RFC can unify the two as a single `RefHandle<T>` /
`Parent<T>` story if a third-party crate would benefit.

## 7. Open questions

- **Multiple children of the same type.** The keep
  pattern already uses unique pp-ref names per child;
  this RFC doesn't need to enumerate "all children of
  type T." If that becomes a load-bearing use case
  (e.g. an autocomplete that needs to focus the *first
  visible* `<pine-select-root>` among many), a follow-up
  can add `refs::components::<T>() -> Vec<Handle<T>>`.
- **Diagnostics on mismatch.** When `get_component::<Foo>(..)`
  returns `None` because the registered scope is of type
  `Bar`, do we silently return `None` (matches today's
  `get_as` behavior) or panic with a typed message? My
  default: return `None`; surface a `tracing::warn!` with
  the actual type name so authors can diagnose without
  guarding every call. Open for review.
- **`pp-ref` on a slot.** If a slot transcludes a child
  component AND the parent puts `pp-ref` on the slot
  outlet, which scope owns the registered ref? Current
  proposal: the slot's defining scope (consistent with
  template-walk parentage). Needs a test once Phase 1
  lands.

## 8. Alternatives considered

### 8.1 Status quo: `Editor::find(&form_root)`

What we shipped today: parent caches a wrapper ref, the
typed handle library does a `querySelector` for the
child's component tag. Works but leaks template structure
into call sites, requires every typed handle library to
ship a `find` helper, and provides no path to typed
methods the child wants to expose beyond what the library
author thought to add at `find` time.

### 8.2 `defineExpose`-style attribute

`#[expose] pub fn editor(...)` on the child, mirrored by
codegen into a typed proxy. Adds a macro, a new visibility
modifier, and synchronization with Rust's existing `pub`
system. Not worth the surface — Rust visibility already
does this job.

### 8.3 Context injection

The child writes a [`create_context!`](../crates/pocopine-core/src/context.rs#L264)
holding `Handle<Self>`, the parent reads it via `Inject<...>`.
Works for the deeply-nested case but requires the child
to opt in with module-level boilerplate per exposed
handle, and the parent then has no name keyed lookup —
context is one-instance-per-scope-subtree. Useful when
you don't know the child's exact mount point;
overpowered for the named-child case.

### 8.4 Reactive subscription via `pp-model`

The original keep shape. Defeated by feedback loops and
`tick::next` mirror lag. Documented in the
`feedback_richtext_no_two_way_binding` memory note. Not
revisited.

## 9. Risks

| Risk | Mitigation |
|---|---|
| Walker hook fires too late (mount happens after refs registration) → name resolves to old child after a `pp-if` swap | `register_component` is called by the same code path that materializes the child scope; subsequent swaps run `clear_scope` on the parent before remount. Add a test that swaps a child via `pp-if` and confirms post-swap lookup returns the new child. |
| `Handle<T>` outlives its scope (parent grabs a handle, child unmounts, parent's `with`/`update` panics) | Same risk as `Parent<T>` today. `Handle::with` already guards via `Scope::find` (returns gracefully on missing scope). Add a regression test for the unmount race. |
| Two siblings with same `pp-ref` name in a `pp-for` row | `pp-ref` inside a `pp-for` row is already documented as last-wins. Component refs follow the same rule. Considered for follow-up but out of scope here. |
| Increased thread-local memory footprint | `COMPONENT_REFS` is one extra `HashMap<ScopeId, HashMap<String, ScopeId>>`. Bounded by number of named child refs per scope (typically 0–5). Trivial. |

## 10. Verification

Per checkpoint:

```bash
cargo fmt --check -p pocopine-core
cargo clippy -p pocopine-core --target wasm32-unknown-unknown -- -D warnings
cargo test -p pocopine-core --lib
# After Phase 3 (keep migration):
cargo build -p keep-example --target wasm32-unknown-unknown
RICHTEXT_SMOKE_PORT=5249 npx playwright test --reporter=line
```

New tests added by this RFC:

- `refs::get_component_returns_typed_handle_for_named_child`
- `refs::get_component_returns_none_for_mismatched_type`
- `refs::get_component_evicts_with_scope`
- `refs::get_component_after_pp_if_swap_resolves_new_child`
- `refs::get_component_outside_scope_context_is_none`
- Phase 3: keep `save` flow continues to round-trip
  markdown after dropping `Editor::find(&form_root)` (one
  Playwright test in the keep suite, or a wasm-pack
  bindgen test if keep doesn't have Playwright yet).
