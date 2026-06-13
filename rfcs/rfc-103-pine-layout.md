# RFC 103 - `pine-layout`: headless responsive layout primitives

| Field | Value |
|---|---|
| **Status** | Implemented (v1 — Foundation + AppShell; SplitPane/ListDetail deferred to v2) |
| **Author** | pocopine team |
| **Created** | 2026-06-13 |
| **Related** | [`rfc-092-pocopine-stylekit.md`](./rfc-092-pocopine-stylekit.md) (breakpoint scale + container queries this crate aligns to), [`docs/internal/research/layout-design-systems.md`](../docs/internal/research/layout-design-systems.md) (fact-checked Apple/Material/Atlassian research base), core Pine (`crates/pine` — the headless / zero-CSS doctrine) |

## 1. Summary

`pine-layout` is a new Pine sub-library for **scaffolding responsive
applications**. It ships **zero CSS** — exactly like core Pine. Its
value is not opinions about how layouts *look*; it is:

1. **One canonical breakpoint vocabulary** (`base`/`sm`/`md`/`lg`/`xl`/
   `2xl`) resolved from the viewport, surfaced three ways — a
   `data-breakpoint` attribute (for CSS), a `BREAKPOINT`/`SHELL`
   context (for descendant components), and a `pp:update:breakpoint`
   model event (for app logic).
2. **An adaptive-navigation state machine** (`<pine-app-shell>`) that
   derives a `nav_mode` of `drawer` → `rail` → `sidebar` from the
   breakpoint and manages the modal-drawer behavior (focus trap,
   scroll lock, Escape, ARIA) — leaving every pixel of styling to the
   author.
3. **A small set of thin structural primitives** (`Container`,
   `Stack`, `Inline`, `Grid`/`GridItem`) that give a consistent,
   documented vocabulary + `data-*` hooks for author CSS or Stylekit
   utilities.

It is a *responsiveness-behavior* library, not a CSS library.

## 2. Motivation & prior art

The design was grounded in a fact-checked sweep of the three major
design systems (full citations in the research doc):

| Dimension | Apple HIG | Material 3 | Atlassian DS |
|---|---|---|---|
| Size model | `compact`/`regular` | window size classes | **6 px breakpoints** `xxs…xl` |
| Grid | Auto Layout / stacks | 4/8dp grid | **2→6→12 cols** |
| Spacing | 8pt baseline | 4dp/8dp | **8px base**, `space.0…space.1000` |
| Multi-pane | Split View (⅓/⅔) | List-Detail · Feed · Supporting (50/50→70/30) | grid-driven |
| Adaptive nav | tab bar → sidebar/split | bottom bar → rail → drawer | side nav |
| Reflow rule | collapse in compact | **combined-min-width** (`SlidingPaneLayout`) | column count |

All three converge on: **named viewport tiers → a few layout
primitives → canonical multi-pane patterns that reflow narrow→wide**,
and diverge on exact numbers (and unit — px vs dp vs pt). The one
portable algorithm worth stealing is Android's *combined-min-width*
pane-reflow rule (deferred to v2 with SplitPane).

## 3. The decisive constraint: align to Stylekit

`pocopine-stylekit` already ships the Tailwind breakpoint scale
(`crates/pocopine-stylekit/src/registry.rs:3140`):

| tier | min-width |
|------|-----------|
| `sm` | 640px |
| `md` | 768px |
| `lg` | 1024px |
| `xl` | 1280px |
| `2xl`| 1536px |

`pine-layout`'s reactive breakpoint **must** resolve to these exact
thresholds, or `data-breakpoint == "md"` and the `md:` utility prefix
would disagree. This settles the "Atlassian-6 vs Material-tiers"
question the research left open: **reuse Stylekit's 5** (plus `base`).

## 4. The headless contract

No `.css` file, no `style=` for visual properties. Every component
exposes state four ways:

```
1. semantic class ....... <div class="pine-grid">                  (author CSS target)
2. data-* attributes .... data-breakpoint="md"  data-cols="12"     (attribute selectors)
                          data-nav-mode="rail"  data-state="open"
3. reactive context ..... create_context!(SHELL) + child #[observe(SHELL)]
4. model events ......... #[model] breakpoint / nav_open  (pp:update:*, pp-model bind)
```

The author writes the layout CSS, or drops Stylekit utilities straight
onto the elements (`<pine-grid class="grid grid-cols-12 md:grid-cols-6">`).
Stylekit only scans app source (not dependency crates), so the crate
deliberately self-contains its contract in `data-*`/context/model and
never relies on the app's Stylekit glob picking up its templates.

## 5. Breakpoint engine — `src/breakpoint.rs`

- `enum Breakpoint { Base, Sm, Md, Lg, Xl, Xxl }` with `as_str`,
  `from_token`, `rank`, `min_width`, and an ascending `TIERS` table.
- `install(on_change)` registers one `matchMedia("(min-width: Npx)")`
  per threshold (mirroring the established `prefers-reduced-motion`
  pattern in `pocopine-core/src/animate/motion.rs`), resolves the
  largest matching tier, calls `on_change` on every crossing, and
  detaches all listeners on scope unmount. The initial value is seeded
  through `tick::next` to clear the `on_ready` immutable borrow.
- **wasm-gated**: on non-wasm / SSR there is no viewport, so the
  breakpoint stays at its configured `initial` (default `base`).

`<pine-breakpoint>` is a thin standalone provider over the engine for
non-AppShell pages.

## 6. AppShell — adaptive-nav state machine

`nav_mode` is derived from the breakpoint and the `rail_at` /
`sidebar_at` thresholds (defaults `md` / `xl`):

```
breakpoint:  base   sm     md    lg     xl      2xl
nav_mode:    drawer drawer rail  rail   sidebar sidebar
             └ off-canvas ┘     └ rail ┘  └ full sidebar ┘
```

Six parts, all headless (`#[component(role = "panel")]` → `<div>` with
landmark ARIA `role` attributes, matching how `pine::separator` sets
its role):

| Tag | Role | Behavior |
|---|---|---|
| `<pine-app-shell>` | `scope` | owns the engine + state machine; provides `SHELL`; `#[model] breakpoint`, `#[model] nav_open`; emits `data-breakpoint`/`data-nav-mode`/`data-nav-open` |
| `<pine-app-shell-header>` | `banner` | landmark region |
| `<pine-app-shell-sidebar>` | `navigation` | adaptive nav; `data-nav-mode`/`data-state`; in drawer mode: focus trap + scroll lock + Escape |
| `<pine-app-shell-content>` | `main` | landmark region |
| `<pine-app-shell-footer>` | `contentinfo` | landmark region |
| `<pine-app-shell-trigger>` | `interactive` (`<button>`) | hamburger toggle; `aria-expanded` + `aria-controls` → sidebar id |

Growing the viewport out of drawer mode auto-closes an open drawer so
focus/scroll are released. The drawer's modal runtime (saved focus +
trap handle) lives in a per-scope thread-local side-table keyed by the
sidebar's scope id — the same shape `pine::overlay` uses, since those
handles aren't serde-friendly component fields. The sidebar reacts to
`nav_open` via `watch_scope_field_scoped` (the proven cross-scope watch
Splitter uses), not an `#[observe]`→`#[watch]` chain.

## 7. Structural primitives

All `#[component(role = "panel")]`, all headless — they emit a class +
`data-*` mirrors of their props and nothing else:

- `<pine-container>` — `size`, `safe_area`
- `<pine-stack>` — `gap`, `align`, `justify`
- `<pine-inline>` — `gap`, `align`, `justify`, `wrap`
- `<pine-grid>` (default `cols="12"`) + `<pine-grid-item>` (`span`, `start`)

## 8. Accessibility

Landmark roles on every shell region; `<button>` trigger with
`aria-expanded`/`aria-controls`; drawer focus trap + scroll lock +
Escape + focus restore. Breakpoint changes mutate `data-*` only — no
focus or scroll side-effects.

## 9. Testing

- **Host unit tests** (`cargo test -p pine-layout`, deterministic, no
  DOM): the full `Breakpoint` enum and the `nav_mode` state machine
  across all tiers + custom thresholds + toggle/open/close semantics +
  drawer auto-close.
- **Browser tests** (`wasm-pack test --firefox --headless`): rendered
  `data-*` hooks, ARIA wiring (trigger `aria-controls` → sidebar id),
  and the drawer toggle.

## 10. Deliberate non-goals / deferred to v2

- **Scrim / outside-click drawer dismissal.** A capture-phase
  `@click.outside` on the sidebar treats the toggle as "outside" and
  cancels it (the known Pine dropdown trigger-while-open bug). v1 ships
  Escape + toggle; a dedicated backdrop sub-component is the v2 fix.
- **`<pine-split-pane>` / `<pine-list-detail>`** multi-pane reflow via
  the combined-min-width rule (`ResizeObserver`; ⅓/⅔ + 50/50/70-30).
- **Container-query-driven** component responsiveness (`@container` —
  Stylekit already supports it).
- **`pocopine` umbrella re-export** — apps `use pine_layout::…` until
  the API stabilizes (per the defer-umbrella-reexports doctrine).

## 11. Implementation status (v1)

| Unit | Status |
|---|---|
| Crate scaffold + workspace registration | ✅ |
| Breakpoint engine + `<pine-breakpoint>` | ✅ |
| `Container`/`Stack`/`Inline`/`Grid`/`GridItem` | ✅ |
| AppShell compound (6 parts) + nav state machine | ✅ |
| Host unit tests (9) + browser tests (6) | ✅ green |
| SplitPane / ListDetail · scrim dismissal · container queries | ⏳ v2 |
