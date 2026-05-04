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
  <pine-line-series key="target"
                    label="Target"
                    color="#1d6fd8"
                    show_markers="true"
                    pp-bind:data="target"></pine-line-series>
</pine-cartesian-chart>
```

The child tags are definition components. They register with the nearest
`pine-cartesian-chart`; the root renders one valid SVG tree. That keeps SVG
namespace handling, responsive sizing, scales, and paint order in one place.

## Components

- `pine-cartesian-chart`: root SVG owner.
- `pine-chart-grid`: enables x/y grid lines.
- `pine-x-axis`: enables the x axis and optional label.
- `pine-y-axis`: enables the y axis and optional label.
- `pine-line-series`: registers one line series from numeric `Vec<ChartPoint>`
  or categorical `Vec<ChartBar>`.
- `pine-bar-series`: registers one categorical bar series from `Vec<ChartBar>`.

`pine-line-chart` and `pine-bar-chart` remain the presets for simple dashboards. Use
`pine-cartesian-chart` when the chart needs explicit child composition or when a
chart should mix bars, lines, areas, scatters, references, and annotations.

## Data

Line-only numeric charts use the same `ChartPoint` type as `pine-line-chart`:

```rust
use pine_charts::ChartPoint;

let actual = vec![
    ChartPoint::new(0.0, 12.0),
    ChartPoint::new(1.0, 18.0),
    ChartPoint::new(2.0, 9.0),
];
```

Combo charts use categorical `ChartBar` data. When any `pine-bar-series` or
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

## Current Scope

This phase supports numeric line charts and categorical bar/line combo charts.
Area, scatter, references, and annotations should join this root in follow-up
phases so the preset components and the composable root share the same geometry
contracts.
