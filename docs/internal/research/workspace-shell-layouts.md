# Multi-Region Workspace-Shell Layouts — Research for `pine-layout` v2

> Fact-checked research base for the content-heavy "workspace shell" (the
> `rail/sidebar │ main │ detail aside │ panel` frame with float/anchor
> trailing panels). Compiled 2026-06-13. 25/25 claims survived 3-vote
> adversarial verification. Claims marked ⚠️ are unverified.

## TL;DR

The canonical content-heavy apps converge on a small, repeatable **named-region
model** with three orthogonal per-region capabilities — **resizable**,
**collapsible**, **floatable** — and one trailing panel that toggles
**dock (anchor) ↔ float (overlay)**. Resizing is always a splitter/sash with
min/max + snap-to-collapse + persisted sizes — which is exactly Pine's existing
`splitter`. pine-layout v1's `drawer→rail→sidebar` nav is the *leading* edge of
this model; v2 adds the *trailing* edge.

```
 outside ◄───────────────────────── regions ─────────────────────────► outside
 [LeftPanel] [Sidebar] │      Main / Content       │ [Aside] [RightPanel]
   368px   240→rail20   │   flex-fill, scrolls      │  280px    368px
            resize       │   the ONLY remainder      │ resize+FLOAT
            collapse                                    collapse
                          [ BottomPanel — relocatable, resize, collapse ]
```

## Verified region dimensions — Atlassian `@atlaskit/page-layout`

Confirmed verbatim against the published build + `atlassian-frontend-mirror`
source constants:

| Region | Default | Notes |
|--------|---------|-------|
| Banner | 56px tall | full-width, sticky |
| TopNavigation | 56px tall | sticky |
| LeftPanel | 368px | outer secondary nav, flush |
| **LeftSidebar** | **240px** | **resizable, collapses to 20px rail (16px mobile) — NOT zero** |
| Main | flex | the only remainder/scroll region |
| RightSidebar | 280px | trailing |
| RightPanel | 368px | outer, mirror of LeftPanel |
| Aside | — | trailing contextual panel |

**LeftSidebar resize/collapse mechanics** (from `resize-control` source):
- drag handle (`data-grab-area` / `data-resize-control` / `data-resize-button`)
- release `width < 200` → collapse to **20px rail** (`collapseLeftSidebar`)
- `200 ≤ width < 240` → snap back up to 240
- `width ≥ 240` → keep dragged width
- collapsed rail shows a **240px hover flyout** after **200ms**, **300ms** transition (desktop; mobile = click-toggle + Esc)
- state attrs: `data-is-sidebar-dragging`, `data-is-sidebar-collapsing`

⚠️ `@atlaskit/page-layout` is **deprecated** in favour of
`@atlaskit/navigation-system` (SideNav/Panel/Aside). The numbers above are
accurate in the still-published build; the **new** system's names/defaults were
**not** characterized — open gap. pine-layout should use its own headless names
informed by both, not mirror a deprecated API.

## VS Code workbench — the multi-region skeleton

Six named areas (+ Title/Status bars): **Activity Bar · Primary Side Bar**
(left default; Explorer/Search/SCM) **· Editor · Panel** (bottom) **· Secondary
Side Bar** (opposite Primary; default Chat) **· Status Bar**. Key behaviors:
- **Primary Side Bar side is user-configurable** (left↔right; `workbench.sideBar.location`).
- **Secondary Side Bar** is independently toggleable; **context-dependent default
  visibility** (shown with a folder open, hidden in an empty window).
- **Panel is relocatable** to left/right/bottom/top of the editor.
- **Priority contrast**: Primary = high-visibility (extensions contribute Views);
  Secondary = auxiliary (populated by dragging Views in). → a region model should
  encode a visibility priority.

## Float ↔ Anchor — a first-class dual mode

| Source | Anchored / dock | Floating / overlay |
|--------|-----------------|--------------------|
| **PatternFly** drawer | `inline` — beside content, compacts/pushes (still visible) | `overlay` — on top, must close to see covered content |
| **Workday Canvas** Side Panel | push **and resize** content (expand/collapse toggle) | `alternate` variant over a scrim; page non-interactive; close button, no collapse |
| **Dockview** | edge-docked groups, snap to border | detach group to freely-positioned floating overlay |

- PatternFly: **add a splitter to resize the inline drawer**; omit it when content
  has enough space; orientation vertical or horizontal. → compose Pine's splitter.
- ⚠️ Vendors document the two modes as **configured alternatives** ("either…or…",
  a variant prop), **not** a live in-place dock↔float flip preserving
  content/scroll/focus. **That live toggle is pine-layout's own design to build.**

## Sidebar collapse — two documented targets

- **Atlaskit**: collapse to a **20px rail** + hover flyout (re-expand affordance).
- **Linear**: **full collapse to hidden** (`[` shortcut / edge-click / command
  palette). → pine-layout should support both (rail = v1 AppShell default; hide
  as an option).

## Resizable-panel UX contract (W3C APG + react-resizable-panels + allotment + reka-ui)

splitter/sash drag · per-panel **min/max** · **snap-to-collapse threshold** ·
**double-click-to-reset** · **persisted sizes** · **keyboard resize** (arrows,
Shift = larger step, Home/End to extremes; `role="separator"` + `aria-valuenow`).

**Pine `splitter` already has**: per-panel min/max, keyboard (1% / Shift 10% /
Home-End), `role="separator"` + `aria-valuenow`, and `pp-model:sizes`
persistence. **Missing for parity**: explicit snap-to-collapse threshold +
double-click-reset.

## Responsive collapse order (synthesis)

As width shrinks, collapse **outside-in**:
1. **RightPanel / LeftPanel** (outer secondary) hide first
2. **Aside** switches anchored → **float** (stops reserving column width)
3. **BottomPanel** shrinks or hides
4. **Sidebar** steps `sidebar → rail → drawer` (existing AppShell progression)

Gate N side-by-side panes on a **combined-min-width** rule: show them only when
`viewport ≥ Σ(pane min-widths) + Main floor (~600–640px)` — the same
combined-min-width heuristic from the v1 layout research (Android
`SlidingPaneLayout`).

## Implications for `pine-layout`

1. **Extend the existing AppShell** rather than a separate shell — the trailing
   regions (`Aside`, `Panel`) are opt-in additions to the v1 `Header/Sidebar/
   Content/Footer` set; the leading `drawer→rail→sidebar` nav already exists.
2. **The `Aside` is the one floatable region** — reuse v1's drawer runtime
   (scrim + focus-trap + scroll-lock + Esc) for its float mode; anchored mode is
   an in-grid resizable column.
3. **Resize = compose a splitter** — but Pine's `splitter` lives in `crates/pine`
   (a primitives layering issue for `pine-layout` to depend on). Options:
   (a) pine-layout ships its own minimal single-split resize handle;
   (b) extract the resize engine to `pocopine-core` (per the *core-owns-engines*
   doctrine) and share it with `pine::splitter`; (c) author composes
   `pine::splitter` themselves. Decision pending.
4. **Stay headless** — every region emits a class + `data-*` (`data-aside-mode`,
   `data-aside-state`, `data-region`, etc.); author CSS grid does the layout, as
   in v1.

## Caveats

- `@atlaskit/page-layout` deprecated (constants still accurate); navigation-system
  not characterized.
- Live dock↔float flip is **our** design, not a cited vendor behavior.
- **ClickUp / Notion / Slack** docking specifics did **not** survive verification
  (no first-party source); only **Linear** did. Treat "those apps dock/float the
  AI panel" as unconfirmed.
- Synthesis (region set, capability matrix, collapse order, ~600–640px Main floor)
  is opinionated recommendation aggregated from the verified claims + the existing
  codebase — not a single cited spec.

## Primary sources

- Atlassian page-layout: https://atlaskit.atlassian.com/packages/design-system/page-layout · https://www.npmjs.com/package/@atlaskit/page-layout
- Atlassian navigation-system: https://atlassian.design/components/navigation-system/layout
- VS Code UI: https://code.visualstudio.com/docs/getstarted/userinterface · https://code.visualstudio.com/docs/configure/custom-layout · https://code.visualstudio.com/api/ux-guidelines/sidebars
- PatternFly drawer: https://www.patternfly.org/components/drawer/design-guidelines/
- Workday Canvas Side Panel: https://canvas.workday.com/components/containers/side-panel
- Dockview: https://dockview.dev/ · https://dockview.dev/docs/core/groups/floatingGroups/
- Linear collapsible sidebar: https://linear.app/changelog/unpublished-collapsible-sidebar
- W3C APG window splitter: https://www.w3.org/WAI/ARIA/apg/patterns/windowsplitter/
- react-resizable-panels: https://github.com/bvaughn/react-resizable-panels · allotment: https://github.com/johnwalley/allotment
