# RFC 004 — `pp-for` (list iteration)

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-003-router.md`](./rfc-003-router.md), [Alpine's `x-for`](https://alpinejs.dev/directives/for) |

## 1. Summary

Add `pp-for` as the one canonical way to render a Rust collection as
DOM nodes. Lives on a `<template>` element (Alpine-style), clones the
template body per item, and binds each clone against a **loop scope**
that exposes the loop variable + `$index`/`$first`/`$last` magics
while falling through to the enclosing scope for any other key.

```html
<template pp-for="story in stories">
  <li class="story">
    <a pp-bind:href="story.url" pp-text="story.title"></a>
    <small>by <span pp-text="story.author"></span></small>
  </li>
</template>
```

## 2. Motivation

Today, every list renders via `pp-html`: the component builds an HTML
string in Rust and the walker does one `innerHTML` set. Works, but:

* HTML escaping is the author's responsibility (easy to forget when
  you add a new field).
* No per-item reactivity — a single field change forces rebuilding
  the entire list's HTML string.
* Loop + formatting logic lives outside the template file, split
  across `.poco` and `.rs`. The template stops describing the shape
  of what's rendered.
* Recursive trees (e.g. HN comments) become recursive string
  builders in Rust instead of recursive component templates.

`pp-for` moves iteration into the template and lets the framework
own cloning, scope creation, and (eventually) keyed diffing. It's
the missing piece for real list-heavy UIs; every other pp-* directive
is cheap sugar compared to what this unlocks.

## 3. Goals

* Alpine-compatible syntax on a `<template>` host:
  `<template pp-for="item in items">…</template>`.
* Per-item scope that plays with existing directives without
  changes.
* **Scope fall-through**: the loop scope exposes `item` + the loop
  magics; every other key falls through to the enclosing component's
  scope (so `$store.x` and any parent field still resolve inside
  the loop).
* Nested `pp-for` (pp-for inside pp-for) just works — each level
  is its own loop scope; outer variables fall through.
* Recursive components — a `<hn-comment>` whose template contains
  `<template pp-for="reply in children"><hn-comment …/></template>`
  just works. pp-for + tag-based mounting compose.

## 4. Non-goals (v0)

* **Keyed diffing.** v0 clears the prior clones and rebuilds on every
  items change. O(n) per change. Keyed reuse is RFC-005.
* **Transitions.** `pp-transition` is a separate RFC — enter/leave
  animations hook into pp-for but aren't part of this one.
* **Object iteration.** Only `Vec<T>` / JS arrays. No
  `for (k, v) in map` style.
* **Range syntax.** No `pp-for="i in 0..10"`.
* **Destructuring.** No `pp-for="(item, i) in items"` — use
  `$index` instead.

## 5. Design

### 5.1 Syntax

```
pp-for="<ident> in <path>"
```

* `<ident>` — the loop variable name, any valid Rust/JS identifier.
* `<path>` — dotted path evaluated against the enclosing scope
  proxy via the existing `resolve_path`. Must resolve to a JS array.

Host must be `<template>`. Alpine's choice: the browser never renders
a `<template>`'s content before JS runs, so there's no FOUC. pp-for
on any other element is rejected at walk time with a `console.error`
and the directive becomes a no-op.

### 5.2 Template body

v0 requires **exactly one** element inside the `<template>`. Text
nodes or multiple siblings are rejected. Fragments are a future
extension.

```html
<!-- OK -->
<template pp-for="x in xs">
  <li pp-text="x.name"></li>
</template>

<!-- Rejected in v0 -->
<template pp-for="x in xs">
  <li>a</li>
  <li>b</li>
</template>
```

### 5.3 Loop scope

Each clone is bound to a hand-written `LoopScope` in `pocopine-core`:

```rust
struct LoopScope {
    item_name: &'static str,  // e.g. "story"
    item:      JsValue,       // current array element
    index:     usize,
    total:     usize,
    parent:    JsValue,       // enclosing scope's proxy
}

impl ComponentState for LoopScope {
    fn get(&self, key: &str) -> JsValue { ... }
    fn set(&mut self, _k: &str, _v: JsValue) { /* no-op */ }
    fn keys(&self) -> &'static [&'static str] { ... }
    fn invoke(&mut self, _k: &str, _a: &Array) -> JsValue { JsValue::UNDEFINED }
}
```

Get trap order of resolution:

1. Key is `<item_name>` → `item.clone()`.
2. Key is `$index` → `JsValue::from_f64(index as f64)`.
3. Key is `$first` → `JsValue::from_bool(index == 0)`.
4. Key is `$last` → `JsValue::from_bool(index + 1 == total)`.
5. Key starts with `$` → delegate to existing magic resolver.
6. Everything else → `Reflect::get(&self.parent, key.into())` — falls
   through to the enclosing component scope's proxy.

Writes on a loop scope are a no-op. The loop variable is a snapshot
of the array element, not a live reference — mutating it wouldn't
round-trip to the Vec anyway. If a user needs to mutate (e.g., toggle
a checkbox), they dispatch a handler on the parent scope with the
index or the id as an argument (a future handler-event milestone).

### 5.4 Re-render model (v0: whole-rebuild)

`pp-for::run` binds a single effect:

```rust
effect(|| {
    let items_js = resolve_path(&parent_proxy, items_expr);
    let arr = Array::from(&items_js);
    let n = arr.length() as usize;

    // Tear down prior clones and their scopes.
    for el in prior_clones.drain(..) {
        el.remove(); // MutationObserver → release_subtree cleans effects
    }

    // Build fresh.
    for i in 0..n {
        let item = arr.get(i as u32);
        let loop_scope = Scope::new(Rc::new(RefCell::new(LoopScope { ... })));
        let clone = template_content.clone_node(true).dyn_into::<Element>().ok();
        bind_scope_to(clone, loop_scope.id, loop_scope.proxy);
        template_element.parent().insert_before(clone, template_element);
        walker::walk(&clone); // existing walker binds pp-* directives
        prior_clones.push(clone);
    }
});
```

Reactivity on `items` itself comes free — the effect reads the
parent proxy for `items`, which tracks the dep; when the parent
scope's `set` trap fires for `items` (or the collection is replaced
via a handler mutation), the effect reruns and rebuilds.

**Cost**: O(n) DOM operations per items change. Acceptable for v0;
most pocopine lists today are <100 items. Keyed diffing in RFC-005
reduces this to O(diff).

### 5.5 Nested iteration

Outer loop's `LoopScope.parent` is the component scope. Inner loop's
`LoopScope.parent` is the outer `LoopScope`'s proxy. Fall-through
walks the chain naturally.

```html
<template pp-for="section in sections">
  <h2 pp-text="section.title"></h2>
  <template pp-for="item in section.items">
    <!-- resolves item → inner scope; section → outer scope; app_name → component -->
    <p><span pp-text="item.name"></span> (<span pp-text="section.title"></span> in <span pp-text="app_name"></span>)</p>
  </template>
</template>
```

### 5.6 Recursive components

Comments in HN are the driving use case. A `<hn-comment>` component
receives the comment node as a prop; its own `.poco` contains:

```html
<article>
  <div class="meta" pp-text="author"></div>
  <div class="body" pp-html="text"></div>
  <template pp-for="reply in children">
    <hn-comment pp-bind:node="reply"></hn-comment>
  </template>
</article>
```

Tag-based mounting already handles instantiating child `<hn-comment>`s
— pp-for just iterates `children` and produces the tag. No new
recursion primitives.

### 5.7 Interaction with other directives

* `pp-text`, `pp-html`, `pp-bind:X`, `pp-show`, `pp-on:X`, `pp-model`
  inside the template body work unchanged. They resolve through the
  loop scope's proxy; anything not loop-local falls through to the
  parent.
* `pp-init` on the cloned root element fires **once per item** —
  same semantics as any component mount.
* `pp-if` (future): would sibling a `<template pp-for>` naturally
  for conditional empty states.

### 5.8 Cleanup

When the component owning the `<template pp-for>` unmounts, the
template element itself is part of the subtree and gets removed.
The `MutationObserver` sees every clone and runs `release_subtree`
on each — existing machinery, no new code path. When the loop
re-renders and drops clones, same story.

## 6. Runtime responsibilities

New in `pocopine-core`:

* `crates/pocopine-core/src/directives/for_.rs` — the directive itself.
  Called out of `walker::bind` when the element is a `<template>` with
  a `pp-for` attribute.
* `crates/pocopine-core/src/loop_scope.rs` — the `LoopScope` type and
  its `ComponentState` impl, including parent-proxy fall-through.
* `walker::bind` — early branch for `<template>` elements carrying
  `pp-for`, short-circuits the normal directive pass (`<template>`
  contents aren't walked normally; the for_ directive takes over).
* `magics::resolve` — no change. `$index`/`$first`/`$last` live on
  the LoopScope, not in the magic resolver; they're keys the scope's
  own `get` handles.

No compiler (`pocopine-macros`) changes.

## 7. Implementation plan

1. `LoopScope` with parent-proxy fall-through (unit tests on host:
   get resolution order, $index/$first/$last values, set is a no-op).
2. Walker hook: `<template>` + `pp-for` attribute → dispatch to
   `for_::run`. Return early so the template's content isn't walked
   as DOM.
3. `for_::run` — parse `"ident in path"`, read items via
   `resolve_path`, iterate with whole-rebuild strategy.
4. Release + unmount semantics: verify prior clones are GC'd and
   their effects released via the MutationObserver path.
5. Rewrite HN's `render_story_list` as a `pp-for` template; delete
   `render_story_list`, `render_comment_tree`, `count_comments` —
   they become declarative markup.
6. Introduce `<hn-comment>` as a recursive component to exercise the
   tree-via-pp-for path.
7. Update `docs/components/03-composition.md` §iteration. Remove
   the "deferred" hedges in RFC-001 §5.9 that called out `pp-for`
   as blocked.
8. Follow-up RFC-005 for keyed diffing.

## 8. Alternatives considered

* **Keep `pp-html` everywhere.** Rejected — string builders in Rust
  can't express per-item reactivity cleanly, and HTML escaping on
  every new field is footgun-prone.
* **Iterate on the repeating element itself**
  (`<li pp-for="…">…</li>`, Vue-style). Rejected — the host and
  template become the same element, which means the initial
  pre-hydration render shows one placeholder. Alpine's `<template>`
  host renders nothing in the browser before wasm loads — no FOUC.
* **Tuple destructuring** (`pp-for="(item, i) in items"`). Rejected
  for v0 — `$index` covers it with zero expression-parser work.
* **A dedicated `<pp-for>` tag** (like `<pp-outlet>`). Rejected —
  `<template>` is semantically correct (its content isn't rendered)
  and lets browsers hide the template before wasm hydrates, for free.
* **Index-based keying as the v0 default** (instead of rebuild).
  Rejected — index-as-key is a worse default than rebuild (reorders
  look like updates; `pp-model` inputs carry stale state across
  reorders). Better to be correct-and-O(n) than stale-and-O(diff).

## 9. Unresolved questions

* **Keyed diffing strategy for RFC-005.** Two options:
  * Explicit `pp-key="story.id"` on the template. Simple; matches
    Alpine's `:key`. Requires users to remember it for reorders.
  * Auto-key by `serde_json` hash of the item. Zero config; possibly
    slow; breaks when items are structurally identical.
* **Fragments in the template body.** A comment-tree row often wants
  to emit a wrapper + children-slot as two siblings. Workaround: wrap
  in `<div class="contents">` with `display: contents`. A cleaner
  fragment support is deferred.
* **Two-way `pp-model` on a loop variable.**
  `<input pp-model="item.name">` inside a loop has no live write
  path back to the Vec. Probably remains a soft rejection (no-op
  write) with a dev-build warning.
* **`pp-transition` interaction.** How enter/leave classes hook into
  clone add/remove. Separate RFC, but LoopScope should probably
  expose enough hooks that the transition directive doesn't need
  special knowledge of pp-for.

## 10. Related / follow-up directives

Mentioned here so the broader directive landscape is visible; each
gets its own RFC when it lands.

* **`pp-cloak`** — tiny: hide an element until pocopine is done
  binding. One line of CSS + walker strips the attribute after the
  initial pass. Ship alongside pp-for so list hosts can be cloaked
  during the first render.
* **`pp-transition`** — enter/leave animations. Companion to pp-for
  (and a future `pp-if`) since iteration changes are the main source
  of insert/remove events. Separate RFC because the surface is big
  (classes vs. declarative durations, timing functions, concurrent
  sequences).
* **`pp-if`** — conditional render. Could be approximated with
  `pp-show` today, but `pp-if` actually unmounts (frees effects,
  scope). Small directive on its own.

## 11. Migration / impact

No breaking changes. pp-html remains; components can adopt pp-for
incrementally per list. The HN example is the first migration target
(commit alongside the implementation) because it's the most obvious
payoff — deletes the string-builder helpers and gets a per-item
reactive tree.
