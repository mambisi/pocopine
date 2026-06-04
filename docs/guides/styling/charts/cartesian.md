---
title: "Cartesian Composition"
description: "pine-cartesian-chart is the compound-component surface for Cartesian charts. It follows the same pattern as Pine UI primitives: children describe chart parts, and the root owns state, scales, SVG rendering, layer order, and validation."
---

# Cartesian Composition

`pine-cartesian-chart` is the compound-component surface for Cartesian charts.
It follows the same pattern as Pine UI primitives: children describe chart
parts, and the root owns state, scales, SVG rendering, layer order, and
validation.

```html
<pine-cartesian-chart label="Revenue">
  <pine-chart-grid></pine-chart-grid>
  <pine-x-axis label="Week"></pine-x-axis>
  <pine-y-axis label="Revenue"></pine-y-axis>

  <pine-bar-series key="actual"
                   label="Actual"
                   color="#16a085"
                   pp-bind:data="actual"></pine-bar-series>
  <pine-cartesian-reference-line key="goal"
                                 label="Goal"
                                 y="16"
                                 stroke_dasharray="4 4"></pine-cartesian-reference-line>
  <pine-area-series key="band"
                    label="Trend band"
                    fill="#1d6fd833"
                    color="#1d6fd8"
                    pp-bind:points="band"></pine-area-series>
  <pine-line-series key="target"
                    label="Target"
                    color="#1d6fd8"
                    show_markers="true"
                    pp-bind:data="target"></pine-line-series>
  <pine-scatter-series key="samples"
                       label="Samples"
                       color="#5b6ee1"
                       pp-bind:points="samples"></pine-scatter-series>
  <pine-cartesian-reference-dot key="release"
                                label="Release"
                                x="2"
                                y="18"></pine-cartesian-reference-dot>
  <pine-cartesian-reference-label key="release-label"
                                  text="Release"
                                  x="2"
                                  y="18"
                                  dy="-10"></pine-cartesian-reference-label>
</pine-cartesian-chart>
```

Child tags are definition components. Each registers with the nearest
`pine-cartesian-chart`; the root renders one valid SVG tree. That keeps SVG
namespace handling, responsive sizing, scale computation, and paint order in
one place.

## Components

- `pine-cartesian-chart`: root SVG owner. Accepts `label`, `width`, `height`,
  `margin_top/right/bottom/left`, `x_min`, `x_max`, `y_min`, `y_max`,
  `padding_inner`, `padding_outer`, `series_padding_inner`, `animate`,
  `animation_duration`, `animation_easing`, and `empty_message`.
- `pine-chart-grid`: enables grid lines. Use the `x` and `y` boolean props to
  control which axes show grid lines (both default to `true`).
- `pine-x-axis`: enables the x axis and optional axis label.
- `pine-y-axis`: enables the y axis and optional axis label.
- `pine-line-series`: one line series from numeric `Vec<ChartPoint>` (`points`
  prop) or categorical `Vec<ChartBar>` (`data` prop). Accepts `show_markers`,
  `marker_radius`, and `stroke_width`.
- `pine-bar-series`: one categorical bar series from `Vec<ChartBar>` (`data` prop).
- `pine-area-series`: one numeric area series from `Vec<ChartPoint>` (`points`
  prop). Accepts `fill`, `color`, and `stroke_width`.
- `pine-scatter-series`: one numeric scatter series from `Vec<ChartPoint>`
  (`points` prop). Accepts `point_radius`.
- `pine-cartesian-reference-line`: one horizontal or vertical reference line in
  data space. Set exactly one of `x` or `y`. Accepts `color`, `stroke_width`,
  `stroke_dasharray`, and `layer` (defaults to `"reference-background"`).
- `pine-cartesian-reference-dot`: one reference point in data space. Accepts
  `radius`, `fill`, `stroke`, `stroke_width`, and `layer` (defaults to
  `"reference-foreground"`).
- `pine-cartesian-reference-label`: one text label anchored in data space.
  Accepts `x`, `y`, `text`, `dx`, `dy`, `angle`, `fill`, `text_anchor`, and
  `font_weight`. Labels always paint in the final label layer.

`pine-line-chart` and `pine-bar-chart` are presets for simple single-series
dashboards. Use `pine-cartesian-chart` when the chart needs explicit child
composition or when a chart should mix bars, lines, areas, scatters,
references, and annotations.

Every series child accepts a `visible` prop. Hidden children stay mounted and
keep their props, but do not contribute to domains, axes, paths, bars, or
points. Use this with interactive legends when filtering should be controlled
by the application rather than by the chart.

## Data

Numeric charts use `ChartPoint`:

```rust
use pine_charts::ChartPoint;

let actual = vec![
    ChartPoint::new(0.0, 12.0),
    ChartPoint::new(1.0, 18.0),
    ChartPoint::new(2.0, 9.0),
];
```

Combo charts use categorical `ChartBar`. When any `pine-bar-series` or
categorical `pine-line-series` is present, the root switches the x axis to a
band scale and validates that every categorical series uses the same labels in
the same order.

```rust
use pine_charts::ChartBar;

let actual = vec![
    ChartBar::new("W1", 12.0),
    ChartBar::new("W2", 18.0),
    ChartBar::new("W3", 9.0),
];

let target = vec![
    ChartBar::new("W1", 10.0),
    ChartBar::new("W2", 16.0),
    ChartBar::new("W3", 14.0),
];
```

Area and scatter children always use numeric `ChartPoint` data. In a
categorical combo chart, the `x` value is treated as a zero-based category
position: `ChartPoint::new(0.0, value)` lands on the first category and
`ChartPoint::new(1.5, value)` lands halfway between the second and third
category centers.

Reference dots and labels follow the same coordinate rule. Reference lines use
the mapped `x` position for a vertical line or the mapped `y` position for a
horizontal line.

The root recomputes whenever a child series or reference component changes. For
live charts, prefer updating the bound data vector in one state write — using
`pp-bind:points` or `pp-bind:data` — rather than driving many individual child
prop writes in the same tick.

Use `pine-layer-chart` when the chart needs absolute SVG composition with no
shared axes or scales. Use `pine-cartesian-chart` when any series needs shared
axes and a common coordinate space.
