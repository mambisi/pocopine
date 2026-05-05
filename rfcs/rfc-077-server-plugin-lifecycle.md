# RFC 077 - Server plugin lifecycle

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-05 |
| **Related** | [`rfc-066-server-function-auth.md`](./rfc-066-server-function-auth.md), [`rfc-067-redis-background-jobs.md`](./rfc-067-redis-background-jobs.md), [`rfc-069-observability.md`](./rfc-069-observability.md), [`rfc-076-app-plugin-lifecycle.md`](./rfc-076-app-plugin-lifecycle.md) |
| **Supersedes** | - |

## 1. Summary

Add a host-side plugin lifecycle that mirrors the frontend app plugin shape
without pretending the runtime constraints are the same.

The proposed server API is:

```rust
pub trait ServerPlugin {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn install(self, server: Server) -> Server;
}

impl Server {
    pub fn new(router: axum::Router) -> Self;
    pub fn plugin<P: ServerPlugin>(self, plugin: P) -> Self;
    pub fn provide_plugin<T: Send + Sync + 'static>(self, service: T) -> Self;
    pub fn hook_plugin<T, E>(self) -> Self
    where
        T: ServerHook<E> + Send + Sync + 'static,
        E: Clone + Send + Sync + 'static;

    pub async fn serve(self, addr: impl ToSocketAddrs) -> std::io::Result<()>;
}
```

Existing `pocopine_server::serve(router, addr)` stays as a compatibility
wrapper around `Server::new(router).serve(addr)`.

## 2. Motivation

The frontend plugin work in RFC 076 fixes the client-side ownership problem:
observability, analytics, live features, and devtools can be installed by an
app without editing core. The server still has the old shape:

```rust
let router = app::__routes(Router::new().fallback_service(static_files("pkg")));
serve(router, addr).await?;
```

That gives applications a router and a serve helper, but no first-class place
for reusable crates to install:

- logging/tracing subscribers;
- HTTP request telemetry middleware;
- server-function telemetry hooks;
- auth/session middleware presets;
- job lifecycle observers;
- Cloudflare/AWS/deploy-target adapters;
- health/readiness endpoints.

The observability plugin should be installable on both sides with the same
mental model:

```rust
pocopine::app! {
    plugins: [pocopine_observability::frontend(config.clone())],
    // ...
}

pocopine_server::Server::new(router)
    .plugin(pocopine_observability::server(config))
    .serve(addr)
    .await?;
```

## 3. Goals

- Provide a server-side plugin lifecycle parallel to `AppPlugin`.
- Keep axum as the underlying router and middleware model.
- Store server plugin services in `Arc<T>` and require `Send + Sync`.
- Validate hook/service wiring before binding the socket.
- Preserve the existing `serve(router, addr)` API.
- Give observability a stable hook surface for server boot, HTTP requests,
  server functions, and jobs.
- Keep vendor exporters outside `pocopine-server`.

## 4. Non-goals

- No dynamic plugin loading.
- No compile-time proof that every required plugin service is installed.
- No replacement for axum `Router`, `Layer`, `Extension`, or `State`.
- No plugin dependency solver in the first slice.
- No vendor-specific exporters in core server crates.
- No async plugin installation in the first slice. Async work belongs in
  middleware, background tasks, or lifecycle hooks.

## 5. Design

### 5.1 Server builder

`Server` wraps the axum router and plugin registry until serve time:

```rust
pub struct Server {
    router: axum::Router,
    plugins: ServerPluginRegistry,
    startup_hooks: Vec<Box<dyn FnOnce(&ServerServices) + Send>>,
}
```

Application code still owns router composition:

```rust
let router = Router::new().fallback_service(static_files("pkg"));
let router = app::__routes(router).with_auth(auth_provider);

Server::new(router)
    .plugin(logging_plugin())
    .plugin(observability_plugin())
    .serve(addr)
    .await?;
```

Plugins may transform the server by adding middleware, endpoints, services, or
hooks:

```rust
impl ServerPlugin for ObservabilityServerPlugin {
    fn name(&self) -> &'static str {
        "pocopine-observability-server"
    }

    fn install(self, server: Server) -> Server {
        server
            .provide_plugin(Observability::new(self.config))
            .layer(observability_http_layer())
            .hook_plugin::<Observability, ServerBootStarted>()
            .hook_plugin::<Observability, ServerBootFailed>()
            .hook_plugin::<Observability, ServerListening>()
            .hook_plugin::<Observability, ServerFunctionCompleted>()
            .hook_plugin::<Observability, ServerFunctionFailed>()
    }
}
```

The first implementation should expose only the methods needed by real
integrations:

- `plugin`;
- `provide_plugin`;
- `hook_plugin`;
- `layer`;
- `route` / `nest` if needed for health endpoints;
- `router_mut` only if a concrete integration cannot be expressed otherwise.

### 5.2 Services and hooks

Frontend plugin services are `Rc<T>` because wasm app code is single-threaded.
Server services must be `Arc<T>` and `T: Send + Sync + 'static`.

```rust
pub struct ServerPluginHandle<T: Send + Sync + 'static> {
    service: Arc<T>,
}

pub trait ServerHook<E>: Send + Sync + 'static {
    fn call(&self, event: E);
}
```

The registry records provider names exactly like RFC 076:

- duplicate services panic immediately and name both providers;
- hook requirements are validated before bind;
- missing hook services render/log one startup error and return
  `std::io::ErrorKind::InvalidInput` from `serve`.

### 5.3 Server events

The first event set should be stable and privacy-aware:

```rust
ServerBootStarted {
    addr,
}

ServerListening {
    addr,
}

ServerBootFailed {
    reason,
}

HttpRequestStarted {
    method,
    path,
    route_pattern,
    request_id,
}

HttpRequestCompleted {
    method,
    path,
    route_pattern,
    request_id,
    status,
    duration_ms,
}

HttpRequestFailed {
    method,
    path,
    route_pattern,
    request_id,
    reason,
    duration_ms,
}

ServerFunctionStarted {
    function,
    request_id,
}

ServerFunctionCompleted {
    function,
    request_id,
    duration_ms,
}

ServerFunctionRejected {
    function,
    request_id,
    status,
    reason,
}

ServerFunctionFailed {
    function,
    request_id,
    error_class,
    duration_ms,
}
```

Request bodies, response bodies, cookies, authorization headers, and raw server
function payloads are never part of framework events. Observability plugins may
derive fields such as payload size or error class, but raw payload logging
stays opt-in application code.

Job lifecycle events should either be added to this same hook registry or
bridged from `pocopine-jobs` through a small server plugin:

```rust
JobStarted { job_name, job_id, attempt, max_attempts }
JobCompleted { job_name, job_id, duration_ms }
JobRetryScheduled { job_name, job_id, attempt, retry_in_ms }
JobDeadLettered { job_name, job_id, attempts, error_class }
```

The preferred first slice is to make jobs emit through their existing tracing
targets and let the observability plugin consume them. A later slice can add
typed job hooks if callers need in-process behavior.

### 5.4 HTTP middleware

HTTP events should be emitted from a tower layer installed by a plugin:

```rust
Server::new(router)
    .plugin(pocopine_observability::server(config))
    .serve(addr)
    .await?;
```

This keeps plain `pocopine_server::serve(router, addr)` lightweight. Apps that
do not install the observability plugin pay no request middleware cost.

Route pattern capture should prefer axum's matched-path extension when present.
If no matched pattern exists, emit `None` and keep the raw path subject to the
same redaction rules as frontend route events.

### 5.5 Server functions

The generated `#[server]` route code already emits tracing events for guards,
body read/parse failures, completion, and failures. RFC 077 should add a typed
server hook bridge without weakening the existing tracing contract:

- keep `pocopine.trace` / `pocopine.log` emissions;
- add typed events beside the tracing events;
- use the generated function name as the stable operation key;
- include status/error class, not raw payloads.

This lets observability plugins choose either in-process hooks, tracing
subscribers, or both.

### 5.6 Compatibility

The existing helper remains:

```rust
pub async fn serve(router: Router, addr: &str) -> std::io::Result<()> {
    Server::new(router).serve(addr).await
}
```

This keeps current examples working while allowing new examples to demonstrate
the richer shape:

```rust
let router = app::__routes(Router::new().fallback_service(static_files("pkg")));

Server::new(router)
    .plugin(pocopine_logging::server_plugin())
    .plugin(pocopine_observability::server_plugin(config))
    .serve("0.0.0.0:3000")
    .await
```

## 6. Privacy and Reliability

Server plugins run in a higher-risk environment than browser plugins:

- they can see credentials and request bodies;
- they may run on multi-threaded runtimes;
- they may affect every request in production.

The first server plugin implementation must therefore enforce:

- `Send + Sync` service bounds;
- validation before bind;
- duplicate-service diagnostics;
- no raw request/payload fields in framework events;
- no global subscriber installed by runtime crates;
- explicit app opt-in for exporters and middleware;
- failure isolation inside analytics/exporter crates.

## 7. Phased Plan

### Phase 1 - Server builder and compatibility

- Add `Server` and `ServerPlugin`.
- Implement `serve(router, addr)` as a wrapper.
- Add plugin service registry with metadata and validation.
- Add `ServerBootStarted`, `ServerListening`, and `ServerBootFailed` hooks.
- Add unit tests for duplicate providers, missing services, and serve wrapper
  compatibility.

### Phase 2 - Observability HTTP layer

- Add opt-in request telemetry layer.
- Emit `HttpRequestStarted`, `HttpRequestCompleted`, and
  `HttpRequestFailed`.
- Use axum matched paths when available.
- Add tests that assert no cookies, auth headers, or bodies enter events.

### Phase 3 - Server function typed hooks

- Extend macro-generated server routes to emit typed events beside current
  tracing events.
- Cover guard rejection, body read/parse failure, success, and app failure.
- Reuse existing `server_auth` integration tests as the capture harness.

### Phase 4 - Jobs bridge

- Decide whether jobs need typed hooks or whether tracing events plus
  observability subscribers are sufficient.
- If typed hooks are added, keep them in `pocopine-jobs` and expose a server
  plugin that forwards them into the shared observability service.

## 8. Open Questions

1. Should `Server::serve` accept `impl ToSocketAddrs`, `SocketAddr`, or keep
   `&str` for compatibility and simplicity?
2. Should server plugin services be extractable from axum request handlers, or
   should request handlers use axum `State` / `Extension` directly?
3. Should typed server hooks be sync like frontend hooks, or should they return
   futures for exporters that need async work?
4. How much router mutation should plugins be allowed to perform before
   `Server::serve`?
