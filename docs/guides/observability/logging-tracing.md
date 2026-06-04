---
title: "Logging, tracing, and observability"
description: "Browser and backend logging, structured observed events, analytics sinks, and privacy labels."
---

# Logging, tracing, and observability

pocopine uses `tracing` as the instrumentation API. Code emits spans and
events. Application entrypoints decide where those events go. This page
covers browser console logging, backend log formatting, structured observed
events, OTLP trace export, and analytics fan-out — the full observability
stack introduced in RFC 069.

## Mental model

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

Framework and library code emits `tracing` events or typed
`ObservedEvent`s. It does not install global subscribers or vendor
exporters. The final application entrypoint owns that.

## Enable the feature

Add the features you need to the umbrella crate:

```toml
[dependencies]
pocopine = { version = "...", features = ["logging", "analytics"] }
tracing = "0.1"
```

Use only what you need:

```toml
pocopine = { version = "...", features = ["logging"] }
```

The `logging` and `analytics` features each enable `observe` automatically,
because both depend on the shared event contract.

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

`ConsoleLoggingConfig::debug()` writes a compact text line.
`ConsoleLoggingConfig::json()` writes a structured console object with
`level`, `target`, `message`, and `fields`, which is usually easier to
inspect in browser DevTools.

## Frontend observability plugin

Install framework observability through the app plugin system:

```rust
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    pocopine::app! {
        components: [AppShell, HomePage, ReportPage],
        plugins: [
            pocopine::logging::frontend_observability(),
        ],
        routes: [
            ("/", HomePage),
            ("/report/:id", ReportPage),
        ],
    };
}
```

The default plugin installs JSON browser console logging and translates
typed framework lifecycle hooks into `ObservedEvent`s:

| Framework hook | Observed event |
|---|---|
| `AppBootStarted` | `frontend_app_started` trace |
| `AppBootCompleted` | `frontend_app_boot_completed` trace |
| `AppBootFailed` | `frontend_app_boot_failed` log |
| `RouteNavigationStarted` | `route_navigation_started` trace |
| `RouteNavigationCompleted` | `route_view` analytics (matched route) or `route_navigation_completed` trace (unmatched) |
| `RouteNavigationFailed` | `route_navigation_failed` log |
| `ServerFunctionClientStarted` | `server_function_client_started` trace |
| `ServerFunctionClientCompleted` | `server_function_client_completed` trace |
| `ServerFunctionClientFailed` | `server_function_client_failed` log |
| `ComponentMounted` | `component_view` analytics |
| `ComponentUnmounted` | `component_unmounted` trace |

`RouteNavigationCompleted` emits an analytics `route_view` event only when
the navigation resolved to a matched route and component. Unmatched
navigations produce a trace event instead. Route events report the route
pattern, such as `/report/:id`, not the concrete path.

Raw route params, DOM text, request arguments, and response bodies are
never exported.

For tests or apps that initialize their own subscriber, disable the console
subscriber and keep only the lifecycle hooks:

```rust
let plugin = pocopine::logging::frontend_observability_with_config(
    pocopine::logging::FrontendObservabilityConfig::default()
        .without_console_logging()
        .with_service("admin-ui")
        .with_environment("staging"),
);
```

The plugin also provides `Plugin<FrontendObservability>` to components:

```rust
use pocopine::logging::FrontendObservability;
use pocopine::observe::{FieldPrivacy, ObservedEvent};

#[handlers]
impl ReportPage {
    pub fn opened(&self) {
        self.plugin::<FrontendObservability>().emit(
            ObservedEvent::analytics("report_opened")
                .field("source", "cta", FieldPrivacy::Public),
        );
    }
}
```

Use `Option<Plugin<FrontendObservability>>` in reusable components when the
component should work whether or not the app installed observability.

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

Without an explicit target, `tracing` uses the Rust module path as the
target, such as `my_app::pages::home`.

To pass app module targets through:

```rust
init_console_logging(
    ConsoleLoggingConfig::debug().with_target_prefix("my_app")
)?;
```

To pass all targets through:

```rust
init_console_logging(
    ConsoleLoggingConfig::debug().without_target_prefix()
)?;
```

## Backend logging

On the server side, install server logging once near process startup:

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

## Backend observability plugin

Server apps can install backend lifecycle observability through the server
plugin system. Install routes first, then install the observability plugin
so the HTTP request layer wraps the completed router:

```rust
use pocopine::logging::{
    init_server_logging, server_observability_with_config,
    ServerLoggingConfig, ServerObservabilityConfig,
};
use pocopine_server::{axum::Router, static_files, Server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    init_server_logging(ServerLoggingConfig::json())
        .expect("logging should initialize");

    let router = Router::new().nest_service("/", static_files("pkg"));

    Server::new(router)
        .plugin(server_observability_with_config(
            ServerObservabilityConfig::new()
                .with_service("blog-api")
                .with_environment("production"),
        ))
        .serve("0.0.0.0:3000")
        .await
}
```

`server_observability()` uses the default config. It installs
`pocopine_server::request_event_layer()` and translates typed server
hooks into `ObservedEvent`s:

| Server hook | Observed event |
|---|---|
| `ServerBootStarted` | `server_boot_started` trace |
| `ServerListening` | `server_listening` trace |
| `ServerBootFailed` | `server_boot_failed` log |
| `HttpRequestStarted` | `http_request_started` trace |
| `HttpRequestCompleted` | `http_request_completed` trace |
| `HttpRequestFailed` | `http_request_failed` log |
| `ServerFunctionStarted` | `server_function_started` trace |
| `ServerFunctionCompleted` | `server_function_completed` trace |
| `ServerFunctionRejected` | `server_function_rejected` log |
| `ServerFunctionFailed` | `server_function_failed` log |

Server-function events include both `function` (the short Rust function
identifier) and `function_path` (the fully-qualified `module::function`
path, useful for debugging name collisions).

HTTP events report axum's matched route pattern, such as `/posts/:id`,
rather than the concrete request path. Headers, cookies, query strings,
request bodies, response bodies, and raw server-function payloads are
never exported by the plugin. For unmatched routes, the concrete path is
omitted by default; opt in only when that value is acceptable for your
deployment:

```rust
ServerObservabilityConfig::new()
    .with_unmatched_paths(true);
```

Disable event groups when another plugin owns that surface:

```rust
ServerObservabilityConfig::new()
    .with_http_requests(false)
    .with_server_functions(true)
    .with_boot(true);
```

When server-function hooks are enabled, the plugin still installs the
request event layer so generated `#[server]` handlers can share the HTTP
`request_id` with request events. Jobs are intentionally not mapped into
typed server hooks: `pocopine-jobs` already emits structured
`pocopine.trace` / `pocopine.log` events.

## OTLP trace export

`pocopine-logging` can install an OpenTelemetry layer for OTLP trace
export. This is server-only and feature-gated so normal console/JSON
logging stays lightweight.

With the umbrella crate:

```toml
[dependencies]
pocopine = { version = "...", features = ["logging-otlp"] }
```

Or directly:

```toml
[dependencies]
pocopine-logging = { version = "...", features = ["otlp"] }
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

Traces export over OTLP/gRPC with the OpenTelemetry SDK batch processor.
Logs still go to the local compact/pretty/JSON formatter. Production
deployments can route JSON logs with their platform log agent and route
traces through an OpenTelemetry Collector or OTLP-compatible backend.
Direct vendor SDKs are intentionally out of scope for this layer.

For a runnable smoke path, see
[`examples/observability-smoke`](../../../examples/observability-smoke/).
It starts a small host server, installs `logging-otlp`, exposes one
`#[server(public)]` endpoint, and includes an OpenTelemetry Collector
config that prints received spans with the debug exporter.

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

Observed events carry:

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

When an `ObservedEvent` is emitted into `tracing`, the shared context and
event fields are recorded as stable tracing fields instead of a single
Debug blob. This is important for JSON logs, `tracing-opentelemetry`, and
log agents that consume typed tracing values.

Top-level event fields use fixed names:

```text
event_name
event_version
event_class
event_priority
event_privacy
```

Context fields use fixed names:

```text
observed_context_has_service      observed_context_service
observed_context_has_environment  observed_context_environment
observed_context_has_route        observed_context_route
observed_context_has_component    observed_context_component
observed_context_has_trace_id     observed_context_trace_id
observed_context_has_session_id   observed_context_session_id
observed_context_has_user_id_hash observed_context_user_id_hash
```

Each optional context field has a paired `observed_context_has_*` boolean
so exporters can distinguish a missing value from an intentionally empty
string.

User event fields are exported through fixed slots because `tracing`
callsites require static field names. Slots preserve the original field
name, privacy label, kind, and typed value:

```text
observed_field_slots = 8
observed_field_count = 2
observed_field_overflowed = false
observed_field_0_present = true
observed_field_0_name = "duration_ms"
observed_field_0_privacy = "public"
observed_field_0_kind = "f64"
observed_field_0_value_string = ""
observed_field_0_value_bool = false
observed_field_0_value_i64 = 0
observed_field_0_value_u64 = 0
observed_field_0_value_f64 = 12.7
observed_field_1_present = true
observed_field_1_name = "route"
observed_field_1_privacy = "public"
observed_field_1_kind = "string"
observed_field_1_value_string = "/settings"
observed_field_1_value_bool = false
observed_field_1_value_i64 = 0
observed_field_1_value_u64 = 0
observed_field_1_value_f64 = 0.0
```

All five typed sub-fields (`value_string`, `value_bool`, `value_i64`, `value_u64`, `value_f64`) are always emitted for every slot; inactive ones carry their zero values. Use `observed_field_N_kind` to determine which value field carries the active data.

Eight slots are reserved. If an event carries more fields,
`observed_field_count` still records the full count and
`observed_field_overflowed` is set to `true`; keep framework events
coarse and stable rather than shipping wide payloads. pocopine also emits
one warning per process the first time an event overflows the reserved
slots.

`pocopine::logging::log_event()` and `pocopine::observe::emit_tracing()`
do not apply redaction. They emit the event as supplied. For analytics or
telemetry sinks that must strip private fields, use `AnalyticsClient` or
call `ObservedEvent::redacted(...)` before emitting.

## Analytics and telemetry

Analytics is separate from logging. Logs are operational records.
Analytics events are intentional product or telemetry events with a
stable schema.

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

`AnalyticsClient` redacts before dispatch and keeps going when one sink
fails. Sink panics are caught and reported in the delivery report. The
default redaction policy allows pseudonymous fields; call
`.with_redaction(RedactionPolicy::public_only())` to restrict further.

On server targets, analytics sinks must be `Send + Sync` so the client
can be stored in shared server state such as axum state. On wasm targets
this bound is relaxed because browser SDK handles are usually JavaScript
objects. The closure example above spells out
`&pocopine::observe::ObservedEvent` so Rust can infer the host closure
sink against that `Send + Sync` blanket implementation.

Server exporters that buffer work should use a bounded queue.
`BoundedAnalyticsSink` wraps another sink, accepts up to `capacity`
redacted events, rejects new events when full, and exposes counters for
pending, enqueued, dropped, delivered, and failed operations:

```rust
use pocopine::analytics::{
    AnalyticsClient, BoundedAnalyticsSink, JsonLinesAnalyticsSink,
};

let exporter = BoundedAnalyticsSink::new(
    JsonLinesAnalyticsSink::stdout(),
    1024,
);
let metrics = exporter.clone();

let analytics = AnalyticsClient::new()
    .with_sink(exporter);

let report = analytics.emit(pocopine::analytics::route_view("/settings"));
if !report.all_succeeded() {
    let snapshot = metrics.metrics();
    tracing::warn!(
        target: "pocopine.log",
        dropped = snapshot.dropped,
        failed = snapshot.failed,
        "analytics exporter backpressure"
    );
}
```

Call `analytics.flush()` during graceful shutdown to drain queued events
into the wrapped sink. Flush keeps going after per-event exporter errors
and counts those failures in the wrapper metrics.

`JsonLinesAnalyticsSink` writes one `ObservedEvent` JSON object per line.
When attached to `AnalyticsClient`, it receives events after the client's
redaction policy has run. This is the simplest server exporter for
container logs, AWS CloudWatch log agents, Cloudflare-style log pipelines,
and local smoke tests. Use `JsonLinesAnalyticsSink::stdout()`,
`::stderr()`, `::file(path)`, or `::new(writer)` for a custom
`std::io::Write`.

Run the smoke binary:

```sh
cargo run -p observability-smoke --bin analytics_exporter
```

For OTLP, use the structured `tracing` fields described above and
`pocopine-logging`'s `logging-otlp` feature. For JSON-log agents, consume
the same `observed_context_*` and `observed_field_*` keys from the JSON
formatter.

## Browser vendor bridges

The wasm analytics module provides adapters around caller-supplied
JavaScript functions or SDK handles. pocopine does not directly initialize
Firebase, Google Analytics, Cloudflare, AWS, or OpenTelemetry in the
core runtime.

That boundary is intentional:

- the app owns vendor SDK initialization;
- pocopine owns redaction and event shape;
- vendor adapters translate redacted observed events into vendor calls.

For Firebase/Google Analytics-style usage, initialize the SDK in the app,
then attach the adapter as an analytics sink.

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
- Runtime and library crates do not install global subscribers.
- Server exporters that buffer work should use `BoundedAnalyticsSink` or
  an equivalent bounded queue and count drops.
- Async exporters added later must preserve the same bounded-queue and
  drop/error-counter contract.

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
