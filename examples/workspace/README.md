# pine-workspace — content-heavy multi-region shell

A showcase of [`pine-layout`](../../crates/pine-layout)'s **workspace** family
(RFC-105): a resizable rail/sidebar │ main │ a resizable detail **Aside** │ a
collapsible bottom console — the Atlassian/VS-Code-style frame for productivity
apps.

## Run

```bash
cargo run -p pocopine-cli -- dev --path examples/workspace
```

Open the URL it prints (defaults to <http://localhost:5243>; the configured port
is 3012 — pass `--port` to choose).

## Try it

- **Drag the splitters** between regions to resize (double-click a handle to
  reset; arrow keys nudge when focused).
- **Detail** toggles the trailing Aside (a resizable docked column).
- **Resize the window**: the sidebar auto-collapses to a rail (hover it for the
  flyout).
- **Console** toggles the bottom panel (drag its top edge to resize).

## Zero CSS in the library

`pine-layout` ships no stylesheet. The whole layout in
[`styles.css`](./styles.css) keys off the regions' headless hooks:

| Hook | Region | Drives |
|------|--------|--------|
| `--pine-sidebar-size` / `--pine-aside-size` / `--pine-bottom-size` | sidebar / aside / bottom | the resizable dimension |
| `data-state="expanded\|collapsed"` / `data-flyout` | sidebar | rail collapse + hover flyout |
| `data-aside-state="open\|closed"` | aside | show / hide |
| `data-dragging` | any resizable | suppress transitions mid-drag |
| `data-breakpoint` | workspace root | the live tier (badge) |

Region state (sidebar collapsed, aside open, bottom open) is held in app fields
and two-way-bound with `pp-model` — see `src/lib.rs` / `WorkspaceDemo.poco`.
