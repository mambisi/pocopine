# Observability smoke example

This example is a small host-side server that installs pocopine JSON logging and
OTLP trace export, then exposes one `#[server(public)]` endpoint.

## Run a collector

With a local `otelcol` binary:

```sh
otelcol --config examples/observability-smoke/otel-collector.yaml
```

With Docker:

```sh
docker run --rm -p 4317:4317 \
  -v "$PWD/examples/observability-smoke/otel-collector.yaml:/etc/otelcol/config.yaml" \
  otel/opentelemetry-collector-contrib:latest \
  --config=/etc/otelcol/config.yaml
```

## Run the example

```sh
POCOPINE_SERVICE_NAME=pocopine-otlp-smoke \
POCOPINE_OTLP_ENDPOINT=http://127.0.0.1:4317 \
cargo run -p observability-smoke --bin server
```

Open <http://127.0.0.1:3010>, submit the form, and watch the collector output.
You should see spans named `pocopine.server_function` and
`observability_smoke.echo`, with fields such as `function`, `route`, and
`message_len`.

The example intentionally does not log or export the raw message body.

## Run the analytics JSON-lines exporter

This binary emits a couple of redacted analytics events through
`BoundedAnalyticsSink<JsonLinesAnalyticsSink<_>>`:

```sh
cargo run -p observability-smoke --bin analytics_exporter
```

The JSON-lines output is suitable for stdout/file log agents such as container
logs, AWS CloudWatch agents, Cloudflare log pipelines, or local smoke tests.
Exporter metrics are printed to stderr.

## CI smoke test

The repository CI runs:

```sh
bash scripts/ci/otlp_smoke.sh
```

That test starts a fake OTLP gRPC collector in-process, calls the generated
server-function route, and asserts:

- spans named `pocopine.server_function` and `observability_smoke.echo` arrive;
- `function`, `route`, `message_len`, and `service.name` metadata are present;
- the raw request message is absent from exported telemetry;
- local p95 request latency stays below a broad CI budget.

The analytics exporter test can run without Docker or a collector:

```sh
bash scripts/ci/observability_exporters.sh
```

It asserts JSON-lines shape, redaction, bounded queue drops, delivery counts,
and flush behavior through the public analytics API.
