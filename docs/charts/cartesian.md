# Cartesian Composition

`pine-cartesian-chart` is the compound-component surface for line-based
Cartesian charts. It follows the same pattern as Pine UI primitives: children
describe chart parts, and the root owns state, scales, SVG rendering, layer
order, and validation.

```html
<pine-cartesian-chart label="Revenue">
  <pine-chart-grid></pine-chart-grid>
  <pine-x-axis label="Week"></pine-x-axis>
  <pine-y-axis label="Revenue"></pine-y-axis>

  <pine-line-series key="actual"
                    label="Actual"
                    color="#1d6fd8"
                    show_markers="true"
                    pp-bind:points="actual"></pine-line-series>
  <pine-line-series key="target"
                    label="Target"
                    color="#d96c2c"
                    pp-bind:points="target"></pine-line-series>
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
- `pine-line-series`: registers one line series from `Vec<ChartPoint>`.

`pine-line-chart` remains the preset for simple dashboards. Use
`pine-cartesian-chart` when the chart needs explicit child composition or when a
future chart should mix lines, areas, scatters, references, and annotations.

## Data

Line data uses the same `ChartPoint` type as `pine-line-chart`:

```rust
use pine_charts::ChartPoint;

let actual = vec![
    ChartPoint::new(0.0, 12.0),
    ChartPoint::new(1.0, 18.0),
    ChartPoint::new(2.0, 9.0),
];
```

## Current Scope

This phase adds line-series composition. Area, scatter, and bar series should
join this root in follow-up phases so the preset components and the composable
root share the same geometry contracts.
