# RFC 077 - Server plugin lifecycle

| Field | Value |
|---|---|
| **Status** | Implemented (Phases 1-3); Phase 4 closed — typed job hooks rejected (2026-05-09), middleware-chain alternative deferred until demand |
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
body read/parse failures, completion, and failures. RFC 077 adds a typed
server hook bridge without weakening the existing tracing contract:

- keep `pocopine.trace` / `pocopine.log` emissions;
- add typed events beside the tracing events;
- use the generated function name as the stable operation key;
- include status/error class, not raw payloads.

This lets observability plugins choose either in-process hooks, tracing
subscribers, or both.

#### 5.5.1 How macro-generated handlers reach the registry

`#[server]` route handlers are static axum handlers — they take a
`Request`, return a response, and have no per-instance state. There's no
opportunity to thread an `&Server` reference into them. RFC 077 solves
this by storing the active registry as a process-global behind a
`LazyLock<RwLock<Arc<PluginRegistry>>>`, and exposing the operations
the macro needs as **public free functions** in
`pocopine_server::plugin`:

- `pocopine_server::has_*_hooks()` — `AtomicU16::load` against the cached
  hook bitmask. Cheap enough to call before constructing each event.
- `pocopine_server::emit::<E>(event)` — looks up the dispatch vec for
  `TypeId::of::<E>()` and runs every registered closure.
- `pocopine_server::next_request_id()` — fallback when no `RequestId`
  is in extensions.
- `pocopine_server::active_plugin::<T>()` — for any code that wants the
  service handle directly.

The macro emits direct calls to these:

```rust
if ::pocopine_server::has_server_function_started_hooks() {
    ::pocopine_server::emit(::pocopine_server::ServerFunctionStarted {
        function: #fn_name_str,
        request_id: __pocopine_request_id,
    });
}
```

`Server::serve` (or `try_finalize`) is the only API that mutates the
registry — it calls `pocopine_server::plugin::activate(registry)`,
which atomically swaps the registry behind the `RwLock` and stores
the new bitmask under `Release` ordering. After that point the
registry is read-only for the lifetime of the process; no
`Arc<ServerPluginRegistry>` is ever layered into request extensions
or axum `State`, because none is needed.

The **one** piece of state that flows via extensions is the
correlation [`RequestId`]. `request_event_layer` stamps it on the
request before downstream handlers see it; the macro reads
`parts.extensions.get::<RequestId>()` and falls back to
`next_request_id()` when the layer wasn't installed. This keeps
HTTP-layer events and server-function events on the same id without
forcing `request_event_layer` to be installed for typed server
function events to fire.

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

### Phase 1 - Server builder and compatibility ✅

- ✅ Added `Server` and `ServerPlugin` (`crates/pocopine-server/src/server.rs`).
- ✅ Reimplemented `serve(router, addr)` as a wrapper around `Server::new(router).serve(addr)`.
- ✅ Plugin service registry (`crates/pocopine-server/src/plugin.rs`) with provider
  metadata, duplicate-provider panics, and pre-bind validation.
- ✅ `ServerBootStarted`, `ServerListening`, and `ServerBootFailed` hooks.
- ✅ Tests in `crates/pocopine-server/tests/server_plugin.rs`.

### Phase 2 - Observability HTTP layer ✅

- ✅ Opt-in `request_event_layer()` exposed from `pocopine_server` —
  observability plugins install it via `Server::layer(...)`.
- ✅ Emits `HttpRequestStarted`, `HttpRequestCompleted`, and
  `HttpRequestFailed`. The layer also stamps a [`RequestId`] into request
  extensions so server-function events can inherit the same correlation id.
- ✅ Uses axum's `MatchedPath` for `route_pattern` when available.
- ✅ Tests in `crates/pocopine-server/tests/server_request_events.rs` assert
  matched/unmatched routing, 5xx classification, and that no headers / cookies
  / query strings / bodies enter framework events.

### Phase 3 - Server function typed hooks ✅

- ✅ `#[server]` macro emits `ServerFunctionStarted`,
  `ServerFunctionCompleted`, `ServerFunctionRejected`, and
  `ServerFunctionFailed` beside the existing tracing events.
- ✅ Started fires before guard / body work; rejected covers guard rejection,
  body read failure, body parse failure (each with a stable `reason` and the
  appropriate semantic `status`); completed/failed split on user-handler `Ok`
  vs `Err`.
- ✅ `request_id` is read from `RequestId` in request extensions when present
  (set by `request_event_layer`), so HTTP-layer and server-function events
  share an id end-to-end. Falls back to a fresh id when no HTTP layer ran.
- ✅ Tests in `crates/pocopine/tests/server_function_events.rs`.

### Phase 4 - Jobs bridge — typed hooks rejected; middleware chain on demand

The job runtime in `pocopine-jobs` already emits structured tracing events
(`pocopine.trace` / `pocopine.log` targets per RFC-069). The first slice of
this RFC shipped server-side typed hooks for HTTP requests and `#[server]`
calls; jobs are subscribed via tracing subscribers in observability plugins.

The original sketch promoted those tracing events to typed
`JobStarted` / `JobCompleted` / `JobRetryScheduled` / `JobDeadLettered`
hooks if a downstream plugin needed in-process behavior. **A 2026-05-09
study of the Sidekiq and Celery plugin ecosystems concluded the typed
lifecycle-hook shape is the wrong primitive and should not ship.** Future
agents and contributors picking up this work should not propose
`JobStarted` / `JobCompleted` / etc. without first reading this section.

#### Why the typed-hook shape is rejected

Sidekiq (Ruby) and Celery (Python) are the two largest production
ecosystems with the typed observe-and-act lifecycle-hook surface this RFC
sketched. Both have ~10+ years of plugin authorship to draw on. The
empirical pattern across both:

- **~90–95% of plugins are observe-only.** Sentry, OpenTelemetry, Datadog,
  New Relic, AppSignal, Prometheus exporters, Flower, `django-celery-results`,
  `sidekiq-statsd`, `sidekiq-status`, `rollbar-sidekiq`: all wrap execution,
  emit a metric or capture an exception, re-raise unchanged. Tracing serves
  every one of them.
- **Sidekiq death handlers are fire-and-forget in production.** Every
  documented `sidekiq_retries_exhausted` / `Sidekiq.death_handlers` usage
  (Slack pings, Sentry capture, log lines) treats the callback as
  non-blocking. Nobody relies on the framework waiting for the handler to
  complete before acking the dead-letter, despite the API technically
  allowing it. The "transactional DLQ pipeline" use case the original
  Phase 4 anticipated does not exist in the wild.
- **Genuine observe-and-act needs cluster into exactly three patterns**,
  none of which fit a lifecycle hook:
  1. **Uniqueness/locking** (`sidekiq-unique-jobs`, `celery-once`,
     `celery-singleton`): mutate the enqueue payload before it's written;
     hold a lock around execution and release it after.
  2. **Throttling/rate-limiting** (`sidekiq-throttled`): suppress execution
     and reschedule the job, replacing rather than reacting to the lifecycle.
  3. **Context wrapping** (`apartment-sidekiq`, tracing-context propagation):
     lexical scope around the call (e.g., `Apartment::Tenant.switch { yield }`).

  All three require **wrapping execution**, not observing a transition. A
  `JobFailed` callback can't suppress-and-reschedule; a `JobStarted`
  callback can't lexically scope the body that follows.

- **Celery community evidence is the killer data point.** Celery already
  has typed observe-and-act signals (`task_prerun`, `task_failure`,
  `task_retry`). The act-pattern plugins deliberately bypass them.
  `celery-once` and `celery-singleton` override the task base class
  instead, because raising from `task_prerun` does not cancel the task
  ([celery #7792](https://github.com/celery/celery/issues/7792)). When
  the typed-hook surface was tried, it failed the act-cases so badly the
  community routed around it.

#### What to build if demand arrives

If a real plugin author surfaces a need for in-process job behaviour the
existing tracing events can't express, the right primitive is **a single
around-job middleware chain** mirroring the fetch middleware chain shipped
in RFC-078 §5.10:

```rust
trait JobMiddleware: 'static {
    fn call(&self, job: JobInvocation, next: JobNext) -> JobMiddlewareFuture;
}
```

One observe-and-act surface handles all three real act-patterns
(lock-around-execution, suppress-and-reschedule, lexical context) plus
the observe-only case (`next.run(job).await`, then record). Same
freeze-at-boot trust contract as `pocopine_core::fetch`. Designing this
now without a consumer would over-fit on guesses — when a real plugin
shows up, design against its actual shape.

#### What not to build

Do not ship `JobStarted` / `JobCompleted` / `JobRetryScheduled` /
`JobDeadLettered` as typed `ServerHook<E>` events. The cost (four emit
sites in every job's hot path, four event types to maintain, four
Plugin-trait method declarations) buys nothing tracing doesn't already
provide for the 95% observe-only case, and falls short of what the 5%
act-case actually needs. The Celery experience is the proof.

## 8. Open Questions — resolved

1. ~~Should `Server::serve` accept `impl ToSocketAddrs`, `SocketAddr`, or keep
   `&str`?~~ **Resolved**: `&str`. Matches the legacy `serve(router, addr)`
   signature so the wrapper relationship is one-line. Apps that need
   `SocketAddr` directly can stringify (`addr.to_string()`); apps that need
   `ToSocketAddrs` resolution can pre-resolve before passing in.
2. ~~Should server plugin services be extractable from axum request handlers,
   or should handlers use axum `State` / `Extension` directly?~~ **Resolved**:
   handlers continue to use axum `State` / `Extension`. Plugin services are
   accessed via `pocopine_server::active_plugin::<T>()` from anywhere in
   process — used by typed-hook closures and any code that wants the
   service handle. Mixing two state mechanisms in route handlers would
   conflict with axum's well-established type-driven extraction.
3. ~~Should typed server hooks be sync, or return futures for async exporters?~~
   **Resolved**: sync. Mirrors the frontend `Hook<E>` shape. Async exporters
   that need a runtime should fan out to a background task via a channel
   inside the hook body — the hook is just a fast notification, not the
   place to do network I/O.
4. ~~How much router mutation should plugins perform before
   `Server::serve`?~~ **Resolved**: plugins use `Server::layer`,
   `Server::route`, and `Server::router_mut` as escape hatch for arbitrary
   `Router -> Router` transforms. The full axum API stays accessible — the
   builder doesn't try to gate which middleware are "OK" for plugins to
   install.
