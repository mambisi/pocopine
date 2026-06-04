# RFC 046 — `Children` extractor on `LifecycleContext`

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-04-24 |
| **Supersedes** | — |
| **Related** | [RFC 011](./rfc-011-scoped-slots.md), [RFC 022](./rfc-022-pp-roving.md), [RFC 032](./rfc-032-lifecycle-element-param.md), [RFC 047](./rfc-047-slots-magic.md) |

## 1. Summary

Add a `Children<'a>` extractor to the RFC-032 `LifecycleContext`
surface. `on_mount` / `on_ready` handlers that need to iterate,
count, or type-cast the **direct rendered children of the
component's rendered root** declare a parameter of type
`Children` and receive a read-only hook-time view over those
children.

Scope is deliberately narrow: this is **root-child
introspection**, not a general slotted-descendant API. Deep
descendant queries (find-all-nested-`<pine-tree-item>`,
query-for-a-teleported-listbox) stay on the author-written
`query_selector` path; `Children` covers the "direct children
of *my* root" case, which is the one RFC 032's `El` extractor
stops one level short of.

Slot-**presence** (answering "did the user pass
`<template pp-slot="footer">`?") is the companion question
handled by RFC 047 via `$slots.footer` in templates and
`Children::has_slot("footer")` / `slots::has(scope_id, "footer")`
from Rust. RFC 046 is presence-free: iteration and typed
access only.

```rust
// PineRadioGroupRoot.poco renders <root><slot/></root>, so each
// <pine-radio-group-item> the user passes ends up as a direct
// child of the rendered root. ARIA metadata is per-direct-child:
// this is exactly the case Children::iter() was designed for.
#[handlers]
impl PineRadioGroupRoot {
    pub fn on_mount(&mut self, children: Children) {
        let total = children.len();
        for (i, item) in children.iter().enumerate() {
            let _ = item.set_attribute("aria-setsize", &total.to_string());
            let _ = item.set_attribute("aria-posinset", &(i + 1).to_string());
        }
    }
}
```

`Children` is a zero-cost projection of `LifecycleContext`: it
carries the rendered root element and scope id, and resolves
iterators lazily. It does **not** introduce reactivity, does
**not** replace `<slot>` (RFC 011), and does **not** force
single-call-site semantics — a handler can hold onto it for the
duration of the hook.

## 2. Motivation

### 2.1 The pattern `<slot>` alone can't express

Compound primitives routinely need to *reason* about their
children at mount time:

* **ARIA metadata.** `aria-setsize`, `aria-posinset`,
  `aria-activedescendant` all require knowing how many children
  exist and assigning per-index values. Today that's a DOM walk
  through `self_el.children()` the author writes by hand.
* **Keyboard navigation setup.** `pp-roving` (RFC 022) already
  covers the declarative case, but programmatic setup — e.g.
  "focus the first enabled item when the menu opens" — needs a
  typed handle on the children list.
* **Conditional behaviour keyed on count.** "Run the compact
  layout when there are more than five direct items" is
  `children.len() > 5`; today that's an `el.children().length()`
  + cast dance in a reactive bool.
* **Child mutation observation.** `MutationObserver` on the root
  needs an anchor; `Children::root()` gives it one line.

Today those handlers all open with the same boilerplate: derive
`scope_id`, fetch the root via `refs::get_on(scope, "root")`,
call `.children()`, cast each to `HtmlElement`, iterate. RFC 032
made the root extraction typed (`El`); this RFC does the same
for "one level down."

### 2.2 What authors have today

pocopine already ships one slot-reflection API — and Pine
primitives universally avoid it. The gap between "exists" and
"used" is the whole reason this RFC exists.

**The existing `Slots` extractor.** Introduced in RFC 032 §4.3
Tier 4; implemented at `crates/pocopine-core/src/lifecycle.rs:357-363`:

```rust
pub struct Slots(pub Vec<String>);

impl<'a> From<LifecycleContext<'a>> for Slots {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        Slots(slots::names_for(ctx.scope_id))
    }
}
```

What you get: a `Vec<String>` of slot names captured at mount
(`"default"` plus any `pp-slot="name"` the user supplied). No
DOM, no fragments, no element counts — names only. Useful for
"did the user declare a named slot at all?" and not much else.

**Where slots are actually stored.** The walker captures user
content into a thread-local at `crates/pocopine-core/src/slots.rs:39-50`:

```rust
// One entry per mounted component scope.
STORES: HashMap<ScopeId, SlotStore>
// Each SlotStore carries a by_name: HashMap<String, UserSlot>,
// each UserSlot holds the DocumentFragment + pp-let ident +
// owner scope/proxy. Populated at walker.rs:352 via
// slots::put(scope_id, slot_store); capture at walker.rs:665-730.
```

`Slots` exposes the keys of `by_name`. Everything else — the
fragments, the default bucket's contents, the rendered output
— is internal.

**What Pine does instead.** Every primitive that reasons about
its children today skips `Slots` and walks the DOM from a
lifecycle hook. The sites below illustrate the *broader*
ergonomics gap around child/descendant introspection — not all
of them are direct-root-child walks, and this RFC's v1
`Children` does not directly replace every one:

| Primitive | File:line | What it does | `Children` v1 replaces? |
|---|---|---|---|
| `PineRadioGroupRoot` / menu-style compounds | (any primitive that stamps ARIA on its own direct children) | `el.children()` + per-item `set_attribute("aria-posinset", …)` | **Yes** — direct-child iteration is exactly this API |
| `PineTreeItem` | `crates/pine/src/tree/mod.rs:278-284` | `el.query_selector("pine-tree-item")` — finds any nested item anywhere in the subtree → `has_children: bool` | **No** — descendant query, not root-child iteration |
| `PineSplitterGroup` | `crates/pine/src/splitter/mod.rs:435-454` | iterate `parent.children()` of a container to count `<pine-splitter-panel>`s for sizing | **Partially** — the walk itself fits, but the "on a specific sibling container, not on `El`" twist still needs a manual `query_selector` to find the container first |
| `PineCombobox` | `crates/pine/src/combobox/mod.rs:341, 555` | `query_selector` for a user-placed listbox / teleported option wrapper | **No** — descendant query + teleport-crossing lookup |

The shared pattern beneath all of them:

1. Pull `El` via the RFC-032 extractor.
2. Optionally `tick::after_flush()` when the *descendant* you
   care about isn't guaranteed mounted (see §4.5 for what
   *direct* children guarantee).
3. `el.query_selector("…")` or iterate `el.children()`.
4. Stash the result in a reactive `bool` / `usize` field.
5. Template reads the field via `pp-show` / `pp-class` /
   `{{ …}}`.

**Why this pattern is load-bearing but awkward — and what v1
`Children` does and doesn't help with.**

- **What it helps.** Primitives whose interest is the direct
  children of their own rendered root — ARIA metadata stamping
  (`aria-setsize`, `aria-posinset`), "focus the first enabled
  child," counting rendered items. All of those collapse into
  a one-line `children.iter()` / `children.len()` /
  `children.is_empty()` read from a hook.
- **What it doesn't.** Arbitrary descendant queries
  (`pine-tree-item` nested anywhere; a teleported listbox
  mounted under `<body>`; cross-container sizing) stay on the
  hand-written `query_selector` path. v1 `Children` doesn't
  try to be a tree walker or a teleport-aware finder.
- **Slot-presence probes live in RFC 047.** "Did the user
  pass a `<template pp-slot="footer">`?" is a template concern
  first and a handler concern second, so it ships as
  `$slots.footer` (template) + `Children::has_slot("footer")`
  / `slots::has(scope_id, "footer")` (Rust) in RFC 047, not
  here. This RFC stays focused on iteration + typed access.
- **Typed casting.** Per-item `dyn_into` boilerplate collapses
  into `children.get_as::<T>(i)`.

### 2.3 Why not just `el.children()`?

Authors *can* write `el.children()` today with the RFC-032 `El`
extractor. Three reasons to wrap it:

1. **Typed casting in one line.** `children.get_as::<HtmlLiElement>(0)`
   beats `el.children().item(0).and_then(|e| e.dyn_into().ok())`.
2. **Consistent surface with other extractors.** `Children`
   sits alongside `El`, `Refs`, `HostEl` — same teaching story,
   same "picked at parameter time" affordance.
3. **Collapses the reactive-bool dance for direct-child cases.**
   When a primitive's question is "how many direct children do I
   have?" or "any at all?", today's pattern (`el.children()` →
   set `has_children: bool` → template reacts) compresses to a
   one-line `children.is_empty()` / `children.len()` read in
   the hook. This applies *only* where direct-root-child
   semantics suffice; the `tick::after_flush` uses that exist
   specifically to wait on nested-component mounts (e.g.
   `PineTreeItem`'s descendant scan at `tree/mod.rs:278`) stay
   on the async path — the lifecycle contract in §4.5 makes
   that distinction precise.

### 2.4 Prior art

The user-visible name `Children` is deliberate — every major
reactive framework exposes the same concept under the same name.

| Framework | Shape | Notes |
|---|---|---|
| **Vue 3** | `useSlots()` / `this.$slots` | Map of slot-name → VNode render function. `$slots.default?.().length` to count. VNode-level, not DOM-level. |
| **Svelte 4** | `$$slots.default` (bool) + `<slot>` | Boolean per named slot; no iteration of children. |
| **Svelte 5** | `children` prop (snippet) | `let { children } = $props()`; rendered via `{@render children()}`. Snippets are opaque — no list-of-children API. |
| **React** | `React.Children.count` / `.map` / `.toArray` | Utility module over the `children` prop; closest analogue to this RFC. |
| **Solid** | `children()` helper | `const c = children(() => props.children)` — returns resolved DOM nodes; iterable as an array. |

Pocopine's model is closest to Solid's: slots are materialised
into real DOM at mount time, and "children" is therefore a
live-DOM concept, not a VNode concept. This RFC surfaces that
DOM cleanly without inventing new mental model.

## 3. Non-goals

* **No reactivity.** `Children` is a *snapshot* of the rendered
  children at the moment the hook fires. If the child set
  changes later (new `pp-for` iteration, `pp-if` toggle), the
  handler observes that via the normal directive path or a
  `MutationObserver` it installs itself. We don't wrap a
  reactive signal around the children list; that's a larger
  design question (see §6.3).
* **No write API.** `Children` is read-only. Mutating the DOM
  via appendChild / removeChild bypasses the walker's scope
  bookkeeping — same hazard as today. Authors who need to
  render children dynamically use `pp-for` / `pp-if` in the
  component's own template.
* **No slot-presence probes.** Answering "did the user pass
  `<template pp-slot="footer">`?" is RFC 047's scope
  (`$slots.footer` in templates, `Children::has_slot` /
  `slots::has` in Rust). RFC 046 is iteration + typed access
  only — the two surfaces complement each other but are spec'd
  separately.
* **No slot-content iteration for unrendered slots.**
  Iterating the nodes of an unrendered slot
  (`children.of_slot_source("item")`) is not in v1 — rendered
  output is the canonical surface, and `<slot>` materialises
  exactly once when the walker hits it.
* **No generic `Children<T>` typing.** The element type is
  always `web_sys::Element`; authors cast per-item via
  `get_as::<T>()`. Typing the whole collection would require
  constraining `<slot>` content at the component level (compile-
  time slot typing — see RFC 011 §10, deferred).
* **No override of `<slot>` semantics.** `<slot>` placement,
  named-slot resolution, default content fallback, and scoped
  slots (RFC 011) all continue to work exactly as today.
  `Children` is a read-only *reflection* of that machinery, not
  a replacement.

## 4. Design

### 4.1 `Children<'a>`

`Children<'a>` is a **hook-time** view: its lifetime is tied to
the `LifecycleContext<'a>` it was built from, so instances only
exist for the duration of the hook that received them. Methods
read a mix of the borrowed `Element` and (read-only) scope
registry state, but the struct itself is not designed to be
held across async boundaries or stashed in `self`. Authors who
want to query "does scope X have slot Y?" from an event handler
later use a separate scope-indexed API (see §4.3).

Surface:

```rust
/// Hook-time view of the direct rendered children of a
/// component's rendered root, plus a probe against the captured
/// slot map. Built from a `LifecycleContext` — see RFC 032.
///
/// Not intended to be held across async boundaries. The
/// element reference is borrowed from the lifecycle context;
/// for longer-lived queries use `scope` / `slots` accessors
/// directly.
pub struct Children<'a> {
    el: &'a web_sys::Element,
    scope_id: ScopeId,
}

impl<'a> From<LifecycleContext<'a>> for Children<'a> {
    fn from(ctx: LifecycleContext<'a>) -> Self {
        Children { el: ctx.el, scope_id: ctx.scope_id }
    }
}

impl<'a> Children<'a> {
    /// Top-level rendered children of the component's root.
    /// Excludes text nodes; returns `Element`s only, in document
    /// order.
    pub fn iter(&self) -> ChildrenIter<'_> { /* ... */ }

    /// Number of rendered `Element` children.
    pub fn len(&self) -> usize { self.el.children().length() as usize }
    pub fn is_empty(&self) -> bool { self.len() == 0 }

    /// The n-th rendered child, or `None`.
    pub fn get(&self, index: usize) -> Option<web_sys::Element> {
        self.el.children().item(index as u32)
    }

    /// The n-th child pre-cast via `dyn_into::<T>()`. Returns
    /// `None` on out-of-bounds or cast failure.
    pub fn get_as<T: wasm_bindgen::JsCast>(&self, index: usize) -> Option<T> {
        self.get(index).and_then(|e| e.dyn_into().ok())
    }

    /// Direct children whose local tag name (case-insensitive)
    /// matches `tag`. Yields in document order. Useful when a
    /// compound holds heterogeneous children and cares about one
    /// kind — e.g. a context menu iterating `pine-context-menu-item`
    /// while ignoring separators, labels, or groups.
    pub fn of_tag(&self, tag: &str)
        -> impl Iterator<Item = web_sys::Element> + '_
    { /* ... */ }

    /// Typed convenience — yields direct children whose tag
    /// matches `T::NAME` (the kebab-case tag emitted by
    /// `#[component]`). Refactor-safe: renaming the component
    /// automatically re-targets the filter.
    pub fn of<T: Component>(&self)
        -> impl Iterator<Item = web_sys::Element> + '_
    { self.of_tag(T::NAME) }

    /// Count of direct children matching `tag`. One-line sugar
    /// over `of_tag(tag).count()` for ARIA maths.
    pub fn count_of_tag(&self, tag: &str) -> usize { /* ... */ }

    /// Count of direct children matching `T::NAME`.
    pub fn count_of<T: Component>(&self) -> usize { self.count_of_tag(T::NAME) }

    /// The rendered root — same element `El` extractor returns.
    /// Handy for `MutationObserver` installs and for descendant
    /// searches (`children.root().query_selector_all(...)`) when
    /// the item you want isn't a direct child.
    pub fn root(&self) -> &web_sys::Element { self.el }
}
```

**On filtering semantics.** `of_tag` / `of::<T>()` match the
**local tag name** of each direct child — case-insensitive,
exact. `<pine-context-menu-item>` matches
`of_tag("pine-context-menu-item")`; `<pine-menu-item>`
doesn't. Components authored with a custom `name = "..."` on
`#[component]` still match because `T::NAME` carries that same
override.

**Direct children only — deliberately.** Real compounds nest
items under groups (e.g. `<pine-context-menu-group>
<pine-context-menu-item/> </pine-context-menu-group>`). If the
primitive wants *all* items regardless of nesting depth, it
reaches for the DOM's own descendant selector via
`children.root().query_selector_all("pine-context-menu-item")`
— that's a one-liner and the contract is clear. We don't add a
recursive `find_all::<T>()` in v1; the direct-child filter
covers the common case, and `root()` + `query_selector_all` is
the escape hatch for deeper searches.

`has_slot` is intentionally *not* on `Children`. Slot-presence
lives in RFC 047 — either via `$slots.name` in templates or
via the `Children::has_slot` extension method added there,
which delegates to `pocopine::slots::has(scope_id, name)`.

`ChildrenIter<'_>` is a thin adapter over `HtmlCollection` that
yields `web_sys::Element`; implementation detail.

### 4.2 What "children" means

The rendered children of the element that `El` extractor returns
— i.e. the scope's *rendered root* (RFC 032 §4.3). On the
normal mount path that's the template root; under `pp-as` it's
the hoisted user element; under `pp-for` clones it's the cloned
root.

For a template that uses `<slot>`, the `<slot>` placeholder is
replaced at mount time by (a) the user-provided slot content, or
(b) the `<slot>`'s default children. `Children::iter()` therefore
walks the *post-materialisation* DOM — the same elements the
browser rendered.

For a template with multiple wrapped children (e.g.
`<div><header/><slot/><footer/></div>`), `Children::iter()`
yields `header`, the slot's materialised root(s), and `footer`
— every element in the rendered root, in order.

### 4.3 Lifetime semantics

`Children<'a>` borrows `&'a Element` from `LifecycleContext<'a>`,
so its lifetime is the hook's call frame. Every method reads
either from that borrowed element or from its cached
`HtmlCollection` — nothing outlives the hook. Authors who
need a value past the hook clone what they need out of the
iterator (`let list: Vec<_> = children.iter().collect();`) or
stash individual elements via `web_sys::Element::clone()` (a
JS handle bump).

No method on `Children<'a>` reads scope-lifetime state. Slot-
registry access is intentionally routed through RFC 047's
module-level `slots::has(scope_id, name)`, which takes a bare
`ScopeId` and can be called from any code that has one —
without needing a live `Children` to be in scope. The split
keeps `Children<'a>`'s surface honest: hook-local object,
hook-local reads.

### 4.4 Tier placement (RFC 032 §4.3)

`Children` slots into **Tier 3 — domain-useful**. Non-trivial
value for primitives that do ARIA / keyboard work, but not
every hook needs it. Cost is a small view struct plus a
map lookup — pays for itself the first time a compound would
have hand-rolled `el.children()` + cast.

### 4.5 Lifecycle timing contract

The walker runs a post-order mount pass
(`crates/pocopine-core/src/walker.rs:102-124`, with
`fire_mount_hook` dispatched at line 122 *after* the recursive
child walk at 118-120; the comment at lines 137-140 spells the
invariant out: "Runs post-order so the handler sees the fully-
bound subtree"). `<slot>` materialisation happens synchronously
inside that recursive child walk, in-place (`walker.rs:103-106`
→ `materialize_slot` at 913-1059), *before* the parent's
`on_mount` fires.

The guarantees `Children` makes in each hook follow directly.

| Hook | What `children.iter()` sees | What it still doesn't guarantee |
|---|---|---|
| `on_setup(&mut self)` | **Not callable.** The rendered root doesn't exist yet; RFC 032 §3 already excludes this hook from the extractor surface. | — |
| `on_mount(&mut self, children: Children)` | Direct rendered children of the rendered root — including user-provided slot content (materialised in-place before this hook) and any nested custom-element tags *as elements* (scopes bound, refs registered). | That the *internal template* of a nested custom-element child has finished its first effect pass. Post-order mount means each descendant's `on_mount` has fired, but the reactive flush from their trigger scope is another microtask away. |
| `on_ready(&self, children: Children)` | Same DOM as `on_mount`, plus: the parent scope's first effect pass has settled (fired via `tick::next` — `walker.rs:188`, `tick.rs:18-25`). | That the DOM hasn't *later* been mutated by user directives (`pp-for` insertion, `pp-teleport` relocation) that ran on a different scope's microtask. |
| `on_unmount(&mut self)` | **Not callable** — extractors are excluded from teardown per RFC 032 §3. | — |

**What this contract buys us for §2.3 point 4.** The
`tick::after_flush` dance in `PineTreeItem`
(`crates/pine/src/tree/mod.rs:278`) exists to wait for *nested
`<pine-tree-item>` descendants* to have queried whether *they*
have `<pine-tree-item>` descendants — an N-deep dependency
chain that post-order mount alone doesn't collapse. v1
`Children` does not remove that use of `tick::after_flush`; it
only removes the dance for direct-root-child questions, where
post-order mount already guarantees the children are present.

**What it does not buy us.**

- **Descendants across nested components.** `Children::iter()`
  is direct-child only; ask about grandchildren by walking one
  of the yielded elements yourself.
- **Teleported content.** A `<pp-teleport>` subtree mounts
  outside its template origin. Those elements are not direct
  children of the teleport host's rendered root and will not
  appear in `children.iter()`. Use `teleport::host_of`
  (RFC 006) for that.
- **Future mutations.** `Children` is a snapshot at hook time.
  Subsequent `pp-for` iterations or external DOM mutations
  don't update it; install a `MutationObserver` on
  `children.root()` if that's what the primitive needs.

### 4.6 Worked examples

**RadioGroup — set aria-setsize / aria-posinset on direct items.**
A clean fit: each `<pine-radio-group-item>` the user passes is a
direct rendered child of the root, and post-order mount
(§4.5) guarantees they exist by the time `on_mount` fires.

```rust
#[handlers]
impl PineRadioGroupRoot {
    pub fn on_mount(&mut self, children: Children) {
        let total = children.len();
        for (i, item) in children.iter().enumerate() {
            let _ = item.set_attribute("aria-setsize", &total.to_string());
            let _ = item.set_attribute("aria-posinset", &(i + 1).to_string());
        }
    }
}
```

**Toolbar — skip setup when the user passed no items.**

```rust
#[handlers]
impl PineToolbarRoot {
    pub fn on_mount(&mut self, children: Children) {
        if children.is_empty() {
            return; // nothing to wire up
        }
        // install roving-tabindex, assign keyboard ownership, etc.
    }
}
```

**ContextMenu — iterate only `<pine-context-menu-item>` direct
children, skipping separators / labels / groups.**

```rust
#[handlers]
impl PineContextMenuContent {
    pub fn on_mount(&mut self, children: Children) {
        let total = children.count_of::<PineContextMenuItem>();
        for (i, item) in children.of::<PineContextMenuItem>().enumerate() {
            let _ = item.set_attribute("aria-setsize", &total.to_string());
            let _ = item.set_attribute("aria-posinset", &(i + 1).to_string());
        }
    }
}
```

For items nested inside `<pine-context-menu-group>` (a
realistic template), the descendant form is one line via the
DOM escape hatch:

```rust
let all = children.root()
    .query_selector_all("pine-context-menu-item").unwrap();
```

**RadioGroup — focus the checked radio, or the first radio.**

```rust
#[handlers]
impl PineRadioGroupRoot {
    pub fn on_ready(&self, children: Children) {
        // `:data-state="checked"` marks the selected radio.
        let target = children
            .iter()
            .find(|el| el.get_attribute("data-state").as_deref() == Some("checked"))
            .or_else(|| children.get(0));
        if let Some(el) = target {
            let _ = el.dyn_into::<web_sys::HtmlElement>().map(|h| h.focus());
        }
    }
}
```

**Dialog — install a MutationObserver on the content subtree.**

```rust
#[handlers]
impl PineDialogContent {
    pub fn on_ready(&self, children: Children, ctx: LifecycleContext) {
        let observer = MutationObserver::new(/* ... */).unwrap();
        observer.observe(children.root()).unwrap();
        // stash observer in self so on_unmount can disconnect
    }
}
```

## 5. Implementation

### 5.1 `Children::iter`

Wrap `HtmlCollection` as a lazy iterator. `HtmlCollection::item`
is a live accessor, but `Children` is a *snapshot* contract —
the iterator caches the length at construction and indexes by
the cached length. Mutations during iteration are the author's
problem (same as iterating a JS array while mutating it).

```rust
pub struct ChildrenIter<'a> {
    coll: web_sys::HtmlCollection,
    i: u32,
    end: u32,
    _m: PhantomData<&'a ()>,
}

impl<'a> Iterator for ChildrenIter<'a> {
    type Item = web_sys::Element;
    fn next(&mut self) -> Option<Self::Item> {
        if self.i >= self.end { return None; }
        let el = self.coll.item(self.i);
        self.i += 1;
        el
    }
}
```

### 5.2 Re-export paths

| Crate | Export |
|---|---|
| `pocopine-core` | `pub use lifecycle::Children;` |
| `pocopine` | `pub use pocopine_core::Children;` + `pocopine::prelude::Children` |
| `pocopine::__private` | no additional export needed; extractor lives on the user-facing surface |

### 5.3 Tests

* **Unit** — `Children::len` / `Children::iter` / `get_as` on a
  synthetic scope with three stubbed children. Assert order and
  count; assert `get_as::<HtmlLiElement>` round-trips.
* **Integration (walker-backed)** — spin up a
  `<pine-radio-group>` component in the existing walker test
  harness with three items, call `on_mount`, assert
  `aria-setsize` / `aria-posinset` landed on each item.
* **Empty root** — mount a component whose rendered root has no
  element children; assert `children.is_empty()` is `true` and
  `children.iter().next()` is `None`.

## 6. Alternatives considered

### 6.1 Expose `el.children()` directly

Status quo with RFC 032. Authors write `el.children().item(i)`
plus `dyn_into` per cast. Works, but repeats across every
compound and scatters the pattern behind the abstraction wall
of the primitive crate. `Children` collapses the cast dance
and makes direct-child iteration a typed extractor on par with
`El` / `Refs`.

### 6.2 `ChildrenList<T: JsCast>` typed collection

Typed wrapper where `T` is the expected element type
(e.g. `Children<HtmlLiElement>`). Loses flexibility — one
`<slot>` often yields a mix (`<li>` and `<template pp-for>`
clones). v1 keeps the untyped `Element` collection + per-item
`get_as::<T>()`. A future RFC can add typed specialisations if
pattern demands it.

### 6.3 Reactive `Children` signal

Expose a `Signal<Vec<Element>>` that updates when children are
added / removed. Conceptually nice; three problems:

* Plumbing — requires a `MutationObserver` installed at mount
  for every component that takes `Children`, whether the
  handler reads it reactively or not.
* Cost — most handlers just want a one-shot snapshot (aria
  setup, initial focus). Paying for the observer is wasteful.
* Identity — inside `pp-for`, children churn on every re-render.
  A reactive `Children` signal would fire constantly; authors
  would usually want to debounce, which is a new problem.

Authors who need mutation observation install a
`MutationObserver` themselves with `children.root()` as the
anchor — one existing API, clearly scoped. If a real reactive
children signal turns out to be load-bearing for Pine
compounds, it earns its own follow-up RFC.

### 6.4 Vue-style `Slots` (slot name → render function)

Our slots are materialised DOM, not deferred VNodes. A
name-keyed render-function map doesn't match pocopine's
one-shot slot-mount model. Slot-presence queries (the main
thing Vue authors reach this API for) are RFC 047's scope.

## 7. Rollout

1. Land `Children<'a>` + `From<LifecycleContext>` impl in
   `pocopine-core`, re-exported from `pocopine` and its prelude.
2. Migrate one Pine primitive as a reference — most likely
   `PineRadioGroupRoot`, whose ARIA setup is the cleanest
   showcase.
3. Update `docs/guides/components/` with a short "children extractor"
   section referencing RFC 032's lifecycle extractor list.

No migration is required for existing code. `Children` is
additive alongside the other RFC-032 extractors.

## 8. Open questions

* **`Children::iter` order under `pp-as`.** `pp-as` hoists a
  user-supplied element as the rendered root. Its children
  *are* whatever the user wrote — which may or may not be what
  the component author expected. Document the rule as
  "`Children` reflects the rendered root's direct children,
  whatever those are"; don't try to normalise.
* **Interaction with teleport (RFC 006).** A teleported subtree
  is mounted under a different DOM ancestor, but its rendered
  root is still the same element from the scope's perspective.
  `Children::iter` walks that element — no special-casing.
* **Should we ship `children.first()` / `children.last()`
  sugar?** Trivial to add; waste if nobody uses them. Wait for
  the migration to show which accessors pay.
* **Interaction with `pp-for` / `pp-if` in the rendered root.**
  Those directives re-compute child lists on their own cadence.
  `Children` snapshots at hook time; re-running the hook (not
  possible today — `on_ready` fires once per scope) would give
  a fresh snapshot. Authors who need mid-life "what are my
  children now" install a `MutationObserver`.
