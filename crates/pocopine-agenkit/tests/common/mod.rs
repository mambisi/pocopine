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
}

struct EventNameVisitor<'a>(&'a mut Option<String>);

impl Visit for EventNameVisitor<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event_name" {
            *self.0 = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "event_name" && self.0.is_none() {
            *self.0 = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

impl<S: Subscriber> Layer<S> for TraceCapture {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut name = None;
        event.record(&mut EventNameVisitor(&mut name));
        if let Some(name) = name {
            self.events.lock().unwrap().push(name);
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
