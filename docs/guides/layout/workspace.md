---
title: "Workspace — multi-region shell"
description: "<pine-workspace>: a content-heavy frame (resizable rail/sidebar │ main │ detail aside │ outer + bottom panels) with an own resize engine. Headless — you style the grid."
---

# Workspace

`<pine-workspace>` is the content-heavy frame for productivity apps — the
Atlassian / VS-Code shape: a resizable rail/sidebar, the main region, a
trailing detail **Aside**, optional outer panels, and a bottom panel. Every
panel resizes (drag or keyboard); the sidebar collapses to a rail with a
hover flyout. It's **headless** — it manages the *behavior* and publishes
each panel's size; **your CSS grid** does the layout.

```
 [panel]  sidebar │            main             │  aside   [panel]
  collapse  resize │      flex-fill remainder      │  resize  collapse
           rail/flyout                              open/close
                    └──────────── bottom ──────────┘
                          resize · collapse · edge
```

It's an **independent family** from `<pine-app-shell>` — not an extension.
Use it when you need resizable multi-region content; use the AppShell for a
simple adaptive nav.

## Anatomy

| Tag | Role | Capabilities |
|---|---|---|
| `<pine-workspace>` | scope | root: breakpoint engine + `WORKSPACE` context + the grid container |
| `<pine-workspace-header>` | `banner` | top bar |
| `<pine-workspace-sidebar>` | `navigation` | **resize** · collapse-to-rail · hover flyout · auto-rail |
| `<pine-workspace-main>` | `main` | the flex-fill remainder |
| `<pine-workspace-aside>` | `complementary` | **resize** · open/close (a docked detail column) |
| `<pine-workspace-panel side="start\|end">` | `navigation` | outer secondary nav · collapse |
| `<pine-workspace-bottom>` | `complementary` | **vertical resize** · collapse · edge-relocatable |
| `<pine-workspace-footer>` | `contentinfo` | status bar |

## The size custom property

A resizable panel's width/height is genuinely dynamic, so the component
publishes it as a CSS custom property on its own element; your CSS reads it
with `var()` and the grid uses an `auto` track:

```css
.pine-workspace { display: grid; grid-template-columns: auto 1fr auto; }
.pine-workspace-sidebar { width: var(--pine-sidebar-size, 260px); }
.pine-workspace-aside   { width: var(--pine-aside-size, 320px); }
.pine-workspace-bottom  { height: var(--pine-bottom-size, 240px); }
```

| Region | Custom property |
|---|---|
| sidebar | `--pine-sidebar-size` |
| aside | `--pine-aside-size` |
| bottom | `--pine-bottom-size` (height) |

While a drag is in progress the panel carries **`data-dragging`** — gate
your transitions on it so the splitter tracks the pointer 1:1:

```css
[data-dragging] { transition: none !important; }
```

## Region props & state

### Root `<pine-workspace>`

| Field | Kind | Default | Surfaced as |
|---|---|---|---|
| `breakpoint` | `#[model]` String | engine-driven | `data-breakpoint` · `pp:update:breakpoint` |
| `initial` | `#[prop]` String | `base` | tier before resolve / on SSR |

### `<pine-workspace-sidebar>`

| Field | Kind | Default | Notes |
|---|---|---|---|
| `label` | `#[prop]` | | nav landmark name |
| `size` | `#[model]` f64 | 260 | width; `pp-model:size` to persist |
| `collapsed` | `#[model]` bool | false | user rail state; `pp-model:collapsed` |
| `rail_size` | `#[prop]` f64 | 56 | width when collapsed |
| `max` | `#[prop]` f64 | 480 | max expanded width |
| `collapse_at` | `#[prop]` f64 | 160 | drag below this → snap to rail |
| `default` | `#[prop]` f64 | 260 | double-click reset target |
| `collapse_below` | `#[prop]` String | `lg` | tier below which it auto-rails |

Emits: `data-state` (`expanded`/`collapsed`), `data-flyout` (hover while
collapsed), `data-dragging`, and `--pine-sidebar-size`.

> The rendered collapse state is `collapsed || responsive_rail` — the user
> intent (`pp-model:collapsed`) and the responsive auto-rail are kept in
> **separate** fields so the two never fight.

### `<pine-workspace-aside>`

A resizable, collapsible docked column.

| Field | Kind | Default | Notes |
|---|---|---|---|
| `label` | `#[prop]` | | landmark name |
| `side` | `#[prop]` String | `end` | `end` (right) or `start` (left) — sets the resize direction |
| `open` | `#[model]` bool | false | shown/hidden; `pp-model:open` |
| `size` | `#[model]` f64 | 320 | width; `pp-model:size` |
| `min` / `max` / `default` | `#[prop]` f64 | 240 / 560 / 320 | resize bounds + reset |

Emits: `data-side`, `data-aside-state` (`open`/`closed`), `data-dragging`,
`--pine-aside-size`.

### `<pine-workspace-bottom>`

| Field | Kind | Default | Notes |
|---|---|---|---|
| `label` | `#[prop]` | | landmark name |
| `edge` | `#[prop]` String | `bottom` | `bottom` or `top` (sets drag direction; style via `data-edge`) |
| `open` | `#[model]` bool | false | `pp-model:open` |
| `size` | `#[model]` f64 | 240 | height; `pp-model:size` |
| `min` / `max` / `default` | `#[prop]` f64 | 120 / 600 / 240 | resize bounds + reset |

Emits: `data-edge`, `data-state`, `data-dragging`, `--pine-bottom-size`.

### `<pine-workspace-panel>`

| Field | Kind | Default | Notes |
|---|---|---|---|
| `side` | `#[prop]` String | `start` | `start` / `end` |
| `label` | `#[prop]` | | landmark name |
| `open` | `#[model]` bool | true | `pp-model:open` |

Emits: `data-side`, `data-state` (`open`/`closed`).

## Resize handles

Each resizable region embeds a grab handle (`role="separator"` +
`aria-orientation` + `aria-valuenow/min/max`). Style it as a thin strip on
the panel's edge:

```css
.pine-workspace-sidebar { position: relative; }
.pine-workspace-sidebar-resize { position: absolute; top: 0; right: 0; width: 10px; height: 100%; cursor: ew-resize; }
.pine-workspace-sidebar-resize::after { content: ""; position: absolute; inset: 0 0 0 auto; width: 2px; }
.pine-workspace-sidebar-resize:hover::after { background: var(--accent); }
/* aside handle → .pine-workspace-aside-resize (left edge);
   bottom handle → .pine-workspace-bottom-resize (top edge, cursor ns-resize) */
```

Keyboard (when the handle is focused): **arrows** nudge ±16px, **Home/End**
jump to min/max, **double-click** resets to `default`. The size is a
`#[model]`, so `pp-model:size="my_layout"` persists and restores it.

## State is app-controlled

There's no generic `<pine-workspace-trigger>` — regions are driven by your
app via `pp-model`, which keeps the policy in your hands and the components
headless:

```html
<pine-workspace pp-model:breakpoint="bp">
  <pine-workspace-sidebar pp-model:collapsed="sidebar_collapsed" …>
  <pine-workspace-aside  pp-model:open="aside_open" …>
  <pine-workspace-bottom pp-model:open="bottom_open" …>
```

```rust
#[handlers]
impl WorkspaceDemo {
    pub fn toggle_sidebar(&mut self) { self.sidebar_collapsed = !self.sidebar_collapsed; }
    pub fn toggle_aside(&mut self)   { self.aside_open = !self.aside_open; }
    pub fn toggle_bottom(&mut self)  { self.bottom_open = !self.bottom_open; }
}
```

The **sidebar's auto-rail is built in** (it watches the workspace breakpoint
and rails below `collapse_below`). For other responsive policy — e.g. hide
the outer panels on narrow — drive it from the app with a `#[watch(bp)]`
(the `bp` field is bound via `pp-model:breakpoint`):

```rust
#[watch(bp)]
fn on_bp(&mut self, bp: String, _: Option<String>) {
    self.panel_open = !matches!(bp.as_str(), "base" | "sm" | "md");
}
```

## A minimal workspace

```html
<!-- src/WorkspaceDemo.poco -->
<pine-workspace class="ws" pp-model:breakpoint="bp">
  <pine-workspace-header class="ws-header">
    <button class="ws-btn" pp-on:click="toggle_sidebar">☰</button>
    <span class="ws-brand">My&nbsp;App</span>
    <span class="ws-grow"></span>
    <button class="ws-btn" pp-on:click="toggle_aside">Detail</button>
  </pine-workspace-header>

  <pine-workspace-sidebar class="ws-sidebar" label="Primary" pp-model:collapsed="sidebar_collapsed">
    <nav>…icon + <span class="nav-label">label</span> rows…</nav>
  </pine-workspace-sidebar>

  <pine-workspace-main class="ws-main">…page…</pine-workspace-main>

  <pine-workspace-aside class="ws-aside" label="Detail" pp-model:open="aside_open">
    <div class="ws-aside-inner">…task detail / AI / comments…</div>
  </pine-workspace-aside>

  <pine-workspace-footer class="ws-footer"><span pp-text="bp"></span></pine-workspace-footer>
</pine-workspace>
```

```css
.ws {
  display: grid; height: 100vh;
  grid-template-columns: auto 1fr auto;
  grid-template-rows: 48px 1fr auto;
  grid-template-areas: "header header header" "sidebar main aside" "footer footer footer";
}
.ws-header { grid-area: header; } .ws-sidebar { grid-area: sidebar; position: relative; }
.ws-main { grid-area: main; overflow: auto; } .ws-aside { grid-area: aside; position: relative; }
.ws-footer { grid-area: footer; }

.ws-sidebar { width: var(--pine-sidebar-size, 260px); overflow: hidden; }
.ws-sidebar[data-state="collapsed"] .nav-label { display: none; }
.ws-sidebar[data-state="collapsed"][data-flyout] { position: absolute; inset: 0 auto 0 0; width: 240px; z-index: 60; }
.ws-sidebar[data-state="collapsed"][data-flyout] .nav-label { display: inline; }

.ws-aside { width: var(--pine-aside-size, 320px); }
.ws-aside[data-aside-state="closed"] { display: none; }

[data-dragging] { transition: none !important; }
```

(See [`examples/workspace`](../../../examples/workspace) for the full,
styled version.)

## Accessibility

Landmark roles per region; resize handles are `role="separator"` with
`aria-orientation` + `aria-valuenow/min/max` and full keyboard support
(W3C APG window-splitter pattern).

## Gotchas

- **Own-field reactions need `#[watch(field)]`, not
  `watch_scope_field_scoped`** — the latter is for *cross-scope* watching
  (a region reading the root's `breakpoint`); it does **not** fire for a
  component's own fields. (Bit us: a `pp-model` field updated its bindings
  but a same-scope watch never ran.)
- **Resize handles are clipped by `overflow: hidden`** — keep the handle
  *inside* the panel edge (`right: 0`), or the grab strip vanishes.
- **The custom property must sit on the panel**, not the grid container —
  CSS custom properties inherit *down*, so the grid can't read a child's
  `--pine-*-size`. That's why each panel uses an `auto` track and sets its
  own size.

## Deferred

The Aside's **floating/overlay** mode (dock↔float toggle) was prototyped and
pulled from v1 to keep the shell clean — the `floating` overlay runtime
stays for the AppShell drawer, and re-introducing the Aside float is a
contained follow-up. Also future: drag-to-detach floating groups, panel
drag-to-dock, a generic workspace trigger. See `rfcs/rfc-105-pine-workspace.md`.
