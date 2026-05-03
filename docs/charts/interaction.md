# Interaction

The first interaction layer is intentionally narrow: `PineLineChart` supports
nearest-point hover state. It renders SVG crosshair lines, an SVG marker, and an
HTML tooltip container.

## Contract

Pointer movement over the chart SVG is converted into SVG-space coordinates.
Hover activates only while the pointer is inside the plot rectangle, not while
it is over margins, axes, or tick labels. The component selects the nearest
sampled point by x distance and exposes:

- `.pine-chart-hover`
- `.pine-chart-crosshair`
- `.pine-chart-hover-marker`
- `.pine-chart-tooltip`
- `.pine-chart-tooltip-x`
- `.pine-chart-tooltip-y`
- `data-hover`
- `data-tooltip-x="left|right"`
- `data-tooltip-y="above|below"`
- `data-x`
- `data-y`
- CSS variables `--pine-chart-tooltip-x` and `--pine-chart-tooltip-y`

The chart owns sampled-point lookup, crosshair geometry, marker coordinates, and
tooltip data attributes. Tooltip coordinates are emitted as percentages so they
scale with responsive SVG sizing. Applications own colors, marker radius
overrides, tooltip positioning, typography, borders, shadows, and transitions.

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
```

This keeps the primitive useful by default without forcing a dashboard layout or
theme onto the application.
