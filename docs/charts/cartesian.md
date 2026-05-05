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
- `pine-area-series`: registers one numeric area series from `Vec<ChartPoint>`.
- `pine-scatter-series`: registers one numeric scatter series from
  `Vec<ChartPoint>`.
- `pine-cartesian-reference-line`: registers one horizontal or vertical
  reference line in data space. Set exactly one of `x` or `y`.
- `pine-cartesian-reference-dot`: registers one reference point in data space.
- `pine-cartesian-reference-label`: registers one text label anchored in data
  space.

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

Area and scatter children use numeric `ChartPoint` data. In a categorical combo
chart, their `x` values are treated as zero-based category positions, so
`ChartPoint::new(0.0, value)` lands on the first category and
`ChartPoint::new(1.5, value)` lands halfway between the second and third
category centers.

Reference dots and labels follow the same coordinate rule. Reference lines use
the mapped `x` position for vertical lines or the mapped `y` position for
horizontal lines. Lines and dots accept `layer="reference-background"` or
`layer="reference-foreground"`; labels always paint in the final label layer.

Free-form annotations still belong to `pine-layer-chart`. Use the Cartesian
root when the chart needs shared axes and scales; use the layered root when the
app needs absolute SVG composition.
