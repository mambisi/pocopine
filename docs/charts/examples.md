# Examples

The `examples/charts` app is the first integration target for Pine Charts.

```bash
cargo run -p pocopine-cli -- build --path examples/charts
cargo run -p pocopine-cli -- run --path examples/charts --port 8025
```

Open `http://localhost:8025` and switch between the two datasets. The demo keeps
all visual styling in `index.html` CSS so the chart crate can stay unstyled.

The example intentionally uses the same public API an application would use:

- `pine_charts::register_all()` at startup,
- `Vec<ChartLineSeries>`, `Vec<ChartScatterSeries>`,
  `Vec<ChartAreaSeries>`, and `Vec<ChartBarSeries>` data in the parent
  component,
- `line_legend_items(&line_series)` to drive a separate line legend,
- `scatter_legend_items(&scatter_series)` to drive a separate scatter legend,
- `area_legend_items(&area_series)` to drive a separate area legend,
- `bar_legend_items(&bar_series)` to drive a separate legend,
- `<pine-line-chart pp-bind:series="line_series">` in the template,
- `<pine-scatter-chart pp-bind:series="scatter_series">` in the template,
- `<pine-area-chart pp-bind:series="area_series">` in the template,
- `<pine-bar-chart pp-bind:series="bar_series" pp-bind:mode="bar_mode">` in
  the template,
- `x_label` and `y_label` attributes for chart axis labels,
- `<pine-chart-legend pp-bind:items="line_legend">`,
  `<pine-chart-legend pp-bind:items="scatter_legend">`,
  `<pine-chart-legend pp-bind:items="area_legend">`, and
  `<pine-chart-legend pp-bind:items="bar_legend">` in the template,
- CSS selectors from the chart styling contract.

This example should be extended whenever a new chart primitive becomes public.

## Browser Tests

Pine Charts also has browser integration coverage, matching the Pine crate's
`wasm-bindgen-test` style:

```bash
wasm-pack test --firefox --headless crates/pine-charts
```

The browser suite mounts compiled Pocopine fixtures and verifies SVG output,
reactive bound data updates, hover behavior, click/keyboard selection, and
empty/invalid states.
