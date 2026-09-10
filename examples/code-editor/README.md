# Native code editor example

Run from the workspace root:

```sh
pocopine dev --path examples/code-editor
# Production bundle and server:
pocopine build --path examples/code-editor --release
pocopine run --path examples/code-editor --release --port 3044
```

The example includes two independent editors, save/reset, undo/redo,
indentation, literal find/replace, read-only/disabled controls and explicit
recovery of interrupted composition. `initial-value` is a one-time seed;
saving pulls committed text from the scoped handle.

`src/client/mod.rs` contains the application integration. Its exported probe
functions are only for this example's browser tests, and are not part of the
`pine-code` API. `clear_editor_probes()` releases intentionally retained test
snapshots before memory measurements.

```sh
npx playwright install chromium firefox webkit
npm run test:code-editor
npm run test:code-editor-perf
```

The Playwright configuration starts the example through the Pocopine CLI.
If reusing an already running server, rebuild with the required debug/release
flag first; the tests cannot infer the compilation profile from a URL.
Performance tests run serially in Chromium and write JSON attachments with
hardware, browser, bundle size, latency, DOM locality and memory results.
See [VALIDATION.md](VALIDATION.md) for the recorded run and its limitations.
