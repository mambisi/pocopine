---
title: "Examples"
description: "Run the built-in charts example app to see every Pine Charts primitive in action."
---

# Examples

The `examples/charts` app exercises every public Pine Charts primitive in a
single runnable application. Build and start it from the repo root:

```bash
cargo run -p pocopine-cli -- build --path examples/charts
cargo run -p pocopine-cli -- run --path examples/charts --port 8025
```

Open `http://localhost:8025` and switch between the two datasets. All visual
styling lives in `index.html` CSS so the chart crate itself stays unstyled.

## What the example covers

The example is a composition test rather than a theme showcase. It demonstrates
that applications can assemble chart primitives into a full product UI:

- combo Cartesian charts with bars, areas, lines, scatter points, and reference
  marks sharing one scale system,
- custom layered SVG scenes with explicit paint order,
- app-owned custom tooltips driven by `pp:chart:hover`,
- persistent selection details driven by `pp:chart:select`,
- interactive legends that mutate application-owned visible data,
- responsive wrappers that keep SVG text and circular marks unstretched,
- empty states when filtering hides all visible data,
- independent radial progress rings for non-part-of-total metrics.

It uses the same public API your application uses:

- `pine_charts::register_all()` at startup,
- `<pine-cartesian-chart>` with `pine-chart-grid`, `pine-x-axis`, and
  `pine-y-axis` guide children,
- `<pine-cartesian-chart>` mixing bar, line, and scatter series with Cartesian
  reference lines, dots, and labels,
- `<pine-layer-chart>` with child layers, reference dots, labels, and icons for
  custom SVG compositions,
- `Vec<ChartLineSeries>`, `Vec<ChartScatterSeries>`,
  `Vec<ChartAreaSeries>`, and `Vec<ChartBarSeries>` data in the parent
  component,
- `Vec<ChartPieSlice>` plus bound `inner_radius`, `start_angle`, and
  `end_angle` props for pie, donut, and half-donut variants,
- `Vec<ChartRadialBar>` plus bound `inner_radius` and `ring_gap` props for
  progress-ring variants,
- `line_legend_items(&line_series)` to drive a separate line legend,
- `scatter_legend_items(&scatter_series)` to drive a separate scatter legend,
- `area_legend_items(&area_series)` to drive a separate area legend,
- `bar_legend_items(&bar_series)` to drive a separate bar legend,
- `pie_legend_items(&pie_data)` to drive pie/donut legends,
- `radial_bar_legend_items(&radial_data)` to drive radial bar legends,
- `<pine-line-chart pp-bind:series="line_series">` in the template,
- `<pine-scatter-chart pp-bind:series="scatter_series">` in the template,
- `<pine-area-chart pp-bind:series="area_series">` in the template,
- `<pine-bar-chart pp-bind:series="bar_series" pp-bind:mode="bar_mode">` in
  the template,
- `<pine-pie-chart pp-bind:data="pie_data">` in the template,
- `<pine-radial-bar-chart pp-bind:data="radial_data">` in the template,
- `x_label` and `y_label` attributes for chart axis labels,
- `tooltip="none"` plus `pp:chart:hover` / `pp:chart:hover-end` handlers for an
  app-owned tooltip,
- `pp:chart:select` / `pp:chart:select-end` handlers for an app-owned detail
  panel,
- `<pine-chart-legend pp-bind:items="line_legend">`,
  `<pine-chart-legend pp-bind:items="scatter_legend">`,
  `<pine-chart-legend pp-bind:items="area_legend">`,
  `<pine-chart-legend pp-bind:items="bar_legend">`,
  `<pine-chart-legend pp-bind:items="pie_legend">`, and
  `<pine-chart-legend pp-bind:items="radial_legend">` in the template,
- CSS selectors from the chart styling contract.

## Browser tests

Pine Charts has browser integration coverage using `wasm-bindgen-test`:

```bash
wasm-pack test --firefox --headless crates/pine-charts
```

The browser suite mounts compiled pocopine fixtures and verifies SVG output,
reactive bound data updates, hover behavior, click/keyboard selection,
pie/donut slices, responsive panel sizing, radial centering, half-donut center
text, narrow-width resizing, and empty/invalid states.
