# Logging, tracing, and observability

This doc explains the first observability slice introduced by
[`RFC 069`](../rfcs/rfc-069-observability.md): browser console logging,
backend logging, structured observed events, and analytics fan-out.

## Mental model

Pocopine uses `tracing` as the instrumentation API. Code emits spans and
events. Application entrypoints decide where those events go.

```text
app / pocopine runtime
  -> tracing events and spans
  -> pocopine-observe event contract
  -> pocopine-logging subscribers
  -> pocopine-analytics sinks
```

The crates have different jobs:

| Crate | Purpose |
|---|---|
| `pocopine-observe` | Shared event schema, context, priority, privacy labels, redaction, and fixed tracing targets. |
| `pocopine-logging` | Browser console logging and server log formatting. |
| `pocopine-analytics` | Redacted analytics/telemetry fan-out to custom or vendor sinks. |

Framework/library code should emit `tracing` events or typed
`ObservedEvent`s. It should not install global subscribers or vendor
exporters. The final application entrypoint owns that.

## Enable the feature

For the umbrella crate:

```toml
[dependencies]
pocopine = { path = "../../crates/pocopine", features = ["logging", "analytics"] }
tracing = "0.1"
```

Use only what you need:

```toml
pocopine = { path = "../../crates/pocopine", features = ["logging"] }
```

The `logging` and `analytics` features also enable `observe`, because both
use the shared event contract.

## Browser console logging

On `wasm32`, install console logging once during app startup:

```rust
use pocopine::logging::{init_console_logging, ConsoleLoggingConfig};

#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    let _ = init_console_logging(ConsoleLoggingConfig::debug());

    tracing::info!(target: "pocopine.log", "browser app started");
    tracing::debug!(
        target: "pocopine.log",
        component = "Calendar",
        "component mounted"
    );

    pocopine::App::new().run();
}
```

The browser layer maps tracing levels to the matching browser console call:

| Tracing level | Browser API |
|---|---|
| `ERROR` | `console.error` |
| `WARN` | `console.warn` |
| `INFO` | `console.info` |
| `DEBUG` / `TRACE` | `console.debug` |

### Why `target` matters

`ConsoleLoggingConfig::debug()` filters to targets that start with
`pocopine` so the browser console is not flooded by unrelated dependency
logs.

This shows:

```rust
tracing::info!(target: "pocopine.log", "app started");
```

This may be filtered out:

```rust
tracing::info!("app started");
```

Without an explicit target, `tracing` uses the Rust module path as the target,
such as `my_app::pages::home`.

If you want app module targets instead, configure the prefix:

```rust
init_console_logging(
    ConsoleLoggingConfig::debug().with_target_prefix("my_app")
)?;
```

If you want everything:

```rust
init_console_logging(
    ConsoleLoggingConfig::debug().without_target_prefix()
)?;
```

## Backend logging

On the host/server side, install server logging once near process startup:

```rust
use pocopine::logging::{init_server_logging, ServerLoggingConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_server_logging(
        ServerLoggingConfig::json()
            .with_env_filter("info,pocopine=debug")
    )?;

    tracing::info!(target: "pocopine.log", "server started");
    tracing::warn!(
        target: "pocopine.log",
        route = "/api/posts",
        "slow request"
    );

    Ok(())
}
```

For human-readable development logs:

```rust
pocopine::logging::init_default()?;
```

For structured production logs:

```rust
init_server_logging(
    ServerLoggingConfig::json().with_env_filter("info,pocopine=debug")
)?;
```

If `with_env_filter` is not provided, the server logger reads `RUST_LOG`.
If `RUST_LOG` is unset, the default filter is:

```text
info,pocopine=debug
```

## OTLP trace export

`pocopine-logging` can also install an OpenTelemetry layer for OTLP trace
export. This is host-only and feature-gated so normal console/JSON logging
stays lightweight.

With the umbrella crate:

```toml
[dependencies]
pocopine = { path = "../../crates/pocopine", features = ["logging-otlp"] }
```

Or directly:

```toml
[dependencies]
pocopine-logging = { path = "../../crates/pocopine-logging", features = ["otlp"] }
```

Install local logs and OTLP traces with one subscriber:

```rust
use pocopine::logging::{init_server_logging, OtlpConfig, ServerLoggingConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_server_logging(
        ServerLoggingConfig::json()
            .with_env_filter("info,pocopine=debug")
            .with_otlp(
                OtlpConfig::grpc("http://localhost:4317")
                    .with_service_name("blog-api")
            )
    )?;

    Ok(())
}
```

For environment-driven setup:

```rust
init_server_logging(
    ServerLoggingConfig::compact()
        .with_otlp_from_env()
)?;
```

The OTLP config reads these variables, in order:

| Field | Variables | Default |
|---|---|---|
| endpoint | `POCOPINE_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4317` |
| service name | `POCOPINE_SERVICE_NAME`, `OTEL_SERVICE_NAME` | `pocopine-app` |

This first slice exports traces over OTLP/gRPC with the OpenTelemetry SDK batch
processor. Logs still go to the local compact/pretty/JSON formatter. Production
deployments can route JSON logs with their platform log agent and route traces
through an OpenTelemetry Collector or OTLP-compatible backend. Direct vendor SDKs
are intentionally out of scope for this layer.

For a runnable smoke path, see
[`examples/observability-smoke`](../examples/observability-smoke/). It starts a
small host server, installs `logging-otlp`, exposes one `#[server(public)]`
endpoint, and includes an OpenTelemetry Collector config that prints received
spans with the debug exporter.

## Structured observed events

Use `ObservedEvent` when you want a stable framework-facing event schema
instead of a one-off debug log.

```rust
use pocopine::observe::{FieldPrivacy, ObservedEvent};

let event = ObservedEvent::log("server_started")
    .field("port", 3000_u32, FieldPrivacy::Public)
    .field("environment", "dev", FieldPrivacy::Public);

pocopine::logging::log_event(&event);
```

Observed events include:

- `name`
- `version`
- `class`
- `priority`
- `privacy`
- shared context
- field privacy labels

Fixed tracing targets:

| Event class | Target |
|---|---|
| log | `pocopine.log` |
| trace | `pocopine.trace` |
| metric | `pocopine.metric` |
| analytics | `pocopine.analytics` |

## Analytics and telemetry

Analytics is separate from logging. Logs are operational records. Analytics
events are intentional product or telemetry events with a stable schema.

```rust
use pocopine::analytics::{route_view, AnalyticsClient, AnalyticsError};

let analytics = AnalyticsClient::new()
    .with_sink(|event: &pocopine::observe::ObservedEvent| {
        // Send to your vendor SDK, HTTP endpoint, or local test collector.
        tracing::debug!(target: "pocopine.analytics", event = ?event);
        Ok::<(), AnalyticsError>(())
    });

let report = analytics.emit(route_view("/settings"));

if !report.all_succeeded() {
    tracing::warn!(
        target: "pocopine.log",
        failed = report.failed,
        "analytics delivery failed"
    );
}
```

`AnalyticsClient` redacts before dispatch and keeps going when one sink fails.
Sink panics are caught and reported in the delivery report.

On host targets, analytics sinks must be `Send + Sync` so the client can be
stored in shared server state such as axum state. On wasm targets this bound is
relaxed because browser SDK handles are usually JavaScript objects. The closure
example above spells out `&pocopine::observe::ObservedEvent` so Rust can infer
the host closure sink against that `Send + Sync` blanket implementation.

## Browser vendor bridges

The wasm analytics module provides adapters around caller-supplied JavaScript
functions or SDK handles. Pocopine does not directly initialize Firebase,
Google Analytics, Cloudflare, AWS, or OpenTelemetry for you in the core
runtime.

That boundary is intentional:

- the app owns vendor SDK initialization;
- pocopine owns redaction and event shape;
- vendor adapters translate redacted observed events into vendor calls.

For Firebase/Google Analytics-style usage, initialize the SDK in the app, then
attach the adapter as an analytics sink.

## Privacy rules

The default posture is allowlist-based.

| Privacy label | Default behavior |
|---|---|
| `Public` | May be exported. |
| `Pseudonymous` | Exported only when the redaction policy allows it. |
| `Sensitive` | Dropped unless an explicitly trusted sink allows it. |

Do not mark these as public:

- raw auth headers;
- cookies;
- tokens;
- emails;
- raw model payloads;
- DOM text;
- route params that may contain user data.

Prefer stable names and coarse fields:

```rust
ObservedEvent::analytics("checkout_started")
    .field("plan", "pro", FieldPrivacy::Public)
    .field("session_id", session_id_hash, FieldPrivacy::Pseudonymous);
```

## Reliability rules

Observability must not change application behavior.

- Exporter failure must not panic the app.
- Analytics sink failure must not stop other sinks.
- Analytics sinks receive redacted events, not raw debug fields.
- Runtime/library crates do not install global subscribers.
- Async exporters added later must use bounded queues and count drops.

## Common recipes

### Log from browser code

```rust
tracing::info!(target: "pocopine.log", "button clicked");
```

### Log from backend code

```rust
tracing::error!(
    target: "pocopine.log",
    error = %err,
    "server function failed"
);
```

### Create a trace span

```rust
let span = tracing::info_span!(
    target: "pocopine.trace",
    "server_function",
    name = "save_profile"
);

let _entered = span.enter();
```

### Emit a stable analytics event

```rust
analytics.emit(
    pocopine::analytics::event("feature_used")
        .field("feature", "calendar", pocopine::observe::FieldPrivacy::Public)
);
```

### Disable the browser target filter

```rust
init_console_logging(
    ConsoleLoggingConfig::debug().without_target_prefix()
)?;
```
