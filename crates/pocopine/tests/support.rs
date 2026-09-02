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
pub struct CapturedEvent {
    pub target: String,
    pub fields: BTreeMap<String, String>,
    /// Names of the enclosing spans, root first (RFC-123).
    pub spans: Vec<String>,
}

impl CapturedEvent {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Enclosing span names, root first.
    #[allow(dead_code)]
    pub fn ancestry(&self) -> Vec<&str> {
        self.spans.iter().map(String::as_str).collect()
    }
}

/// One span the capture saw open, with every field recorded on it —
/// at creation and later via `Span::record`.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct CapturedSpan {
    pub id: u64,
    pub name: String,
    pub target: String,
    pub parent: Option<u64>,
    pub fields: BTreeMap<String, String>,
}

#[allow(dead_code)]
impl CapturedSpan {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Clone, Default)]
pub struct TraceCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl TraceCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer {
            events: Arc::clone(&self.events),
            spans: Arc::clone(&self.spans),
        });
        tracing::subscriber::with_default(subscriber, f)
    }

    #[allow(dead_code)]
    pub fn spans(&self) -> Vec<CapturedSpan> {
        self.spans
            .lock()
            .expect("trace capture lock poisoned")
            .clone()
    }

    /// Every span opened with `name`, in creation order.
    #[allow(dead_code)]
    pub fn spans_named(&self, name: &str) -> Vec<CapturedSpan> {
        self.spans()
            .into_iter()
            .filter(|span| span.name == name)
            .collect()
    }

    /// The one span opened with `name`; panics if there are zero or many.
    #[allow(dead_code)]
    pub fn span(&self, name: &str) -> CapturedSpan {
        let mut found = self.spans_named(name);
        assert_eq!(found.len(), 1, "expected exactly one `{name}` span");
        found.remove(0)
    }

    #[allow(dead_code)]
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events
            .lock()
            .expect("trace capture lock poisoned")
            .clone()
    }

    #[allow(dead_code)]
    pub fn events_with_message(&self, target: &str, message: &str) -> Vec<CapturedEvent> {
        // Message text is part of these observability contract assertions;
        // rename emitted messages only as a deliberate telemetry change.
        self.events
            .lock()
            .expect("trace capture lock poisoned")
            .iter()
            .filter(|event| event.target == target && event.field("message") == Some(message))
            .cloned()
            .collect()
    }
}

struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
    spans: Arc<Mutex<Vec<CapturedSpan>>>,
}

impl<S> Layer<S> for CaptureLayer
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
            .expect("trace capture lock poisoned")
            .push(CapturedSpan {
                id: id.into_u64(),
                name: attrs.metadata().name().to_owned(),
                target: attrs.metadata().target().to_owned(),
                parent,
                fields: visitor.fields,
            });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        values.record(&mut visitor);
        let mut spans = self.spans.lock().expect("trace capture lock poisoned");
        if let Some(span) = spans.iter_mut().find(|span| span.id == id.into_u64()) {
            span.fields.extend(visitor.fields);
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
            .expect("trace capture lock poisoned")
            .push(CapturedEvent {
                target: event.metadata().target().to_owned(),
                fields: visitor.fields,
                spans,
            });
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: BTreeMap<String, String>,
}

impl FieldVisitor {
    fn insert(&mut self, field: &Field, value: impl Into<String>) {
        self.fields.insert(field.name().to_owned(), value.into());
    }
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, value);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, value.to_string());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, format!("{value:?}"));
    }
}
