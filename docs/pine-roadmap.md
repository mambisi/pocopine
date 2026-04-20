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
| PineDialog* (compound, 8 parts) | stable | Root/Trigger/Portal/Overlay/Content/Title/Description/Close |
| PineAlertDialog* (compound, 9 parts) | stable | Root/Trigger/Portal/Overlay/Content/Title/Description/Action/Cancel; `role="alertdialog"`, no overlay-dismiss |
| PineRadioGroup* (compound, 3 parts) | stable | Root/Item/Indicator; `role="radiogroup"`, `pp-roving.both`, `pp-model:value` |
| PineToggle | stable | Single `aria-pressed` button, `pp-model:pressed` |
| PineToggleGroup* (compound, 2 parts) | stable | Root/Item; `type="single\|multiple"`, `pp-roving.both` |
| PineHoverCard* (compound, 4 parts) | stable | Root/Trigger/Portal/Content; `open_delay` + `close_delay`, shared timer so pointer can move trigger→content without close |
| PineContextMenu* (compound, 6 parts) | stable | Root/Trigger/Portal/Content/Item/Separator; right-click opens at pointer coords |
| PineLabel | stable | `<label for>` wrapper — `target` prop flows to `for` attr |
| PineSeparator | stable | horizontal / vertical, `decorative` prop drops ARIA |
| PineProgress* (compound, 2 parts) | stable | Root/Indicator; `role="progressbar"` + indeterminate state on negative value |
| PineAspectRatio | stable | CSS `aspect-ratio` wrapper |
| PineToolbar* (compound, 4 parts) | stable | Root/Button/Link/Separator; `pp-roving.both`, orientation-aware separator |
| PinePopover* (compound, 5 parts) | stable | Root/Trigger/Portal/Content/Close; auto-anchor via Trigger stamp |
| PineDropdownMenu* (compound, 13 parts) | stable | Root/Trigger/Portal/Content/Item/Separator/Group/Label/CheckboxItem/ItemIndicator/RadioGroup/RadioItem |
| PineCollapsible* (compound) | stable | Root/Trigger/Content — second compound; `pp-model:open` |
| PineAccordion* (compound) | stable | Root/Item/Trigger/Content; `type="single\|multiple"`, `collapsible` |
| PineAvatar* (compound) | stable | Root/Image/Fallback; `pp-show`-gated fallback during load |
| PineTabs* (compound, 4 parts) | stable | Root/List/Trigger/Content |
| PineTooltip* (compound, 5 parts) | stable | Provider/Root/Trigger/Portal/Content; Provider enforces singleton policy + default delay |
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
5. ~~**Arrow**~~ — done. Minimal v0: Content provides its `side`
   via a context key, Arrow mirrors it for `data-side` styling.
   Does NOT yet track the resolved side after a collision flip
   (would need `pp-anchor::reposition` to expose the resolved
   side through a side-table — saved for a follow-up).
6. ~~**Sub/SubTrigger/SubContent**~~ — v0 done. Click-to-open
   submenu; parent stays open; Escape / ArrowLeft closes the
   sub only. Hover-intent timers deferred. Anchor install
   requires two nested `tick::next` deferrals so `pp-ref="menu"`
   is registered by the walker's pp-if path before the anchor
   selector resolves.
7. ~~**Content config props**~~ — done. `anchor::install(...)`
   exposed as a public helper; DropdownMenu / Popover / Tooltip
   Content parts install the anchor imperatively in `on_ready`
   using `side` / `align` / `side_offset` props. pp-anchor
   directive form still works for non-compound use.

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
| 1 | ~~**Collapsible**~~ | Done — second compound validated the pattern. |
| 2 | ~~**Accordion**~~ | Done — Root/Item/Trigger/Content with `type="single\|multiple"` + `collapsible`. |
| 3 | ~~**RadioGroup**~~ | Done — 3-part compound: Root/Item/Indicator. `role="radiogroup"`, `pp-roving.both` for arrow-key nav, `pp-model:value` round-trips via `pp:update:model` emitted from Root. Indicator `pp-show`-gated on its enclosing Item's `checked`. |
| 4 | ~~**Toggle / ToggleGroup**~~ | Done — standalone `<pine-toggle>` (single `aria-pressed` button, `pp-model:pressed`) + 2-part ToggleGroup (Root/Item) with `type="single\|multiple"` selection and `pp-roving.both`. |
| 5 | ~~**Avatar**~~ | Done — Root/Image/Fallback compound. |
| 6 | ~~**AlertDialog**~~ | Done — 9-part compound: Root/Trigger/Portal/Overlay/Content/Title/Description/Action/Cancel. Content renders `role="alertdialog"`; `dismiss_on_overlay` defaults `false`. Author's side-effect handler goes on the `<pine-alert-dialog-action>` tag (fallthrough skips `@`) so it fires alongside the framework's own `close()`. |
| 7 | ~~**HoverCard**~~ | Done — 4-part compound (Root/Trigger/Portal/Content). `open_delay` + `close_delay` on Root; Content tracks its own mouseenter/mouseleave to cancel the close timer, so users can move the pointer from Trigger across the gap into Content without the card vanishing. Focus opens immediately (no delay) per Radix. |
| 8 | ~~**ContextMenu**~~ | v0 done — 6-part compound (Root/Trigger/Portal/Content/Item/Separator). `contextmenu` event captures pointer (clientX, clientY); Content positions itself absolutely at those coords, not anchored to Trigger. Richer parts (Sub, CheckboxItem, RadioGroup) can follow. |
| 9 | ~~**Tooltip provider**~~ | Done — `<pine-tooltip-provider>` wraps a subtree; descendants inherit its `delay_duration` default and obey a singleton policy (only one tooltip open at a time — opening a second evicts the first via `Handle::update`). |

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

Monolithic → compound migration status:

- ~~**Dialog**~~ → done (Root/Trigger/Portal/Overlay/Content/Title/Description/Close)
- ~~**Popover**~~ → done (Root/Trigger/Portal/Content/Close)
- ~~**Tabs**~~ → done (Root/List/Trigger/Content)
- ~~**Tooltip**~~ → done (Root/Trigger/Portal/Content)

The substrate pieces needed for a Radix-style compound are now
all in place:

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
