//! HTTP request telemetry layer.
//!
//! A thin axum middleware that opens the `pocopine.http.request` span
//! (RFC-123 §3.1) and emits [`HttpRequestStarted`],
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
//! ## The span
//!
//! Every request gets a `pocopine.http.request` span carrying
//! `http.request.method`, `http.route` (axum's [`MatchedPath`], when
//! one matched), `url.path`, `pocopine.request_id`, and — when the
//! client sent one — `session.id`. `http.response.status_code`,
//! `otel.status_code`, and `error.type` are recorded once the
//! response is known. The span is **filter-gated, never hook-gated**:
//! it exists whenever the subscriber enables `pocopine.trace` at
//! `INFO`, regardless of which plugins are installed, so every event
//! emitted while the request is in flight — framework or app — hangs
//! from it.
//!
//! ## Cost
//!
//! Apps with the span filtered off **and** no HTTP-event hooks
//! **and** no `ServerFunction*` hooks pay one callsite check and two
//! relaxed atomic loads per request — the layer short-circuits before
//! allocating a `RequestId` or inserting anything into request
//! extensions. The [`RequestId`] stamp lights up as soon as the span
//! is enabled *or* either hook family becomes active, since the
//! `#[server]` macro reads the stamp out of extensions to share a
//! correlation id with the HTTP layer.

use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::Route;
use pocopine_core::fetch::CLIENT_SESSION_HEADER;
use pocopine_observe::{TRACE_TARGET, fields, spans};
use tower::Layer;
use tracing::Instrument as _;
use tracing::field::Empty;

use crate::plugin::{self, HttpRequestCompleted, HttpRequestFailed, HttpRequestStarted, RequestId};

/// Response header carrying the request's `pocopine.request_id`, so a
/// browser devtools user can find the request's span and events.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Knobs for [`request_event_layer_with`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RequestEventOptions {
    /// Echo the allocated `pocopine.request_id` as an
    /// [`REQUEST_ID_HEADER`] response header. Default `true`.
    pub request_id_header: bool,
    /// Accept an incoming W3C `traceparent` as the request span's
    /// remote parent (RFC-123 §5.3). Only observed with the `otel`
    /// feature; default `true`.
    pub accept_trace_context: bool,
}

impl Default for RequestEventOptions {
    fn default() -> Self {
        Self {
            request_id_header: true,
            accept_trace_context: true,
        }
    }
}

impl RequestEventOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_request_id_header(mut self, enabled: bool) -> Self {
        self.request_id_header = enabled;
        self
    }

    pub fn with_accept_trace_context(mut self, enabled: bool) -> Self {
        self.accept_trace_context = enabled;
        self
    }
}

/// Build the HTTP request span + event layer with default
/// [`RequestEventOptions`]. The returned value is a tower [`Layer`]
/// suitable for [`crate::Server::layer`].
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
    request_event_layer_with(RequestEventOptions::default())
}

/// [`request_event_layer`] with explicit [`RequestEventOptions`].
pub fn request_event_layer_with(
    options: RequestEventOptions,
) -> impl Layer<
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
    middleware::from_fn(move |request: Request, next: Next| async move {
        request_event_middleware(options, request, next).await
    })
}

async fn request_event_middleware(
    options: RequestEventOptions,
    mut request: Request,
    next: Next,
) -> Response {
    let has_http_hooks = plugin::has_http_request_hooks();
    let has_server_fn_hooks = plugin::has_server_function_hooks();

    let route_pattern = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string());

    // Field names are spelled inline (tracing takes identifiers) and
    // must match `pocopine_observe::fields`.
    let span = tracing::info_span!(
        target: TRACE_TARGET,
        spans::HTTP_REQUEST,
        otel.kind = "server",
        otel.name = Empty,
        http.request.method = %request.method(),
        http.route = Empty,
        url.path = request.uri().path(),
        pocopine.request_id = Empty,
        session.id = Empty,
        http.response.status_code = Empty,
        otel.status_code = Empty,
        error.type = Empty,
    );

    // The zero-cost path: nothing downstream can observe this request.
    if span.is_disabled() && !has_http_hooks && !has_server_fn_hooks {
        return next.run(request).await;
    }

    let request_id = plugin::next_request_id();
    request.extensions_mut().insert(RequestId(request_id));
    span.record(fields::REQUEST_ID, request_id);
    match &route_pattern {
        Some(route) => {
            span.record(fields::HTTP_ROUTE, route.as_str());
            span.record(
                fields::OTEL_NAME,
                format!("{} {route}", request.method()).as_str(),
            );
        }
        None => {
            span.record(fields::OTEL_NAME, request.method().as_str());
        }
    }
    let session_id = client_session_id(request.headers());
    if let Some(session) = session_id.as_deref() {
        span.record(fields::SESSION_ID, session);
    }
    #[cfg(feature = "otel")]
    if options.accept_trace_context {
        otel::adopt_remote_parent(&span, request.headers());
    }

    let started = Instant::now();
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();

    if has_http_hooks && plugin::has_http_request_started_hooks() {
        span.in_scope(|| {
            plugin::emit(HttpRequestStarted {
                method: method.clone(),
                path: path.clone(),
                route_pattern: route_pattern.clone(),
                request_id,
                session_id: session_id.clone(),
            })
        });
    }

    let mut response = next.run(request).instrument(span.clone()).await;
    let duration_ms = started.elapsed().as_secs_f64() * 1_000.0;
    let status = response.status().as_u16();
    span.record(fields::HTTP_RESPONSE_STATUS_CODE, status);

    if status >= 500 {
        let reason = classify_5xx(status);
        span.record(fields::OTEL_STATUS_CODE, "ERROR");
        span.record(fields::ERROR_TYPE, reason);
        if has_http_hooks && plugin::has_http_request_failed_hooks() {
            span.in_scope(|| {
                plugin::emit(HttpRequestFailed {
                    method,
                    path,
                    route_pattern,
                    request_id,
                    session_id,
                    reason,
                    duration_ms,
                })
            });
        }
    } else {
        span.record(fields::OTEL_STATUS_CODE, "OK");
        if has_http_hooks && plugin::has_http_request_completed_hooks() {
            span.in_scope(|| {
                plugin::emit(HttpRequestCompleted {
                    method,
                    path,
                    route_pattern,
                    request_id,
                    session_id,
                    status,
                    duration_ms,
                })
            });
        }
    }

    if options.request_id_header
        && let Ok(value) = HeaderValue::from_str(&request_id.to_string())
    {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    #[cfg(feature = "otel")]
    otel::inject_trace_context(&span, response.headers_mut());

    response
}

/// The client's per-page-load session id, if it sent one and it is
/// shaped like one. Anything else is dropped rather than recorded:
/// the value lands in span fields and log lines, so it must not carry
/// arbitrary text.
fn client_session_id(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(CLIENT_SESSION_HEADER)?.to_str().ok()?;
    let ok_len = (8..=64).contains(&value.len());
    let ok_chars = value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    (ok_len && ok_chars).then(|| value.to_string())
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

/// W3C trace-context bridging (RFC-123 §5.3). Dependency-only: the
/// `otel` feature pulls `opentelemetry` + `tracing-opentelemetry` for
/// these two calls and nothing else; `pocopine-logging`'s `otlp`
/// feature enables it. The propagator is whatever the logging init
/// registered globally (a `TraceContextPropagator` under `otlp`).
#[cfg(feature = "otel")]
mod otel {
    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use opentelemetry::propagation::{Extractor, Injector};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    struct HeaderExtractor<'a>(&'a HeaderMap);

    impl Extractor for HeaderExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|v| v.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(HeaderName::as_str).collect()
        }
    }

    struct HeaderInjector<'a>(&'a mut HeaderMap);

    impl Injector for HeaderInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(key.as_bytes()),
                HeaderValue::from_str(&value),
            ) {
                self.0.insert(name, value);
            }
        }
    }

    /// Parent the request span under an incoming `traceparent`, when
    /// the headers carry a valid one. No-op otherwise.
    pub(super) fn adopt_remote_parent(span: &tracing::Span, headers: &HeaderMap) {
        use opentelemetry::trace::TraceContextExt as _;
        let parent = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(headers))
        });
        if parent.span().span_context().is_valid() {
            span.set_parent(parent);
        }
    }

    /// Echo the request span's trace context on the response, so a
    /// browser devtools user can paste `traceparent` into the backend.
    pub(super) fn inject_trace_context(span: &tracing::Span, headers: &mut HeaderMap) {
        let context = span.context();
        opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&context, &mut HeaderInjector(headers))
        });
    }
}
