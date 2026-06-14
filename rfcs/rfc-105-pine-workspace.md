# RFC 105 - `pine-workspace`: content-heavy multi-region workspace shell

| Field | Value |
|---|---|
| **Status** | Implemented (full region set) |
| **Author** | pocopine team |
| **Created** | 2026-06-14 |
| **Related** | [`rfc-103-pine-layout.md`](./rfc-103-pine-layout.md) (the v1 AppShell + breakpoint engine this builds on), [`docs/internal/research/workspace-shell-layouts.md`](../docs/internal/research/workspace-shell-layouts.md) (fact-checked Atlassian/VS Code/PatternFly/Workday/Dockview research), `crates/pine/src/splitter` (the resize reference — *not* a dependency) |

## 1. Summary

`pine-layout`'s v1 `<pine-app-shell>` (RFC-103) covers the **leading** edge of an
app frame — adaptive `drawer→rail→sidebar` nav. RFC-105 adds the **trailing**
edge for content-heavy / productivity apps: a separate **`<pine-workspace>`**
family with a multi-region frame —

```
 [panel] rail/sidebar │ main │ detail aside │ [panel]
                              └── bottom ──┘
```

— where the trailing **Aside is a resizable docked column**, the **Sidebar**
resizes + collapses to a rail with a hover flyout, and a **Bottom** panel
resizes vertically. (A floating/overlay mode for the Aside was prototyped and
**deferred** — see §6.) Still **headless** (zero CSS): regions emit
a class + `data-*` state + a `--pine-*-size` custom property for the one dynamic
(resizable) dimension; author CSS does the layout.

## 2. Grounding (research)

Verified (25/25 claims) against primary sources — see the research doc:
- **Atlassian `@atlaskit/page-layout`**: named regions; LeftSidebar resizes,
  collapses to a **20px rail** (not zero) below a 200px drag threshold, hover
  flyout. Region defaults (sidebar 240 / aside 280 / panel 368).
- **VS Code workbench**: Activity Bar · Primary/Secondary Side Bars · Editor ·
  relocatable Panel; independently-toggled trailing bar.
- **Float vs anchor** is a first-class dual mode: PatternFly `overlay`/`inline`,
  Workday "push+resize OR float over scrim", Dockview detach-to-float. (Vendors
  document them as *configured alternatives*; the **live in-place flip** is our
  design.)

## 3. Region model (`src/workspace/`)

```
<pine-workspace>          root: grid container · breakpoint engine · WORKSPACE ctx
  <pine-workspace-header>     banner (sticky)
  <pine-workspace-panel side=…>  outer secondary nav · collapsible
  <pine-workspace-sidebar>    primary nav · RESIZE + collapse-to-rail + flyout
  <pine-workspace-main>       content · flex-fill remainder
  <pine-workspace-aside>      detail/AI panel · resizable docked column · collapsible
  <pine-workspace-bottom>     console · vertical resize + collapse + edge
  <pine-workspace-footer>     status bar (sticky)
```

| Region | resizable | collapsible |
|---|:--:|:--:|
| Sidebar | ✅ grab handle | ✅ → rail + flyout |
| Aside | ✅ | ✅ (`open`) |
| Bottom | ✅ (vertical) | ✅ (edge-relocatable) |
| Panel (outer) | — | ✅ |
| Header/Footer/Main | — | Header/Footer show/hide |

## 4. Headless contract

No `.css`, no `style=` for visual properties. Each region emits: a class; `data-*`
state (`data-state`, `data-aside-state`, `data-flyout`, `data-dragging`, …); and
a **`--pine-<region>-size`** custom property carrying the one dynamic dimension a
static stylesheet can't express. Author CSS:

```css
.pine-workspace { display: grid; grid-template-columns: auto 1fr auto; }
.pine-workspace-sidebar { width: var(--pine-sidebar-size, 260px); }
.pine-workspace-aside { width: var(--pine-aside-size, 320px); }
.pine-workspace-aside[data-aside-state="closed"] { display: none; }
[data-dragging] { transition: none; }   /* freeze transitions mid-drag */
```

## 5. The own resize engine (`src/workspace/resize.rs`)

Self-contained — **no dependency on `pine`/`splitter`**. A pure, host-tested
`resolve_size(start, delta, invert, min, max, collapse_at)` (clamp + snap) plus a
wasm `start_drag` that installs document `pointermove`/`up`/`cancel` via
`pocopine::events::{on, ev}` (held in a `ListenerHandle` vec, cleared on
release), replicating `splitter`'s pattern. Each resizable region embeds a
`role="separator"` handle (keyboard arrows / Home-End / double-click-reset) and
stores `#[model] size` (persisted via `pp-model:size`).

## 6. Aside — resizable docked column

The Aside is an in-grid, resizable column shown/hidden via `pp-model:open`
(`data-aside-state`), with the same resize handle + `--pine-aside-size`
contract as the Sidebar.

**Floating deferred.** A dock↔float toggle (overlay + scrim + the shared
`floating` focus-trap runtime, with auto-float below a breakpoint) was
prototyped on top of this. It worked, but the overlay layering and live
toggle added enough complexity — and enough rough edges (focus-targeting on
re-open, scrim stacking contexts) — that it's **pulled from v1** to keep the
shell clean. The `floating` runtime (`src/floating.rs`) remains, shared by the
AppShell drawer; re-introducing the Aside float is a contained follow-up. The
float-vs-anchor research (§2) stands for when it lands.

## 7. Responsive — decentralized

The root runs the breakpoint engine; each region **watches the root's
`breakpoint`** and derives its own state from its threshold, so the collapse
order falls out outside-in (panels hide → bottom collapses → sidebar rails),
with no central solver. The Sidebar keeps user intent (`collapsed`, `pp-model`)
**separate** from the responsive override (`responsive_rail`, internal) so the
two never race; the rendered state is their OR.

## 8. Decisions

- **Own resize**, not a `pine::splitter` dependency (avoids a layering inversion;
  the engine is small).
- **New family**, not an AppShell extension — AppShell stays the simple nav shell.
- **No generic `<pine-workspace-trigger>`** — regions are app-controlled via
  `pp-model` (open/collapsed) + region-local affordances; cleaner and headless.
  A generic trigger can come later.
- **Aside is anchored-only** in v1 — float deferred (§6).

## 9. Accessibility

Landmark roles per region (`banner`/`navigation`/`main`/`complementary`/
`contentinfo`); resize handles `role="separator"` + `aria-orientation` +
`aria-valuenow/min/max` + keyboard (W3C APG window-splitter).

## 10. Verification

- **Host unit tests**: `resolve_size` clamp/snap; `nav_mode` (shared helper);
  AppShell's existing tests unchanged.
- **Browser tests** (`tests/workspace.rs`): regions render with landmarks, the
  size var + resize handle + ARIA, aside state, sidebar auto-collapse.
- **Playwright** drive of `examples/workspace`: sidebar/aside drag-resize, the
  Detail toggle, and narrow-viewport auto-rail — all confirmed.

## 11. Implementation status

| Unit | Status |
|---|---|
| Resize engine + shared `nav_mode` helper | ✅ |
| Root · Main · Sidebar (resize/rail/flyout) | ✅ |
| Aside (resizable docked column + collapse) | ✅ |
| Outer Panel · Bottom · Header · Footer | ✅ |
| `floating` runtime (shared with AppShell drawer) | ✅ |
| Browser + host tests · `examples/workspace` | ✅ green |
| Aside float (overlay/dock toggle) · drag-to-detach · panel drag-to-dock · generic trigger | ⏳ later |
