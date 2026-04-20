# RFC 034 — `pp-roving.virtual` (activedescendant mode)

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-20 |
| **Supersedes** | — |
| **Related** | [RFC 022](./rfc-022-pp-roving.md), [WAI-ARIA 1.2 Combobox pattern](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/) |

## 1. Summary

Add an **activedescendant** mode to `pp-roving`, opted into via
the `.virtual` modifier. In this mode DOM focus stays on the
host element while arrow keys move a *virtual* highlight
through items, communicated to assistive tech via the host's
`aria-activedescendant` attribute. The canonical consumer is an
editable Combobox whose `<input>` must keep caret and typing
state while the user navigates the popup list with the arrow
keys.

Before:

```html
<!-- Command palette — focus transfers into the listbox. -->
<input type="text" pp-roving:cmd-list>
<ul id="cmd-list" role="listbox" pp-roving>…</ul>
```

After, new editable-combobox pattern:

```html
<!-- Combobox — caret stays in the input; arrow keys highlight
     items virtually. -->
<input role="combobox"
       type="text"
       aria-controls="opts"
       pp-roving:opts.virtual>
<ul id="opts" role="listbox">
  <li role="option" id="opt-1">Apple</li>
  <li role="option" id="opt-2">Banana</li>
</ul>
```

On each ArrowUp/Down/Home/End keydown, the input's
`aria-activedescendant` tracks the id of the currently-highlighted
option, and the option receives `data-highlighted="true"` so
authors can style the "virtually focused" row. DOM focus never
moves.

## 2. Surface

```html
<host pp-roving:<listbox-id>.virtual[.<orientation>][.<mod>...]
       [.items.<selector>]>
```

`.virtual` requires the `:<listbox-id>` argument. The host can
be any element but is almost always an `<input>`.

### 2.1 Orientation + wrapping

Same as the base directive — `.vertical` (default) / `.horizontal`
/ `.both`, plus `.nowrap` to clamp at the ends.

### 2.2 Items

Resolved **inside the linked listbox** (scoped to `#listbox-id`
rather than the host's own subtree). Default selector is
`[role="option"]`. Override with the `.items.<selector>` modifier
from RFC-022.

Hidden items are skipped. The runtime treats an item as hidden
when ANY of: `[hidden]` attribute, computed `display: none`, or
`aria-hidden="true"`. This lets Combobox filter its list via
`pp-show` (which sets `display:none`) without the activedescendant
pointer landing on invisible matches.

### 2.3 Active-item attributes

On move:
- Host: `aria-activedescendant="<id>"`.
- All items in the listbox: `data-highlighted` attribute is set
  to `"true"` on the active one, removed from the rest.

Items that lack an `id` get one auto-assigned via
`data-pine-roving-id="pine-roving-{n}"` + the same string set
as the actual `id` so `aria-activedescendant` resolves. Authors
are encouraged to set explicit ids.

### 2.4 What this mode does NOT do

- It does not manipulate `tabindex` on items — they're never
  directly focused.
- It does not fire any "select" / "activate" event. The host
  owns activation (e.g. `@keydown.enter` on the input looks up
  the active option by its id and dispatches its own handler).
- It does not replace the base `pp-roving` on the listbox —
  in virtual mode the listbox stays a plain list; no second
  pp-roving is needed (or wanted, since that would move real
  focus).

## 3. Implementation

`crates/pocopine-core/src/directives/roving.rs`:

1. Parser picks up `virtual` (or `activedescendant`) in the
   modifier chain → `Mode::Virtual`.
2. `run()` routes to a new `install_virtual()` when both
   `.virtual` and a listbox arg are present.
3. `install_virtual()`:
   - Locates the listbox via `document.getElementById`.
   - Queries items with the resolved selector, filtered
     through `is_item_visible()` + `is_item_disabled()`.
   - Auto-stamps missing ids.
   - Seeds `aria-activedescendant` to the first enabled item.
   - Installs a `keydown` listener on the host that maps
     arrow keys to next/prev/first/last, updates
     `aria-activedescendant`, toggles `data-highlighted`.
   - `preventDefault` on handled keys so the input's caret
     doesn't move during navigation.
4. `release(el)` drops the state slot symmetric to the
   roving-tabindex branch.

Full implementation is ~80 lines. No new web-sys features
(all APIs already enabled).

## 4. Edge cases

- **Listbox id missing at install time.** Console warning, no
  listener installed. Matches how the entry-transfer form
  handles a missing listbox.
- **All items hidden / disabled.** Every keydown is a no-op.
  `aria-activedescendant` is removed from the host.
- **Items mutate between keypresses** (Combobox filter).
  Re-queried each press, so the cycle ring always reflects
  current visibility. If the previously-active id is now
  hidden/disabled, the next keydown resets the active to the
  first visible enabled item.
- **Listbox in a teleported subtree.** Resolved by id lookup
  against the document, not host descendants — so Combobox's
  input can live inside the trigger area while the listbox is
  teleported to `<body>`.

## 5. Migration / interop

Additive. No existing `pp-roving` usage changes behaviour —
absence of `.virtual` routes through the original
roving-tabindex path. The `.virtual` modifier simply picks the
alternate installer.

RFC-022 §2 said "virtual selection via activedescendant…
defer[red]. We can add a `.virtual` mode later if a Pine
component actually requires it." That time has arrived:
PineCombobox needs it.
