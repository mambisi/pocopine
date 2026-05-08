# Axes and Grid

`PineLineChart`, `PineScatterChart`, `PineAreaChart`, and `PineBarChart` emit a
basic SVG guide layer:

- grid lines for x ticks,
- grid lines for y ticks,
- x and y axis domain lines,
- tick marks and labels,
- optional x and y axis labels from `x_label` and `y_label`.

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
- `.pine-chart-axis-label`
- `.pine-chart-axis-label-x`
- `.pine-chart-axis-label-y`
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

.pine-chart-axis-label {
  fill: currentColor;
  font: 600 12px system-ui;
}
```

## Composition Boundary

Preset charts (`pine-line-chart`, `pine-scatter-chart`, `pine-area-chart`, and
`pine-bar-chart`) still own their generated axes and grid. That keeps the common
case compact: set `x_label` / `y_label`, bind data, and style the emitted hooks.
(`pine-pie-chart` is radial and has no axes or grid.)

`pine-cartesian-chart` exposes guide ownership as child components:

```html
<pine-cartesian-chart label="Revenue">
  <pine-chart-grid></pine-chart-grid>
  <pine-x-axis label="Week"></pine-x-axis>
  <pine-y-axis label="Revenue"></pine-y-axis>
  <!-- series and reference children -->
</pine-cartesian-chart>
```

Those child tags are definitions, not nested SVG nodes. They register intent with
the nearest Cartesian root, and the root still renders one valid SVG tree. This
preserves namespace correctness, shared scales, responsive sizing, and paint
order while letting applications opt into or omit guides explicitly.
