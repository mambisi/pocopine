# RFC 019 — `pp-as` (polymorphic rendering / `asChild`)

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`rfc-010-attribute-fallthrough.md`](./rfc-010-attribute-fallthrough.md), [`rfc-011-scoped-slots.md`](./rfc-011-scoped-slots.md), [Radix `asChild`](https://www.radix-ui.com/docs/primitives/guides/composition) |

## 1. Summary

Let a component hoist its **child element** as the rendered root
instead of its own template wrapper. Same pattern as Radix's
`asChild` prop / Base UI's `render` prop — the canonical way a
headless library escapes its own default tag without forcing
authors into ".replaceAsChild()"-shaped hacks.

```html
<!-- PineButton.poco: template is a <button class="pine-btn"> wrapper. -->
<button class="pine-btn" pp-on:click="on_click">
  <slot></slot>
</button>
```

```html
<!-- Usage: render the button's styling onto an <a> instead of a <button>. -->
<pine-button pp-as>
  <a href="/docs" target="_blank">Read docs</a>
</pine-button>
```

Resulting DOM:

```html
<a class="pine-btn" href="/docs" target="_blank">Read docs</a>
```

The template's root tag (`<button>`) is discarded. The user's
element (`<a>`) becomes the component root. Template root's
attributes (class, `pp-on:click`, everything else) merge onto the
user's element. The component scope binds to the user's element
so directives and reactivity still work.

## 2. Non-goals

- **Template bodies with structure** (`<div class="foo"><icon
  /><slot/></div>`). Pine v0 keeps templates **trivially
  wrappy** when they want to support `pp-as`: exactly
  `<element attrs...><slot></slot></element>`. Anything else is
  authored without `pp-as`. Supporting complex hoisting is a
  later RFC (mirrors Radix's `Slot.Slottable`).
- **Multi-element user content.** User must provide a *single*
  element child. Multiple children ⇒ `pp-as` is ignored and the
  normal mount path runs.
- **Per-attribute merge strategy configuration.** Class and style
  merge; everything else "user wins on conflict" — same rule as
  Radix and upstream HTML `href`-gets-kept-when-conflict.
- **Dynamic `pp-as`** (`pp-as="condition"`). Presence-only for
  v0. Binding to a condition plus dynamic re-hoisting mid-life is
  not worth the complexity until a real use case shows up.

## 3. Surface

```html
<component-tag pp-as>
  <single-user-element ...>
    ...children...
  </single-user-element>
</component-tag>
```

Attribute presence is sufficient. Attribute value, if any, is
ignored (no standard way to parse "true" vs "false" etc. until
we have expression-level directive args elsewhere).

### 3.1 Matching constraints

For `pp-as` to take effect:

1. The component's template root must contain **exactly one**
   `<slot>` element and no other element children. (Text / comment
   nodes around the `<slot>` are ignored.) Violations fall back to
   the normal mount path with a console warning.
2. The default slot content (i.e. the tag's direct children) must
   contain exactly one element node. Other element counts fall
   back to the normal mount path, silently.
3. Named-slot templates (`<template pp-slot="...">`) among the
   children are ignored for the "single element" count and are
   *discarded* under `pp-as` — they don't compose cleanly with a
   flattened root.

## 4. Merge rules

Between the template root's attributes and the user's element:

| attribute | rule |
|---|---|
| `class`    | space-joined. Template's classes appended to the user's. |
| `style`    | `;`-joined. Template's declarations appended. User wins on duplicate properties (later declaration loses per CSS cascade — matches `apply_fallthrough_attrs`). |
| `pp-*`     | written to the user element **if absent**. User's own `pp-*` wins. |
| everything else (`href`, `disabled`, `aria-*`, `id`, `data-*`, …) | user wins on conflict. |
| `pp-data`  | dropped (scope binding happens through the scope registry, not the attribute). |
| `pp-as`    | dropped (internal marker, meaningless after the rewrite). |

Fallthrough from the *component-tag* attributes (RFC-010) still
applies — onto the new (user) root, with the same rule (class
append, style append, others overwrite if not declared as props).

## 5. Semantics

### 5.1 Scope binding

The scope binds to the **user's element** (not the template
wrapper, which no longer exists). This means:

- `$el` inside handlers resolves to the user element.
- `refs::get_as::<HtmlAnchorElement>("root")` returns the `<a>`
  when you wrote `pp-ref="root"` on it.
- Fallthrough attrs land on the user element.
- Directives authored on the template root (`pp-on:click`,
  `pp-bind:disabled`) bind against the user element's scope and
  fire when the user element receives events.

### 5.2 Directive transfer

`pp-*` attributes copied from the template root to the user
element are re-dispatched through the directive registry during
the walk, exactly as if the author had authored them on the user
element directly. No special-case handling per directive — they
all share the same bind entry point.

### 5.3 Event listener invariance

Because directive bind re-runs on the user element, `pp-on:click`
attaches its listener to the user element (the `<a>`), not the
absent `<button>`. Browser click semantics are preserved —
Enter-key on focused link, space on focused link (not a thing),
modifier-click for new-tab, etc., all behave as HTML specifies for
the *actual* element the user rendered. That's the whole point of
`pp-as`.

### 5.4 Slot materialisation

Ordinarily the walker's `materialize_slot` would replace `<slot>`
with captured user content. Under `pp-as` the `<slot>` never
makes it into the live DOM — the user's element **is** the
content, so the slot is effectively pre-materialised.

## 6. Implementation

Modify `walker::mount_component`:

```rust
fn mount_component(el: &Element, tag: &str) {
    if el.has_attribute("pp-as") {
        if let Some(user_root) = take_single_child_element(el) {
            return mount_component_as(el, tag, user_root);
        }
        // Fallback: structural mismatch, normal path.
    }
    // existing path
}
```

New helper `mount_component_as`:

1. `instantiate(tag)` + `apply_static_props`.
2. `el.set_inner_html(&template_html_for(tag))` — same as now, so
   we get the template root.
3. Verify the template root matches the "single `<slot>`" rule.
   If not, abort `pp-as` by re-installing `user_root` inside a
   synthesised default slot and falling back. (In practice just
   log a warning and re-run the normal path.)
4. Grab the template root's attributes into a `Vec<(String,
   String)>`.
5. Remove the template root from `el`. Insert `user_root` in its
   place.
6. For each harvested attr, apply the table in §4 against
   `user_root`.
7. Pin scope on `user_root` (`SCOPE_ID_KEY`, `SCOPE_PROXY_KEY`).
8. `apply_fallthrough_attrs(el, &user_root, &scope)` — same
   fallthrough code; it doesn't care which element is the root.
9. Call `slots::put` with an **empty** store — `<slot>` won't be
   walked, so no slot materialisation happens.

No new Cargo features, no new modules. ~80 net lines in
`walker.rs`.

## 7. Edge cases

- **Template root has `pp-if` / `pp-for`.** Out of scope for v0
  — the template must be a plain wrapper. If `pp-if` sits on the
  template root the component's whole lifecycle is already
  outside `pp-as`'s domain (the root is conditional rendering,
  not a pass-through). Document as unsupported.
- **User element has `pp-ref`.** Fine. The ref registers against
  the component scope, which is what authors want.
- **Conflict: user `class="x"` + template `class="y"`.** Merged:
  `class="x y"`. Same as RFC-010 fallthrough; user's classes
  come first. (This matches Radix, which concatenates with user
  first.)
- **User element provides a `pp-on:click`, template too.** The
  user's handler wins by name collision on the attribute level —
  the user's `pp-on:click="..."` is already on the element, and
  the template's `pp-on:click` lookup in the harvested attrs hits
  the "user wins" branch, so we skip it. Only one `click`
  listener installed.
- **User element is a Web Component / custom element.** Works —
  directives bind against any element.
- **Nested components with `pp-as`.** Each mount is independent.

## 8. Example — Pine Button with a router link

```html
<!-- AppNav.poco -->
<nav>
  <pine-button pp-as variant="ghost">
    <a pp-route href="/docs">Docs</a>
  </pine-button>
</nav>
```

```rust
#[component(template = "PineButton.poco")]
pub struct PineButton {
    variant: String,
}
```

```html
<!-- PineButton.poco -->
<button pp-bind:class="cx!('pine-btn', variant == 'ghost' => 'pine-btn-ghost')">
  <slot></slot>
</button>
```

Resulting DOM, with RFC-003's `pp-route` directive intercepting
clicks:

```html
<a class="pine-btn pine-btn-ghost" href="/docs" pp-route>Docs</a>
```

A Ctrl+click still opens in a new tab (native `<a>` behaviour),
because the rendered element genuinely is an anchor — pine-button
didn't wrap it in a `<button>`.
