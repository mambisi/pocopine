---
title: "Axes and Grid"
description: "How pine-charts renders axes, grid lines, and tick labels, and how to style or compose them."
---

# Axes and Grid

`pine-line-chart`, `pine-scatter-chart`, `pine-area-chart`, and `pine-bar-chart`
each emit a Cartesian guide layer inside their SVG output:

- grid lines aligned to x ticks (line/scatter/area) and y ticks (all four),
- x and y axis domain lines,
- tick marks and tick labels,
- optional axis labels set via the `x_label` and `y_label` props.

`pine-bar-chart` renders only horizontal (y) grid lines because its x axis is
categorical — each band corresponds to a label, not a numeric tick. Stacked bars
derive the y domain from the sum of positive stacks and the sum of negative
stacks, so the baseline remains meaningful for mixed-sign data.

The guide layer carries no default colors, fonts, or stroke widths. All visual
treatment comes from CSS selectors described below.

## Styling Hooks

The following CSS classes are emitted on the SVG guide elements:

| Selector | Element |
|---|---|
| `.pine-chart-grid` | `<g>` wrapping all grid lines |
| `.pine-chart-grid-line` | individual grid `<line>` |
| `.pine-chart-grid-line-x` | x-tick grid line |
| `.pine-chart-grid-line-y` | y-tick grid line |
| `.pine-chart-axes` | `<g>` wrapping both axes |
| `.pine-chart-axis` | axis domain `<line>` |
| `.pine-chart-axis-x` | x axis domain line |
| `.pine-chart-axis-y` | y axis domain line |
| `.pine-chart-axis-label` | axis label `<text>` |
| `.pine-chart-axis-label-x` | x axis label |
| `.pine-chart-axis-label-y` | y axis label |
| `.pine-chart-tick` | `<g>` wrapping one tick mark + label |
| `.pine-chart-tick-x` | x-axis tick group |
| `.pine-chart-tick-y` | y-axis tick group |
| `.pine-chart-tick-line` | tick mark `<line>` |
| `.pine-chart-tick-label` | tick `<text>` |

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
`pine-bar-chart`) own their axes and grid directly. The common workflow is to set
`x_label` / `y_label` props, bind data, and style the emitted hooks.
`pine-pie-chart` and `pine-radial-bar-chart` are radial and have no axes or grid.

`pine-cartesian-chart` externalises guide ownership as child components:

```html
<pine-cartesian-chart label="Revenue over time">
  <pine-chart-grid></pine-chart-grid>
  <pine-x-axis label="Week"></pine-x-axis>
  <pine-y-axis label="Revenue"></pine-y-axis>
  <!-- series and reference children -->
</pine-cartesian-chart>
```

`pine-chart-grid` accepts `x` and `y` boolean props to enable or disable each
grid direction independently:

```html
<pine-chart-grid x="false" y="true"></pine-chart-grid>
```

These child components are definition nodes — they render as hidden elements and
register intent with the nearest `pine-cartesian-chart` root. The root produces
one valid SVG tree, preserving namespace correctness, shared scales, responsive
sizing, and correct paint order while letting you opt into or omit guides
explicitly.
