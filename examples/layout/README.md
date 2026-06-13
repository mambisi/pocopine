# pine-layout — responsive app shell

A styled showcase of [`pine-layout`](../../crates/pine-layout): a single
`<pine-app-shell>` whose navigation adapts from an off-canvas **drawer**
(narrow) → an icon **rail** (medium) → a full **sidebar** (wide) as the
viewport crosses the Stylekit breakpoints.

## Run

```bash
cargo run -p pocopine-cli -- dev --path examples/layout
```

Then open the URL it prints (the dev server defaults to
<http://localhost:5243>; pass `--port` to pick another) and **resize the
window**. The corner badge shows the live breakpoint tier.

## The point: zero CSS in the library

`pine-layout` ships no stylesheet. Every visual decision in this demo
lives in [`styles.css`](./styles.css), keyed off the `data-*` hooks the
components emit:

| Hook | Where | Drives |
|------|-------|--------|
| `data-nav-mode="drawer\|rail\|sidebar"` | shell + sidebar + trigger | the whole adaptive layout |
| `data-state="open\|closed"` | sidebar | the drawer slide-in |
| `data-breakpoint="md"` | shell | (available for CSS / `pp-model`) |
| `data-cols` / `data-span` / `data-gap` | grid / items | the 12-col content grid |

The breakpoint is also two-way bound into app state via
`pp-model:breakpoint="bp"` and rendered in the badge — proof the tier is
available to logic, not just CSS.

## What's exercised

- `<pine-app-shell>` + Header / Sidebar / Content / Footer / Trigger —
  adaptive nav, modal drawer (focus trap, scroll lock, **Esc** to close,
  ARIA `aria-controls` wiring).
- `<pine-container>` / `<pine-stack>` / `<pine-inline>` / `<pine-grid>` +
  `<pine-grid-item>` — the headless structural primitives.
- [`pine-motion`](../../crates/pine-motion) — the drawer slide is a
  `pine_motion::animate(&sidebar, …, Spring::gentle())` fired from a
  `#[watch(nav_open)]` (see `src/lib.rs`).

## Notes

- The drawer slide is animated **imperatively** with `pine-motion`, only
  on a user toggle — *not* a CSS `transition`. That's deliberate: a
  declarative `transition: transform` would also fire when a breakpoint
  reflow flips the nav into drawer mode, animating an unwanted slide-out.
  Driving it from the toggle means resizing into drawer width just snaps.
- The drawer closes via the hamburger toggle or **Esc**. Scrim /
  outside-click dismissal is deferred to `pine-layout` v2 (see
  [RFC-103](../../rfcs/rfc-103-pine-layout.md) §10) — a capture-phase
  outside listener would fight the toggle.
- Responsive column counts (the grid collapsing to one column on narrow
  screens) are a plain author media query in `styles.css`; the library
  only emits the `data-cols` / `data-span` hooks.
