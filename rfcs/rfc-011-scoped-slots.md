# RFC 011 — Scoped slots

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md) §5.5 (slot basics), [Vue scoped slots](https://vuejs.org/guide/components/slots.html#scoped-slots) |

## 1. Summary

Extend `<slot>` in two ways:

1. **Named slots.** A component can expose multiple insertion
   points. A `<slot name="header">` picks up children the user
   wrapped with `<template pp-slot="header">`.
2. **Scoped slots.** A component can pass per-instance data into the
   slot so the user's template can read it. The component writes
   `<slot name="item" :item="foo" :index="i">`; the user reads
   `<template pp-slot="item" pp-let="ctx">…{{ ctx.item }}…</template>`.

Together these unblock every compound component pattern (list
items, table rows, combobox options, menu items, toast renderers).

```html
<!-- PineCombobox.poco -->
<div class="pine-combobox">
  <template pp-for="it in items" pp-key="it.id">
    <li>
      <slot name="item" :item="it" :index="$index">
        <!-- default: plain label -->
        <span pp-text="it.label"></span>
      </slot>
    </li>
  </template>
</div>
```

```html
<!-- user -->
<pine-combobox :items="countries">
  <template pp-slot="item" pp-let="ctx">
    <img pp-bind:src="ctx.item.flag" class="w-4 h-4" />
    <span pp-text="ctx.item.name"></span>
  </template>
</pine-combobox>
```

## 2. Motivation

Today's slot is single-instance, unnamed, and opaque. It's enough
for `<pine-card><h1>…</h1><p>…</p></pine-card>` where the children
are static structure, but it breaks the moment the component wants
the user to render **each item** of a collection (combobox,
autocomplete, menu, table). Without scoped slots every such
component either:

* Takes a string prop and renders with `pp-html` (no reactive
  bindings inside, XSS risk, no composability).
* Ships a fixed item shape (`PineCombobox<T: { label: String, ... }>`).
* Can't exist in Pine at all.

Vue / Svelte / React render props all solve this by giving the
slot content a piece of context. That's what this RFC imports.

## 3. Non-goals

* **Multiple children per named slot.** v0 slots accept one
  `<template pp-slot="name">` per name — the template's body (one
  or more elements) is what mounts. If the user writes two
  templates with the same name, the second wins with a console
  warning.
* **Slot **forwarding** through nested components** (Vue's
  `<slot name="x" v-bind="slotProps" />` forwarded on to another
  component). Authors who need this in v0 must thread props
  manually.
* **`pp-slot`-less wrapper syntax** (Vue 2's slot-attribute-on-any-
  element form). Only `<template pp-slot="…">` is the entry point,
  to keep the grammar simple and the anchor easy to find.
* **Slot-only components** (slot surfaces with no template root).
  All components still have a concrete root.
* **Dynamic slot names** (`pp-slot="[computed]"`). Names are
  static strings at parse time.

## 4. Surface

### 4.1 Component side

A `<slot>` inside a component template supports:

| Attribute | Meaning |
|---|---|
| `name="<id>"` | This slot's identifier. Defaults to `"default"` (the single-slot case today). |
| `:<prop>="<path>"` | Expose `<prop>` to the user's `pp-let` scope. Bound reactively — changing the source updates the user's template. |
| (children) | Default content used when the user didn't provide this slot. |

`:prop="path"` is the same dotted-path grammar `pp-bind` uses.

### 4.2 User side

The user's provided content is a `<template>` inside the component
tag:

```html
<pine-combobox>
  <template pp-slot="item" pp-let="ctx">…</template>
  <template pp-slot="empty">…</template>
</pine-combobox>
```

| Attribute | Meaning |
|---|---|
| `pp-slot="<id>"` | Which slot this template fills. `"default"` when omitted. |
| `pp-let="<ident>"` | Introduce `<ident>` into the template's scope; its fields are the `:prop`s the component bound. Optional — non-scoped slots don't need it. |

Templates written directly (no `pp-slot`) go to the default slot, as today.

## 5. Semantics

### 5.1 Capture

At mount time, before the component's template is cloned, the
walker captures the tag's children as today. Children that are
`<template pp-slot="name">` get stored in a
`HashMap<SlotName, Node>` keyed by name; everything else maps to
`"default"`. This replaces the existing capture-then-relocate
behaviour in `walker::mount_component`.

### 5.2 Slot mounting

During the component template walk, each `<slot>` element is
processed by a new directive (`slot`):

1. Look up the slot by `name`.
2. If the user provided a template for it, clone the template's
   body (one element child — same constraint as `pp-for`).
3. Otherwise use the `<slot>`'s own default children.
4. Build a **slot scope** — a small `ComponentState` whose `get`
   reads the `:prop`s (resolved at read time against the
   component's proxy via `resolve_path`). The user's `pp-let`
   identifier names this scope.
5. `bind_borrowed_scope_to` the slot scope onto the cloned
   content's root so the `pp-let` ident works inside, with
   fall-through to the component's own scope for anything else.
6. Insert the cloned content in place of the `<slot>` element (or
   replace the `<slot>` wholesale).
7. `walker::walk` the clone so its pp-* directives bind.

### 5.3 Reactivity

The slot scope's `get` calls `resolve_path` on the component's
proxy each time, so any change to a `:prop`'s source field fires
the normal effect path — `pp-text="ctx.item.name"` rebinds cleanly
when `ctx.item.name` changes. The `ctx` identifier is a thin proxy
holding scope-id + name-to-path map; no separate data.

### 5.4 Multiple instances (inside `pp-for`)

A slot inside a `pp-for` body in the component template materialises
once per iteration. Each instance gets its own slot scope built
against the `:prop`s evaluated in that iteration's `LoopScope`
(which is already pinned on the clone root). The user's template
is cloned once per slot instance.

### 5.5 Fallback content

If the user didn't provide the named slot, the component's
`<slot>` default children render in place. This keeps
back-compat — `<pine-card>` with the old default-slot pattern
still works.

## 6. Examples

### 6.1 Header / body / footer

```html
<!-- PineDialog.poco -->
<div class="pine-dialog">
  <header class="pine-dialog-header"><slot name="header">Title</slot></header>
  <div    class="pine-dialog-body">  <slot></slot></div>
  <footer class="pine-dialog-footer"><slot name="footer"></slot></footer>
</div>
```

```html
<pine-dialog>
  <template pp-slot="header">Delete account</template>
  Are you sure?
  <template pp-slot="footer">
    <button pp-on:click="cancel">Cancel</button>
    <button pp-on:click="confirm">Delete</button>
  </template>
</pine-dialog>
```

### 6.2 Scoped list items

```html
<!-- PineMenu.poco -->
<ul class="pine-menu">
  <template pp-for="it in items" pp-key="it.id">
    <li role="menuitem">
      <slot name="item" :item="it" :active="it.id == focused_id">
        <!-- default -->
        <span pp-text="it.label"></span>
      </slot>
    </li>
  </template>
</ul>
```

```html
<pine-menu :items="commands" :focused-id="focused">
  <template pp-slot="item" pp-let="ctx">
    <span pp-text="ctx.item.label"></span>
    <kbd pp-show="ctx.item.shortcut" pp-text="ctx.item.shortcut"></kbd>
    <span pp-show="ctx.active">◀</span>
  </template>
</pine-menu>
```

### 6.3 Empty-state slot

```html
<!-- PineCombobox.poco -->
<ul>
  <template pp-if="!items.length">
    <slot name="empty"><li>No results</li></slot>
  </template>
  <template pp-for="it in items" pp-key="it.id">
    <li><slot name="item" :item="it">…</slot></li>
  </template>
</ul>
```

## 7. Implementation

### 7.1 `<slot>` as a directive

Register a `slot` directive keyed on the element's tag — unlike
every other directive which keys on attr name, the slot directive
fires whenever the walker hits a `<slot>` tag. Dispatch happens
from `walker::bind` before the other attr-driven passes, because
the slot materialisation changes the walked element (the slot is
replaced by the slot content).

### 7.2 Capture changes

`walker::capture_child_nodes` is generalised into
`capture_slot_content(tag) -> HashMap<String, Node>`:

* Direct children of `<my-comp>` that are `<template pp-slot="x">`
  → key `"x"`, node is the template's content fragment.
* Other direct children → appended to the `"default"` bucket (a
  DocumentFragment).

The captured map is stashed on the component tag (e.g. a private
JS key `__pp_slot_map`) so the `<slot>` directive can read it
when processing the template.

### 7.3 Slot scope

Internal `SlotScope` struct. `ComponentState::get` evaluates the
dotted path against the owning component's proxy (the one
`enclosing_scope` returned for the `<slot>` tag, which is always
the component's template root). Read-only.

```rust
pub struct SlotScope {
    /// The `pp-let` identifier — `"ctx"` in our examples.
    ident: String,
    /// Per-exposed-prop, the path to resolve on the owner scope.
    /// Populated from `:foo="path"` on the `<slot>`.
    props: Vec<(String, String)>,
    /// Owner scope proxy for path resolution fall-through.
    owner: JsValue,
}
```

`get(ident)` returns a fresh JS object with each prop resolved via
`resolve_path(owner, path)` — same plumbing that lets `LoopScope`
fall through today.

### 7.4 pp-slot / pp-let bookkeeping

No new directives in the registry — `pp-slot` and `pp-let` are
**attribute markers** read by the walker when processing a
`<template>` inside a component tag. Writing `pp-slot="item"`
alone on a template means "this is the slot's content" and has
no other effect; `pp-let="ctx"` tells the slot directive which
name to use for the scope.

## 8. Edge cases

* **Named slot provided but component never renders it.** Captured
  content sits in the map unused; GC'd when the component unmounts.
* **Slot used in two places in the same template.** Both mount from
  the same captured template but each gets its own clone + slot
  scope. Change a `:prop` source and all active clones update.
* **Slot inside `pp-if`.** The `<slot>` directive inside the clone
  only fires when pp-if's body mounts — same walker rules that
  cover `pp-for`.
* **Slot content refers to a field the component doesn't expose.**
  `resolve_path` returns `UNDEFINED`; directive gracefully renders
  nothing. Consider a `console.warn` in debug mode.
* **Multiple `pp-slot="item"` templates.** Second (and later) hits
  emit a warning; last-one-wins for compatibility.
* **User's slot template owns a reactive effect that mutates
  `ctx.item`.** Not supported — slot scopes are read-only. Authors
  mutate through the component's own events (RFC-009 pattern).

## 9. Alternatives considered

* **Inline render functions** (React's render-prop / children-as-
  function). Doesn't match the "HTML-first" model the rest of
  pocopine commits to.
* **`<slot-as="ident">` syntax** on the user side — no `<template>`
  wrapper, slot scope applied to the next child. Terser but
  breaks the uniform "template hosts a directive" story that
  pp-for / pp-if already established.
* **Emit a `pp:slot:<name>` event per render and let the user
  listen.** Conceptually fine but miserable DX for declarative
  rendering.

## 10. Out of scope (future)

* **Forwarding slots** (`<slot name="x" v-bind="$attrs">`).
* **Dynamic slot names.**
* **Slot events** — a way for slot content to call back into the
  component (beyond what `$dispatch` already gives).
* **Compile-time slot typing** — typed `SlotProps<T>` for the
  scope so authors get rust-analyzer completion on `ctx.item`.
