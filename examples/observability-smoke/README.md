# OTLP observability smoke example

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
