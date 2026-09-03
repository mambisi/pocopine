//! RFC-123 §5.5 — the browser end of the trunk.
//!
//! Three spans, opened at the seams that already emit the paired hooks:
//! `pocopine.client.boot` around [`crate::App::run`], `pocopine.client.navigation`
//! per page view (the root of the trace every server-function call of that
//! view joins), and `pocopine.client.server_function` around each call. The
//! browser mints its own W3C ids and records them as fields
//! (`pocopine.trace_id` / `pocopine.span_id` / `pocopine.parent_span_id`); the
//! fetch layer sends `traceparent` from them — only when the relay is on,
//! because a client span that never reaches the backend would make every
//! server trace the child of a span nobody received — and the relay ships
//! closed spans to the server, which re-emits them under those same ids.
//!
//! Same rules as the server side: target `pocopine.trace`, names by
//! constant, structural fields only, no `#[instrument]`. Ids are
//! correlation, not secrets: entropy is hashed through `pocopine-crypto`
//! only so the shape is uniform.

use std::cell::{Cell, RefCell};

use pocopine_observe::{TRACE_TARGET, fields, spans};
use tracing::Span;
use tracing::field::Empty;

/// The trace and span id of the current page view, for callers that need
/// to parent something to it by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewContext {
    pub trace_id: String,
    pub span_id: String,
}

struct View {
    span: Span,
    context: ViewContext,
}

thread_local! {
    /// The current page view. Replacing it drops — closes — the previous one.
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    /// The boot span, kept as the fallback parent for calls made before the
    /// first navigation — and every call in an app without routes.
    static BOOT: RefCell<Option<View>> = const { RefCell::new(None) };
    /// Whether the relay is on, and therefore whether `traceparent` is sent.
    static RELAY: Cell<bool> = const { Cell::new(false) };
}

/// Turn the `traceparent` request header on or off. The frontend
/// observability plugin sets this together with the relay; app code
/// normally never calls it.
pub fn set_trace_relay_enabled(enabled: bool) {
    RELAY.with(|cell| cell.set(enabled));
}

pub fn trace_relay_enabled() -> bool {
    RELAY.with(Cell::get)
}

/// The ids of the current page view, if a navigation has happened.
pub fn current_view() -> Option<ViewContext> {
    VIEW.with(|view| view.borrow().as_ref().map(|v| v.context.clone()))
}

/// The ids of the boot span, once the app has started booting.
pub fn boot_context() -> Option<ViewContext> {
    BOOT.with(|boot| boot.borrow().as_ref().map(|v| v.context.clone()))
}

/// Close the current page view now — the relay calls this on page hide so
/// the view span reaches the backend with the calls it parented, instead of
/// staying open in a tab that is going away.
pub fn close_view() {
    VIEW.with(|view| view.borrow_mut().take());
}

/// Run `f` inside the current page view's span, or plainly if there is none.
pub(crate) fn in_view<T>(f: impl FnOnce() -> T) -> T {
    let span = VIEW.with(|view| view.borrow().as_ref().map(|v| v.span.clone()));
    match span {
        Some(span) => span.in_scope(f),
        None => f(),
    }
}

/// A fresh 32-hex W3C trace id.
pub fn mint_trace_id() -> String {
    mint_hex(32)
}

/// A fresh 16-hex W3C span id.
pub fn mint_span_id() -> String {
    mint_hex(16)
}

fn mint_hex(len: usize) -> String {
    let mut seed: Vec<u8> = Vec::with_capacity(40);
    #[cfg(target_arch = "wasm32")]
    {
        for _ in 0..3 {
            seed.extend_from_slice(&js_sys::Math::random().to_bits().to_le_bytes());
        }
        seed.extend_from_slice(&js_sys::Date::now().to_bits().to_le_bytes());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        seed.extend_from_slice(&now.to_le_bytes());
        seed.extend_from_slice(&COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        seed.extend_from_slice(&std::process::id().to_le_bytes());
    }
    let mut hex = pocopine_crypto::blake3_hex(&seed);
    hex.truncate(len);
    // A W3C id must not be all zeros; a hash of fresh entropy never is in
    // practice, but the contract is cheap to keep.
    if hex.bytes().all(|b| b == b'0') {
        hex.replace_range(..1, "1");
    }
    hex
}

/// `pocopine.client.boot` — a root with its own trace id, remembered as
/// the fallback parent for calls made outside any page view.
pub(crate) fn boot_span() -> Span {
    let context = ViewContext {
        trace_id: mint_trace_id(),
        span_id: mint_span_id(),
    };
    let span = tracing::info_span!(
        target: TRACE_TARGET,
        parent: None,
        spans::CLIENT_BOOT,
        otel.kind = "internal",
        session.id = crate::fetch::client_session_id(),
        pocopine.trace_id = context.trace_id.as_str(),
        pocopine.span_id = context.span_id.as_str(),
        otel.status_code = Empty,
        error.type = Empty,
    );
    BOOT.with(|boot| {
        *boot.borrow_mut() = Some(View {
            span: span.clone(),
            context,
        })
    });
    span
}

pub(crate) fn close_ok(span: &Span) {
    span.record(fields::OTEL_STATUS_CODE, "OK");
}

pub(crate) fn close_err(span: &Span, reason: &str) {
    span.record(fields::OTEL_STATUS_CODE, "ERROR");
    span.record(fields::ERROR_TYPE, reason);
}

/// A navigation began: open `pocopine.client.navigation` with a fresh trace
/// id and make it the current view. The previous view's span closes here.
/// Returns a handle so the caller can enter the span for the mount.
pub(crate) fn navigation_started(
    path: &str,
    route_pattern: Option<&str>,
    component: Option<&str>,
) -> Span {
    let context = ViewContext {
        trace_id: mint_trace_id(),
        span_id: mint_span_id(),
    };
    let span = tracing::info_span!(
        target: TRACE_TARGET,
        parent: None,
        spans::CLIENT_NAVIGATION,
        otel.kind = "internal",
        url.path = path,
        http.route = Empty,
        pocopine.component = Empty,
        session.id = crate::fetch::client_session_id(),
        pocopine.trace_id = context.trace_id.as_str(),
        pocopine.span_id = context.span_id.as_str(),
        otel.status_code = Empty,
        error.type = Empty,
    );
    if let Some(route) = route_pattern {
        span.record(fields::HTTP_ROUTE, route);
    }
    if let Some(component) = component {
        span.record(fields::COMPONENT, component);
    }
    let handle = span.clone();
    VIEW.with(|view| *view.borrow_mut() = Some(View { span, context }));
    handle
}

/// The current navigation finished mounting.
pub(crate) fn navigation_completed() {
    VIEW.with(|view| {
        if let Some(view) = view.borrow().as_ref() {
            close_ok(&view.span);
        }
    });
}

/// The current navigation failed for a stable `reason`.
pub(crate) fn navigation_failed(reason: &str) {
    VIEW.with(|view| {
        if let Some(view) = view.borrow().as_ref() {
            close_err(&view.span, reason);
        }
    });
}

/// `pocopine.client.server_function` for one call, plus the ids the fetch
/// layer needs for `traceparent`.
pub(crate) struct CallSpan {
    pub(crate) span: Span,
    trace_id: String,
    span_id: String,
}

impl CallSpan {
    pub(crate) fn open(route: &str) -> Self {
        let span_id = mint_span_id();
        // Parent: the page view, else the boot span, else a root.
        let view = VIEW
            .with(|view| {
                view.borrow()
                    .as_ref()
                    .map(|v| (v.span.clone(), v.context.clone()))
            })
            .or_else(|| {
                BOOT.with(|boot| {
                    boot.borrow()
                        .as_ref()
                        .map(|v| (v.span.clone(), v.context.clone()))
                })
            });
        let (trace_id, parent_span_id) = match &view {
            Some((_, context)) => (context.trace_id.clone(), Some(context.span_id.clone())),
            None => (mint_trace_id(), None),
        };
        let session = crate::fetch::client_session_id();
        let span = match &view {
            Some((parent, _)) => tracing::info_span!(
                target: TRACE_TARGET,
                parent: parent,
                spans::CLIENT_SERVER_FUNCTION,
                otel.kind = "client",
                http.request.method = "POST",
                http.route = route,
                session.id = session,
                pocopine.trace_id = trace_id.as_str(),
                pocopine.span_id = span_id.as_str(),
                pocopine.parent_span_id = Empty,
                http.response.status_code = Empty,
                pocopine.request_id = Empty,
                otel.status_code = Empty,
                error.type = Empty,
            ),
            None => tracing::info_span!(
                target: TRACE_TARGET,
                parent: None,
                spans::CLIENT_SERVER_FUNCTION,
                otel.kind = "client",
                http.request.method = "POST",
                http.route = route,
                session.id = session,
                pocopine.trace_id = trace_id.as_str(),
                pocopine.span_id = span_id.as_str(),
                pocopine.parent_span_id = Empty,
                http.response.status_code = Empty,
                pocopine.request_id = Empty,
                otel.status_code = Empty,
                error.type = Empty,
            ),
        };
        if let Some(parent) = parent_span_id {
            span.record(fields::PARENT_SPAN_ID, parent.as_str());
        }
        Self {
            span,
            trace_id,
            span_id,
        }
    }

    /// The W3C `traceparent` for this call — sent only when the relay is on.
    pub(crate) fn traceparent(&self) -> Option<String> {
        trace_relay_enabled().then(|| format!("00-{}-{}-01", self.trace_id, self.span_id))
    }

    pub(crate) fn response(&self, status: u16, request_id: Option<u64>) {
        self.span.record(fields::HTTP_RESPONSE_STATUS_CODE, status);
        if let Some(request_id) = request_id {
            self.span.record(fields::REQUEST_ID, request_id);
        }
    }

    pub(crate) fn completed(&self) {
        close_ok(&self.span);
    }

    pub(crate) fn failed(&self, kind: &str) {
        close_err(&self.span, kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_w3c_shapes_and_do_not_repeat() {
        let a = mint_trace_id();
        let b = mint_trace_id();
        assert_eq!(a.len(), 32);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(a, b);
        let s = mint_span_id();
        assert_eq!(s.len(), 16);
        assert!(s.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn traceparent_is_sent_only_with_the_relay() {
        set_trace_relay_enabled(false);
        let call = CallSpan::open("/api/x");
        assert_eq!(call.traceparent(), None);
        set_trace_relay_enabled(true);
        let header = CallSpan::open("/api/x").traceparent().expect("header");
        assert!(header.starts_with("00-") && header.ends_with("-01"));
        assert_eq!(header.len(), 55);
        set_trace_relay_enabled(false);
    }

    #[test]
    fn a_call_joins_the_current_view_or_the_boot() {
        let _boot = boot_span();
        let boot = boot_context().expect("boot context");
        let before_navigation = CallSpan::open("/api/warmup");
        assert_eq!(
            before_navigation.trace_id, boot.trace_id,
            "no view yet: the boot parents it"
        );

        let _nav = navigation_started("/threads/1", Some("/threads/:id"), Some("Thread"));
        let view = current_view().expect("view");
        assert_ne!(view.trace_id, boot.trace_id);
        let call = CallSpan::open("/api/summarize");
        assert_eq!(call.trace_id, view.trace_id);

        let _nav2 = navigation_started("/other", None, None);
        assert_ne!(
            current_view().unwrap().trace_id,
            view.trace_id,
            "new view, new trace"
        );
        close_view();
        assert_eq!(current_view(), None);
        let after_close = CallSpan::open("/api/late");
        assert_eq!(
            after_close.trace_id, boot.trace_id,
            "back to the boot as parent"
        );
    }
}
