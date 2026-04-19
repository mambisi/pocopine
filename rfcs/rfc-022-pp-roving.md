# RFC 022 — `pp-roving` (roving tabindex / arrow navigation)

| Field | Value |
|---|---|
| **Status** | Accepted |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-013-pp-on-key-modifiers.md`](./rfc-013-pp-on-key-modifiers.md), [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md), [WAI-ARIA Authoring Practices — Keyboard Navigation Inside Components](https://www.w3.org/WAI/ARIA/apg/practices/keyboard-interface/) |

## 1. Summary

A container-level directive that manages **roving tabindex** and
arrow-key focus navigation for child items. Every accessible
Menu, Listbox, Tabs, RadioGroup, Tree, Toolbar, and Command
Palette follows the same pattern — `pp-roving` ships it as a
one-liner.

```html
<!-- Menu (vertical, default) -->
<ul role="menu" pp-roving>
  <li role="menuitem" tabindex="-1">Copy</li>
  <li role="menuitem" tabindex="-1">Paste</li>
  <li role="menuitem" tabindex="-1">Delete</li>
</ul>
```

```html
<!-- Tabs (horizontal) -->
<div role="tablist" pp-roving.horizontal>
  <button role="tab" tabindex="-1">Profile</button>
  <button role="tab" tabindex="-1">Security</button>
</div>
```

```html
<!-- Command palette — arrow keys on the input move focus into
     the list; wrap-around; Home / End jump to edges. -->
<div>
  <input type="text" pp-roving:listbox />
  <ul id="listbox" role="listbox">
    <li role="option" tabindex="-1">Open file</li>
    <li role="option" tabindex="-1">Go to line</li>
    <li role="option" tabindex="-1">Git: Push</li>
  </ul>
</div>
```

## 2. Non-goals

- **Type-ahead search** (press "c" → jump to next item starting
  with "c"). Useful but niche; defer.
- **Virtual selection via `aria-activedescendant`** where focus
  stays on an input and items receive a highlight class but
  never real DOM focus. `pp-roving:<listbox-id>` (the
  command-palette form) uses real focus transfer — simpler
  mental model and works with screen readers without extra
  plumbing. We can add a `.virtual` mode later if a Pine
  component actually requires it.
- **Multi-select state**. Roving only coordinates focus; the
  component decides what "select" means (usually on Enter /
  Space via `pp-on`).
- **Grid navigation** (2-D: up/down/left/right across a table).
  A later RFC can add `pp-roving.grid` with a `rows`/`cols`
  arg.

## 3. Surface

```html
<container pp-roving[:<listbox-id>][.<orientation>][.<mod>...]>
  <item role="…" tabindex="-1">…</item>
  …
</container>
```

### 3.1 Orientation

| modifier       | arrow keys used             | default? |
|----------------|-----------------------------|----------|
| `.vertical`    | `ArrowUp` / `ArrowDown`     | ✔       |
| `.horizontal`  | `ArrowLeft` / `ArrowRight`  |         |
| `.both`        | all four                    |         |

Home / End always jump to first / last regardless of orientation.

### 3.2 Item selector

Items match this default selector (matches ARIA "widget" roles
inside roving containers):

```
[role="menuitem"], [role="menuitemradio"], [role="menuitemcheckbox"],
[role="option"], [role="tab"], [role="radio"], [role="treeitem"]
```

Authors can override with the `items` modifier:

```html
<ul pp-roving.items.li>
  <li>…</li>
</ul>
```

`items.<selector>` — `<selector>` is a single CSS identifier
(`.foo`, `div`, `li`). For complex selectors the author can fall
back to explicit `role=` attrs.

### 3.3 Disabled items

Items with `aria-disabled="true"` or `[disabled]` are skipped
during navigation — the cursor jumps to the next enabled item.
Matching a disabled item via Home / End also skips to the next
enabled sibling.

### 3.4 Wrapping

Default: wrap. `ArrowDown` on the last item → first. `.nowrap`
clamps instead.

### 3.5 Command-palette form

`pp-roving:<id>` on an `<input>` element takes the id of a
listbox that lives *elsewhere* in the DOM:

- Arrow keys on the input transfer focus into the listbox at the
  first enabled item (top or bottom depending on direction).
- The listbox must itself still be `pp-roving` for its own
  items. The input form is *entry-only* — once focus is inside,
  the listbox's own roving takes over.
- `Escape` on the input is a no-op at the directive level (authors
  usually want to close a palette; wire that via `pp-on:keydown.escape`).

## 4. Semantics

### 4.1 Install

Walker sees `pp-roving` on an element, calls the directive. At
bind time:

1. Resolve item selector (default or overridden).
2. Query descendants. Set `tabindex="-1"` on every item except
   the first enabled one, which gets `tabindex="0"`.
3. Install a `keydown` listener on the container.

### 4.2 Keydown behaviour

For each arrow key / Home / End, the listener:

1. Finds the live list of enabled items (re-queried each press
   so `pp-for` additions / removals pick up without manual
   reset).
2. Finds the currently-focused one via `document.activeElement`.
3. Computes the target index.
4. `preventDefault` + updates tabindex values (new target → 0,
   everyone else → -1) + calls `focus()` on the target.

### 4.3 Re-entering the container

When focus leaves the container (tab out) and later returns via
tab, the browser lands on whichever item currently has
`tabindex="0"` — i.e. the last-focused one. That's how roving
tabindex is supposed to behave.

### 4.4 `pp-for`-driven item lists

Each keydown re-queries descendants, so items added or removed
by `pp-for` are picked up automatically. No manual `refresh()`
call needed.

### 4.5 Teardown

`walker::release_subtree` calls `roving::release(&el)` which
removes the keydown listener. Symmetric to `resize` / `intersect`
/ `anchor`.

## 5. Implementation

New module `crates/pocopine-core/src/directives/roving.rs` —
~220 lines:

```rust
pub fn run(call: &DirectiveCall) {
    let orientation = parse_orientation(&call.modifiers);
    let wrap = !call.modifiers.iter().any(|m| m == "nowrap");
    let items_selector = parse_items_selector(&call.modifiers);
    let linked_listbox_id = call.arg.clone();
    // ...
}
```

`release(el)` tears down listener; registry entry in
`directives/mod.rs`; `walker::release_subtree` hook.

No new web-sys features — everything needed (KeyboardEvent,
Element, Node, HtmlElement) is already on.

## 6. Edge cases

- **No items match the selector.** Listener still installs but
  every keydown is a no-op. Safe.
- **User sets `tabindex="0"` on multiple items before the
  directive runs.** Install step normalises: first enabled
  stays 0, rest go to -1. Author's wish for a different
  initial item is honoured if they set `tabindex="0"` on
  exactly that one and `tabindex="-1"` on the others — the
  install step's "first enabled" fallback only engages when no
  item is already at 0.
- **Item disabled after first mount.** Keydown handler skips
  it; if the currently-focused item becomes disabled, authors
  should move focus (e.g. via `focus::auto_focus_first` on the
  container).
- **Nested roving containers.** Independent; each listener
  binds to its own container. The inner container stops the
  outer from seeing the key via normal bubble semantics — the
  outer listener sees the event first (capture is off for
  roving, as it should be), inner container's listener handles
  it, and if it `preventDefault`s the arrow key the outer does
  nothing (because the inner's handling moved focus, so
  `document.activeElement` is now inside the inner container —
  the outer won't match anything and bails).
- **Command-palette form with invalid listbox id.** No focus
  transfer; arrow keys retain their default behaviour (cursor
  movement in the input). Console warning.

## 7. Example — Pine Menu

```html
<!-- PineMenu.poco -->
<ul role="menu" class="pine-menu" pp-roving @keydown.escape="close">
  <slot />
</ul>
```

```html
<!-- Usage -->
<template pp-teleport="body" pp-if="open">
  <pine-menu pp-anchor:bottom-start="trigger">
    <li role="menuitem" tabindex="-1" @click="copy">Copy</li>
    <li role="menuitem" tabindex="-1" @click="paste">Paste</li>
    <li role="menuitem" tabindex="-1" aria-disabled="true">Delete</li>
  </pine-menu>
</template>
```

Arrow keys step across items, skipping the disabled "Delete".
Escape closes. Enter-to-activate is handled by the `@click`
handlers (native `<li>` click semantics fire on Enter when the
item has focus — or authors can add `@keydown.enter="copy"` for
explicit control).

## 8. Example — Command palette

```html
<div class="cmdk" @keydown.escape="close">
  <input type="text"
         placeholder="Type a command"
         pp-roving:cmd-list
         @input="filter" />

  <ul id="cmd-list" role="listbox" pp-roving @keydown.enter="run">
    <li pp-for="cmd in filtered_commands"
        :key="cmd.id"
        role="option"
        tabindex="-1">
      {cmd.title}
    </li>
  </ul>
</div>
```

- Typing filters the list.
- `ArrowDown` on the input transfers focus into the listbox's
  first item.
- Inside the listbox, `pp-roving` owns arrow navigation and
  wrap.
- `Enter` fires `run` with the focused option.
- `Escape` closes the palette.
