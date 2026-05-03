use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::Layer;

#[derive(Clone, Debug)]
pub struct CapturedEvent {
    pub target: String,
    pub fields: BTreeMap<String, String>,
}

impl CapturedEvent {
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }
}

#[derive(Clone, Default)]
pub struct TraceCapture {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl TraceCapture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer {
            events: Arc::clone(&self.events),
        });
        tracing::subscriber::with_default(subscriber, f)
    }

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
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        self.events
            .lock()
            .expect("trace capture lock poisoned")
            .push(CapturedEvent {
                target: event.metadata().target().to_owned(),
                fields: visitor.fields,
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
