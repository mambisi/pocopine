# Examples

The `examples/charts` app is the first integration target for Pine Charts.

```bash
cd examples/charts
wasm-pack build --target web --out-dir pkg
python3 -m http.server 8000
```

Open `http://localhost:8000` and switch between the two datasets. The demo keeps
all visual styling in `index.html` CSS so the chart crate can stay unstyled.

The example intentionally uses the same public API an application would use:

- `pine_charts::register_all()` at startup,
- `Vec<ChartPoint>` data in the parent component,
- `<pine-line-chart pp-bind:points="points">` in the template,
- CSS selectors from the chart styling contract.

This example should be extended whenever a new chart primitive becomes public.
