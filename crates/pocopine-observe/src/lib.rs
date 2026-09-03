//! Stable observability event contract shared by pocopine logging,
//! tracing, telemetry, and analytics integrations.
//!
//! Core/runtime crates should emit `tracing` spans and events, or
//! construct an [`ObservedEvent`] when they need a stable public schema.
//! Exporters live in `pocopine-logging` and `pocopine-analytics`.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use tracing::Level;

pub const LOG_TARGET: &str = "pocopine.log";
pub const TRACE_TARGET: &str = "pocopine.trace";
pub const METRIC_TARGET: &str = "pocopine.metric";
pub const ANALYTICS_TARGET: &str = "pocopine.analytics";

/// The span space (RFC-123 §2): the closed set of `tracing` span names
/// framework crates may open. Every span carries `target: TRACE_TARGET`
/// and one of these names; a crate never spells a span name inline.
///
/// Adding a span means adding a constant here (and a row to RFC-123 §2.2).
/// Framework crates never use `#[tracing::instrument]` — it names the span
/// after the function, targets the module path, and records every argument
/// by `Debug` (§2.6).
pub mod spans {
    /// `Server::serve` from plugin validation through bind. Root.
    pub const SERVER_BOOT: &str = "pocopine.server.boot";
    /// One HTTP request through the request layer (`otel.kind = server`).
    /// Root, or the child of an accepted incoming `traceparent`.
    pub const HTTP_REQUEST: &str = "pocopine.http.request";
    /// One `#[server]` function invocation. Child of [`HTTP_REQUEST`].
    pub const SERVER_FUNCTION: &str = "pocopine.server_function";
    /// One job attempt in a worker (`otel.kind = consumer`). Root.
    pub const JOB_RUN: &str = "pocopine.job.run";
    /// One agenkit flow invocation.
    pub const AI_RUN: &str = "pocopine.ai.run";
    /// One conversational-runtime turn.
    pub const AI_TURN: &str = "pocopine.ai.turn";
    /// One agenkit step: custom, agent, parallel group or branch, reducer,
    /// retrieval. `pocopine.ai.step_kind` says which.
    pub const AI_STEP: &str = "pocopine.ai.step";
    /// One model call (`otel.kind = client`), `gen_ai.*` fields.
    pub const AI_MODEL: &str = "pocopine.ai.model";
    /// One tool execution inside an agent loop.
    pub const AI_TOOL: &str = "pocopine.ai.tool";
    /// One WebSocket session on the realtime gateway (`otel.kind = server`).
    /// Child of [`HTTP_REQUEST`]; lives for the socket (RFC-123 Phase 4).
    pub const REALTIME_SESSION: &str = "pocopine.realtime.session";
    /// One inbound frame handled, or one outbound frame delivered, on a
    /// realtime session. Child of [`REALTIME_SESSION`].
    pub const REALTIME_MESSAGE: &str = "pocopine.realtime.message";
    /// One live (SSE) event produced for a subscriber. Child of the
    /// [`HTTP_REQUEST`] whose body carries the stream.
    pub const LIVE_EVENT: &str = "pocopine.live.event";
    /// One fan-out update folded into a collab document by the per-topic
    /// apply loop. Root: the loop belongs to no request.
    pub const COLLAB_APPLY: &str = "pocopine.collab.apply";
    /// One collab checkpoint + fan-out trim. Root.
    pub const COLLAB_CHECKPOINT: &str = "pocopine.collab.checkpoint";
    /// The browser app boot (wasm only). Root (RFC-123 §5.5).
    pub const CLIENT_BOOT: &str = "pocopine.client.boot";
    /// One page view in the browser (wasm only): root of the trace every
    /// server-function call of that view joins.
    pub const CLIENT_NAVIGATION: &str = "pocopine.client.navigation";
    /// One server-function call from the browser (`otel.kind = client`).
    /// Child of [`CLIENT_NAVIGATION`], or a root before the first navigation.
    pub const CLIENT_SERVER_FUNCTION: &str = "pocopine.client.server_function";

    /// Every span name, for exhaustive checks.
    pub const ALL: &[&str] = &[
        SERVER_BOOT,
        HTTP_REQUEST,
        SERVER_FUNCTION,
        JOB_RUN,
        AI_RUN,
        AI_TURN,
        AI_STEP,
        AI_MODEL,
        AI_TOOL,
        REALTIME_SESSION,
        REALTIME_MESSAGE,
        LIVE_EVENT,
        COLLAB_APPLY,
        COLLAB_CHECKPOINT,
        CLIENT_BOOT,
        CLIENT_NAVIGATION,
        CLIENT_SERVER_FUNCTION,
    ];
}

/// Span field names (RFC-123 §2.3): OpenTelemetry semantic-convention names
/// where one exists, `pocopine.`-prefixed otherwise.
///
/// `tracing` macros take field names as bare identifiers, so a span
/// *declares* its fields by spelling these inline; the constants exist for
/// [`tracing::Span::record`] calls and for tests, and the inline spellings
/// must match them.
pub mod fields {
    /// `tracing-opentelemetry` control field: `server` / `client` /
    /// `internal` / `consumer`.
    pub const OTEL_KIND: &str = "otel.kind";
    /// `tracing-opentelemetry` control field: overrides the exported span
    /// name (HTTP spans use `{method} {route}` per semconv).
    pub const OTEL_NAME: &str = "otel.name";
    /// `tracing-opentelemetry` control field: `OK` / `ERROR`, recorded at close.
    pub const OTEL_STATUS_CODE: &str = "otel.status_code";
    /// Stable error classification, never a message. Recorded at close.
    pub const ERROR_TYPE: &str = "error.type";

    pub const HTTP_REQUEST_METHOD: &str = "http.request.method";
    pub const HTTP_ROUTE: &str = "http.route";
    pub const HTTP_RESPONSE_STATUS_CODE: &str = "http.response.status_code";
    pub const URL_PATH: &str = "url.path";
    /// Per-page-load client session id, from the `x-pocopine-session`
    /// request header (RFC-123 §5.4 / Phase 3).
    pub const SESSION_ID: &str = "session.id";

    /// The process-local `RequestId` the request layer stamps.
    pub const REQUEST_ID: &str = "pocopine.request_id";
    pub const FUNCTION: &str = "pocopine.function";
    pub const FUNCTION_PATH: &str = "pocopine.function_path";

    pub const JOB_NAME: &str = "pocopine.job.name";
    pub const JOB_ID: &str = "pocopine.job.id";
    pub const JOB_QUEUE: &str = "pocopine.job.queue";
    pub const JOB_ATTEMPT: &str = "pocopine.job.attempt";
    pub const JOB_MAX_ATTEMPTS: &str = "pocopine.job.max_attempts";
    pub const JOB_BACKEND: &str = "pocopine.job.backend";

    pub const AI_FLOW: &str = "pocopine.ai.flow";
    pub const AI_AGENT: &str = "pocopine.ai.agent";
    pub const AI_RUN_ID: &str = "pocopine.ai.run_id";
    pub const AI_TRACE_ID: &str = "pocopine.ai.trace_id";
    /// The same `StepId` the agenkit `TraceEvent` stream carries — the join
    /// key between the span tree and the metering stream.
    pub const AI_STEP_ID: &str = "pocopine.ai.step_id";
    pub const AI_STEP_KIND: &str = "pocopine.ai.step_kind";
    pub const AI_STEP_NAME: &str = "pocopine.ai.step_name";
    pub const AI_PARALLEL_GROUP_ID: &str = "pocopine.ai.parallel_group_id";

    pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
    pub const GEN_AI_PROVIDER_NAME: &str = "gen_ai.provider.name";
    pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
    pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
    pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
    pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";

    /// The gateway-minted id of one WebSocket session (not the client's
    /// page-load `session.id`).
    pub const REALTIME_SESSION_ID: &str = "pocopine.realtime.session_id";
    pub const REALTIME_TOPIC_REF: &str = "pocopine.realtime.topic_ref";
    /// `control` / `subscribe` / `unsubscribe` / `data`.
    pub const MESSAGE_KIND: &str = "pocopine.message.kind";
    /// `in` (received from the peer) or `out` (delivered to the peer).
    pub const MESSAGE_DIRECTION: &str = "pocopine.message.direction";
    pub const MESSAGE_BYTES: &str = "pocopine.message.bytes";
    pub const MESSAGE_SEQ: &str = "pocopine.message.seq";
    pub const LIVE_KIND: &str = "pocopine.live.kind";
    pub const LIVE_CURSOR: &str = "pocopine.live.cursor";
    pub const COLLAB_TOPIC: &str = "pocopine.collab.topic";
    pub const COLLAB_SEQ: &str = "pocopine.collab.seq";
    /// The W3C `traceparent` a job was enqueued under, when the enqueuer
    /// had one (RFC-123 Phase 4).
    pub const JOB_ENQUEUE_TRACEPARENT: &str = "pocopine.job.enqueue_traceparent";

    /// Browser spans carry their own W3C ids as fields (RFC-123 §5.5): the
    /// client mints them, sends `traceparent` from them, and the relay ships
    /// them to the backend verbatim.
    pub const TRACE_ID: &str = "pocopine.trace_id";
    pub const SPAN_ID: &str = "pocopine.span_id";
    pub const PARENT_SPAN_ID: &str = "pocopine.parent_span_id";
    /// The route component a browser navigation mounted.
    pub const COMPONENT: &str = "pocopine.component";
}

/// RFC-123 §5.5 — the relay contract between the browser and the server:
/// one closed client span as the browser ships it, and the validation the
/// server applies before it does anything with it. Shared so both sides
/// and their tests agree on one definition.
pub mod client_relay {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};

    use super::{fields, spans};

    /// The route the server observability plugin installs on opt-in.
    pub const PATH: &str = "/_pocopine/trace";
    /// Most records one post may carry.
    pub const MAX_BATCH: usize = 64;
    /// Largest accepted post body, in bytes.
    pub const MAX_BODY_BYTES: usize = 32 * 1024;
    /// Longest accepted field value.
    pub const MAX_VALUE_LEN: usize = 256;
    /// How far a record's timestamps may sit from the server clock, in ms.
    pub const MAX_CLOCK_SKEW_MS: f64 = 5.0 * 60.0 * 1000.0;
    /// Posts per minute per `session.id` before the server answers 429.
    pub const MAX_POSTS_PER_MINUTE: u32 = 10;

    /// One closed browser span. Every value is a string: the client's
    /// field visitor renders them and the server converts the few it
    /// knows are numeric.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    pub struct ClientSpanRecord {
        pub name: String,
        pub trace_id: String,
        pub span_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub parent_span_id: Option<String>,
        pub start_unix_ms: f64,
        pub end_unix_ms: f64,
        #[serde(default)]
        pub fields: BTreeMap<String, String>,
    }

    impl ClientSpanRecord {
        /// The `session.id` the record carries, if any.
        pub fn session_id(&self) -> Option<&str> {
            self.fields.get(fields::SESSION_ID).map(String::as_str)
        }

        pub fn duration_ms(&self) -> f64 {
            (self.end_unix_ms - self.start_unix_ms).max(0.0)
        }
    }

    /// Why a record was refused. Stable names, never the offending value.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum RelayError {
        UnknownSpanName,
        MalformedTraceId,
        MalformedSpanId,
        MalformedParentSpanId,
        TimestampOutOfRange,
        FieldNotAllowed,
        ValueTooLong,
        QueryInPath,
    }

    impl RelayError {
        pub fn as_str(self) -> &'static str {
            match self {
                Self::UnknownSpanName => "unknown_span_name",
                Self::MalformedTraceId => "malformed_trace_id",
                Self::MalformedSpanId => "malformed_span_id",
                Self::MalformedParentSpanId => "malformed_parent_span_id",
                Self::TimestampOutOfRange => "timestamp_out_of_range",
                Self::FieldNotAllowed => "field_not_allowed",
                Self::ValueTooLong => "value_too_long",
                Self::QueryInPath => "query_in_path",
            }
        }
    }

    /// The `pocopine.client.*` names the relay accepts.
    pub const NAMES: &[&str] = &[
        spans::CLIENT_BOOT,
        spans::CLIENT_NAVIGATION,
        spans::CLIENT_SERVER_FUNCTION,
    ];

    const COMMON_FIELDS: &[&str] = &[
        fields::OTEL_KIND,
        fields::OTEL_STATUS_CODE,
        fields::ERROR_TYPE,
        fields::SESSION_ID,
        fields::TRACE_ID,
        fields::SPAN_ID,
        fields::PARENT_SPAN_ID,
    ];

    /// The fields each span may carry (RFC-123 §2.3), beyond the common
    /// ones. Anything else is refused: a relayed record must never carry
    /// free text.
    pub fn allowed_fields(name: &str) -> Option<&'static [&'static str]> {
        match name {
            spans::CLIENT_BOOT => Some(&[]),
            spans::CLIENT_NAVIGATION => {
                Some(&[fields::URL_PATH, fields::HTTP_ROUTE, fields::COMPONENT])
            }
            spans::CLIENT_SERVER_FUNCTION => Some(&[
                fields::HTTP_REQUEST_METHOD,
                fields::HTTP_ROUTE,
                fields::HTTP_RESPONSE_STATUS_CODE,
                fields::REQUEST_ID,
            ]),
            _ => None,
        }
    }

    fn hex_id(value: &str, len: usize) -> bool {
        value.len() == len
            && value.bytes().all(|b| b.is_ascii_hexdigit())
            && !value.bytes().all(|b| b == b'0')
    }

    /// Validate one record against the server clock (`now_unix_ms`).
    pub fn validate(record: &ClientSpanRecord, now_unix_ms: f64) -> Result<(), RelayError> {
        let allowed = allowed_fields(&record.name).ok_or(RelayError::UnknownSpanName)?;
        if !hex_id(&record.trace_id, 32) {
            return Err(RelayError::MalformedTraceId);
        }
        if !hex_id(&record.span_id, 16) {
            return Err(RelayError::MalformedSpanId);
        }
        if let Some(parent) = &record.parent_span_id
            && !hex_id(parent, 16)
        {
            return Err(RelayError::MalformedParentSpanId);
        }
        let in_window = |t: f64| t.is_finite() && (t - now_unix_ms).abs() <= MAX_CLOCK_SKEW_MS;
        if !in_window(record.start_unix_ms)
            || !in_window(record.end_unix_ms)
            || record.end_unix_ms < record.start_unix_ms
        {
            return Err(RelayError::TimestampOutOfRange);
        }
        for (key, value) in &record.fields {
            if !COMMON_FIELDS.contains(&key.as_str()) && !allowed.contains(&key.as_str()) {
                return Err(RelayError::FieldNotAllowed);
            }
            if value.len() > MAX_VALUE_LEN {
                return Err(RelayError::ValueTooLong);
            }
            if key == fields::URL_PATH && (value.contains('?') || value.contains('#')) {
                return Err(RelayError::QueryInPath);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn record(name: &str) -> ClientSpanRecord {
            let mut fields = BTreeMap::new();
            fields.insert(
                "session.id".to_owned(),
                "7f3a9c1e5b2d4f60a8c1e2d3f4a5b6c7".to_owned(),
            );
            fields.insert("otel.status_code".to_owned(), "OK".to_owned());
            ClientSpanRecord {
                name: name.to_owned(),
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".to_owned(),
                span_id: "c1f2e3d4a5b6c7d8".to_owned(),
                parent_span_id: Some("a1b2c3d4e5f60718".to_owned()),
                start_unix_ms: 1_000.0,
                end_unix_ms: 1_800.0,
                fields,
            }
        }

        #[test]
        fn a_well_formed_call_record_is_accepted() {
            let mut r = record("pocopine.client.server_function");
            r.fields.insert("http.route".into(), "/api/x".into());
            r.fields
                .insert("http.response.status_code".into(), "200".into());
            assert_eq!(validate(&r, 1_500.0), Ok(()));
            assert_eq!(r.duration_ms(), 800.0);
        }

        #[test]
        fn refusals_are_named_and_never_echo_values() {
            let bad_name = record("pocopine.http.request");
            assert_eq!(
                validate(&bad_name, 1_500.0),
                Err(RelayError::UnknownSpanName)
            );
            let mut bad_id = record("pocopine.client.boot");
            bad_id.trace_id = "00000000000000000000000000000000".into();
            assert_eq!(
                validate(&bad_id, 1_500.0),
                Err(RelayError::MalformedTraceId)
            );
            let mut free_text = record("pocopine.client.boot");
            free_text
                .fields
                .insert("message".into(), "drop table".into());
            assert_eq!(
                validate(&free_text, 1_500.0),
                Err(RelayError::FieldNotAllowed)
            );
            let mut query = record("pocopine.client.navigation");
            query.fields.insert("url.path".into(), "/a?token=x".into());
            assert_eq!(validate(&query, 1_500.0), Err(RelayError::QueryInPath));
            let mut stale = record("pocopine.client.boot");
            stale.start_unix_ms = 1_000.0 - MAX_CLOCK_SKEW_MS - 1.0;
            assert_eq!(
                validate(&stale, 1_500.0),
                Err(RelayError::TimestampOutOfRange)
            );
            let mut long = record("pocopine.client.boot");
            long.fields
                .insert("error.type".into(), "x".repeat(MAX_VALUE_LEN + 1));
            assert_eq!(validate(&long, 1_500.0), Err(RelayError::ValueTooLong));
        }
    }
}

/// A span-aware capture layer for tests in other crates: every span with
/// its fields (recorded at open and later), its parent, whether it closed,
/// and every event with the names of its enclosing spans, root first.
/// Behind the `test-support` feature; never enabled by a runtime crate.
#[cfg(feature = "test-support")]
pub mod test_support {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::registry::LookupSpan;

    #[derive(Clone, Debug)]
    pub struct CapturedSpan {
        pub id: u64,
        pub name: String,
        pub target: String,
        pub parent: Option<u64>,
        pub fields: BTreeMap<String, String>,
        pub closed: bool,
    }

    impl CapturedSpan {
        pub fn field(&self, name: &str) -> Option<&str> {
            self.fields.get(name).map(String::as_str)
        }
    }

    #[derive(Clone, Debug)]
    pub struct CapturedEvent {
        pub target: String,
        pub fields: BTreeMap<String, String>,
        /// Enclosing span names, root first.
        pub spans: Vec<String>,
        /// Id of the innermost enclosing span.
        pub span: Option<u64>,
    }

    impl CapturedEvent {
        pub fn field(&self, name: &str) -> Option<&str> {
            self.fields.get(name).map(String::as_str)
        }

        pub fn ancestry(&self) -> Vec<&str> {
            self.spans.iter().map(String::as_str).collect()
        }
    }

    #[derive(Clone, Default)]
    pub struct SpanCapture {
        spans: Arc<Mutex<Vec<CapturedSpan>>>,
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl SpanCapture {
        pub fn new() -> Self {
            Self::default()
        }

        /// Run `f` with this capture installed as the thread-local default
        /// subscriber. Drive a current-thread runtime inside `f` so spawned
        /// tasks run under it too.
        pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
            let subscriber = tracing_subscriber::registry().with(self.clone());
            tracing::subscriber::with_default(subscriber, f)
        }

        pub fn spans(&self) -> Vec<CapturedSpan> {
            self.spans.lock().expect("span capture poisoned").clone()
        }

        pub fn spans_named(&self, name: &str) -> Vec<CapturedSpan> {
            self.spans()
                .into_iter()
                .filter(|span| span.name == name)
                .collect()
        }

        /// The one span opened with `name`; panics if there are zero or many.
        pub fn span(&self, name: &str) -> CapturedSpan {
            let mut found = self.spans_named(name);
            assert_eq!(
                found.len(),
                1,
                "expected exactly one `{name}` span: {:?}",
                self.spans()
            );
            found.remove(0)
        }

        pub fn events(&self) -> Vec<CapturedEvent> {
            self.events.lock().expect("span capture poisoned").clone()
        }

        pub fn events_with_message(&self, target: &str, message: &str) -> Vec<CapturedEvent> {
            self.events()
                .into_iter()
                .filter(|event| event.target == target && event.field("message") == Some(message))
                .collect()
        }
    }

    #[derive(Default)]
    struct FieldVisitor(BTreeMap<String, String>);

    impl Visit for FieldVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_owned(), value.to_owned());
        }
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }
        fn record_i64(&mut self, field: &Field, value: i64) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }
        fn record_u64(&mut self, field: &Field, value: u64) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }
        fn record_f64(&mut self, field: &Field, value: f64) {
            self.0.insert(field.name().to_owned(), value.to_string());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.0.insert(field.name().to_owned(), format!("{value:?}"));
        }
    }

    impl<S> Layer<S> for SpanCapture
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
            let mut visitor = FieldVisitor::default();
            attrs.record(&mut visitor);
            let parent = ctx
                .span(id)
                .and_then(|span| span.parent().map(|parent| parent.id().into_u64()));
            self.spans
                .lock()
                .expect("span capture poisoned")
                .push(CapturedSpan {
                    id: id.into_u64(),
                    name: attrs.metadata().name().to_owned(),
                    target: attrs.metadata().target().to_owned(),
                    parent,
                    fields: visitor.0,
                    closed: false,
                });
        }

        fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
            let mut visitor = FieldVisitor::default();
            values.record(&mut visitor);
            let mut spans = self.spans.lock().expect("span capture poisoned");
            if let Some(span) = spans.iter_mut().find(|span| span.id == id.into_u64()) {
                span.fields.extend(visitor.0);
            }
        }

        fn on_close(&self, id: Id, _ctx: Context<'_, S>) {
            let mut spans = self.spans.lock().expect("span capture poisoned");
            if let Some(span) = spans.iter_mut().find(|span| span.id == id.into_u64()) {
                span.closed = true;
            }
        }

        fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            let spans = ctx
                .event_scope(event)
                .map(|scope| {
                    scope
                        .from_root()
                        .map(|span| span.name().to_owned())
                        .collect()
                })
                .unwrap_or_default();
            self.events
                .lock()
                .expect("span capture poisoned")
                .push(CapturedEvent {
                    target: event.metadata().target().to_owned(),
                    fields: visitor.0,
                    spans,
                    span: ctx.event_span(event).map(|span| span.id().into_u64()),
                });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    Log,
    Trace,
    Metric,
    Analytics,
}

impl EventClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Trace => "trace",
            Self::Metric => "metric",
            Self::Analytics => "analytics",
        }
    }

    pub fn target(self) -> &'static str {
        match self {
            Self::Log => LOG_TARGET,
            Self::Trace => TRACE_TARGET,
            Self::Metric => METRIC_TARGET,
            Self::Analytics => ANALYTICS_TARGET,
        }
    }
}

impl fmt::Display for EventClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventPriority {
    Critical,
    High,
    Normal,
    Low,
}

impl EventPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Normal => "normal",
            Self::Low => "low",
        }
    }

    pub fn level(self) -> Level {
        match self {
            Self::Critical => Level::ERROR,
            Self::High => Level::WARN,
            Self::Normal => Level::INFO,
            Self::Low => Level::DEBUG,
        }
    }
}

impl fmt::Display for EventPriority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldPrivacy {
    Public,
    Pseudonymous,
    Sensitive,
}

impl FieldPrivacy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Pseudonymous => "pseudonymous",
            Self::Sensitive => "sensitive",
        }
    }
}

impl fmt::Display for FieldPrivacy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FieldValue {
    String(String),
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
}

impl FieldValue {
    pub fn as_debug_value(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::I64(value) => value.to_string(),
            Self::U64(value) => value.to_string(),
            Self::F64(value) => value.to_string(),
        }
    }
}

impl From<&str> for FieldValue {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for FieldValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for FieldValue {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for FieldValue {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<i32> for FieldValue {
    fn from(value: i32) -> Self {
        Self::I64(value.into())
    }
}

impl From<u64> for FieldValue {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<u32> for FieldValue {
    fn from(value: u32) -> Self {
        Self::U64(value.into())
    }
}

impl From<f64> for FieldValue {
    fn from(value: f64) -> Self {
        Self::F64(value)
    }
}

impl From<f32> for FieldValue {
    fn from(value: f32) -> Self {
        Self::F64(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedField {
    pub value: FieldValue,
    pub privacy: FieldPrivacy,
}

impl ObservedField {
    pub fn new(value: impl Into<FieldValue>, privacy: FieldPrivacy) -> Self {
        Self {
            value: value.into(),
            privacy,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObserveContext {
    pub service: Option<String>,
    pub environment: Option<String>,
    pub route: Option<String>,
    pub component: Option<String>,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub user_id_hash: Option<String>,
}

impl ObserveContext {
    pub fn with_service(mut self, service: impl Into<String>) -> Self {
        self.service = Some(service.into());
        self
    }

    pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    pub fn with_route(mut self, route: impl Into<String>) -> Self {
        self.route = Some(route.into());
        self
    }

    pub fn with_component(mut self, component: impl Into<String>) -> Self {
        self.component = Some(component.into());
        self
    }

    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_user_id_hash(mut self, user_id_hash: impl Into<String>) -> Self {
        self.user_id_hash = Some(user_id_hash.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ObservedEvent {
    pub name: String,
    pub version: u16,
    pub class: EventClass,
    pub priority: EventPriority,
    pub privacy: FieldPrivacy,
    pub context: ObserveContext,
    pub fields: BTreeMap<String, ObservedField>,
}

impl ObservedEvent {
    pub fn new(name: impl Into<String>, class: EventClass) -> Self {
        Self {
            name: name.into(),
            version: 1,
            class,
            priority: EventPriority::Normal,
            privacy: FieldPrivacy::Public,
            context: ObserveContext::default(),
            fields: BTreeMap::new(),
        }
    }

    pub fn log(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Log)
    }

    pub fn trace(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Trace)
    }

    pub fn metric(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Metric)
    }

    pub fn analytics(name: impl Into<String>) -> Self {
        Self::new(name, EventClass::Analytics).privacy(FieldPrivacy::Pseudonymous)
    }

    pub fn version(mut self, version: u16) -> Self {
        self.version = version;
        self
    }

    pub fn priority(mut self, priority: EventPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn privacy(mut self, privacy: FieldPrivacy) -> Self {
        self.privacy = privacy;
        self
    }

    pub fn context(mut self, context: ObserveContext) -> Self {
        self.context = context;
        self
    }

    pub fn field(
        mut self,
        name: impl Into<String>,
        value: impl Into<FieldValue>,
        privacy: FieldPrivacy,
    ) -> Self {
        self.insert_field(name, value, privacy);
        self
    }

    pub fn insert_field(
        &mut self,
        name: impl Into<String>,
        value: impl Into<FieldValue>,
        privacy: FieldPrivacy,
    ) {
        self.fields
            .insert(name.into(), ObservedField::new(value, privacy));
    }

    pub fn redacted(&self, policy: RedactionPolicy) -> Self {
        let mut event = self.clone();
        event.fields.retain(|_, field| policy.allows(field.privacy));
        if !policy.include_pseudonymous {
            event.context.session_id = None;
            event.context.user_id_hash = None;
        }
        event
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RedactionPolicy {
    pub include_pseudonymous: bool,
    pub include_sensitive: bool,
}

impl RedactionPolicy {
    pub const fn public_only() -> Self {
        Self {
            include_pseudonymous: false,
            include_sensitive: false,
        }
    }

    pub const fn allow_pseudonymous() -> Self {
        Self {
            include_pseudonymous: true,
            include_sensitive: false,
        }
    }

    pub const fn allow_sensitive() -> Self {
        Self {
            include_pseudonymous: true,
            include_sensitive: true,
        }
    }

    pub fn allows(self, privacy: FieldPrivacy) -> bool {
        match privacy {
            FieldPrivacy::Public => true,
            FieldPrivacy::Pseudonymous => self.include_pseudonymous,
            FieldPrivacy::Sensitive => self.include_sensitive,
        }
    }
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self::public_only()
    }
}

const OBSERVED_TRACING_FIELD_SLOTS: usize = 8;
static OBSERVED_TRACING_OVERFLOW_WARNED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug)]
struct ObservedTracingField<'a> {
    present: bool,
    name: &'a str,
    privacy: &'static str,
    kind: &'static str,
    value_string: &'a str,
    value_bool: bool,
    value_i64: i64,
    value_u64: u64,
    value_f64: f64,
}

impl<'a> ObservedTracingField<'a> {
    const EMPTY: Self = Self {
        present: false,
        name: "",
        privacy: "",
        kind: "",
        value_string: "",
        value_bool: false,
        value_i64: 0,
        value_u64: 0,
        value_f64: 0.0,
    };

    fn from_field(name: &'a str, field: &'a ObservedField) -> Self {
        let mut slot = Self {
            present: true,
            name,
            privacy: field.privacy.as_str(),
            ..Self::EMPTY
        };

        match &field.value {
            FieldValue::String(value) => {
                slot.kind = "string";
                slot.value_string = value;
            }
            FieldValue::Bool(value) => {
                slot.kind = "bool";
                slot.value_bool = *value;
            }
            FieldValue::I64(value) => {
                slot.kind = "i64";
                slot.value_i64 = *value;
            }
            FieldValue::U64(value) => {
                slot.kind = "u64";
                slot.value_u64 = *value;
            }
            FieldValue::F64(value) => {
                slot.kind = "f64";
                slot.value_f64 = *value;
            }
        }

        slot
    }
}

#[derive(Clone, Debug)]
struct ObservedTracing<'a> {
    event: &'a ObservedEvent,
    fields: [ObservedTracingField<'a>; OBSERVED_TRACING_FIELD_SLOTS],
    field_count: u64,
    field_overflowed: bool,
}

impl<'a> ObservedTracing<'a> {
    fn new(event: &'a ObservedEvent) -> Self {
        let mut fields = [ObservedTracingField::EMPTY; OBSERVED_TRACING_FIELD_SLOTS];

        for (slot, (name, field)) in fields.iter_mut().zip(event.fields.iter()) {
            *slot = ObservedTracingField::from_field(name, field);
        }

        Self {
            event,
            fields,
            field_count: event.fields.len() as u64,
            field_overflowed: event.fields.len() > OBSERVED_TRACING_FIELD_SLOTS,
        }
    }

    fn event_name(&self) -> &str {
        &self.event.name
    }

    fn event_class(&self) -> &'static str {
        self.event.class.as_str()
    }

    fn event_priority(&self) -> &'static str {
        self.event.priority.as_str()
    }

    fn event_privacy(&self) -> &'static str {
        self.event.privacy.as_str()
    }

    fn service(&self) -> &str {
        self.event.context.service.as_deref().unwrap_or("")
    }

    fn has_service(&self) -> bool {
        self.event.context.service.is_some()
    }

    fn environment(&self) -> &str {
        self.event.context.environment.as_deref().unwrap_or("")
    }

    fn has_environment(&self) -> bool {
        self.event.context.environment.is_some()
    }

    fn route(&self) -> &str {
        self.event.context.route.as_deref().unwrap_or("")
    }

    fn has_route(&self) -> bool {
        self.event.context.route.is_some()
    }

    fn component(&self) -> &str {
        self.event.context.component.as_deref().unwrap_or("")
    }

    fn has_component(&self) -> bool {
        self.event.context.component.is_some()
    }

    fn trace_id(&self) -> &str {
        self.event.context.trace_id.as_deref().unwrap_or("")
    }

    fn has_trace_id(&self) -> bool {
        self.event.context.trace_id.is_some()
    }

    fn session_id(&self) -> &str {
        self.event.context.session_id.as_deref().unwrap_or("")
    }

    fn has_session_id(&self) -> bool {
        self.event.context.session_id.is_some()
    }

    fn user_id_hash(&self) -> &str {
        self.event.context.user_id_hash.as_deref().unwrap_or("")
    }

    fn has_user_id_hash(&self) -> bool {
        self.event.context.user_id_hash.is_some()
    }

    fn slot(&self, index: usize) -> ObservedTracingField<'a> {
        self.fields[index]
    }
}

/// Emits an observed event into `tracing`.
///
/// This function preserves the event exactly as supplied. It does not apply
/// redaction; call [`ObservedEvent::redacted`] first, or emit through
/// `pocopine-analytics`, when private fields must be stripped before export.
pub fn emit_tracing(event: &ObservedEvent) {
    let observed = ObservedTracing::new(event);

    if observed.field_overflowed && !OBSERVED_TRACING_OVERFLOW_WARNED.swap(true, Ordering::Relaxed)
    {
        tracing::warn!(
            target: "pocopine.log",
            event_name = observed.event_name(),
            observed_field_count = observed.field_count,
            observed_field_slots = OBSERVED_TRACING_FIELD_SLOTS as u64,
            "observed event field slots overflowed; extra fields omitted from tracing output"
        );
    }

    macro_rules! emit_at_level {
        ($target:literal, $level:expr, $observed:expr) => {{
            let f0 = $observed.slot(0);
            let f1 = $observed.slot(1);
            let f2 = $observed.slot(2);
            let f3 = $observed.slot(3);
            let f4 = $observed.slot(4);
            let f5 = $observed.slot(5);
            let f6 = $observed.slot(6);
            let f7 = $observed.slot(7);

            tracing::event!(
                target: $target,
                $level,
                event_name = $observed.event_name(),
                event_version = $observed.event.version,
                event_class = $observed.event_class(),
                event_priority = $observed.event_priority(),
                event_privacy = $observed.event_privacy(),
                observed_context_has_service = $observed.has_service(),
                observed_context_service = $observed.service(),
                observed_context_has_environment = $observed.has_environment(),
                observed_context_environment = $observed.environment(),
                observed_context_has_route = $observed.has_route(),
                observed_context_route = $observed.route(),
                observed_context_has_component = $observed.has_component(),
                observed_context_component = $observed.component(),
                observed_context_has_trace_id = $observed.has_trace_id(),
                observed_context_trace_id = $observed.trace_id(),
                observed_context_has_session_id = $observed.has_session_id(),
                observed_context_session_id = $observed.session_id(),
                observed_context_has_user_id_hash = $observed.has_user_id_hash(),
                observed_context_user_id_hash = $observed.user_id_hash(),
                observed_field_slots = OBSERVED_TRACING_FIELD_SLOTS as u64,
                observed_field_count = $observed.field_count,
                observed_field_overflowed = $observed.field_overflowed,
                observed_field_0_present = f0.present,
                observed_field_0_name = f0.name,
                observed_field_0_privacy = f0.privacy,
                observed_field_0_kind = f0.kind,
                observed_field_0_value_string = f0.value_string,
                observed_field_0_value_bool = f0.value_bool,
                observed_field_0_value_i64 = f0.value_i64,
                observed_field_0_value_u64 = f0.value_u64,
                observed_field_0_value_f64 = f0.value_f64,
                observed_field_1_present = f1.present,
                observed_field_1_name = f1.name,
                observed_field_1_privacy = f1.privacy,
                observed_field_1_kind = f1.kind,
                observed_field_1_value_string = f1.value_string,
                observed_field_1_value_bool = f1.value_bool,
                observed_field_1_value_i64 = f1.value_i64,
                observed_field_1_value_u64 = f1.value_u64,
                observed_field_1_value_f64 = f1.value_f64,
                observed_field_2_present = f2.present,
                observed_field_2_name = f2.name,
                observed_field_2_privacy = f2.privacy,
                observed_field_2_kind = f2.kind,
                observed_field_2_value_string = f2.value_string,
                observed_field_2_value_bool = f2.value_bool,
                observed_field_2_value_i64 = f2.value_i64,
                observed_field_2_value_u64 = f2.value_u64,
                observed_field_2_value_f64 = f2.value_f64,
                observed_field_3_present = f3.present,
                observed_field_3_name = f3.name,
                observed_field_3_privacy = f3.privacy,
                observed_field_3_kind = f3.kind,
                observed_field_3_value_string = f3.value_string,
                observed_field_3_value_bool = f3.value_bool,
                observed_field_3_value_i64 = f3.value_i64,
                observed_field_3_value_u64 = f3.value_u64,
                observed_field_3_value_f64 = f3.value_f64,
                observed_field_4_present = f4.present,
                observed_field_4_name = f4.name,
                observed_field_4_privacy = f4.privacy,
                observed_field_4_kind = f4.kind,
                observed_field_4_value_string = f4.value_string,
                observed_field_4_value_bool = f4.value_bool,
                observed_field_4_value_i64 = f4.value_i64,
                observed_field_4_value_u64 = f4.value_u64,
                observed_field_4_value_f64 = f4.value_f64,
                observed_field_5_present = f5.present,
                observed_field_5_name = f5.name,
                observed_field_5_privacy = f5.privacy,
                observed_field_5_kind = f5.kind,
                observed_field_5_value_string = f5.value_string,
                observed_field_5_value_bool = f5.value_bool,
                observed_field_5_value_i64 = f5.value_i64,
                observed_field_5_value_u64 = f5.value_u64,
                observed_field_5_value_f64 = f5.value_f64,
                observed_field_6_present = f6.present,
                observed_field_6_name = f6.name,
                observed_field_6_privacy = f6.privacy,
                observed_field_6_kind = f6.kind,
                observed_field_6_value_string = f6.value_string,
                observed_field_6_value_bool = f6.value_bool,
                observed_field_6_value_i64 = f6.value_i64,
                observed_field_6_value_u64 = f6.value_u64,
                observed_field_6_value_f64 = f6.value_f64,
                observed_field_7_present = f7.present,
                observed_field_7_name = f7.name,
                observed_field_7_privacy = f7.privacy,
                observed_field_7_kind = f7.kind,
                observed_field_7_value_string = f7.value_string,
                observed_field_7_value_bool = f7.value_bool,
                observed_field_7_value_i64 = f7.value_i64,
                observed_field_7_value_u64 = f7.value_u64,
                observed_field_7_value_f64 = f7.value_f64,
            );
        }};
    }

    macro_rules! emit_for_target {
        ($target:literal, $observed:expr) => {
            match $observed.event.priority.level() {
                Level::ERROR => emit_at_level!($target, Level::ERROR, $observed),
                Level::WARN => emit_at_level!($target, Level::WARN, $observed),
                Level::INFO => emit_at_level!($target, Level::INFO, $observed),
                Level::DEBUG | Level::TRACE => emit_at_level!($target, Level::DEBUG, $observed),
            }
        };
    }

    match event.class {
        EventClass::Log => emit_for_target!("pocopine.log", observed),
        EventClass::Trace => emit_for_target!("pocopine.trace", observed),
        EventClass::Metric => emit_for_target!("pocopine.metric", observed),
        EventClass::Analytics => emit_for_target!("pocopine.analytics", observed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_space_is_closed_prefixed_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for name in spans::ALL {
            assert!(
                name.starts_with("pocopine."),
                "span `{name}` is outside the pocopine.* space"
            );
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "span `{name}` must be lowercase snake_case segments"
            );
            assert!(seen.insert(*name), "span `{name}` listed twice");
        }
        assert_eq!(spans::ALL.len(), 17);
    }

    #[test]
    fn span_fields_use_semconv_or_pocopine_prefix() {
        for name in [
            fields::HTTP_REQUEST_METHOD,
            fields::HTTP_ROUTE,
            fields::HTTP_RESPONSE_STATUS_CODE,
            fields::URL_PATH,
            fields::SESSION_ID,
            fields::GEN_AI_REQUEST_MODEL,
            fields::GEN_AI_USAGE_INPUT_TOKENS,
            fields::ERROR_TYPE,
        ] {
            assert!(!name.starts_with("pocopine."), "{name} is a semconv name");
        }
        for name in [
            fields::REQUEST_ID,
            fields::FUNCTION,
            fields::JOB_NAME,
            fields::AI_STEP_ID,
        ] {
            assert!(
                name.starts_with("pocopine."),
                "{name} needs the pocopine prefix"
            );
        }
    }
    use std::collections::BTreeMap;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::Layer;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::Context;
    use tracing_subscriber::prelude::*;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("test writer lock")
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedWriter {
        type Writer = TestWriter;

        fn make_writer(&'a self) -> Self::Writer {
            TestWriter(Arc::clone(&self.0))
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum CapturedValue {
        Bool(bool),
        I64(i64),
        U64(u64),
        F64(f64),
        Str(String),
        Debug(String),
    }

    #[derive(Clone, Debug, PartialEq)]
    struct CapturedEvent {
        target: String,
        fields: BTreeMap<String, CapturedValue>,
    }

    #[derive(Clone, Default)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    impl<S> Layer<S> for CaptureLayer
    where
        S: tracing::Subscriber,
    {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut visitor = CaptureVisitor::default();
            event.record(&mut visitor);
            self.events
                .lock()
                .expect("captured events lock")
                .push(CapturedEvent {
                    target: event.metadata().target().to_string(),
                    fields: visitor.fields,
                });
        }
    }

    #[derive(Default)]
    struct CaptureVisitor {
        fields: BTreeMap<String, CapturedValue>,
    }

    impl Visit for CaptureVisitor {
        fn record_bool(&mut self, field: &Field, value: bool) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::Bool(value));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::I64(value));
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::U64(value));
        }

        fn record_f64(&mut self, field: &Field, value: f64) {
            self.fields
                .insert(field.name().to_string(), CapturedValue::F64(value));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.fields.insert(
                field.name().to_string(),
                CapturedValue::Str(value.to_string()),
            );
        }

        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.fields.insert(
                field.name().to_string(),
                CapturedValue::Debug(format!("{value:?}")),
            );
        }
    }

    #[test]
    fn redacts_non_public_fields_by_default() {
        let event = ObservedEvent::analytics("checkout")
            .field("plan", "pro", FieldPrivacy::Public)
            .field("session", "s-1", FieldPrivacy::Pseudonymous)
            .field("email", "person@example.test", FieldPrivacy::Sensitive);

        let redacted = event.redacted(RedactionPolicy::public_only());

        assert!(redacted.fields.contains_key("plan"));
        assert!(!redacted.fields.contains_key("session"));
        assert!(!redacted.fields.contains_key("email"));
    }

    #[test]
    fn can_allow_pseudonymous_without_sensitive_fields() {
        let context = ObserveContext::default().with_session_id("s-1");
        let event = ObservedEvent::analytics("route_view")
            .context(context)
            .field("route", "/settings", FieldPrivacy::Public)
            .field("session", "s-1", FieldPrivacy::Pseudonymous)
            .field("token", "secret", FieldPrivacy::Sensitive);

        let redacted = event.redacted(RedactionPolicy::allow_pseudonymous());

        assert_eq!(redacted.context.session_id.as_deref(), Some("s-1"));
        assert!(redacted.fields.contains_key("route"));
        assert!(redacted.fields.contains_key("session"));
        assert!(!redacted.fields.contains_key("token"));
    }

    #[test]
    fn emit_tracing_json_output_uses_structured_observed_fields() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(SharedWriter(Arc::clone(&output))),
        );
        let event = ObservedEvent::metric("queue_depth")
            .context(
                ObserveContext::default()
                    .with_service("worker")
                    .with_route("/jobs"),
            )
            .field("active", true, FieldPrivacy::Public)
            .field("depth", 42_u64, FieldPrivacy::Public)
            .field("route", "/jobs", FieldPrivacy::Public);

        tracing::subscriber::with_default(subscriber, || emit_tracing(&event));

        let output = String::from_utf8(output.lock().expect("json output lock").clone())
            .expect("json output is utf8");
        let line = output.lines().next().expect("one json line");
        let json: serde_json::Value = serde_json::from_str(line).expect("valid json log line");
        let fields = json
            .get("fields")
            .and_then(serde_json::Value::as_object)
            .expect("json event fields");

        assert_eq!(fields.get("event_name").unwrap(), "queue_depth");
        assert_eq!(fields.get("event_class").unwrap(), "metric");
        assert_eq!(fields.get("observed_context_has_service").unwrap(), true);
        assert_eq!(fields.get("observed_context_service").unwrap(), "worker");
        assert_eq!(fields.get("observed_context_route").unwrap(), "/jobs");
        assert_eq!(fields.get("observed_field_count").unwrap(), 3);
        assert_eq!(fields.get("observed_field_0_name").unwrap(), "active");
        assert_eq!(fields.get("observed_field_0_kind").unwrap(), "bool");
        assert_eq!(fields.get("observed_field_0_value_bool").unwrap(), true);
        assert_eq!(fields.get("observed_field_1_name").unwrap(), "depth");
        assert_eq!(fields.get("observed_field_1_kind").unwrap(), "u64");
        assert_eq!(fields.get("observed_field_1_value_u64").unwrap(), 42);
        assert_eq!(fields.get("observed_field_2_name").unwrap(), "route");
        assert_eq!(fields.get("observed_field_2_kind").unwrap(), "string");
        assert_eq!(
            fields.get("observed_field_2_value_string").unwrap(),
            "/jobs"
        );
        assert!(!fields.contains_key("context"));
        assert!(!fields.contains_key("fields"));
    }

    #[test]
    fn emit_tracing_records_otlp_facing_typed_slots() {
        let capture = CaptureLayer::default();
        let events = Arc::clone(&capture.events);
        let subscriber = tracing_subscriber::registry().with(capture);
        let event = ObservedEvent::analytics("cta")
            .field("clicks", 7_u64, FieldPrivacy::Public)
            .field("ok", true, FieldPrivacy::Public)
            .field("ratio", 0.5_f64, FieldPrivacy::Public);

        tracing::subscriber::with_default(subscriber, || emit_tracing(&event));

        let events = events.lock().expect("captured events lock");
        let event = events.first().expect("captured event");
        assert_eq!(event.target, ANALYTICS_TARGET);
        assert!(!event.fields.contains_key("context"));
        assert!(!event.fields.contains_key("fields"));
        assert_eq!(
            event.fields.get("observed_field_0_name"),
            Some(&CapturedValue::Str("clicks".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_0_value_u64"),
            Some(&CapturedValue::U64(7))
        );
        assert_eq!(
            event.fields.get("observed_field_1_name"),
            Some(&CapturedValue::Str("ok".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_1_value_bool"),
            Some(&CapturedValue::Bool(true))
        );
        assert_eq!(
            event.fields.get("observed_field_2_name"),
            Some(&CapturedValue::Str("ratio".to_string()))
        );
        assert_eq!(
            event.fields.get("observed_field_2_value_f64"),
            Some(&CapturedValue::F64(0.5))
        );
    }
}
