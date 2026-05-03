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
- `Vec<ChartPoint>` and `Vec<ChartBar>` data in the parent component,
- `<pine-line-chart pp-bind:points="points">` in the template,
- `<pine-bar-chart pp-bind:data="bars">` in the template,
- CSS selectors from the chart styling contract.

This example should be extended whenever a new chart primitive becomes public.

## Browser Tests

Pine Charts also has browser integration coverage, matching the Pine crate's
`wasm-bindgen-test` style:

```bash
wasm-pack test --firefox --headless crates/pine-charts
```

The browser suite mounts compiled Pocopine fixtures and verifies SVG output,
reactive bound data updates, and empty/invalid states.
