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

### 2.4 Content — **biggest gap**
| reka | have? | note |
|---|---|---|
| auto-anchor to trigger (no prop) | **no** | we require author-provided `anchor` attribute selector |
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

1. **Auto-anchor** (Trigger-owned element becomes Content's anchor
   without the author writing a selector). Requires either a
   substrate hook that runs before children walk, or an effect
   that installs `pp-anchor` after mount.
2. **Item `select` event with preventDefault**. Aligns with reka;
   lets authors keep the menu open after an action. Needs
   `emit`-returnable semantics (currently `emit` is fire-and-forget).
3. **Sub/SubTrigger/SubContent**. Submenu flyouts — heavy lift,
   but it's the other defining Radix feature.
4. **CheckboxItem / RadioItem / RadioGroup / ItemIndicator**. The
   "stateful item" variants. Cheap once the select event is in.
5. **Group / Label / Separator / Arrow**. Visual primitives. Small.
6. **Content config props** (`side`, `sideOffset`, `align`,
   `alignOffset`) — prop plumbing, no substrate work.

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

Ranked by (leverage × simplicity on current substrate):

| # | Component | Why |
|---|---|---|
| 1 | **Separator** | Trivial (`role="separator"`); every compound uses one |
| 2 | **Label** | One directive, clicks-through `<label for>`; form ergonomics |
| 3 | **Accordion / Collapsible** | Layout staple, reuses `pp-transition` |
| 4 | **RadioGroup** | Pairs with Checkbox/Switch we already ship |
| 5 | **Avatar** | Image + fallback — independent, no compound needed |
| 6 | **Toggle / ToggleGroup** | Same shape as Switch + pp-roving |
| 7 | **DropdownMenu — Sub + CheckboxItem + RadioItem** | Finishes our compound before moving on |
| 8 | **AlertDialog** | ~30 lines on top of Dialog |
| 9 | **HoverCard** | Popover + hover/delay (reuse Tooltip timing) |
| 10 | **ContextMenu** | DropdownMenu + pointer-coord anchor |

Do not start until a user asks (we ship *what's needed*, not a
Radix-parity race):

- Date/time primitives — large, locale-dependent, wait for demand.
- Color primitives — niche; wait.
- Combobox/Select — needs solid listbox + typeahead; do *after*
  DropdownMenu is fully fleshed out, the pieces compose.
- Toast — needs a global store surface.

---

## 5. Compound-rewrite backlog

v0 shipped monolithic. With provide/inject (RFC-027),
`watch_scope_field` (added in DropdownMenu work), and the
CTX_PARENT plumbing, any of these can now be refactored to
compound:

- **Dialog** → Root/Trigger/Portal/Overlay/Content/Title/Description/Close
- **Popover** → Root/Trigger/Portal/Content
- **Tabs** → Root/List/Trigger/Content
- **Tooltip** → Provider/Root/Trigger/Content

Not urgent — the monolithic shape works. Do it when a consumer's
layout needs the ceded control (e.g. putting the trigger deep
inside a card, which `pp-as` already mostly handles).
