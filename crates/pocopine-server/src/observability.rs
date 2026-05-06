//! HTTP request telemetry layer.
//!
//! A thin axum middleware that emits [`HttpRequestStarted`],
//! [`HttpRequestCompleted`], and [`HttpRequestFailed`] events for
//! every request that flows through it. Observability plugins
//! install it with [`crate::Server::layer`]:
//!
//! ```no_run
//! use pocopine_server::{axum::Router, request_event_layer, Server};
//!
//! # async fn run() -> std::io::Result<()> {
//! Server::new(Router::new())
//!     .layer(request_event_layer())
//!     .serve("0.0.0.0:3000")
//!     .await
//! # }
//! ```
//!
//! **Install after routes.** axum's `Router::layer` (which
//! [`crate::Server::layer`] calls under the hood) only wraps routes
//! that exist at the call site — routes added later (e.g. by other
//! plugins via `Server::route` or `Server::router_mut`) silently
//! bypass the layer and emit no events.
//!
//! Apps that don't install any HTTP-event hook pay only an atomic
//! load per request — the layer short-circuits before allocating
//! anything for the event. The middleware always stamps a
//! [`RequestId`] into request extensions even when no HTTP hooks
//! are wired, so downstream `#[server]` route handlers can inherit
//! the same correlation id; the stamp is a single map insert and
//! costs less than the relaxed atomic load that gates the events.

use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::Route;
use tower::Layer;

use crate::plugin::{self, HttpRequestCompleted, HttpRequestFailed, HttpRequestStarted, RequestId};

/// Build the HTTP request event layer. The returned value is a
/// tower [`Layer`] suitable for [`crate::Server::layer`].
///
/// Note: the layer must be applied as a layer on the **router**
/// (`Server::layer` or `Router::layer`) so axum has a chance to
/// populate `MatchedPath` in the request extensions.
pub fn request_event_layer() -> impl Layer<
    Route,
    Service = impl tower::Service<
        Request,
        Response = Response,
        Error = std::convert::Infallible,
        Future = impl Send,
    > + Clone
                  + Send
                  + Sync
                  + 'static,
> + Clone
       + Send
       + Sync
       + 'static {
    middleware::from_fn(request_event_middleware)
}

async fn request_event_middleware(mut request: Request, next: Next) -> Response {
    // Stamp the correlation id before checking HTTP hooks: the
    // `#[server]` macro reads this from request extensions, so
    // server-function events can share an id with the HTTP layer
    // even when only ServerFunction* hooks are wired (no HTTP
    // observer).
    let request_id = plugin::next_request_id();
    request.extensions_mut().insert(RequestId(request_id));

    if !plugin::has_http_request_hooks() {
        return next.run(request).await;
    }

    let started = Instant::now();
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let route_pattern = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());

    if plugin::has_http_request_started_hooks() {
        plugin::emit(HttpRequestStarted {
            method: method.clone(),
            path: path.clone(),
            route_pattern: route_pattern.clone(),
            request_id,
        });
    }

    let response = next.run(request).await;
    let duration_ms = elapsed_ms(started);
    let status = response.status().as_u16();

    if status >= 500 {
        if plugin::has_http_request_failed_hooks() {
            plugin::emit(HttpRequestFailed {
                method,
                path,
                route_pattern,
                request_id,
                reason: classify_5xx(status),
                duration_ms,
            });
        }
    } else if plugin::has_http_request_completed_hooks() {
        plugin::emit(HttpRequestCompleted {
            method,
            path,
            route_pattern,
            request_id,
            status,
            duration_ms,
        });
    }

    response
}

fn elapsed_ms(started: Instant) -> f64 {
    let elapsed = started.elapsed();
    elapsed.as_secs_f64() * 1_000.0
}

fn classify_5xx(status: u16) -> &'static str {
    match status {
        500 => "internal_server_error",
        501 => "not_implemented",
        502 => "bad_gateway",
        503 => "service_unavailable",
        504 => "gateway_timeout",
        _ => "server_error",
    }
}
