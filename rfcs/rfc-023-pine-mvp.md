# RFC 023 — Pine MVP (eight primitives)

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-19 |
| **Supersedes** | — |
| **Related** | [`rfc-014-focus-utilities.md`](./rfc-014-focus-utilities.md), [`rfc-015-pp-anchor.md`](./rfc-015-pp-anchor.md), [`rfc-019-pp-as.md`](./rfc-019-pp-as.md), [`rfc-021-scroll-lock.md`](./rfc-021-scroll-lock.md), [`rfc-022-pp-roving.md`](./rfc-022-pp-roving.md), [Radix Primitives](https://www.radix-ui.com/primitives), [Base UI](https://base-ui.com/), [Headless UI](https://headlessui.com/) |

## 1. Summary

Pine is an unstyled, accessible UI component library that sits on
top of pocopine. Every primitive ships **behavior, keyboard, focus,
and ARIA**, and ships **zero CSS** — authors style via their own
stylesheets targeting semantic class names (`.pine-dialog`,
`.pine-tabs-list`, …) and `data-*` state attributes
(`data-state="open"`, `data-variant="primary"`, etc.).

This RFC documents the v0 surface: eight primitives chosen because
each stress-tests a distinct substrate feature, and together they're
enough to build a real form-heavy application.

## 2. Conventions

### 2.1 Class + data-* pattern

Every component exposes a root class (`pine-<kind>`) and one or
more sub-element classes (`pine-dialog-content`, …). Dynamic state
is exposed via `data-*` attributes — never via class toggling.
Authors style via CSS selectors like:

```css
.pine-dialog-content[data-state="open"] { animation: in 0.2s; }
.pine-tabs-trigger[data-state="active"] { color: blue; }
```

### 2.2 Boolean attributes

Pure-boolean attributes (`disabled`, `data-disabled`) render as
**present-or-absent** (empty-value when true, removed when false).
Boolean **ARIA** attributes (`aria-checked`, `aria-selected`)
render as literal `"true"` / `"false"` strings since that's what
the ARIA spec mandates.

### 2.3 Two-way binding

Components that own single-value state (Switch's `checked`,
Checkbox's `state`, Tabs' `value`, Dialog's `open`, Popover's
`open`, DropdownMenu's `open`) fire a `pp:update:model` custom
event with the new value in `event.detail` whenever they mutate.
Pair with `pp-model="…"` on the tag for two-way binding.

### 2.4 Slots

Dialog uses named slots (`title`, `description`, default); every
other component uses a single default slot. No sub-component
composition in v0 — postponed until pocopine has a
parent-child-context primitive.

### 2.5 Runtime state

Components that hold non-serializable handles (focus traps,
scroll-lock flags, trigger listeners) keep them in a
module-local `thread_local<HashMap<ScopeId, Runtime>>` side-table
rather than on the `#[component]` struct. Drained in `on_unmount`.

## 3. Components

### 3.1 `PineButton` — polymorphic button

```html
<pine-button variant="primary" size="md" @click="save">Save</pine-button>

<!-- Render as <a> via pp-as. -->
<pine-button pp-as variant="ghost">
  <a pp-route href="/docs">Docs</a>
</pine-button>
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `variant` | `String` | `""` | Passed through as `data-variant`. |
| `size` | `String` | `""` | Passed through as `data-size`. |
| `disabled` | `bool` | `false` | Renders `disabled` + `data-disabled` (present-or-absent). |

Inner element: `<button type="button" class="pine-btn">`. The
default slot becomes the button's content. Clicks on the inner
button bubble to the `<pine-button>` tag, so `@click` on the tag
works without prop drilling.

### 3.2 `PineDialog` — modal dialog

```html
<pine-dialog pp-model="open">
  <template pp-slot="title">Delete file?</template>
  <template pp-slot="description">This cannot be undone.</template>
  <div class="actions">
    <button @click="close">Cancel</button>
    <button @click="confirm">Delete</button>
  </div>
</pine-dialog>
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `open` | `bool` | `false` | Bind via `pp-model="open"`. |
| `dismiss_on_overlay` | `bool` | `true` | Clicking the backdrop closes. |
| `dismiss_on_escape` | `bool` | `true` | Escape closes. |

Slots: `title`, `description`, default (body).

ARIA: `role="dialog"`, `aria-modal="true"`,
`aria-labelledby="$id-title"`, `aria-describedby="$id-description"`.

Behaviour: teleports to `<body>` when `open`; installs a focus
trap on open and auto-focuses the first focusable; locks body
scroll (`scroll_lock::lock()`); releases trap + scroll + restores
focus on close.

Handlers: `close()`, `on_overlay_click()`, `on_escape()`.

### 3.3 `PinePopover` — anchored floating panel

```html
<button id="trigger" @click="open = !open">Open</button>
<pine-popover :open="open" anchor="#trigger">
  <p>Floating content.</p>
</pine-popover>
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `open` | `bool` | `false` | |
| `anchor` | `String` | `""` | CSS selector or pp-ref name. |
| `dismiss_on_outside` | `bool` | `true` | |
| `dismiss_on_escape` | `bool` | `true` | |

Placement is fixed to `bottom-start` with a 4px offset and flip
in v0. Teleported to `<body>`. No focus trap, no scroll lock.

### 3.4 `PineDropdownMenu` — menu overlay

```html
<button pp-ref="menu-trig" @click="open = !open">More</button>
<pine-dropdown-menu :open="open" anchor="#menu-trig" @click="close">
  <li role="menuitem" tabindex="-1" @click="copy">Copy</li>
  <li role="menuitem" tabindex="-1" @click="paste">Paste</li>
  <li role="menuitem" tabindex="-1" aria-disabled="true">Delete</li>
</pine-dropdown-menu>
```

| Prop | Type | Default |
|---|---|---|
| `open` | `bool` | `false` |
| `anchor` | `String` | `""` |

Auto-focuses the first enabled menuitem on open. Arrow keys cycle
via `pp-roving`; Home / End; Escape closes; click-outside closes.
Authors author the menu items as `<li role="menuitem">` inside
the default slot.

### 3.5 `PineTabs` — tablist

```html
<pine-tabs pp-model="current" :tabs="tabs"></pine-tabs>
<div role="tabpanel" pp-show="current == 'account'">Account body</div>
<div role="tabpanel" pp-show="current == 'security'">Security body</div>
```

`TabDef`:

```rust
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TabDef {
    pub value: String,
    pub label: String,
    pub disabled: bool,
}
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `tabs` | `Vec<TabDef>` | `[]` | |
| `value` | `String` | `""` | Selected tab value. |

Emits `pp:update:model` on select. Panels are author-owned —
PineTabs renders only the tablist. Horizontal roving tabindex
(arrow left/right), click-delegated selection, Home / End jump.

### 3.6 `PineTooltip` — hover / focus tooltip

```html
<button id="save">Save</button>
<pine-tooltip trigger="#save">Saves your work.</pine-tooltip>
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `trigger` | `String` | `""` | CSS selector of the trigger element. |
| `delay` | `u32` | `700` | ms before mouse-hover triggers show. Focus triggers immediately (WAI-ARIA). |

Teleported to `<body>`. Anchored to the trigger at `top` with 6px
offset and flip. Never steals focus. Mouseleave / blur cancels
pending show and hides instantly.

### 3.7 `PineSwitch` — toggle

```html
<pine-switch pp-model="dark_mode"></pine-switch>
```

| Prop | Type | Default |
|---|---|---|
| `checked` | `bool` | `false` |
| `disabled` | `bool` | `false` |

`role="switch"`, `aria-checked="true"`/`"false"`,
`data-state="checked"`/`"unchecked"`. Click toggles + fires
`pp:update:model` with the new bool.

### 3.8 `PineCheckbox` — tri-state checkbox

```html
<pine-checkbox pp-model="agree"></pine-checkbox>
```

| Prop | Type | Default | Notes |
|---|---|---|---|
| `state` | `String` | `"unchecked"` | `"checked"`, `"unchecked"`, or `"indeterminate"`. |
| `disabled` | `bool` | `false` | |

`role="checkbox"`, `aria-checked` maps `"indeterminate"` →
`"mixed"` (ARIA spec). Click cycles
`unchecked ↔ checked` with `indeterminate → checked` on first
click. Fires `pp:update:model` with the new string state.

## 4. Out of scope (v1+)

- `PineCombobox` / `PineSelect` / `PineCommandPalette` — need
  filtering + virtual selection surface.
- `PineAccordion` / `PineCollapsible` — transition coordination
  RFC first.
- `PineToast` / `PineToaster` — global store + live-region
  announcer.
- `PineSheet` / `PineDrawer` — positioning-variant of Dialog.
- Sub-component composition (`<PineDialogTitle>`, `<PineTab>`) —
  waits on a parent-scope-context primitive.
- `PineRadioGroup` — straightforward on top of `pp-roving`; not in
  MVP8 but uncontroversial.
- Dynamic placement on `PinePopover` / `PineDropdownMenu` /
  `PineTooltip` — currently fixed to bottom-start / top.
- Form wrapper (`PineForm` + validation).
- Theme tokens.

## 5. Substrate fallout

Two substrate improvements fell out of the MVP implementation:

1. **`pp-on` sets `current_el` around the handler call**
   (mirrors `bind` / `html` / `init` / `model` / `show` / `text`).
   Without this, `dispatch_event` from inside any `@click` /
   `@input` / etc. handler was a silent no-op. Pine's Tabs and
   Switch rely on this.
2. **`pp-anchor` falls back to reading the value as a scope
   field** when neither the ref-name nor CSS-selector branches
   match. Backward-compatible (the ref-first / selector-second
   order stays); lets Pine components pass an `anchor` prop and
   reference it in the template as `pp-anchor:top="anchor"`
   without needing the directive arg to be reactive.

Both are covered by existing pocopine tests plus Pine's browser
tests.

## 6. Demo

`examples/pine-demo` renders every primitive with hand-written
demo-only CSS. Serves as the entry point for contributors trying
Pine for the first time.
