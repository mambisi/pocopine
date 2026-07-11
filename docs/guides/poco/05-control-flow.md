---
title: "Control flow"
description: "Showing, hiding, branching, matching enums, and looping in .poco templates — pp-show, pp-if chains, pp-match, pp-for, and the comment anchors behind them."
---

# Control flow

Five directives decide what's in the DOM:

| Directive | What it does | Host |
|---|---|---|
| `pp-show` | toggles `display: none` — element stays mounted | any element |
| `pp-if` / `pp-else-if` / `pp-else` | mounts exactly one branch of a chain | `<template>` siblings |
| `pp-match` / `pp-case` | mounts the arm matching an enum value | `<template>` + `<template>` arms |
| `pp-for` | one clone per item of a `Vec` | `<template>` |

## `pp-show` — cheap toggling

`pp-show` flips `display: none` on the element itself. The subtree
mounts once and stays in the DOM; state inside it (input values, scroll
position, transition state) survives toggling.

```poco
<span class="spinner" pp-show="loading"></span>
<span class="checkmark" pp-show="!loading && done"></span>
```

**Rule: `pp-show` goes on each element, never consolidated under a
wrapping `pp-if`.** Re-parenting siblings under one `<template pp-if>`
remounts the whole subtree on every flip — you lose state and pay a
mount. Independent `pp-show`s pay one style write each.

`pp-show` also works directly on a component host:

```poco
<pine-button pp-show="can_delete" @click="remove">Delete</pine-button>
```

Pocopine writes inline `display: none` to the custom element when the
expression is false. When it becomes true, Pocopine removes that inline value,
restoring the component's generated `display: contents` rule. The component
stays mounted throughout.

## Directives on component tags

Component tags are scope boundaries, so their parent-facing directives have
specific meanings:

| Form | Meaning on a component tag |
|---|---|
| `pp-show="expr"` | hides or reveals the whole component host; does not unmount it |
| `:prop="expr"` / `pp-bind:prop` | writes a declared child prop reactively |
| `@event="handler"` / `pp-on:event` | listens on the custom-element host in the parent scope |
| `pp-model[:prop]="field"` | binds a declared child model channel |
| `pp-ref="name"` | registers the host and enables typed child-handle lookup |

Structural directives still belong on `<template>`, but the branch body may
use a component tag as its single root; no plain-element wrapper is required:

```poco
<template pp-if="editing">
  <pine-button @click="remove">Delete</pine-button>
</template>
```

Other directives are not forwarded through the component boundary. Put them
on the native element that owns the behavior. In particular, positioning,
observation, and visual-transition directives need an element with a layout
box; use a plain wrapper when the caller must own that box because a normal
component host uses `display: contents`.

## `pp-if` chains — mount one branch

When branches are *alternatives* — exactly one should exist at a time —
write a chain of `<template>` siblings:

```poco
<template pp-if="count > 5">
  <p class="big">big</p>
</template>
<template pp-else-if="count > 0">
  <p class="small">small</p>
</template>
<template pp-else>
  <p class="zero">zero</p>
</template>
```

Each branch must have exactly one element root. That root may be a native
element or a component tag, including a component with default or named slot
content.

Chain semantics (Vue's, deliberately):

- **First truthy condition wins.** Conditions after the active branch
  are not evaluated and not tracked — flipping a later condition while
  an earlier one holds causes no work at all.
- **Exactly one clone** is in the DOM, always at the chain's position.
- **Same branch ⇒ no DOM work.** A re-evaluation that lands on the
  same branch index leaves the clone alone.
- **Branch change ⇒ remount.** The outgoing subtree unmounts (leave
  transitions play), the incoming one mounts fresh. State that must
  survive a switch belongs on the component, not inside a branch.

Chain members must be **contiguous `<template>` siblings** — whitespace
and comments between members are fine, any other element terminates the
chain. An orphan `pp-else` / `pp-else-if` (no adjacent head), a member
after `pp-else`, or an expression on `pp-else` is a **compile error**.

### `pp-show` vs `pp-if` — the decision rule

| Use | When |
|---|---|
| `pp-show` | the same element toggling often; state inside must survive; cheap styling flip |
| a chain | branches are structurally different subtrees; only one should *exist*; subtrees are expensive to keep mounted |

If you're writing `pp-show="state == 'a'"` / `pp-show="state == 'b'"` /
`pp-show="state == 'c'"` over sibling subtrees — that's an enum wanting
`pp-match`.

## `pp-match` — enum-driven UI

Rust state machines are enums. `pp-match` dispatches on one directly —
no boolean flag soup, no chained negations:

```rust
#[derive(Default, Serialize, Deserialize)]
enum Status {
    #[default]
    Idle,
    Loading,
    Ready(String),
    Err { code: i32 },
}

#[derive(Default, Serialize, Deserialize)]
#[component]
struct StatusPanel {
    status: Status,
}
```

```poco
<template pp-match="status">
  <template pp-case="Idle | Loading">
    <p class="pending">pending…</p>
  </template>
  <template pp-case="Ready" pp-let="msg">
    <p class="ready">{{msg}}</p>
  </template>
  <template pp-case="_">
    <p class="error">something broke</p>
  </template>
</template>
```

The rules:

- **Arms are literal variant names, not expressions.** `pp-case="Ready"`,
  multi-variant `pp-case="Idle | Loading"`, or the wildcard
  `pp-case="_"`. An expression-shaped arm, a duplicate variant, or an
  arm after `_` is a compile error.
- **Matching follows serde's externally-tagged encoding** (the derive
  default): unit variants match by name; newtype and struct variants
  match by tag, with the payload bound via `pp-let`. A plain `String`
  field matches its value as the tag, so `pp-match` doubles as a string
  switch.
- **`pp-let="msg"` binds the payload** inside the arm — a newtype's
  inner value, or a struct variant's `{ field: … }` object
  (`pp-let="e"` then `e.code`). On the `_` arm it binds the whole
  value.
- **Same variant, new payload ⇒ no remount.** `Ready("one")` →
  `Ready("two")` updates the payload binding in place; the arm's
  subtree and its state survive. Changing variant remounts, like a
  chain branch change.
- **No match and no `_` ⇒ nothing renders.** Add `_` when the enum
  may grow.

This is the canonical pattern for request state, multi-step flows,
connection status — anywhere you'd reach for a state-machine enum in
ordinary Rust. Model the state as the enum, let the template follow it.

## `pp-for` — lists

```poco
<ul>
  <template pp-for="todo in todos" pp-key="todo.id">
    <li class="todo">{{todo.title}}</li>
  </template>
</ul>
```

`pp-key` opts into keyed reconciliation: rows are reused by identity,
reorders move existing DOM nodes, and unchanged rows are skipped
entirely. Prefer an item-rooted key (`todo.id`); `$index` keying is
positional and forfeits reuse on reorder.

Gotchas worth knowing (each enforced or warned at compile time where
possible):

- `pp-if` and `pp-for` need a `<template>` host.
- Nested `pp-for` doesn't iterate the inner loop — flatten to one
  `pp-for` over a pre-flattened list and lay out with CSS Grid.
- `{{interpolation}}` scans direct text children only — iterator
  variables don't cross slot boundaries; pass them as props instead.

## Behind the scenes: templates, fragments, and comment anchors

A `<template>` is the *authoring* syntax for all structural directives
— it parses anywhere (including inside `<table>`), its content is inert
until cloned, and it carries the directive attributes.

At install time the template **leaves the live DOM**: the controller
swaps it for a comment anchor — `<!--pp:cond-->`, `<!--pp:match-->`, or
`<!--pp:for-->` — and inserts clones in front of that comment. Comments
are real nodes with a stable tree position, but they are invisible to
CSS: `:nth-child`, `:last-child`, and Stylekit's `space-*`/`divide-*`
sibling selectors count **only the live content**. (Before this
migration, the in-DOM `<template>` was a phantom element sibling that
broke `li:last-child` and added stray margins.)

Consequences you can rely on:

- The live DOM under a list parent is exactly the rows (plus one
  comment). Structural CSS works naturally.
- Nothing user-facing changed: `<template>` remains the authoring
  syntax; the swap is invisible except in devtools, where the labeled
  comment tells you which controller owns the position.
- Don't query for `template[pp-if]` at runtime — it was never a
  supported API, and the element is gone after install.

The full design is RFC-094.
