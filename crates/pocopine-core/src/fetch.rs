//! Client-side fetch helper for `#[server]` functions.
//!
//! `pocopine::fetch::call` is a thin wrapper over `window.fetch`: it
//! JSON-serializes the argument tuple, POSTs it to `url`, and parses the
//! JSON response as a `Result<R>` where the error type is always
//! [`ServerError`]. Network and deserialization failures surface as
//! [`ServerError::Network`]; server-returned errors come through as
//! [`ServerError::App`] inside the `Err` variant the server serialized.
//!
//! ## Middleware chain
//!
//! Plugins (auth retry, telemetry, request signing) install
//! [`FetchMiddleware`] values via [`install_middleware`]. Every
//! `fetch::call` runs through the installed chain in registration
//! order; each middleware sees a [`FetchRequest`] and decides whether
//! to forward it via [`FetchNext::run`] (returning the response) or
//! short-circuit with `Err(ServerError::…)`.
//!
//! Per RFC-078 §5.10.3 the chain **freezes** at the first
//! [`crate::App::run`] or first `fetch::call`, whichever comes first.
//! Subsequent [`install_middleware`] calls panic with a diagnostic.
//! Middleware is privileged code — sees request bodies and synthesises
//! responses — and the freeze removes the seam where untrusted code
//! could install itself after the trust boundary closed.

/// Request header carrying the client's per-page-load session id
/// (RFC-123 §5.4 / Phase 3). Generated once per app boot on the client,
/// recorded as the `session.id` field of the server's request span. It
/// is a correlation id, never a credential.
pub const CLIENT_SESSION_HEADER: &str = "x-pocopine-session";

/// The client's session id for this app boot: 32 lowercase hex chars,
/// minted once and sent on every server-function call as
/// [`CLIENT_SESSION_HEADER`]. The server records it as `session.id` on
/// its request span, so every call from one page load is one query
/// away in any trace backend (RFC-123 §5.4). It is deliberately *not* a
/// `traceparent`: with no client exporter, that would make every
/// server trace the child of a span no backend ever receives.
pub fn client_session_id() -> &'static str {
    static SESSION: once_cell::sync::Lazy<String> = once_cell::sync::Lazy::new(mint_session_id);
    SESSION.as_str()
}

/// Correlation id, not a secret: entropy is hashed through
/// `pocopine-crypto` only so the shape is uniform and unguessable enough
/// not to collide across tabs.
fn mint_session_id() -> String {
    // Raw entropy bytes in, hex out of `pocopine-crypto` — no hand-rolled
    // encoding on the consumer side.
    let mut seed: Vec<u8> = Vec::with_capacity(48);
    #[cfg(target_arch = "wasm32")]
    {
        for _ in 0..4 {
            seed.extend_from_slice(&js_sys::Math::random().to_bits().to_le_bytes());
        }
        seed.extend_from_slice(&js_sys::Date::now().to_bits().to_le_bytes());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        seed.extend_from_slice(&now.to_le_bytes());
        seed.extend_from_slice(&std::process::id().to_le_bytes());
    }
    let mut hex = pocopine_crypto::blake3_hex(&seed);
    hex.truncate(32);
    hex
}

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use tracing::Instrument as _;

use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{AbortSignal, Request, RequestInit, Response};

use crate::server::{Result as ServerResult, ServerError, ServerStream};

// ─── middleware types ───────────────────────────────────────────────

/// Outgoing request as seen by middleware. Mutating these fields in
/// a middleware before forwarding (`next.run(request)`) lets a
/// plugin add headers, rewrite the URL, sign the body, etc.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FetchRequest {
    pub url: String,
    pub method: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
    /// Browser abort signal for this request. Route loaders populate
    /// this automatically while their future is being polled so
    /// navigation supersession cancels the underlying `window.fetch`.
    pub abort_signal: Option<AbortSignal>,
    /// `true` when the generated server-function stub has declared
    /// the call replay-safe (`#[server(idempotent)]`). Auth middleware
    /// may retry these after token refresh; it must fail closed for
    /// the default `false` case.
    pub(crate) replay_safe: bool,
}

impl FetchRequest {
    /// Set or replace a header (case-insensitive name match).
    pub fn set_header(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if let Some(slot) = self
            .headers
            .iter_mut()
            .find(|(k, _)| k.eq_ignore_ascii_case(&name))
        {
            slot.1 = value;
        } else {
            self.headers.push((name, value));
        }
    }

    /// Whether middleware may replay this request after refreshing
    /// credentials. Defaults to `false` unless the generated
    /// `#[server]` client stub opted in with `#[server(idempotent)]`.
    pub fn is_replay_safe(&self) -> bool {
        self.replay_safe
    }
}

/// Response handed back through the middleware chain. Telemetry
/// middleware can read `status` / `body.len()` for metrics; auth
/// middleware can detect `ServerError::Unauthorized` from the
/// surrounding `Result`. Body is always the raw response text;
/// `fetch::call` does the JSON decode after the chain returns.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FetchResponse {
    pub status: u16,
    pub body: String,
    /// The server's `pocopine.request_id`, from `x-request-id`
    /// (RFC-123 §5.5). Middleware that synthesizes a response leaves it
    /// `None`.
    pub request_id: Option<u64>,
    /// The server's W3C `traceparent` response header, verbatim, when it
    /// sent one.
    pub trace_parent: Option<String>,
}

impl FetchResponse {
    /// A response with no server correlation ids — what middleware that
    /// short-circuits (a cache, a test double) constructs.
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
            request_id: None,
            trace_parent: None,
        }
    }

    /// The trace id half of [`Self::trace_parent`], when it is well-formed.
    pub fn trace_id(&self) -> Option<String> {
        self.trace_parent
            .as_deref()
            .and_then(trace_id_from_traceparent)
    }
}

/// The 32-hex trace id of a W3C `traceparent` (`00-<trace>-<span>-<flags>`),
/// or `None` for anything that is not shaped like one.
pub fn trace_id_from_traceparent(value: &str) -> Option<String> {
    let mut parts = value.split('-');
    let version = parts.next()?;
    let trace_id = parts.next()?;
    let span_id = parts.next()?;
    let flags = parts.next()?;
    let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
    if parts.next().is_some()
        || !hex(version, 2)
        || !hex(trace_id, 32)
        || !hex(span_id, 16)
        || !hex(flags, 2)
        || trace_id.bytes().all(|b| b == b'0')
    {
        return None;
    }
    Some(trace_id.to_ascii_lowercase())
}

/// The server correlation ids a response carried, for the client hooks.
#[derive(Clone, Debug, Default)]
struct ResponseIds {
    request_id: Option<u64>,
    trace_id: Option<String>,
}

impl ResponseIds {
    fn of(response: &FetchResponse) -> Self {
        Self {
            request_id: response.request_id,
            trace_id: response.trace_id(),
        }
    }
}

/// Middleware-chain continuation. `run` either invokes the next
/// installed middleware or (at the bottom) performs the real
/// `window.fetch` call. Cloneable so a middleware can replay (e.g.
/// auth retry after token refresh).
#[derive(Clone)]
pub struct FetchNext {
    index: usize,
    middlewares: Rc<Vec<Rc<dyn FetchMiddleware>>>,
}

impl FetchNext {
    /// Forward `request` through the rest of the chain. Returning
    /// the result tells the caller's middleware to behave as if it
    /// did the work itself; replaying with a different
    /// `FetchRequest` after refresh is a supported retry pattern.
    pub fn run(
        self,
        request: FetchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResponse, ServerError>> + 'static>> {
        if self.index >= self.middlewares.len() {
            Box::pin(perform_fetch(request))
        } else {
            let middleware = self.middlewares[self.index].clone();
            let next = FetchNext {
                index: self.index + 1,
                middlewares: self.middlewares.clone(),
            };
            middleware.call(request, next)
        }
    }
}

/// Future returned by [`FetchMiddleware::call`].
pub type FetchMiddlewareFuture =
    Pin<Box<dyn Future<Output = Result<FetchResponse, ServerError>> + 'static>>;

type MiddlewareChain = Rc<Vec<Rc<dyn FetchMiddleware>>>;

/// Trait-erased fetch middleware. Closures of the right shape
/// implement this via the blanket impl below; plugins that need
/// state typically use a `Rc<MyService>` adapter.
pub trait FetchMiddleware: 'static {
    fn call(&self, request: FetchRequest, next: FetchNext) -> FetchMiddlewareFuture;
}

impl<F, Fut> FetchMiddleware for F
where
    F: Fn(FetchRequest, FetchNext) -> Fut + 'static,
    Fut: Future<Output = Result<FetchResponse, ServerError>> + 'static,
{
    fn call(&self, request: FetchRequest, next: FetchNext) -> FetchMiddlewareFuture {
        Box::pin(self(request, next))
    }
}

// ─── chain registry + freeze contract ───────────────────────────────

thread_local! {
    static MIDDLEWARES: RefCell<Vec<Rc<dyn FetchMiddleware>>> = const { RefCell::new(Vec::new()) };
    /// Set to `true` by `freeze_middleware_chain()`; subsequent
    /// `install_middleware` calls panic. Per RFC-078 §5.10.3
    /// middleware is privileged code, and freezing removes the
    /// seam where untrusted code could install itself after the
    /// trust boundary closed.
    static FROZEN: Cell<bool> = const { Cell::new(false) };
    /// Captured once at the open→frozen transition so per-call
    /// dispatch clones an `Rc` instead of cloning the `Vec`.
    static MIDDLEWARE_SNAPSHOT: RefCell<Option<MiddlewareChain>> =
        const { RefCell::new(None) };
    /// Task-local-ish route-loader fetch context. The router wraps
    /// each loader future so this slot is set only while that future
    /// is being polled, letting generated `#[server]` stubs inherit
    /// the right abort signal without changing every server-function
    /// signature.
    static ACTIVE_ABORT_SIGNAL: RefCell<Option<AbortSignal>> =
        const { RefCell::new(None) };
}

struct AbortSignalScope {
    previous: Option<AbortSignal>,
}

impl Drop for AbortSignalScope {
    fn drop(&mut self) {
        ACTIVE_ABORT_SIGNAL.with(|cell| {
            *cell.borrow_mut() = self.previous.take();
        });
    }
}

fn enter_abort_signal(signal: Option<AbortSignal>) -> AbortSignalScope {
    let previous =
        ACTIVE_ABORT_SIGNAL.with(|cell| std::mem::replace(&mut *cell.borrow_mut(), signal));
    AbortSignalScope { previous }
}

fn current_abort_signal() -> Option<AbortSignal> {
    ACTIVE_ABORT_SIGNAL.with(|cell| cell.borrow().clone())
}

/// Future wrapper used by the router to make loader-owned abort
/// signals visible to generated `#[server]` stubs while a specific
/// loader future is being polled.
pub(crate) struct AbortSignalFuture<F> {
    signal: Option<AbortSignal>,
    future: Pin<Box<F>>,
}

impl<F> Unpin for AbortSignalFuture<F> {}

impl<F: Future> Future for AbortSignalFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let _scope = enter_abort_signal(this.signal.clone());
        this.future.as_mut().poll(cx)
    }
}

pub(crate) fn with_abort_signal_future<F: Future>(
    signal: Option<AbortSignal>,
    future: F,
) -> AbortSignalFuture<F> {
    AbortSignalFuture {
        signal,
        future: Box::pin(future),
    }
}

/// Install a middleware. Must run before the first
/// [`crate::App::run`] or first [`call`]; calls afterwards
/// panic — by design — to keep middleware on the trusted-code
/// path.
pub fn install_middleware<M: FetchMiddleware>(middleware: M) {
    if FROZEN.with(|cell| cell.get()) {
        panic!(
            "fetch::install_middleware called after the chain was frozen. \
             Middleware install must run before the first App::run or \
             fetch::call. Move the install into your plugin's `install` \
             function so it executes before App::run."
        );
    }
    MIDDLEWARES.with(|cell| cell.borrow_mut().push(Rc::new(middleware)));
}

/// Freeze the middleware chain so subsequent
/// [`install_middleware`] calls panic. Idempotent — a second call
/// is a no-op so an explicit `App::run` followed by a stray
/// `fetch::call` doesn't double-trip the freeze.
///
/// Doc-hidden because it's a runtime/`App::run` integration seam,
/// not a stable surface plugins call themselves.
#[doc(hidden)]
pub fn freeze_middleware_chain() {
    let was_frozen = FROZEN.with(|cell| cell.replace(true));
    if !was_frozen {
        MIDDLEWARE_SNAPSHOT.with(|snap| {
            *snap.borrow_mut() = Some(Rc::new(MIDDLEWARES.with(|cell| cell.borrow().clone())));
        });
    }
}

/// Reset the middleware chain to an empty, unfrozen state. Test
/// helper only — equivalent in spirit to `crate::plugin::__reset_for_test`
/// and carries the same don't-call-from-production warning.
#[doc(hidden)]
pub fn __reset_middleware_chain_for_test() {
    MIDDLEWARES.with(|cell| cell.borrow_mut().clear());
    FROZEN.with(|cell| cell.set(false));
    MIDDLEWARE_SNAPSHOT.with(|snap| *snap.borrow_mut() = None);
}

/// Drop every installed middleware **and** freeze the chain.
/// Called from [`crate::App::run`] when plugin validation fails:
/// a plugin may have called [`install_middleware`] from inside its
/// `install` fn before the framework discovered the validation
/// error, and a refused-to-mount app must not leave that
/// privileged code path live for the next `fetch::call`. After
/// this runs, any further `install_middleware` panics — the
/// remediation path is to fix the plugin configuration and
/// restart the runtime, not silently retry installing.
pub(crate) fn clear_and_freeze() {
    MIDDLEWARES.with(|cell| cell.borrow_mut().clear());
    FROZEN.with(|cell| cell.set(true));
    MIDDLEWARE_SNAPSHOT.with(|snap| *snap.borrow_mut() = Some(Rc::new(Vec::new())));
}

fn snapshot_chain() -> MiddlewareChain {
    if let Some(snap) = MIDDLEWARE_SNAPSHOT.with(|cell| cell.borrow().clone()) {
        return snap;
    }
    // Pre-freeze callers (test-only). The real `call` path always
    // freezes first, so production never hits this branch.
    MIDDLEWARES.with(|cell| Rc::new(cell.borrow().clone()))
}

// ─── public call ────────────────────────────────────────────────────
//
// FOLLOW-UP (RFC-107): collapse `call` / `call_replay_safe` /
// `call_with_options` / `call_stream` into one builder —
// `call(url, &args).get::<R>()` / `.stream::<R>()` / `.replay_safe()`. Deferred
// from the streaming-server-fn work: these entry points are macro-generated
// (app authors never write them), so the change is internal-only and touches
// ~30 call sites across pocopine-sync / sync-query / auth-client + tests. Do it
// as its own focused PR, separate from the streaming feature.

/// Post `args` as JSON to `url` and deserialize the JSON response into
/// `Result<R>`. The server is expected to respond with a JSON encoding
/// of `serde_json::to_string(&result)` where `result: Result<R>`.
///
/// On the first call the middleware chain freezes; subsequent
/// [`install_middleware`] calls panic per the
/// [`crate::App::run`] contract.
pub async fn call<A, R>(url: &str, args: &A) -> ServerResult<R>
where
    A: Serialize,
    R: DeserializeOwned,
{
    call_with_options(url, args, FetchOptions::default()).await
}

/// Options for [`call_with_options`].
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct FetchOptions {
    pub(crate) abort_signal: Option<AbortSignal>,
    pub(crate) replay_safe: bool,
}

impl FetchOptions {
    /// Attach an explicit browser abort signal.
    pub fn abort_signal(mut self, signal: Option<AbortSignal>) -> Self {
        self.abort_signal = signal;
        self
    }

    /// Mark whether middleware may replay this call after a recoverable
    /// authentication failure.
    pub fn replay_safe(mut self, enabled: bool) -> Self {
        self.replay_safe = enabled;
        self
    }

    fn with_active_context(mut self) -> Self {
        if self.abort_signal.is_none() {
            self.abort_signal = current_abort_signal();
        }
        self
    }
}

/// Post `args` like [`call`] but with explicit request metadata.
///
/// This is primarily used by macro-generated `#[server]` stubs and
/// by integration tests. Application code should usually call the
/// generated server function directly.
pub async fn call_with_options<A, R>(url: &str, args: &A, options: FetchOptions) -> ServerResult<R>
where
    A: Serialize,
    R: DeserializeOwned,
{
    freeze_middleware_chain();
    let options = options.with_active_context();

    let observe = FetchObservation::new(url);
    // RFC-123 §5.5: `pocopine.client.server_function`, under the current
    // page view; the hook events below fire inside it.
    let call = crate::client_trace::CallSpan::open(&public_url_path(url));
    let body = match serde_json::to_string(args) {
        Ok(body) => body,
        Err(err) => {
            call.failed("serialize");
            call.span
                .in_scope(|| observe.failed("serialize", ResponseIds::default()));
            return Err(ServerError::Network(format!("serialize args: {err}")));
        }
    };

    let mut headers = vec![
        ("content-type".to_string(), "application/json".to_string()),
        (
            CLIENT_SESSION_HEADER.to_string(),
            client_session_id().to_string(),
        ),
    ];
    if let Some(traceparent) = call.traceparent() {
        headers.push(("traceparent".to_string(), traceparent));
    }
    let request = FetchRequest {
        url: url.to_string(),
        method: "POST".to_string(),
        body,
        headers,
        abort_signal: options.abort_signal,
        replay_safe: options.replay_safe,
    };

    let middlewares = snapshot_chain();
    let next = FetchNext {
        index: 0,
        middlewares,
    };
    let response = match next.run(request).instrument(call.span.clone()).await {
        Ok(response) => response,
        Err(err) => {
            let kind = server_error_kind(&err);
            call.failed(kind);
            call.span
                .in_scope(|| observe.failed(kind, ResponseIds::default()));
            return Err(err);
        }
    };

    let ids = ResponseIds::of(&response);
    call.response(response.status, ids.request_id);
    let _inside = call.span.enter();
    if !(200..300).contains(&response.status) {
        call.failed("http_status");
        observe.failed("http_status", ids);
        return Err(ServerError::Network(format!("HTTP {}", response.status)));
    }

    let outer: ServerResult<R> = match serde_json::from_str(&response.body) {
        Ok(outer) => outer,
        Err(err) => {
            call.failed("parse_response");
            observe.failed("parse_response", ids.clone());
            return Err(ServerError::Network(format!("parse response: {err}")));
        }
    };
    match &outer {
        Ok(_) => {
            call.completed();
            observe.completed(response.status, ids);
        }
        Err(err) => {
            let kind = server_error_kind(err);
            call.failed(kind);
            observe.failed(kind, ids);
        }
    }
    outer
}

/// Post `args` and mark the generated request replay-safe.
///
/// `#[server(idempotent)]` stubs use this helper. Auth middleware may
/// replay a request with this marker at most once after a successful
/// refresh. Unmarked requests must be treated as unsafe to replay.
pub async fn call_replay_safe<A, R>(url: &str, args: &A) -> ServerResult<R>
where
    A: Serialize,
    R: DeserializeOwned,
{
    call_with_options(url, args, FetchOptions::default().replay_safe(true)).await
}

// ─── streaming (RFC-107) ─────────────────────────────────────────────

/// Post `args` and consume the response as a stream of `ServerResult<R>`
/// items (RFC-107 streaming server functions).
///
/// The outer `Result` is the HTTP handshake (non-2xx / transport → `Err`);
/// each streamed item is in-band, and a mid-stream `Err` is terminal. The
/// `#[server]` macro generates this call for functions returning
/// `StreamServerResult<R>`; application code calls the generated function.
///
/// Unlike [`call`], `call_stream` does **not** run the buffering middleware
/// chain (that reads the whole body via `Response::text`); request signing /
/// auth-retry for streaming calls is future work (RFC-107 open question).
pub async fn call_stream<A, R>(url: &str, args: &A) -> ServerResult<ServerStream<R>>
where
    A: Serialize,
    R: DeserializeOwned + 'static,
{
    let body = serde_json::to_string(args)
        .map_err(|err| ServerError::Network(format!("serialize args: {err}")))?;
    stream_call(url, &body, current_abort_signal()).await
}

#[cfg(target_arch = "wasm32")]
async fn stream_call<R>(
    url: &str,
    body: &str,
    abort: Option<AbortSignal>,
) -> ServerResult<ServerStream<R>>
where
    R: DeserializeOwned + 'static,
{
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_body(&JsValue::from_str(body));
    init.set_signal(abort.as_ref());

    let call = crate::client_trace::CallSpan::open(&public_url_path(url));
    let headers =
        web_sys::Headers::new().map_err(|e| ServerError::Network(format!("headers: {e:?}")))?;
    let _ = headers.set("content-type", "application/json");
    let _ = headers.set("accept", "text/event-stream");
    let _ = headers.set(CLIENT_SESSION_HEADER, client_session_id());
    if let Some(traceparent) = call.traceparent() {
        let _ = headers.set("traceparent", &traceparent);
    }
    init.set_headers(&headers);

    let req = Request::new_with_str_and_init(url, &init)
        .map_err(|e| ServerError::Network(format!("build request: {e:?}")))?;
    let win =
        web_sys::window().ok_or_else(|| ServerError::Network("no window available".to_string()))?;
    let resp_js = match JsFuture::from(win.fetch_with_request(&req))
        .instrument(call.span.clone())
        .await
    {
        Ok(resp) => resp,
        Err(e) => {
            call.failed("fetch");
            return Err(ServerError::Network(format!("fetch failed: {e:?}")));
        }
    };
    let resp: Response = resp_js
        .dyn_into()
        .map_err(|_| ServerError::Network("fetch returned non-Response".into()))?;

    let status = resp.status();
    let request_id = response_header(&resp, "x-request-id").and_then(|v| v.trim().parse().ok());
    call.response(status, request_id);
    if !(200..300).contains(&status) {
        call.failed("http_status");
        return Err(ServerError::Network(format!("HTTP {status}")));
    }
    // The handshake is the call; the stream's frames are consumed later.
    call.completed();

    let body_stream = resp
        .body()
        .ok_or_else(|| ServerError::Network("response had no body".into()))?;
    let reader = body_stream
        .get_reader()
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| ServerError::Network("could not read the response stream".into()))?;

    Ok(Box::pin(SseStream::<R>::new(reader)))
}

/// Host build compiles this stub but never calls it — the generated client
/// stub that calls `call_stream` is `wasm32`-gated.
#[cfg(not(target_arch = "wasm32"))]
async fn stream_call<R>(
    _url: &str,
    _body: &str,
    _abort: Option<AbortSignal>,
) -> ServerResult<ServerStream<R>>
where
    R: DeserializeOwned + 'static,
{
    Err(ServerError::Network(
        "streaming server-function clients run on wasm".into(),
    ))
}

/// A `Stream` over an SSE response body, decoding `data:` frames into
/// `ServerResult<R>` items. Hand-rolled (no `futures-util`) to keep
/// `pocopine-core`'s wasm bundle lean.
#[cfg(target_arch = "wasm32")]
struct SseStream<R> {
    reader: web_sys::ReadableStreamDefaultReader,
    decoder: crate::sse::SseDecoder,
    pending: std::collections::VecDeque<ServerResult<R>>,
    in_flight: Option<JsFuture>,
    done: bool,
}

// Every field is `Unpin` (web-sys handles, `SseDecoder`, `VecDeque`, and
// `JsFuture` are all `Unpin`); the generic param doesn't change that, so assert
// it unconditionally to allow `Pin::get_mut` in `poll_next`.
#[cfg(target_arch = "wasm32")]
impl<R> Unpin for SseStream<R> {}

#[cfg(target_arch = "wasm32")]
impl<R: DeserializeOwned + 'static> SseStream<R> {
    fn new(reader: web_sys::ReadableStreamDefaultReader) -> Self {
        Self {
            reader,
            decoder: crate::sse::SseDecoder::new(),
            pending: std::collections::VecDeque::new(),
            in_flight: None,
            done: false,
        }
    }

    fn ingest(&mut self, payload: &str) {
        match crate::sse::decode_payload::<R>(payload) {
            crate::sse::Decoded::Item(item) => self.pending.push_back(item),
            crate::sse::Decoded::Done => self.done = true,
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl<R: DeserializeOwned + 'static> futures_core::Stream for SseStream<R> {
    type Item = ServerResult<R>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(item) = this.pending.pop_front() {
                return Poll::Ready(Some(item));
            }
            if this.done {
                return Poll::Ready(None);
            }
            if this.in_flight.is_none() {
                this.in_flight = Some(JsFuture::from(this.reader.read()));
            }
            let fut = this.in_flight.as_mut().expect("in_flight just set");
            match Pin::new(fut).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(read_result) => {
                    this.in_flight = None;
                    match read_result {
                        Err(e) => {
                            this.done = true;
                            return Poll::Ready(Some(Err(ServerError::Network(format!(
                                "stream read failed: {e:?}"
                            )))));
                        }
                        Ok(obj) => {
                            let is_done = js_sys::Reflect::get(&obj, &JsValue::from_str("done"))
                                .ok()
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let value = js_sys::Reflect::get(&obj, &JsValue::from_str("value"))
                                .unwrap_or(JsValue::UNDEFINED);
                            if let Ok(chunk) = value.dyn_into::<js_sys::Uint8Array>() {
                                for payload in this.decoder.push(&chunk.to_vec()) {
                                    this.ingest(&payload);
                                }
                            }
                            if is_done {
                                if let Some(payload) = this.decoder.flush() {
                                    this.ingest(&payload);
                                }
                                this.done = true;
                            }
                        }
                    }
                }
            }
        }
    }
}

// ─── transport ──────────────────────────────────────────────────────

async fn perform_fetch(request: FetchRequest) -> Result<FetchResponse, ServerError> {
    let init = RequestInit::new();
    init.set_method(&request.method);
    init.set_body(&JsValue::from_str(&request.body));
    init.set_signal(request.abort_signal.as_ref());

    let headers =
        web_sys::Headers::new().map_err(|e| ServerError::Network(format!("headers: {e:?}")))?;
    for (name, value) in &request.headers {
        let _ = headers.set(name, value);
    }
    init.set_headers(&headers);

    let req = Request::new_with_str_and_init(&request.url, &init)
        .map_err(|e| ServerError::Network(format!("build request: {e:?}")))?;

    let win =
        web_sys::window().ok_or_else(|| ServerError::Network("no window available".to_string()))?;
    let resp_js = JsFuture::from(win.fetch_with_request(&req))
        .await
        .map_err(|e| ServerError::Network(format!("fetch failed: {e:?}")))?;

    let resp: Response = resp_js
        .dyn_into()
        .map_err(|_| ServerError::Network("fetch returned non-Response".into()))?;

    let status = resp.status();
    let text_js = JsFuture::from(
        resp.text()
            .map_err(|e| ServerError::Network(format!("read body: {e:?}")))?,
    )
    .await
    .map_err(|e| ServerError::Network(format!("read body: {e:?}")))?;

    let body = text_js
        .as_string()
        .ok_or_else(|| ServerError::Network("body was not a string".into()))?;

    // RFC-123 §5.5: the server's ids, so the client side of the call can
    // name the exact server span.
    let request_id = response_header(&resp, "x-request-id").and_then(|v| v.trim().parse().ok());
    let trace_parent = response_header(&resp, "traceparent");

    Ok(FetchResponse {
        status,
        body,
        request_id,
        trace_parent,
    })
}

fn response_header(resp: &Response, name: &str) -> Option<String> {
    resp.headers().get(name).ok().flatten()
}

struct FetchObservation {
    route: Option<String>,
    start_ms: Option<f64>,
}

impl FetchObservation {
    fn new(url: &str) -> Self {
        if !crate::plugin::has_server_function_client_hooks() {
            return Self {
                route: None,
                start_ms: None,
            };
        }
        let route = public_url_path(url);
        crate::plugin::emit(crate::plugin::ServerFunctionClientStarted {
            route: route.clone(),
        });
        Self {
            route: Some(route),
            start_ms: Some(js_sys::Date::now()),
        }
    }

    fn completed(&self, status_code: u16, ids: ResponseIds) {
        let Some(route) = self.route.as_ref() else {
            return;
        };
        crate::plugin::emit(crate::plugin::ServerFunctionClientCompleted {
            route: route.clone(),
            duration_ms: self.elapsed_ms(),
            status_code,
            request_id: ids.request_id,
            trace_id: ids.trace_id,
        });
    }

    fn failed(&self, error_kind: &'static str, ids: ResponseIds) {
        let Some(route) = self.route.as_ref() else {
            return;
        };
        crate::plugin::emit(crate::plugin::ServerFunctionClientFailed {
            route: route.clone(),
            duration_ms: self.elapsed_ms(),
            error_kind,
            request_id: ids.request_id,
            trace_id: ids.trace_id,
        });
    }

    fn elapsed_ms(&self) -> f64 {
        let Some(start_ms) = self.start_ms else {
            return 0.0;
        };
        let elapsed = js_sys::Date::now() - start_ms;
        if elapsed.is_finite() && elapsed >= 0.0 {
            elapsed
        } else {
            0.0
        }
    }
}

fn server_error_kind(err: &ServerError) -> &'static str {
    match err {
        ServerError::App(_) => "app",
        ServerError::Unauthorized(_) => "unauthorized",
        ServerError::Forbidden(_) => "forbidden",
        ServerError::BadRequest(_) => "bad_request",
        ServerError::Network(_) => "network",
    }
}

fn public_url_path(url: &str) -> String {
    let without_query = url.split_once('?').map(|(path, _)| path).unwrap_or(url);
    without_query
        .split_once('#')
        .map(|(path, _)| path)
        .unwrap_or(without_query)
        .to_owned()
}

#[cfg(test)]
mod tests {
    #[test]
    fn traceparent_trace_id_is_parsed_strictly() {
        assert_eq!(
            super::trace_id_from_traceparent(
                "00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01"
            )
            .as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        for bad in [
            "",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7",
            "00-4bf92f3577b34da6a3ce929d0e0e47-00f067aa0ba902b7-01",
            "00-00000000000000000000000000000000-00f067aa0ba902b7-01",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01-extra",
            "zz-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        ] {
            assert_eq!(super::trace_id_from_traceparent(bad), None, "{bad}");
        }
    }

    #[test]
    fn synthesized_responses_carry_no_server_ids() {
        let response = super::FetchResponse::new(200, "{}");
        assert_eq!(response.request_id, None);
        assert_eq!(response.trace_id(), None);
    }

    #[test]
    fn client_session_id_is_stable_and_well_formed() {
        let first = super::client_session_id();
        let second = super::client_session_id();
        assert_eq!(first, second, "minted once per boot");
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }

    use super::*;

    fn reset() {
        __reset_middleware_chain_for_test();
    }

    #[test]
    fn install_records_middleware_in_order() {
        reset();
        install_middleware(|req: FetchRequest, next: FetchNext| async move { next.run(req).await });
        install_middleware(|req: FetchRequest, next: FetchNext| async move { next.run(req).await });
        let snapshot = snapshot_chain();
        assert_eq!(snapshot.len(), 2);
    }

    #[test]
    #[should_panic(expected = "called after the chain was frozen")]
    fn install_panics_after_freeze() {
        reset();
        freeze_middleware_chain();
        install_middleware(|req: FetchRequest, next: FetchNext| async move { next.run(req).await });
    }

    #[test]
    fn freeze_is_idempotent() {
        reset();
        freeze_middleware_chain();
        freeze_middleware_chain();
        // Second freeze is a no-op; FROZEN stays true.
        assert!(FROZEN.with(|cell| cell.get()));
    }

    #[test]
    fn fetch_request_set_header_replaces_case_insensitively() {
        let mut req = FetchRequest {
            url: "/x".into(),
            method: "POST".into(),
            body: "".into(),
            headers: vec![("Content-Type".into(), "text/plain".into())],
            abort_signal: None,
            replay_safe: false,
        };
        req.set_header("content-type", "application/json");
        assert_eq!(req.headers.len(), 1);
        assert_eq!(req.headers[0].1, "application/json");
        // Case of the existing key is preserved (we replace value, not name).
        assert_eq!(req.headers[0].0, "Content-Type");
    }

    #[test]
    fn fetch_request_set_header_appends_when_missing() {
        let mut req = FetchRequest {
            url: "/x".into(),
            method: "POST".into(),
            body: "".into(),
            headers: vec![],
            abort_signal: None,
            replay_safe: false,
        };
        req.set_header("authorization", "Bearer t");
        assert_eq!(
            req.headers,
            vec![("authorization".into(), "Bearer t".into())]
        );
    }

    #[test]
    fn clear_and_freeze_drops_middlewares_and_locks_chain() {
        // RFC-078 §5.10.3: refused-to-mount apps must not leave
        // privileged middleware live for the next `fetch::call`.
        // `App::run`'s validation-failure branch calls this helper
        // after a plugin had a chance to install middleware in its
        // `install` fn but before plugin validation flagged a
        // missing service.
        reset();
        install_middleware(|req: FetchRequest, next: FetchNext| async move { next.run(req).await });
        assert_eq!(snapshot_chain().len(), 1);

        clear_and_freeze();
        assert_eq!(snapshot_chain().len(), 0, "middlewares should be dropped");
        assert!(FROZEN.with(|cell| cell.get()), "chain must stay frozen");

        // Reset for any subsequent test in this binary.
        reset();
    }

    #[test]
    fn fetch_options_default_to_not_replay_safe() {
        let options = FetchOptions::default();
        assert!(!options.replay_safe);
        assert!(options.abort_signal.is_none());
    }

    #[test]
    fn strips_query_and_fragment_from_observed_urls() {
        assert_eq!(public_url_path("/api/search?q=secret#frag"), "/api/search");
        assert_eq!(public_url_path("/api/save"), "/api/save");
    }
}
