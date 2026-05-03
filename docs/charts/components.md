# Chart Components

The first component layer is intentionally small: `PineLineChart` renders a
single SVG line chart from numeric points, and `PineBarChart` renders categorical
values as SVG bars. They are useful on their own, but their main job is to prove
the component contract before richer composition is added.

## Registering

```rust
fn main() {
    pine_charts::register_all();
}
```

## Line Chart

`PineLineChart` accepts `Vec<ChartPoint>` data, chart dimensions, margins, and
optional explicit domains. If a domain is omitted, it is inferred from the
points. Flat domains are expanded so one-point charts still render.

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
  width="640"
  height="320"></pine-line-chart>
```

Line charts also expose a hover crosshair, marker, and tooltip. Pointer movement
is mapped to the nearest sampled point in SVG space. The component owns the
nearest-point state and geometry variables, while the application owns tooltip
placement and visual styling.

Set `show_markers="true"` when every sampled point should render as a visible
SVG marker. Markers are opt-in so dense line charts do not accidentally produce
hundreds of visible circles.

## Bar Chart

`PineBarChart` accepts `Vec<ChartBar>` for a single series or
`Vec<ChartBarSeries>` for grouped and stacked series. The x axis is categorical;
the y axis is numeric and includes a zero baseline by default. Explicit `y_min`
and `y_max` props can override the inferred domain.

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
  mode="grouped"></pine-bar-chart>

<pine-bar-chart
  label="Acquisition"
  pp-bind:series="series"
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
- `.pine-bar-chart`
- `.pine-chart-svg`
- `.pine-chart-grid-line`
- `.pine-chart-axis`
- `.pine-chart-tick-label`
- `.pine-chart-line`
- `.pine-chart-markers`
- `.pine-chart-marker`
- `.pine-chart-hover`
- `.pine-chart-crosshair`
- `.pine-chart-hover-marker`
- `.pine-chart-tooltip`
- `.pine-chart-tooltip-x`
- `.pine-chart-tooltip-y`
- `.pine-chart-bar`
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
- `data-x="<numeric value>"`
- `data-y="<numeric value>"`
- `data-series="<series label>"`
- `data-empty`
- `data-invalid`

The default line path uses `stroke="currentColor"` and `fill="none"`, while bars
use `fill="currentColor"`. Application CSS should own the final visual
treatment:

```css
.pine-line-chart {
  color: var(--series-accent);
}

.pine-chart-line {
  stroke-width: 2;
}

.pine-chart-marker {
  fill: var(--chart-surface);
  stroke: currentColor;
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
