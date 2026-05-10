# Interaction

The first interaction layer is intentionally narrow: `PineLineChart`,
`PineScatterChart`, and `PineAreaChart` support nearest-point hover state. They
render SVG crosshair lines, an SVG marker, and an HTML tooltip container.
`PineBarChart` supports rect hit-testing hover and renders the same HTML
tooltip container with bar-specific category/value metadata.

## Contract

Pointer movement over the chart SVG is converted into SVG-space coordinates.
Hover activates only while the pointer is inside the plot rectangle, not while
it is over margins, axes, or tick labels. The component selects the nearest
sampled point by SVG x/y distance and exposes:

- `.pine-chart-hover`
- `.pine-chart-crosshair`
- `.pine-chart-hover-marker`
- `.pine-chart-tooltip`
- `.pine-chart-tooltip-series`
- `.pine-chart-tooltip-x`
- `.pine-chart-tooltip-y`
- `data-hover`
- `data-tooltip="default|none"`
- `data-tooltip-x="left|right"`
- `data-tooltip-y="above|below"`
- `data-x`
- `data-y`
- `data-series`
- CSS variables `--pine-chart-tooltip-x` and `--pine-chart-tooltip-y`

The chart owns sampled-point lookup, crosshair geometry, marker coordinates, and
tooltip data attributes. Tooltip coordinates are emitted as percentages so they
scale with responsive SVG sizing. Applications own colors, marker radius
overrides, tooltip positioning, typography, borders, shadows, and transitions.
Set `tooltip="none"` when the application should render its own tooltip from
`pp:chart:hover` / `pp:chart:hover-end` events. That suppresses only the built-in
HTML tooltip; hover markers, crosshairs, and data attributes still update.

Bar charts use the same pointer coordinate conversion, but they select the
painted SVG rect under the pointer instead of the nearest numeric sample. The
hovered rect receives `data-hovered`; the tooltip exposes `data-category`,
`data-value`, optional `data-series`, and the same placement attributes and CSS
variables as line, scatter, and area charts. Bars do not render a crosshair by
default because the rect itself is the hover target.

Line markers, scatter points, bars, and pie/donut slices also expose a small
selection contract. The chart root is keyboard focusable. Arrow keys move an
internal focused item, and Enter or Space selects it. Pointer clicks select the
clicked marker, point, bar, or slice. Selection emits a bubbling
`pp:chart:select` event from the selected mark/root. Rendered selectable marks
expose:

- `data-key`
- `data-focused`
- `data-selected`
- `aria-selected="true|false"`

Line selection is marker-based, so visible point marks require
`show_markers="true"`. Keyboard selection still tracks the sampled line data,
but an application only gets visible selected/focused line marks when markers
are rendered.

Pie and donut hover follows the same hook-first model as bars: the hovered slice
receives `data-hovered`, and the tooltip exposes `data-label`, `data-value`, and
`data-percentage`. Applications can use that hook for a grow effect with CSS.
Set `animate="true"` on the chart when those transitions should use the chart's
animation variables. Keyframe entry animations should target keyed marks as they
enter the DOM; add/remove updates then animate only the new line, area, bar,
point, or pie segment instead of restarting the whole chart. Pie/donut slices
expose `data-entering="true"` during that window. Pie/donut enter, exit, and
shape changes are rendered by interpolating sector geometry in component state,
so the visible path itself sweeps between sector angles and radii. Area series
and pie/donut slices expose `data-leaving="true"` during their exit window so
CSS or renderer-owned animation can show removal before the renderer prunes the
mark.

Interactive legends are opt-in with `interactive="true"`. The legend then gives
each item keyboard focus, toggles `data-active`, and emits
`pp:chart:legend-toggle`. The chart data remains application-owned; the event is
the hook for filtering or dimming series.

## Styling

```css
.pine-line-chart {
  position: relative;
}

.pine-chart-tooltip {
  left: var(--pine-chart-tooltip-x);
  opacity: 0;
  position: absolute;
  top: var(--pine-chart-tooltip-y);
  transform: translate(10px, calc(-100% - 10px));
  transition: opacity var(--pine-chart-animation-duration, 120ms)
    var(--pine-chart-animation-easing, ease);
  visibility: hidden;
}

.pine-chart-root[data-hover] .pine-chart-tooltip {
  opacity: 1;
  visibility: visible;
}

.pine-chart-tooltip[data-tooltip-x="left"] {
  transform: translate(calc(-100% - 10px), calc(-100% - 10px));
}

.pine-chart-tooltip[data-tooltip-y="below"] {
  transform: translate(10px, 10px);
}

.pine-chart-tooltip[data-tooltip-x="left"][data-tooltip-y="below"] {
  transform: translate(calc(-100% - 10px), 10px);
}

.pine-chart-bar[data-hovered] {
  opacity: 1;
  stroke: currentColor;
}

.pine-chart-marker[data-focused],
.pine-chart-point[data-focused],
.pine-chart-bar[data-focused],
.pine-chart-pie-slice[data-focused] {
  stroke-dasharray: 3 2;
}

.pine-chart-marker[data-selected],
.pine-chart-point[data-selected],
.pine-chart-bar[data-selected],
.pine-chart-pie-slice[data-selected] {
  stroke-width: 3;
}
```

This keeps the primitive useful by default without forcing a dashboard layout or
theme onto the application.
