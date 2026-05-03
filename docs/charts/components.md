# Chart Components

The first component layer is intentionally small: `PineLineChart` renders a
single SVG line chart from numeric points. It is useful on its own, but its main
job is to prove the component contract before axes, grids, legends, and tooltip
composition are added.

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

## Styling Hooks

The component emits stable hooks:

- `.pine-chart-root`
- `.pine-line-chart`
- `.pine-chart-svg`
- `.pine-chart-grid-line`
- `.pine-chart-axis`
- `.pine-chart-tick-label`
- `.pine-chart-line`
- `.pine-chart-status`
- `data-state="empty|ready|invalid"`
- `data-empty`
- `data-invalid`

The default path uses `stroke="currentColor"` and `fill="none"` so the chart is
visible without a bundled theme. Application CSS should own the final visual
treatment:

```css
.pine-line-chart {
  color: var(--series-accent);
}

.pine-chart-line {
  stroke-width: 2;
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
