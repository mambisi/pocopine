# RFC 069 - Unified observability, logging, and analytics

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-03 |
| **Related** | [`rfc-037-js-bridge.md`](./rfc-037-js-bridge.md), [`rfc-059-server-side-rendering-and-hydration.md`](./rfc-059-server-side-rendering-and-hydration.md), [`rfc-066-server-function-auth.md`](./rfc-066-server-function-auth.md), [`rfc-067-redis-background-jobs.md`](./rfc-067-redis-background-jobs.md) |
| **Supersedes** | - |

## 1. Summary

Add a unified observability spine on top of `tracing` while keeping logs,
traces, telemetry, and analytics separate at the schema and exporter layer.

The implementation is split across three crates:

- `pocopine-observe`: stable event contract, context, priorities, privacy
  labels, redaction policy, and `tracing` emission helpers.
- `pocopine-logging`: server and browser logging subscribers/adapters.
- `pocopine-analytics`: product analytics and telemetry fan-out with
  redaction, sink isolation, and vendor bridge adapters.

Framework crates emit `tracing` events or typed `pocopine-observe` events.
Application entrypoints decide which subscribers and exporters are installed.

## 2. Motivation

Real applications need several observability paths at once:

- Developer logs in the browser console.
- Structured server logs for stdout, AWS, Cloudflare, or another host.
- Trace/telemetry data for request, server function, job, and route timing.
- Product analytics for route views, feature usage, funnels, and performance
  events.

Those paths should not share one loose stringly API. Debug logs eventually
carry sensitive context. Analytics events need stable schemas. Traces need
causality and timing. Logs need operational detail. The framework should give
all of them a shared context spine without treating them as interchangeable.

## 3. Design

### 3.1 Crate boundaries

`pocopine-observe` is the only stable shared contract. It defines:

- event class: `log`, `trace`, `metric`, `analytics`;
- priority: `critical`, `high`, `normal`, `low`;
- field privacy: `public`, `pseudonymous`, `sensitive`;
- shared context: service, environment, route, component, trace ID, session
  ID, and hashed user ID;
- redaction policy;
- fixed `tracing` targets:
  - `pocopine.log`
  - `pocopine.trace`
  - `pocopine.metric`
  - `pocopine.analytics`

`pocopine-logging` owns log presentation and log export. The first slice
ships:

- host/server initialization through `tracing-subscriber`;
- compact, pretty, and JSON server formats;
- `RUST_LOG` / explicit filter support;
- wasm browser console subscriber;
- wasm frontend observability app plugin that subscribes to core lifecycle
  hooks and emits stable `ObservedEvent`s without adding exporter dependencies
  to `pocopine-core`.

`pocopine-analytics` owns analytics and telemetry dispatch. The first slice
ships:

- an `AnalyticsClient`;
- an `AnalyticsSink` trait;
- redaction before dispatch;
- continued delivery after individual sink errors;
- panic isolation around sink calls;
- wasm JS function sinks for user-supplied vendor bridges;
- wasm Firebase and Google Analytics function adapters.

### 3.2 Framework integration rule

Runtime crates must not install global subscribers or exporters. They may emit
`tracing` spans/events and may construct typed `ObservedEvent`s for stable
framework events.

Frontend framework lifecycle instrumentation is installed through the app plugin
surface. Core emits typed hooks such as `AppBoot*`, `RouteNavigation*`,
`ComponentMounted`, `ComponentUnmounted`, and `ServerFunctionClient*`; the
logging plugin converts those hooks into `ObservedEvent`s.

The final application binary or wasm entrypoint installs logging and analytics:

```rust
#[cfg(not(target_arch = "wasm32"))]
pocopine::logging::init_server_logging(
    pocopine::logging::ServerLoggingConfig::json(),
)?;
```

```rust
let analytics = pocopine::analytics::AnalyticsClient::new()
    .with_sink(my_vendor_sink);

analytics.emit(
    pocopine::analytics::route_view("/settings")
);
```

```rust
pocopine::app! {
    components: [AppShell, Home],
    plugins: [
        pocopine::logging::frontend_observability(),
    ],
    routes: [
        ("/", Home),
    ],
};
```

### 3.3 Vendor adapters

Vendor SDKs are adapters, not framework contracts.

- Firebase/Google Analytics are browser adapters that receive caller-provided
  JS functions/SDK handles.
- Cloudflare Workers can consume structured console logs through
  `pocopine-logging`; Analytics Engine-style writes should be a
  `pocopine-analytics` sink.
- AWS logging belongs in `pocopine-logging` as a batched host-side log sink.
- OpenTelemetry belongs in `pocopine-analytics`/telemetry as an adapter from
  `tracing` spans/events, not as the core event model.

This keeps vendor APIs out of `pocopine-core` and out of
`pocopine-observe`.

## 4. Reliability Rules

- Telemetry/exporter failure must not fail app behavior.
- Analytics fan-out continues after a sink returns an error.
- Sink panics are caught and reported as analytics delivery failures.
- Export queues must be bounded when asynchronous exporters are added.
- Overflow should drop low-priority analytics before operational errors.
- Exporters must expose dropped-event and export-error counters once metrics
  sinks land.
- Libraries do not install global subscribers.

## 5. Privacy Rules

The default posture is allowlist-based.

- `public` fields may be exported everywhere.
- `pseudonymous` fields require an explicit policy.
- `sensitive` fields are removed unless the application explicitly opts into
  them for a trusted sink.
- Analytics sinks receive redacted events, not raw debug fields.
- Route params, auth headers, cookies, raw model payloads, DOM text, and user
  identifiers are not public fields.

## 6. Non-goals

- No default network exporter in the first slice.
- No direct Firebase, AWS, Cloudflare, or OpenTelemetry dependency in
  `pocopine-core`.
- No automatic scraping of arbitrary `tracing` fields into analytics.
- No consent-management UI in this RFC.
- No guarantee that all future exporters are synchronous.

## 7. Initial Implementation

Phase 1 creates the three crates, workspace wiring, umbrella crate optional
features, redaction tests, analytics fan-out tests, server logging setup, and
wasm logging/analytics adapter compile checks.

Later phases should add:

- first-class route/server-function/job instrumentation points;
- OpenTelemetry adapter;
- AWS/Cloudflare host sinks;
- bounded async exporter queues;
- metrics for drops and exporter failures;
- devtools panel integration on top of the same event stream.
