---
title: "Server plugins"
description: "Host-side plugin lifecycle: the Server builder, request event layer, and typed server-function hooks."
---

# Server plugins

`pocopine-server` ships a host-side plugin lifecycle that mirrors the
frontend `App` plugin shape from RFC-076. A `ServerPlugin` value installs
tower middleware, plugin-provided services, and lifecycle event hooks
around an axum `Router`. Apps opt into optional integrations
(observability, logging, devtools, deploy adapters) by name in their
`main` instead of editing core startup code.

## Quickstart

```rust
use pocopine::logging::server_observability;
use pocopine_server::axum::Router;
use pocopine_server::{static_files, Server};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let router = Router::new()
        .nest_service("/", static_files("pkg"));

    Server::new(router)
        .plugin(server_observability()) // installs hooks + HTTP request layer
        .serve("0.0.0.0:3000")
        .await
}
```

Plain `pocopine_server::serve(router, addr)` still works as a one-line
wrapper — `Server::new(router).serve(addr)` under the hood. Do not also
call generated `__*_route` helpers before `serve`; linked server
functions are installed by `Server::new`.

## Writing a plugin

Implement `ServerPlugin` (or pass a closure) and return the builder
after installing layers, services, and hooks:

```rust
use pocopine_server::{Server, ServerHook, ServerPlugin, ServerFunctionCompleted};

struct ObservabilityPlugin { config: Config }

impl ServerPlugin for ObservabilityPlugin {
    fn name(&self) -> &'static str {
        "my-observability-server"
    }

    fn install(self, server: Server) -> Server {
        server
            .provide_plugin(Observability::new(self.config))
            .hook_plugin::<Observability, ServerFunctionCompleted>()
    }
}

impl ServerHook<ServerFunctionCompleted> for Observability {
    fn call(&self, event: ServerFunctionCompleted) {
        self.metrics.record(event.function, event.duration_ms);
    }
}
```

A free function that takes `Server -> Server` is also a `ServerPlugin`
via a blanket impl — useful for short closures.

## Services

`Server::provide_plugin::<T>(service)` installs a `T: Send + Sync +
'static` value as a process-global service. It's stored as `Arc<T>`
internally so concurrent request handlers and event hooks can each hold
a clone without coordinating.

Look up the service from anywhere with `pocopine_server::active_plugin::<T>()`.

Duplicate provides for the same `T` panic at install time and name both
providers in the diagnostic — designed to fail loud, not silently
overwrite.

## Hooks

Implement `ServerHook<E>` on a service type and register the dispatch
with `Server::hook_plugin::<T, E>()`:

```rust
impl ServerHook<HttpRequestCompleted> for Observability {
    fn call(&self, event: HttpRequestCompleted) { ... }
}

server.hook_plugin::<Observability, HttpRequestCompleted>()
```

Each event is `Clone + Send + Sync + 'static`. Hooks fire synchronously
on the request task — fan out to a background task via a channel if you
need to do network I/O.

## Lifecycle events

| Event | Source | Fires |
|---|---|---|
| `ServerBootStarted` | `Server::serve` | After plugin validation succeeds, before bind |
| `ServerListening` | `Server::serve` | Once the listener is bound and ready |
| `ServerBootFailed` | `Server::serve` | Runtime boot failure after validation succeeded (`address_parse`, `bind`). **Plugin-validation failures do not fire this event** — see "Validation" below. |
| `HttpRequestStarted` | `request_event_layer` | After axum matched the route |
| `HttpRequestCompleted` | `request_event_layer` | Status known and `< 500` |
| `HttpRequestFailed` | `request_event_layer` | Response status `>= 500` |
| `ServerFunctionStarted` | `#[server]` macro | Top of route handler, before guard/body |
| `ServerFunctionCompleted` | `#[server]` macro | User handler returned `Ok` |
| `ServerFunctionRejected` | `#[server]` macro | Guard / body-read / body-parse rejected |
| `ServerFunctionFailed` | `#[server]` macro | User handler returned `Err` |

`HttpRequest*` and `ServerFunction*` events share a `request_id` when
the HTTP layer is installed — the layer stamps `RequestId` into request
extensions and the macro reads it. Without the HTTP layer, server
functions allocate their own `request_id`, so `ServerFunction*` events
remain correlated within a single call but won't share an id with
external HTTP traces.

Privacy invariant: framework events never carry headers, cookies, query
strings, or request/response bodies. Observability plugins derive
size/error-class fields if they need them.

## Validation

`Server::serve` validates plugin configuration before binding the
listener:

- Every hook registered with `hook_plugin::<T, E>` must have a matching
  `provide_plugin::<T>(service)` somewhere in the install chain.
- Missing services produce one `tracing::error!` per missing entry on
  the `pocopine.log` target and an `io::Error` of kind `InvalidInput`
  from `serve` — the listener is never bound.
- Duplicate `provide_plugin::<T>` calls panic immediately, naming both
  the first and second provider so plugin ordering bugs surface at
  install time, not the first event.

Validation failures **do not emit `ServerBootFailed`.** The registry
is reset to empty before any potential emit so a failing plugin
chain cannot observe its own rejection, and the framework is "no
plugins active" from that point until a successful future
activation (or `__reset_for_test` followed by another). The
`io::Error` returned by `serve` is the only signal.
`ServerBootFailed` is reserved for runtime failures *after*
validation has succeeded — bind-side errors like `address_parse`
and `bind`, where some hooks may already be live.

## Server-function 401/403 status codes

`#[server]` returns a 200 response carrying a JSON-encoded `Result` —
the `status` field on `ServerFunctionRejected` reports the *semantic*
status code (`401` for `ServerError::Unauthorized`, `403` for
`ServerError::Forbidden`, `400` for `ServerError::BadRequest`) so
observability plugins can classify auth rejections distinctly from
the wire transport, which is always 200.

## Hook ordering

Hooks for a given event fire in registration order. Plugins installed
earlier fire earlier; within one plugin, `hook_plugin` calls fire in
source order. This is currently a guarantee — if it ever changes the
RFC will call it out.

## Layer ordering

`Server::layer(layer)` calls axum's `Router::layer` under the hood,
which only wraps routes that exist at the call site. Routes added
later — by another plugin's `Server::route` / `Server::router_mut`
call, or by code that runs after `.layer(...)` — silently bypass the
layer.

Install layers after routes:

```rust
Server::new(Router::new())
    .plugin(adds_health_endpoints())          // adds /healthz
    .layer(request_event_layer())             // wraps user + health routes
    .plugin(observability_plugin(config))
    .serve(addr).await
```

Within a single plugin's `install` fn, the same rule applies: call
`route` / `router_mut` first, then `layer`. The `RouterAuthExt::with_auth`
extension on `axum::Router` has the identical caveat documented in
its rustdoc — same axum constraint.

## active_plugin cost

`active_plugin::<T>()` reads the process-global plugin registry
behind an `RwLock` and returns an `Arc<T>` clone. Each call:

- one `RwLock::read` (~10 ns under no contention),
- one `Arc::clone` of the registry,
- one `HashMap::get` keyed by `TypeId`,
- one `Arc::clone` of the service.

That's fine for one-off lookups (process startup, hook closures), but
calling it on every request from a hot route handler accumulates four
atomic operations per request that you don't need. Two patterns avoid
the per-request cost:

- **Stash on app state.** Look up the plugin once when building the
  router and clone its `Arc` into your axum `State`. Handlers extract
  `State<Arc<T>>` and reach the service through normal axum DI.
- **Read from a request-scoped extension.** A plugin can install a
  short layer that calls `active_plugin::<T>()` once per request,
  inserts the handle into request extensions, and downstream
  handlers extract via `Extension<ServerPluginHandle<T>>`. Same
  cost as today's auth middleware.

Typed hook closures registered via `hook_plugin` capture the type
parameter `T` but **still resolve the concrete service through the
registry on each dispatch**. The cost is one `HashMap::get` (keyed
by `TypeId::of::<T>()`) and one `Arc::clone` per emit; the
registry's `RwLock` read is already held when `emit` enters, so
hooks don't pay it again. For typical observability volume
(hundreds to low-thousands of emits per second) this is in the
noise; for very hot custom events, profile before assuming hooks
are free.

## What plugins can't do (yet)

- Async install (`install` is sync). Plugins that need to pre-warm
  network state should spawn a `tokio::task` from inside `install`.
- Hot-swap services or hooks after `Server::serve` has been called.
  The active plugin set is sampled once at serve time — there's no
  public API for runtime mutation, and the per-request fast paths
  assume the bitmask cache stays stable.
- Dynamic plugin loading from shared libraries. Plugins are linked into
  the binary at compile time.

## One active server per process

The plugin registry — services, hooks, and the bitmask that gates
emit sites — lives in a process-global. A second `Server::serve`
call inside the same process replaces the first server's registry,
so the first server's `active_plugin::<T>()` lookups and
`hook_plugin` dispatches start resolving against the second
server's plugins:

- Services the new server didn't provide return `None` from
  `active_plugin`, or panic from `Plugin<T>` extractors.
- Hooks the old server registered are silently dropped.

Real applications run a single HTTP listener per process and don't
hit this. The patterns that do:

- **Tests that build multiple `Server`s in one process.** Use
  `pocopine_server::__reset_for_test()` between activations to
  return the registry to a clean state, mirroring the helper used
  in the framework's own integration tests.
- **Multi-tenant deployments that expected to run two listeners
  with independent plugin sets.** Don't — compose them into one
  `Server` (one router with merged routes, one plugin chain). If
  per-tenant plugin isolation becomes a real need, the right path
  is a follow-up RFC that moves registry identity into request
  state rather than a process-global.
