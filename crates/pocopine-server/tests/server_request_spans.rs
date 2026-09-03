//! RFC-123 §3.1 — the `pocopine.http.request` span opened by
//! `request_event_layer()`: filter-gated (exists with no hooks at all),
//! carries the semconv fields, records the status at close, stamps the
//! `RequestId` extension, and echoes `x-request-id`.

#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::await_holding_lock)]

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use pocopine_auth::{AuthFuture, AuthProvider, AuthUser};
use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::routing::get;
use pocopine_server::tower::ServiceExt;
use pocopine_server::{
    RequestContext, RequestEventOptions, RequestId, Server, request_event_layer,
    request_event_layer_with,
};
use tracing::Instrument as _;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

fn registry_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone, Debug, Default)]
struct Span {
    name: String,
    target: String,
    parent: Option<u64>,
    fields: BTreeMap<String, String>,
    events_inside: Vec<String>,
    closed: bool,
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<(u64, Span)>>>);

impl Capture {
    fn spans(&self) -> Vec<Span> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|(_, s)| s.clone())
            .collect()
    }
}

#[derive(Default)]
struct Visitor(BTreeMap<String, String>);

impl Visit for Visitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.to_string());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.insert(field.name().into(), format!("{value:?}"));
    }
}

impl<S> Layer<S> for Capture
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        attrs.record(&mut visitor);
        let parent = ctx
            .span(id)
            .and_then(|span| span.parent().map(|parent| parent.id().into_u64()));
        self.0.lock().unwrap().push((
            id.into_u64(),
            Span {
                name: attrs.metadata().name().into(),
                target: attrs.metadata().target().into(),
                parent,
                fields: visitor.0,
                events_inside: Vec::new(),
                closed: false,
            },
        ));
    }

    fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
        let mut spans = self.0.lock().unwrap();
        if let Some((_, span)) = spans.iter_mut().find(|(sid, _)| *sid == id.into_u64()) {
            span.closed = true;
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        values.record(&mut visitor);
        let mut spans = self.0.lock().unwrap();
        if let Some((_, span)) = spans.iter_mut().find(|(sid, _)| *sid == id.into_u64()) {
            span.fields.extend(visitor.0);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = Visitor::default();
        event.record(&mut visitor);
        let message = visitor.0.get("message").cloned().unwrap_or_default();
        if let Some(current) = ctx.event_span(event) {
            let mut spans = self.0.lock().unwrap();
            if let Some((_, span)) = spans
                .iter_mut()
                .find(|(sid, _)| *sid == current.id().into_u64())
            {
                span.events_inside.push(message);
            }
        }
    }
}

fn with_capture<T>(f: impl FnOnce() -> T) -> (Capture, T) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let out = tracing::subscriber::with_default(subscriber, f);
    (capture, out)
}

fn router_with(layer_options: Option<RequestEventOptions>) -> Router {
    let router = Router::new()
        .route(
            "/users/:id",
            get(|request: Request<Body>| async move {
                let stamped = request.extensions().get::<RequestId>().map(|id| id.0);
                tracing::info!(target: "app::handler", "inside the handler");
                format!("stamped:{stamped:?}")
            }),
        )
        .route("/boom", get(|| async { StatusCode::BAD_GATEWAY }));
    let server = Server::new(router);
    let server = match layer_options {
        Some(options) => server.layer(request_event_layer_with(options)),
        None => server.layer(request_event_layer()),
    };
    server.try_finalize().expect("finalize")
}

#[test]
fn request_span_exists_without_any_hooks() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (capture, (status, body, header)) = with_capture(|| {
        rt.block_on(async {
            let router = router_with(None);
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/users/42?token=secret")
                        .header("x-pocopine-session", "abcdef0123456789")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let header = response
                .headers()
                .get("x-request-id")
                .map(|v| v.to_str().unwrap().to_owned());
            let bytes = pocopine_server::axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            (status, String::from_utf8(bytes.to_vec()).unwrap(), header)
        })
    });
    assert_eq!(status, StatusCode::OK);

    let spans = capture.spans();
    assert_eq!(spans.len(), 1, "{spans:?}");
    let span = &spans[0];
    assert_eq!(span.name, "pocopine.http.request");
    assert_eq!(span.target, "pocopine.trace");
    assert_eq!(
        span.fields.get("otel.kind").map(String::as_str),
        Some("server")
    );
    assert_eq!(
        span.fields.get("otel.name").map(String::as_str),
        Some("GET /users/:id")
    );
    assert_eq!(
        span.fields.get("http.request.method").map(String::as_str),
        Some("GET")
    );
    assert_eq!(
        span.fields.get("http.route").map(String::as_str),
        Some("/users/:id")
    );
    assert_eq!(
        span.fields.get("url.path").map(String::as_str),
        Some("/users/42")
    );
    assert_eq!(
        span.fields
            .get("http.response.status_code")
            .map(String::as_str),
        Some("200")
    );
    assert_eq!(
        span.fields.get("otel.status_code").map(String::as_str),
        Some("OK")
    );
    assert_eq!(
        span.fields.get("session.id").map(String::as_str),
        Some("abcdef0123456789")
    );
    assert!(!span.fields.contains_key("error.type"));
    assert!(
        !span.fields.values().any(|v| v.contains("secret")),
        "query strings never enter the span: {span:?}"
    );
    assert_eq!(span.events_inside, ["inside the handler"]);

    // The RequestId extension was stamped because the span is enabled —
    // no hooks needed — and the same id came back as a header.
    let request_id = span
        .fields
        .get("pocopine.request_id")
        .expect("request id field");
    assert_eq!(body, format!("stamped:Some({request_id})"));
    assert_eq!(header.as_deref(), Some(request_id.as_str()));
}

#[test]
fn five_xx_closes_the_span_as_error_and_header_can_be_disabled() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (capture, header) = with_capture(|| {
        rt.block_on(async {
            let router = router_with(Some(
                RequestEventOptions::new().with_request_id_header(false),
            ));
            let response = router
                .oneshot(Request::builder().uri("/boom").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            response.headers().get("x-request-id").cloned()
        })
    });
    assert!(header.is_none(), "header opted out");

    let spans = capture.spans();
    let span = spans
        .iter()
        .find(|s| s.name == "pocopine.http.request")
        .unwrap();
    assert_eq!(
        span.fields
            .get("http.response.status_code")
            .map(String::as_str),
        Some("502")
    );
    assert_eq!(
        span.fields.get("otel.status_code").map(String::as_str),
        Some("ERROR")
    );
    assert_eq!(
        span.fields.get("error.type").map(String::as_str),
        Some("bad_gateway")
    );
}

#[test]
fn unmatched_route_has_no_http_route_and_a_method_only_name() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (capture, ()) = with_capture(|| {
        rt.block_on(async {
            let router = router_with(None);
            let response = router
                .oneshot(
                    Request::builder()
                        .uri("/nope/tenant-7")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        })
    });
    let spans = capture.spans();
    let span = spans
        .iter()
        .find(|s| s.name == "pocopine.http.request")
        .unwrap();
    assert!(!span.fields.contains_key("http.route"));
    assert_eq!(
        span.fields.get("otel.name").map(String::as_str),
        Some("GET")
    );
    assert_eq!(
        span.fields.get("otel.status_code").map(String::as_str),
        Some("OK")
    );
}

/// An auth provider that logs while authenticating — the log line must
/// land inside the request span even though auth is applied outermost by
/// `try_finalize`.
struct LoggingAuth;

impl AuthProvider for LoggingAuth {
    fn authenticate<'a>(&'a self, _ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async {
            tracing::warn!(target: "pocopine.log", "auth provider probe");
            Ok(Some(AuthUser::new("u1")))
        })
    }
}

#[test]
fn request_events_wrap_auth_and_ignore_ambient_spans() {
    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let (capture, status) = with_capture(|| {
        rt.block_on(
            async {
                let router =
                    Server::new(Router::new().route("/users/:id", get(|| async { "hello" })))
                        .with_auth(LoggingAuth)
                        .request_events(RequestEventOptions::default())
                        .try_finalize()
                        .expect("finalize");
                router
                    .oneshot(
                        Request::builder()
                            .uri("/users/1")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap()
                    .status()
            }
            // An ambient span from some outer layer must not become the
            // request span's parent: requests are roots (RFC-123 §2.2).
            .instrument(tracing::info_span!("ambient")),
        )
    });
    assert_eq!(status, StatusCode::OK);

    let spans = capture.spans();
    let request = spans
        .iter()
        .find(|s| s.name == "pocopine.http.request")
        .expect("request span");
    assert_eq!(request.parent, None, "{request:?}");
    assert!(
        request
            .events_inside
            .iter()
            .any(|m| m == "auth provider probe"),
        "auth ran inside the request span: {request:?}"
    );
}

#[test]
fn request_span_covers_a_streaming_body() {
    use futures::StreamExt as _;
    use pocopine_server::axum::body::{Bytes, to_bytes};

    let _lock = registry_lock();
    pocopine_server::__reset_for_test();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let probe = capture.clone();
    tracing::subscriber::with_default(subscriber, || {
        rt.block_on(async {
            let router = Server::new(Router::new().route(
                "/feed",
                get(|| async {
                    // Lazy: each frame is produced when the body is polled,
                    // i.e. after the middleware has already returned.
                    let frames = futures::stream::iter(1..=3u8).map(|n| {
                        tracing::info!(target: "app::feed", frame = n, "producing a frame");
                        Ok::<Bytes, std::convert::Infallible>(Bytes::from(format!("frame {n}\n")))
                    });
                    Body::from_stream(frames)
                }),
            ))
            .request_events(RequestEventOptions::default())
            .try_finalize()
            .expect("finalize");

            let response = router
                .oneshot(Request::builder().uri("/feed").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);

            // Headers are out; the body has not been polled yet.
            let at_headers = probe.spans();
            let request = at_headers
                .iter()
                .find(|s| s.name == "pocopine.http.request")
                .expect("request span");
            assert!(!request.closed, "span stays open for the body");
            assert!(request.events_inside.is_empty(), "{request:?}");
            assert_eq!(
                request
                    .fields
                    .get("http.response.status_code")
                    .map(String::as_str),
                Some("200")
            );

            let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(&body[..], b"frame 1\nframe 2\nframe 3\n");
        })
    });

    let spans = capture.spans();
    let request = spans
        .iter()
        .find(|s| s.name == "pocopine.http.request")
        .expect("request span");
    assert_eq!(
        request.events_inside,
        [
            "producing a frame",
            "producing a frame",
            "producing a frame"
        ],
        "every frame was produced inside the request span"
    );
    assert!(request.closed, "the span closed with the body");
}
