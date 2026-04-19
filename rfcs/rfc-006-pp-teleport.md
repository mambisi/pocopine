# RFC 006 — `pp-teleport` (dialogs, popovers, portals)

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [Alpine's `x-teleport`](https://alpinejs.dev/directives/teleport), [Vue's `<Teleport>`](https://vuejs.org/guide/built-ins/teleport.html), [`rfc-004-pp-for.md`](./rfc-004-pp-for.md), [`rfc-005-pp-transition.md`](./rfc-005-pp-transition.md) |

## 1. Summary

Add `pp-teleport` — a directive that clones a `<template>`'s body and
inserts it at a target location in the DOM, regardless of where the
template itself lives. The template's content still binds against the
enclosing component scope, so event handlers and reactive bindings
keep working.

Primary use cases: **modals, dialogs, popovers, tooltips** — any UI
that needs to escape `overflow: hidden` clipping, `z-index` stacking,
or a parent's transform context.

```html
<!-- inside any component's template -->
<button pp-on:click="open = true">Open dialog</button>

<template pp-teleport="body" pp-if="open">
  <div class="modal-overlay" pp-on:click.self="open = false">
    <div class="modal" role="dialog">
      <h2 pp-text="title"></h2>
      <slot></slot>
      <button pp-on:click="open = false">Close</button>
    </div>
  </div>
</template>
```

The teleported clone reads `open` / `title` from the component scope
that owns the template, but renders as a direct child of `<body>` —
out from under any clipping or z-index parent.

## 2. Motivation

Every serious UI eventually needs a dialog, a popover, or a tooltip.
The standard problem:

* `overflow: hidden` on an ancestor clips the popover.
* `z-index` on an ancestor creates a stacking context that the modal
  can't rise out of.
* `transform` on an ancestor creates a containing block that breaks
  `position: fixed`.

Every one of these is "just move the element up the DOM" — which is
exactly what teleport does. Without it, authors resort to:

* Stateful floating containers outside the component tree (duplicates
  the reactive glue).
* Manual `appendChild` from component `init` handlers (fragile,
  lifecycle isn't tied to anything).
* CSS tricks that only paper over one class of problem.

A first-class directive handles the composition cleanly and makes
the unmount / cleanup story explicit.

## 3. Non-goals

Keep the surface tight:

* **Reactive target.** The target selector is resolved once at
  directive-run time. No live rebinding when the selector's value
  changes.
* **`disabled` / "teleport in place" toggle** (Vue's feature). If you
  need this, use plain `pp-if` without teleport.
* **Multiple destinations.** One template, one target.
* **Teleport inside `pp-for`.** Each iteration would need a unique
  target — out of scope for v0.
* **Dialog lifecycle helpers** (focus trap, scroll-lock, ESC handler,
  `<dialog>` element integration). Those are app-layer; we give you
  the teleport primitive, you build the rest.
* **Shadow DOM targets.** `document.querySelector` only; shadow roots
  need a different traversal.

## 4. Surface

One attribute. The host is a `<template>` element. The value is a CSS
selector plus one alias:

| Selector | Resolves to |
|---|---|
| `"body"` | `document.body` (convenience — the common case) |
| any other | `document.querySelector(<value>)` |

Nothing else: no modifiers, no arg segment, no sub-attributes.

The template's body must contain exactly one element child (same
rule as `pp-if` / `pp-for`; RFC-004 §5.2).

## 5. Semantics

### 5.1 Standalone (`pp-teleport` alone)

On first walk:

1. Clone the template body (first element child).
2. Look up the enclosing scope from the template's original DOM
   position. Pin it onto the clone via
   [`walker::bind_scope_to`](../crates/pocopine-core/src/walker.rs)
   so child directives resolve against the right proxy after the
   move.
3. Insert the clone as the last child of the resolved target.
4. Call `walker::walk(&clone)` so `pp-*` directives bind.
5. Stash the clone reference on the template host (private key
   `__pp_teleported`) so we can find it at cleanup time.

The clone stays mounted for as long as its owning template stays
mounted. There is no reactive expression here — standalone teleport
is "always mount, always visible."

### 5.2 Combined with `pp-if`

`<template pp-teleport="..." pp-if="expr">` — the common shape for a
dialog gated by `open`. `pp-if` owns the mount/unmount cycle;
`pp-teleport` only changes *where* the clone is inserted.

Specifically, `pp-if` consults the template host for a `pp-teleport`
attribute at setup time:

* If present, resolve the target once and use `target.appendChild`
  instead of `parent.insertBefore(clone, template)` on mount.
* Scope pinning happens the same way as standalone teleport (so the
  clone can float anywhere in the DOM and still read the owning
  scope).
* Leave (`false` transition of the `pp-if` expression) removes the
  clone through its current parent (the teleport target), which
  releases effects / scopes via the MutationObserver as usual.

The two attributes compose without either knowing much about the
other. `pp-teleport` is a no-op if `pp-if` is also present — it just
publishes "here is the target, here is the scope to pin" and lets
`pp-if` run the show.

### 5.3 Cleanup

The teleported clone lives **outside** the owning component's
subtree. When the owning component unmounts, the MutationObserver
sees the template disappear but not the clone — so the clone would
leak unless we help.

Cleanup hook: `walker::release_subtree` detects the
`__pp_teleported` private on the template host and removes the clone
explicitly. The MutationObserver then picks up the clone's removal
and releases its effects + scopes in the normal path.

### 5.4 Scope resolution

This is the subtle bit. After teleport, the clone's `parent_element()`
chain no longer walks back to the owning component — it walks back
to `<body>` (or wherever the target is). So
`walker::enclosing_scope` wouldn't find the intended scope.

Fix: resolve the enclosing scope **at teleport time** (from the
template's original position) and pin it on the clone root via
`bind_scope_to`. The walker already consults the per-element
SCOPE_ID_KEY first, so pinned scopes win over the parent chain. Same
mechanism `pp-for` uses for its per-item `LoopScope`.

## 6. Examples

### 6.1 Confirm dialog

```html
<!-- ConfirmDelete.poco -->
<button pp-on:click="open = true">Delete</button>

<template pp-teleport="body" pp-if="open">
  <div class="dialog-backdrop" pp-on:click.self="open = false">
    <div class="dialog" role="dialog">
      <h2>Are you sure?</h2>
      <p>This cannot be undone.</p>
      <div class="dialog__actions">
        <button pp-on:click="open = false">Cancel</button>
        <button class="danger" pp-on:click="confirm">Delete</button>
      </div>
    </div>
  </div>
</template>
```

### 6.2 Tooltip pinned to body

```html
<span
  class="help"
  pp-on:mouseenter="hover = true"
  pp-on:mouseleave="hover = false"
>?</span>

<template pp-teleport="body" pp-if="hover">
  <div class="tooltip">explains the thing</div>
</template>
```

### 6.3 With transitions

Teleport composes with `pp-transition:*` for free — the transition
module attaches to the clone, which lives at the target:

```html
<template
  pp-teleport="body"
  pp-if="open"
  pp-transition:enter="transition duration-200"
  pp-transition:enter-start="opacity-0"
  pp-transition:enter-end="opacity-100"
  pp-transition:leave="transition duration-150"
  pp-transition:leave-start="opacity-100"
  pp-transition:leave-end="opacity-0">
  <div class="modal">…</div>
</template>
```

## 7. Implementation

### 7.1 New module `teleport.rs`

Public API:

```rust
pub fn run(call: &DirectiveCall);

/// Resolve a selector to an element; recognises "body" as a
/// shorthand for `document.body`.
pub fn resolve_target(selector: &str) -> Option<Element>;

/// Called by `walker::release_subtree` for every released element —
/// if the element is a template with a teleported clone, remove
/// the clone so MutationObserver can clean up its subtree.
pub fn release(el: &Element);
```

Private-key stash on the template host:

```
__pp_teleported  →  Element   (the teleported clone root)
```

### 7.2 Integration with `if_.rs`

In `run`, after resolving the template host:

```rust
let teleport_sel = call.el.get_attribute("pp-teleport");
let teleport_target = teleport_sel
    .as_deref()
    .and_then(teleport::resolve_target);
```

Capture `teleport_target: Option<Element>` into the effect closure.
On the mount branch, pick between `target.append_child(clone)` and
the existing `parent.insert_before(clone, template)`. Pin the
enclosing scope on the clone root in the teleport path.

### 7.3 Directive registry

Register `"teleport"` so standalone teleports (without `pp-if`) get
invoked by the walker. When `pp-if` is also present, the `run` in
`teleport.rs` detects that and returns early.

### 7.4 Walker cleanup

```rust
fn release_subtree(node: &Node) {
    if let Ok(el) = node.clone().dyn_into::<Element>() {
        // …existing release paths…
        crate::directives::teleport::release(&el);
    }
}
```

## 8. Edge cases

* **Target selector matches nothing.** Log a console error, do
  nothing else (no fallback insertion). Authors catch the typo at
  runtime with a clear message.
* **Target is inside the template's own subtree.** User error that
  would create a cycle. We don't detect; browser will refuse to
  insert a node into its own descendant anyway.
* **Target is outside the MutationObserver's root (body).** Releases
  still work — `release` is called directly from `release_subtree`
  on the template host, not dependent on the observer firing for the
  clone's location. Supported.
* **Template has `pp-for`, `pp-teleport`, and `pp-if` all three.**
  Not v0. The walker dispatches `for` first; `for_` doesn't consult
  `pp-teleport`. We'll document this collision as undefined behaviour
  and reject in a future lint pass.
* **Scope lookup returns `None` (template at document root, no
  component).** Then the teleported content can't resolve reactive
  bindings. Silent fallback — content mounts, directives that
  reference scope fields get `undefined`. Matches today's
  "orphan directive" behaviour.

## 9. Alternatives considered

* **Move the element, don't clone.** Simpler cleanup, but breaks the
  "template is a static anchor" model that `pp-if` / `pp-for` rely
  on. Also makes it awkward to re-enter after a false→true flip.
  Rejected.
* **Teleport as a plain attribute on a non-template element.** Alpine
  supports this on `<template>` only for the same reason pocopine
  does: the template body is an inert fragment until the directive
  decides to materialize it. Keeps the semantic clear.
* **A `<pp-teleport target="...">` tag** (like `<pp-outlet>`). Would
  need a new reserved element plus registry integration; the
  template-host form composes with every other directive we already
  have.
* **Reactive target (re-teleport when selector changes).** Almost
  never needed — the common case is a static app-shell target. If
  demand shows up, add via a `pp-teleport.dynamic` modifier later.

## 10. Out of scope (future work)

* `<pp-dialog>` component built on top (focus trap, aria wiring,
  scroll lock, ESC handler).
* Re-teleport to a different target reactively.
* Teleport within `pp-for`.
* Integration with `<dialog>` / `::backdrop` so authors can use the
  built-in element while still getting pocopine's reactive bindings.
