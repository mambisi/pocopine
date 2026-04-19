# Pine — component catalog & roadmap

Living document. Tracks what we ship, where we're short of
[reka-ui](https://reka-ui.com/) (our reference peer, Vue-side), and
what we build next.

Reference checkout (gitignored): `tmp/reka-ui/packages/core/src/`.

---

## 1. Shipped (v0 — 8 primitives)

| Pine | Status | API notes |
|---|---|---|
| PineButton | stable | `variant` / `size` / `disabled` + `pp-as` |
| PineDialog | stable | modal, focus trap, scroll lock, `pp-model:open` |
| PinePopover | stable | non-modal, `pp-anchor`, `pp-model:open` |
| PineDropdownMenu* (compound) | stable | Root/Trigger/Portal/Content/Item — see §2 |
| PineTabs | stable | tablist-only; panels author-owned |
| PineTooltip | stable | hover/focus + delay |
| PineSwitch | stable | `role="switch"`, `pp-model:checked` |
| PineCheckbox | stable | tri-state, `pp-model:state` |

`PineDropdownMenu` is the first compound, built in the Radix/reka-ui
shape. All others are still monolithic — candidates for a compound
rewrite once the pattern stabilises (see §3).

---

## 2. DropdownMenu gap analysis vs reka-ui

Our compound shipped the five essentials (Root/Trigger/Portal/
Content/Item). Comparing surface against reka's
`packages/core/src/DropdownMenu/` + `Menu/`:

### 2.1 Root
| reka | have? | note |
|---|---|---|
| `defaultOpen` | no | uncontrolled open |
| `modal` (default `true`) | no | when true: scroll-lock + pointer-events outside |
| `dir` | no | RTL |
| emits `update:open` | no | — can't two-way bind via `pp-model:open` yet |

### 2.2 Trigger
| reka | have? | note |
|---|---|---|
| `disabled` | no | |
| `aria-controls={contentId}` | no | we only set `aria-expanded` + `data-state` |
| `asChild` (polymorphic) | no | `pp-as` works on PineButton; not on Trigger |

### 2.3 Portal
| reka | have? | note |
|---|---|---|
| `to` (target selector) | no | we hardcode `body` |
| `disabled` / `forceMount` | no | |

### 2.4 Content
| reka | have? | note |
|---|---|---|
| auto-anchor to trigger (no prop) | **have** | via `on_setup` — Trigger stamps `data-pine-dm-trigger="{scope_id}"`, Content's `on_setup` reads injected root and sets `anchor = "[data-pine-dm-trigger=\"N\"]"` before pp-anchor binds |
| `side` / `sideOffset` / `align` / `alignOffset` | no | we hardcode `bottom-start` + offset 4 |
| `avoidCollisions` / `sticky` / `hideWhenDetached` | no | `pp-anchor` has `flip` modifier but not these |
| emits `escapeKeyDown` / `pointerDownOutside` / `interactOutside` / `dismiss` / `closeAutoFocus` | no | authors can't veto dismiss |
| `loop` (roving wraparound) | **have** via `pp-roving` default |
| `forceMount` (keep in DOM for animation) | no | `pp-transition` compat story unclear |

### 2.5 Item
| reka | have? | note |
|---|---|---|
| emits `select` (preventable → menu stays open) | **no** | authors use native `@click`; can't veto auto-dismiss |
| `textValue` (typeahead match target) | no | typeahead not wired yet |
| `data-highlighted` | no | `pp-roving` sets `tabindex=0` on current; no data-attr |

### 2.6 Missing parts
Group, Label, Separator, Arrow, CheckboxItem, ItemIndicator,
RadioGroup, RadioItem, Sub, SubTrigger, SubContent, Filter.

### 2.7 Priority additions (next round)
In this order:

1. ~~**Auto-anchor**~~ — done (`af9bf20`).
2. ~~**Group / Label / Separator**~~ — done (`78c49d5`).
3. ~~**Item `select` event with preventDefault**~~ — done
   (`57c9303`). Implementation matched the roadmap's simpler
   path: cancelable `pp:select` CustomEvent + defaultPrevented
   check. No substrate change.
4. ~~**CheckboxItem / RadioItem / RadioGroup / ItemIndicator**~~
   — done (`1bc4202` + `d428c1a`). ItemIndicator switched from
   `pp-if` to `pp-show` due to a pp-if-without-teleport scope-
   pinning gap (see §6).
5. **Arrow**. Small. Needs `pp-anchor` to expose its final side/
   align so the arrow can orient itself — adds a tiny data-attr.
6. **Sub/SubTrigger/SubContent**. Submenu flyouts — requires:
   nested `pp-anchor` (Sub anchors to SubTrigger), hover-intent
   timers, left-arrow close semantics, parent menu stays open.
   Heavier lift, save for last.
7. **Content config props** (`side`, `sideOffset`, `align`,
   `alignOffset`). Prop plumbing over `pp-anchor`'s modifier
   syntax; no substrate work.

---

## 3. Full reka-ui catalog (what exists to port)

Not a to-do list — a *menu*. Pick from this when a user asks for
a new primitive. Shipped marked **[HAVE]**.

### Overlays
- **Dialog** [HAVE]
- **AlertDialog** — required-action variant, `role="alertdialog"`
- **Popover** [HAVE]
- **HoverCard** — Popover opened on hover with delays
- **Tooltip** [HAVE]
- **Toast** — transient notifications, `aria-live`, swipe-to-dismiss
- **DropdownMenu** [HAVE] (partial — see §2)
- **ContextMenu** — right-click / long-press at pointer coords
- **Menubar** — horizontal bar of menus, roving focus across top-level

### Form controls
- **Button** [HAVE]
- **Checkbox** [HAVE]
- **Switch** [HAVE]
- **RadioGroup** — roving focus, form value
- **Toggle** — single `aria-pressed` button
- **ToggleGroup** — set of Toggles, single/multi
- **Label** — click-through `<label for>`
- **Slider** — single/multi-thumb range
- **NumberField** — spinner input, locale-aware
- **PinInput** — OTP-style segmented
- **TagsInput** — chip input
- **Editable** — inline-edit text
- **Rating** — star rating
- **Stepper** — multi-step progress

### Data-picking (list/search)
- **Select** — styled select, single/multi, typeahead
- **Combobox** — input + filtered listbox
- **Autocomplete** — input with suggestions (no selection state)
- **Listbox** — raw listbox primitive

### Date / time
- **Calendar**, **RangeCalendar**
- **DateField**, **DateRangeField**, **TimeField**, **TimeRangeField**
- **DatePicker**, **DateRangePicker**
- **MonthPicker**, **MonthRangePicker**, **YearPicker**, **YearRangePicker**

### Color
- **ColorPicker** (composes Area + Slider + Swatch + Field)
- **ColorArea**, **ColorSlider**, **ColorField**, **ColorSwatch**, **ColorSwatchPicker**

### Navigation
- **Tabs** [HAVE]
- **NavigationMenu** — multi-level site-nav, hover-intent
- **Pagination** — page-number controls
- **Tree** — nested expand/collapse, keyboard nav, multi-select
- **Toolbar** — horizontal button group, roving focus

### Layout
- **Accordion** — collapsible sections, single/multiple
- **Collapsible** — single open/close region (Accordion's primitive)
- **Splitter** — resizable panels
- **ScrollArea** — custom scrollbars over native scroll
- **AspectRatio** — w/h-ratio container

### Display
- **Avatar** — image + fallback + loading state
- **Progress** — determinate/indeterminate bar
- **Separator** — horizontal/vertical rule

Coverage: 8 of ~60.

---

## 4. Recommended next primitives

Reshuffled after the `on_setup` + auto-anchor work. Each compound
primitive now costs about the same (the pattern is mechanical);
the question is what unblocks the most end-user surface.

### 4.1 Close the DropdownMenu surface first (§2.7)

Until DropdownMenu's own missing parts land — Separator, Label,
Group, Item.select-with-prevent — every menu-using demo page has
gaps. Finishing our flagship compound proves the pattern scales
**inside** a component, and the remaining parts are a few lines
each. Do §2.7 items 2-4 before anything else in this table.

### 4.2 Broaden to new components

After DropdownMenu's core is fleshed out:

| # | Component | Why |
|---|---|---|
| 1 | **Collapsible** | Single open/close region — the minimum viable compound (Root/Trigger/Content). Tiny, prove the shape outside DropdownMenu. |
| 2 | **Accordion** | Wraps Collapsible + lifts single-vs-multiple state. Layout staple. |
| 3 | **RadioGroup** | Compound with RadioGroupItem (role="radio") + roving focus. Maps almost 1-to-1 onto the Tabs substrate. |
| 4 | **Toggle / ToggleGroup** | Reuses Switch mechanics + pp-roving; second "set of controls" after RadioGroup. |
| 5 | **Avatar** | Root/Image/Fallback — simple compound that exercises `pp-if` for fallback. Independent of any overlay. |
| 6 | **AlertDialog** | ~30 lines layered on Dialog (forbid dismiss-on-outside, `role="alertdialog"`). |
| 7 | **HoverCard** | Popover + hover delay timers (copy Tooltip's timing). |
| 8 | **ContextMenu** | DropdownMenu + pointer-coord anchor. Free once Sub lands. |
| 9 | **Tooltip provider** | Radix's single-open policy across a subtree (compound-rewrite territory). |

### 4.3 Substrate gaps worth filling opportunistically

Surfaces that hurt multiple components:

- **`pp-anchor` config props**. Today every compound hardcodes
  `bottom-start.offset.4.flip`. Wrap in a helper or add
  prop-based side/align to avoid repeating the modifier syntax.
- **`pp-transition` + `forceMount`**. Without forceMount we can't
  animate dismiss (content is removed before CSS runs). Needed
  for any polished animation story.
- **Global store / portal region**. Blocks Toast.

### 4.4 Hold until asked

- Date/time primitives — large, locale-dependent.
- Color primitives — niche.
- Combobox/Select — composes Listbox + Popover + filter; revisit
  after DropdownMenu is finished.
- Toast — needs a global store + live-region viewport.
- Slider, NumberField, PinInput, Tree, Splitter, ScrollArea,
  NavigationMenu — each non-trivial, wait for demand.

---

## 5. Compound-rewrite backlog

v0 shipped monolithic. The substrate pieces needed for a Radix-
style compound are now all in place:

- `provide` / `inject` — RFC-027
- `watch_scope_field(scope_id, field, cb)` — cross-scope reactive
  subscribe (for children mirroring root state)
- `CTX_PARENT_KEY` stamp on slot-inserted elements — keeps
  inject-chain correct across the slot boundary
- `on_setup(&mut self)` — pre-children-walk hook for
  context-dependent field initialisation
- Attribute-fallthrough skips `@`/`:` shorthand, so authors'
  event bindings stay on the tag instead of clobbering the
  template's own

Any of these can now be refactored to compound:

- **Dialog** → Root/Trigger/Portal/Overlay/Content/Title/Description/Close
- **Popover** → Root/Trigger/Portal/Content
- **Tabs** → Root/List/Trigger/Content
- **Tooltip** → Provider/Root/Trigger/Content

Not urgent — the monolithic shape works. Do it when a consumer's
layout needs the ceded control (e.g. putting the trigger deep
inside a card, which `pp-as` already mostly handles).

---

## 6. Substrate gaps — closed

Surfaced during compound-component work. Both fixed in the
post-radio-group round.

### 6.1 ~~`pp-if` without `pp-teleport` doesn't pin scope~~ — fixed

`pp-if` was only pinning the owning scope on its clone when the
template also had `pp-teleport`. That meant inline clones of a
`<template pp-if>` component-template root walked with no
explicit scope, and nested `<slot>` elements materialised
against the wrong scope (whatever DOM-ancestry happened to hit
first). Fixed by unconditionally pinning
`walker::enclosing_scope(&template_el)` onto the clone, so the
walker sees the correct owning scope whether or not the subtree
is teleported. ItemIndicator moved back from `pp-show` to
`pp-if`.

### 6.2 ~~Cancelable `emit`~~ — fixed

`emit` / `emit_from` / `emit_from_host` stay fire-and-forget
(deferred via tick::next — required for pp-model's mirror
pattern). New synchronous counterparts handle the
fire-and-observe case:

- `emit_cancelable(name, detail) -> bool`
- `emit_cancelable_from(el, name, detail) -> bool`

They fire a `cancelable: true` CustomEvent *synchronously* and
return whether a listener called `preventDefault()`. Pine's
DropdownMenu Item uses the first to implement its
"stay open on prevent" path in one line.
