# Axes and Grid

`PineLineChart` and `PineBarChart` emit a basic SVG guide layer:

- grid lines for x ticks,
- grid lines for y ticks,
- x and y axis domain lines,
- tick marks and labels.

Bar charts omit vertical category grid lines for now, but they use the same axis
and tick-label hooks. Stacked bars infer their y domain from positive and
negative stack totals, so the axis baseline stays meaningful for mixed-sign
data.

This remains intentionally generated from the same low-level scale data. The
component does not own colors, fonts, or stroke widths.

## Styling Hooks

Use these selectors for chart guides:

- `.pine-chart-grid`
- `.pine-chart-grid-line`
- `.pine-chart-grid-line-x`
- `.pine-chart-grid-line-y`
- `.pine-chart-axes`
- `.pine-chart-axis`
- `.pine-chart-axis-x`
- `.pine-chart-axis-y`
- `.pine-chart-tick`
- `.pine-chart-tick-x`
- `.pine-chart-tick-y`
- `.pine-chart-tick-line`
- `.pine-chart-tick-label`

Example:

```css
.pine-chart-grid-line {
  stroke: color-mix(in oklab, currentColor 18%, transparent);
  stroke-width: 1;
}

.pine-chart-axis,
.pine-chart-tick-line {
  stroke: currentColor;
  stroke-width: 1;
}

.pine-chart-tick-label {
  fill: currentColor;
  font: 12px system-ui;
}
```

## Composition Boundary

Axes and grid are currently part of each chart component instead of separate
child components. That is deliberate for the first SVG pass: HTML custom elements
inside SVG have namespace and slotting edge cases, so the first stable component
keeps the SVG tree self-contained. Once that path is proven in examples, the
same generated guide data can be exposed to composable axis and grid components.
