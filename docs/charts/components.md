# Chart Components

The first component layer is intentionally small: `PineLineChart` renders a
single SVG line chart from numeric points, and `PineBarChart` renders categorical
values as SVG bars. They are useful on their own, but their main job is to prove
the component contract before legends, tooltips, and richer composition are
added.

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

## Bar Chart

`PineBarChart` accepts `Vec<ChartBar>` data. The x axis is categorical; the y
axis is numeric and includes a zero baseline by default. Explicit `y_min` and
`y_max` props can override the inferred domain.

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
- `.pine-chart-bar`
- `.pine-chart-status`
- `data-state="empty|ready|invalid"`
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

.pine-chart-bar {
  opacity: 0.85;
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
