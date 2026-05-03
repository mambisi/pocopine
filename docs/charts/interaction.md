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

Bar charts use the same pointer coordinate conversion, but they select the
painted SVG rect under the pointer instead of the nearest numeric sample. The
hovered rect receives `data-hovered`; the tooltip exposes `data-category`,
`data-value`, optional `data-series`, and the same placement attributes and CSS
variables as line, scatter, and area charts. Bars do not render a crosshair by
default because the rect itself is the hover target.

## Styling

```css
.pine-line-chart {
  position: relative;
}

.pine-chart-tooltip {
  left: var(--pine-chart-tooltip-x);
  position: absolute;
  top: var(--pine-chart-tooltip-y);
  transform: translate(10px, calc(-100% - 10px));
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
```

This keeps the primitive useful by default without forcing a dashboard layout or
theme onto the application.
