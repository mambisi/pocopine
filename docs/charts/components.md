# Chart Components

The first component layer is intentionally small: `PineLineChart` renders SVG
line charts from numeric points or named numeric series, `PineScatterChart`
renders point-only numeric series, `PineAreaChart` renders closed SVG area fills
from the same numeric shape, and `PineBarChart` renders categorical values as
SVG bars. They are useful on their own, but their main job is to prove the
component contract before richer composition is added.

## Registering

```rust
fn main() {
    pine_charts::register_all();
}
```

## Line Chart

`PineLineChart` accepts `Vec<ChartPoint>` data, chart dimensions, margins, and
optional explicit domains. If a domain is omitted, it is inferred from the
points. Flat domains are expanded so one-point charts still render. The
`points` prop stays available for simple single-line charts.

```rust
use pine_charts::ChartPoint;

let points = vec![
    ChartPoint::new(0.0, 12.0),
    ChartPoint::new(1.0, 18.0),
    ChartPoint::new(2.0, 9.0),
];
```

In a template, bind data from the parent component:

```html
<pine-line-chart
  label="Revenue"
  pp-bind:points="points"
  x_label="Week"
  y_label="Revenue"
  width="640"
  height="320"></pine-line-chart>
```

For named or multi-line charts, bind `Vec<ChartLineSeries>` to `series`. Every
series is rendered as its own SVG path with `data-series="<label>"`; markers,
hover markers, and tooltips expose the same series label.

```rust
use pine_charts::{line_legend_items, ChartLineSeries, ChartPoint};

let series = vec![
    ChartLineSeries::new(
        "Actual",
        vec![ChartPoint::new(0.0, 12.0), ChartPoint::new(1.0, 18.0)],
    ),
    ChartLineSeries::new(
        "Target",
        vec![ChartPoint::new(0.0, 10.0), ChartPoint::new(1.0, 20.0)],
    ),
];
let legend_items = line_legend_items(&series);
```

```html
<pine-line-chart
  label="Revenue"
  pp-bind:series="series"
  x_label="Week"
  y_label="Revenue"
  width="640"
  height="320"></pine-line-chart>

<pine-chart-legend
  label="Revenue legend"
  pp-bind:items="legend_items"></pine-chart-legend>
```

Line charts also expose a hover crosshair, marker, and tooltip. Pointer movement
is mapped to the nearest sampled point in SVG space. For multi-series charts,
that means nearest x/y distance, not just nearest x position. The component owns
the nearest-point state and geometry variables, while the application owns
tooltip placement and visual styling.

Set `show_markers="true"` when every sampled point should render as a visible
SVG marker. Markers are opt-in so dense line charts do not accidentally produce
hundreds of visible circles. Multi-series markers include `data-series`.
Visible markers can be selected by click or by keyboard from the chart root.

Use `x_label` and `y_label` for optional axis labels. They render as SVG
`<text>` nodes with stable hooks and remain unstyled by default.

## Scatter Chart

`PineScatterChart` accepts `Vec<ChartPoint>` for a single point cloud or
`Vec<ChartScatterSeries>` for named scatter series. It uses the same numeric
domain inference, explicit domains, dimensions, margins, grid, axes, axis
labels, hover crosshair, tooltip, and legend contract as `PineLineChart`, but
it renders only SVG circles.

```rust
use pine_charts::{scatter_legend_items, ChartPoint, ChartScatterSeries};

let series = vec![
    ChartScatterSeries::new(
        "Segment A",
        vec![ChartPoint::new(12.0, 42.0), ChartPoint::new(18.0, 49.0)],
    ),
    ChartScatterSeries::new(
        "Segment B",
        vec![ChartPoint::new(10.0, 35.0), ChartPoint::new(16.0, 39.0)],
    ),
];
let legend_items = scatter_legend_items(&series);
```

```html
<pine-scatter-chart
  label="Cohorts"
  pp-bind:series="series"
  x_label="Size"
  y_label="Retention"
  point_radius="4"
  width="640"
  height="320"></pine-scatter-chart>

<pine-chart-legend
  label="Cohort legend"
  pp-bind:items="legend_items"></pine-chart-legend>
```

Each point exposes `data-x`, `data-y`, and `data-series` attributes. The
`point_radius` prop sets the SVG `r` attribute for every point; visual color,
opacity, stroke, and hover marker styling remain application CSS. Scatter
points can be selected by click or by keyboard from the chart root.

## Area Chart

`PineAreaChart` accepts `Vec<ChartPoint>` for simple area charts or
`Vec<ChartAreaSeries>` for named and multi-area charts. It uses the same
dimensions, margins, explicit domains, grid, axis, marker, legend, and hover
contract as `PineLineChart`, but each series renders both a closed fill path and
an optional stroke path.

```rust
use pine_charts::{area_legend_items, ChartAreaSeries, ChartPoint};

let series = vec![
    ChartAreaSeries::new(
        "Organic",
        vec![ChartPoint::new(0.0, 4.0), ChartPoint::new(1.0, 7.0)],
    ),
    ChartAreaSeries::new(
        "Referral",
        vec![ChartPoint::new(0.0, 3.0), ChartPoint::new(1.0, 5.0)],
    ),
];
let legend_items = area_legend_items(&series);
```

```html
<pine-area-chart
  label="Acquisition"
  pp-bind:series="series"
  x_label="Week"
  y_label="Visits"
  width="640"
  height="320"></pine-area-chart>

<pine-chart-legend
  label="Acquisition legend"
  pp-bind:items="legend_items"></pine-chart-legend>
```

Area fills close to the bottom of the plot rectangle. If an application needs a
semantic zero baseline, set an explicit `y_min="0"` domain.

## Bar Chart

`PineBarChart` accepts `Vec<ChartBar>` for a single series or
`Vec<ChartBarSeries>` for grouped and stacked series. The x axis is categorical;
the y axis is numeric and includes a zero baseline by default. Explicit `y_min`
and `y_max` props can override the inferred domain.
Bar charts expose pointer hover on rendered rects; the hovered bar receives
`data-hovered`, and the tooltip exposes category, value, and optional series
metadata. Bars can be selected by click or by keyboard from the chart root.

```rust
use pine_charts::ChartBar;

let bars = vec![
    ChartBar::new("A", 12.0),
    ChartBar::new("B", 18.0),
    ChartBar::new("C", 9.0),
];
```

```html
<pine-bar-chart
  label="Revenue"
  pp-bind:data="bars"
  x_label="Month"
  y_label="Revenue"
  width="640"
  height="320"></pine-bar-chart>
```

Grouped and stacked bars use a stricter contract: every series must contain the
same category labels in the same order. That keeps the rendered chart
predictable and lets invalid data fail loudly instead of silently shifting bars.

```rust
use pine_charts::{ChartBar, ChartBarSeries};

let series = vec![
    ChartBarSeries::new(
        "Organic",
        vec![ChartBar::new("Jan", 12.0), ChartBar::new("Feb", 18.0)],
    ),
    ChartBarSeries::new(
        "Referral",
        vec![ChartBar::new("Jan", 7.0), ChartBar::new("Feb", 10.0)],
    ),
];
```

```html
<pine-bar-chart
  label="Acquisition"
  pp-bind:series="series"
  x_label="Month"
  y_label="Acquisition"
  mode="grouped"></pine-bar-chart>

<pine-bar-chart
  label="Acquisition"
  pp-bind:series="series"
  x_label="Month"
  y_label="Acquisition"
  mode="stacked"></pine-bar-chart>
```

Use `bar_legend_items(&series)` when a separate legend should mirror the bar
series:

```rust
use pine_charts::bar_legend_items;

let legend_items = bar_legend_items(&series);
```

## Legend

`PineChartLegend` accepts `Vec<LegendItem>` and renders an unstyled HTML list.
The legend is deliberately separate from chart components so applications can
place it above, below, beside, or outside the chart container.

```rust
use pine_charts::LegendItem;

let items = vec![LegendItem::new("Organic"), LegendItem::new("Referral")];
```

```html
<pine-chart-legend
  label="Acquisition legend"
  pp-bind:items="items"></pine-chart-legend>
```

## Styling Hooks

The component emits stable hooks:

- `.pine-chart-root`
- `.pine-line-chart`
- `.pine-scatter-chart`
- `.pine-area-chart`
- `.pine-bar-chart`
- `.pine-chart-svg`
- `.pine-chart-grid-line`
- `.pine-chart-axis`
- `.pine-chart-axis-label`
- `.pine-chart-axis-label-x`
- `.pine-chart-axis-label-y`
- `.pine-chart-tick-label`
- `.pine-chart-areas`
- `.pine-chart-area`
- `.pine-chart-lines`
- `.pine-chart-line`
- `.pine-chart-markers`
- `.pine-chart-marker`
- `.pine-chart-points`
- `.pine-chart-scatter-series`
- `.pine-chart-point`
- `.pine-chart-scatter-points`
- `.pine-chart-scatter-point`
- `.pine-chart-hover`
- `.pine-chart-crosshair`
- `.pine-chart-hover-marker`
- `.pine-chart-tooltip`
- `.pine-chart-tooltip-series`
- `.pine-chart-tooltip-x`
- `.pine-chart-tooltip-y`
- `.pine-chart-bar`
- `.pine-chart-bar-tooltip`
- `.pine-chart-tooltip-category`
- `.pine-chart-tooltip-value`
- `.pine-chart-legend`
- `.pine-chart-legend-list`
- `.pine-chart-legend-item`
- `.pine-chart-legend-marker`
- `.pine-chart-legend-label`
- `.pine-chart-status`
- `data-state="empty|ready|invalid"`
- `data-hover`
- `data-tooltip-x="left|right"`
- `data-tooltip-y="above|below"`
- `data-orientation="horizontal|vertical|..."`
- `data-key="<stable mark key>"`
- `data-x="<numeric value>"`
- `data-y="<numeric value>"`
- `data-series="<series label>"`
- `data-category="<category label>"`
- `data-value="<numeric value>"`
- `data-hovered`
- `data-focused`
- `data-selected`
- `data-empty`
- `data-invalid`

The default line paths use `stroke="currentColor"` and `fill="none"`, while bars
use `fill="currentColor"`. Application CSS should own the final visual
treatment:

```css
.pine-line-chart {
  color: var(--series-accent);
}

.pine-chart-line {
  stroke-width: 2;
}

.pine-chart-area {
  opacity: 0.2;
}

.pine-chart-line[data-series="Target"] {
  color: var(--target-series);
}

.pine-chart-area[data-series="Target"] {
  color: var(--target-series);
}

.pine-chart-marker {
  fill: var(--chart-surface);
  stroke: currentColor;
}

.pine-chart-point {
  fill: currentColor;
  stroke: var(--chart-surface);
}

.pine-chart-marker[data-focused],
.pine-chart-point[data-focused],
.pine-chart-bar[data-focused] {
  stroke-dasharray: 3 2;
}

.pine-chart-marker[data-selected],
.pine-chart-point[data-selected],
.pine-chart-bar[data-selected] {
  stroke-width: 3;
}

.pine-chart-tooltip {
  left: var(--pine-chart-tooltip-x);
  top: var(--pine-chart-tooltip-y);
}

.pine-chart-tooltip[data-tooltip-x="left"] {
  transform: translate(calc(-100% - 10px), calc(-100% - 10px));
}

.pine-chart-bar {
  opacity: 0.85;
}

.pine-chart-bar[data-hovered] {
  opacity: 1;
}

.pine-chart-bar[data-series="Organic"] {
  fill: var(--organic-series);
}

.pine-chart-legend-marker[data-series="Organic"] {
  background: var(--organic-series);
}
```

Future components must keep following this pattern: generate SVG structure and
state hooks, but leave palette, typography, spacing, and dashboard layout to the
application.

## SVG Representation

Pine Charts renders framework-owned SVG as real SVG nodes. Repeated grid and
tick marks use `pp-for` inside `<svg>` and rely on RFC 068's namespace-aware
runtime path, not `pp-html` string injection.

That matters for chart consumers because CSS selectors, DOM inspection, ARIA
tools, and future interaction hooks all see normal SVG elements such as
`<line>`, `<path>`, `<g>`, and `<text>`.
