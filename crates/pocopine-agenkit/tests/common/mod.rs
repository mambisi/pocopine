//! Shared test support: capture emitted `pocopine.trace` event names.
//!
//! `emit_tracing` records the AI event name in the `event_name` tracing field
//! (see `pocopine-observe`). This installs a thread-local subscriber that
//! collects those names so flow/parallel tests can assert the trace tree.
//!
//! Shared across integration-test binaries; not every binary uses every
//! helper, so dead-code is expected here.
#![allow(dead_code)]

use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

/// Collects the `event_name` of every emitted trace event.
#[derive(Clone, Default)]
pub struct TraceCapture {
    events: Arc<Mutex<Vec<String>>>,
    costs: Arc<Mutex<Vec<f64>>>,
}

impl TraceCapture {
    /// Every captured event name, in emission order.
    pub fn names(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    /// Whether an event with this name was emitted.
    pub fn contains(&self, name: &str) -> bool {
        self.events.lock().unwrap().iter().any(|n| n == name)
    }

    /// How many times an event name was emitted.
    pub fn count(&self, name: &str) -> usize {
        self.events
            .lock()
            .unwrap()
            .iter()
            .filter(|n| n.as_str() == name)
            .count()
    }

    /// Every `cost_amount` field value emitted, in order.
    pub fn costs(&self) -> Vec<f64> {
        self.costs.lock().unwrap().clone()
    }
}

#[derive(Default)]
struct EventVisitor {
    name: Option<String>,
    cost: Option<f64>,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event_name" {
            self.name = Some(value.to_string());
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // `emit_tracing` flattens ObservedEvent fields into positional slots
        // (`observed_field_<n>_value_f64`), so there is no field literally named
        // `cost_amount`. The only non-zero f64 emitted for an `ai_model_response`
        // is the cost amount (token counts are integers).
        if value != 0.0 && field.name().ends_with("_value_f64") {
            self.cost = Some(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "event_name" && self.name.is_none() {
            self.name = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

impl<S: Subscriber> Layer<S> for TraceCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        if let Some(name) = visitor.name {
            self.events.lock().unwrap().push(name);
        }
        if let Some(cost) = visitor.cost {
            self.costs.lock().unwrap().push(cost);
        }
    }
}

/// Run `f` with a trace-capturing subscriber installed on the current thread,
/// returning the captured event names. `f` should drive a current-thread
/// runtime so all emissions happen on this thread.
pub fn capture<F: FnOnce()>(f: F) -> TraceCapture {
    let capture = TraceCapture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    tracing::subscriber::with_default(subscriber, f);
    capture
}

/// Build a current-thread runtime and block on `future`.
pub fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
