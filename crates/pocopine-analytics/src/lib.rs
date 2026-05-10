//! Analytics and telemetry dispatch for pocopine applications.
//!
//! This crate intentionally does not own a vendor SDK. It owns the
//! redaction and fan-out rules, then lets apps attach Firebase,
//! Google Analytics, Cloudflare, OpenTelemetry, or custom sinks.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::VecDeque;
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{Arc, Mutex};

use pocopine_observe::{
    emit_tracing, EventPriority, FieldPrivacy, ObserveContext, ObservedEvent, RedactionPolicy,
};

#[cfg(not(target_arch = "wasm32"))]
pub trait AnalyticsSink: Send + Sync {
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError>;

    /// Flush buffered records for sinks that batch work.
    ///
    /// Closure sinks use this no-op default. Write a concrete sink type when
    /// batching or network exporters need real flush behavior.
    fn flush(&self) -> Result<(), AnalyticsError> {
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
pub trait AnalyticsSink {
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError>;

    /// Flush buffered records for sinks that batch work.
    ///
    /// Closure sinks use this no-op default. Write a concrete sink type when
    /// batching or network exporters need real flush behavior.
    fn flush(&self) -> Result<(), AnalyticsError> {
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<F> AnalyticsSink for F
where
    F: for<'event> Fn(&'event ObservedEvent) -> Result<(), AnalyticsError> + Send + Sync,
{
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
        self(event)
    }
}

#[cfg(target_arch = "wasm32")]
impl<F> AnalyticsSink for F
where
    F: for<'event> Fn(&'event ObservedEvent) -> Result<(), AnalyticsError>,
{
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
        self(event)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnalyticsError {
    message: String,
}

impl AnalyticsError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for AnalyticsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AnalyticsError {}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct AnalyticsReport {
    /// Number of configured sinks attempted for this operation.
    pub attempted: usize,
    /// Number of sinks that completed the operation successfully.
    ///
    /// For `emit`, this means the sink accepted the event. For `flush`, this
    /// means the sink flushed successfully.
    pub succeeded: usize,
    /// Number of sinks that returned an error or panicked.
    pub failed: usize,
    pub errors: Vec<AnalyticsError>,
}

impl AnalyticsReport {
    pub fn all_succeeded(&self) -> bool {
        self.failed == 0
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExporterMetrics {
    /// Events currently waiting for this exporter to flush.
    pub pending: usize,
    /// Events accepted into the bounded queue.
    pub enqueued: u64,
    /// Events rejected because the bounded queue was full.
    pub dropped: u64,
    /// Queued events successfully delivered to the wrapped sink.
    pub delivered: u64,
    /// Queued events, sink flushes, or panics that failed during delivery.
    pub failed: u64,
}

/// Bounded queue wrapper for exporter sinks.
///
/// This keeps exporter integrations honest: backpressure is bounded, drops are
/// counted, and flush continues across per-event failures.
#[cfg(not(target_arch = "wasm32"))]
pub struct BoundedAnalyticsSink<S> {
    inner: Arc<S>,
    capacity: usize,
    queue: Arc<Mutex<VecDeque<ObservedEvent>>>,
    metrics: Arc<Mutex<ExporterMetrics>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl<S> BoundedAnalyticsSink<S>
where
    S: AnalyticsSink + 'static,
{
    pub fn new(inner: S, capacity: usize) -> Self {
        Self {
            inner: Arc::new(inner),
            capacity,
            queue: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            metrics: Arc::new(Mutex::new(ExporterMetrics::default())),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn pending(&self) -> usize {
        self.queue
            .lock()
            .expect("analytics exporter queue lock poisoned")
            .len()
    }

    /// Returns a best-effort point-in-time snapshot.
    ///
    /// Monotonic counters are exact, while `pending` reflects queue depth at
    /// the moment the snapshot was read.
    pub fn metrics(&self) -> ExporterMetrics {
        let queue = self
            .queue
            .lock()
            .expect("analytics exporter queue lock poisoned");
        let mut metrics = self
            .metrics
            .lock()
            .expect("analytics exporter metrics lock poisoned")
            .clone();
        metrics.pending = queue.len();
        metrics
    }

    fn add_metrics(&self, enqueued: u64, dropped: u64, delivered: u64, failed: u64) {
        let mut metrics = self
            .metrics
            .lock()
            .expect("analytics exporter metrics lock poisoned");
        metrics.enqueued += enqueued;
        metrics.dropped += dropped;
        metrics.delivered += delivered;
        metrics.failed += failed;
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S> Clone for BoundedAnalyticsSink<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            capacity: self.capacity,
            queue: Arc::clone(&self.queue),
            metrics: Arc::clone(&self.metrics),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<S> AnalyticsSink for BoundedAnalyticsSink<S>
where
    S: AnalyticsSink + 'static,
{
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
        let mut queue = self
            .queue
            .lock()
            .expect("analytics exporter queue lock poisoned");
        if queue.len() >= self.capacity {
            self.add_metrics(0, 1, 0, 0);
            return Err(AnalyticsError::new(format!(
                "analytics export queue full (capacity {})",
                self.capacity
            )));
        }

        queue.push_back(event.clone());
        self.add_metrics(1, 0, 0, 0);
        Ok(())
    }

    fn flush(&self) -> Result<(), AnalyticsError> {
        let queued = {
            let mut queue = self
                .queue
                .lock()
                .expect("analytics exporter queue lock poisoned");
            queue.drain(..).collect::<Vec<_>>()
        };

        let mut delivered = 0u64;
        let mut failed = 0u64;

        for event in queued {
            match catch_unwind(AssertUnwindSafe(|| self.inner.emit(&event))) {
                Ok(Ok(())) => delivered += 1,
                Ok(Err(_)) | Err(_) => {
                    failed += 1;
                }
            }
        }

        match catch_unwind(AssertUnwindSafe(|| self.inner.flush())) {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => {
                failed += 1;
            }
        }

        self.add_metrics(0, 0, delivered, failed);

        if failed == 0 {
            Ok(())
        } else {
            Err(AnalyticsError::new(format!(
                "analytics exporter flush failed for {failed} operation(s)"
            )))
        }
    }
}

pub struct AnalyticsClient {
    sinks: Vec<Box<dyn AnalyticsSink>>,
    redaction: RedactionPolicy,
    emit_tracing_events: bool,
}

impl AnalyticsClient {
    pub fn new() -> Self {
        Self {
            sinks: Vec::new(),
            redaction: RedactionPolicy::allow_pseudonymous(),
            emit_tracing_events: true,
        }
    }

    pub fn with_redaction(mut self, redaction: RedactionPolicy) -> Self {
        self.redaction = redaction;
        self
    }

    pub fn without_tracing_events(mut self) -> Self {
        self.emit_tracing_events = false;
        self
    }

    pub fn with_sink(mut self, sink: impl AnalyticsSink + 'static) -> Self {
        self.add_sink(sink);
        self
    }

    pub fn add_sink(&mut self, sink: impl AnalyticsSink + 'static) {
        self.sinks.push(Box::new(sink));
    }

    pub fn emit(&self, event: ObservedEvent) -> AnalyticsReport {
        let event = event.redacted(self.redaction);
        if self.emit_tracing_events {
            emit_tracing(&event);
        }

        let mut report = AnalyticsReport {
            attempted: self.sinks.len(),
            ..AnalyticsReport::default()
        };

        for sink in &self.sinks {
            match catch_unwind(AssertUnwindSafe(|| sink.emit(&event))) {
                Ok(Ok(())) => report.succeeded += 1,
                Ok(Err(err)) => {
                    report.failed += 1;
                    report.errors.push(err);
                }
                Err(_) => {
                    report.failed += 1;
                    report
                        .errors
                        .push(AnalyticsError::new("analytics sink panicked"));
                }
            }
        }

        report
    }

    pub fn track(&self, name: impl Into<String>) -> AnalyticsReport {
        self.emit(event(name))
    }

    pub fn flush(&self) -> AnalyticsReport {
        let mut report = AnalyticsReport {
            attempted: self.sinks.len(),
            ..AnalyticsReport::default()
        };

        for sink in &self.sinks {
            match catch_unwind(AssertUnwindSafe(|| sink.flush())) {
                Ok(Ok(())) => report.succeeded += 1,
                Ok(Err(err)) => {
                    report.failed += 1;
                    report.errors.push(err);
                }
                Err(_) => {
                    report.failed += 1;
                    report
                        .errors
                        .push(AnalyticsError::new("analytics sink panicked during flush"));
                }
            }
        }

        report
    }
}

impl Default for AnalyticsClient {
    fn default() -> Self {
        Self::new()
    }
}

pub fn event(name: impl Into<String>) -> ObservedEvent {
    ObservedEvent::analytics(name)
}

pub fn route_view(route: impl Into<String>) -> ObservedEvent {
    let route = route.into();
    event("route_view")
        .context(ObserveContext::default().with_route(route.clone()))
        .field("route", route, FieldPrivacy::Public)
}

pub fn component_view(component: impl Into<String>) -> ObservedEvent {
    let component = component.into();
    event("component_view")
        .context(ObserveContext::default().with_component(component.clone()))
        .field("component", component, FieldPrivacy::Public)
}

pub fn timing(name: impl Into<String>, duration_ms: f64, priority: EventPriority) -> ObservedEvent {
    event(name)
        .priority(priority)
        .field("duration_ms", duration_ms, FieldPrivacy::Public)
}

#[cfg(target_arch = "wasm32")]
pub mod web {
    use std::collections::BTreeMap;

    use js_sys::{Function, Object, Reflect};
    use pocopine_observe::{FieldValue, ObservedEvent};
    use wasm_bindgen::JsValue;

    use super::{AnalyticsError, AnalyticsSink};

    pub struct JsFunctionSink {
        callback: Function,
    }

    impl JsFunctionSink {
        pub fn new(callback: Function) -> Self {
            Self { callback }
        }
    }

    impl AnalyticsSink for JsFunctionSink {
        fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
            let value = serde_wasm_bindgen::to_value(event)
                .map_err(|err| AnalyticsError::new(err.to_string()))?;
            self.callback
                .call1(&JsValue::NULL, &value)
                .map(|_| ())
                .map_err(js_error)
        }
    }

    pub struct FirebaseAnalyticsSink {
        analytics: JsValue,
        log_event: Function,
    }

    impl FirebaseAnalyticsSink {
        pub fn new(analytics: JsValue, log_event: Function) -> Self {
            Self {
                analytics,
                log_event,
            }
        }
    }

    impl AnalyticsSink for FirebaseAnalyticsSink {
        fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
            let params = event_params(event)?;
            self.log_event
                .call3(
                    &JsValue::NULL,
                    &self.analytics,
                    &JsValue::from_str(&event.name),
                    &params,
                )
                .map(|_| ())
                .map_err(js_error)
        }
    }

    pub struct GoogleAnalyticsSink {
        gtag: Function,
    }

    impl GoogleAnalyticsSink {
        pub fn new(gtag: Function) -> Self {
            Self { gtag }
        }
    }

    impl AnalyticsSink for GoogleAnalyticsSink {
        fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError> {
            let params = event_params(event)?;
            self.gtag
                .call3(
                    &JsValue::NULL,
                    &JsValue::from_str("event"),
                    &JsValue::from_str(&event.name),
                    &params,
                )
                .map(|_| ())
                .map_err(js_error)
        }
    }

    fn event_params(event: &ObservedEvent) -> Result<JsValue, AnalyticsError> {
        let params = Object::new();
        set(
            &params,
            "event_version",
            JsValue::from_f64(event.version.into()),
        )?;
        set(
            &params,
            "event_class",
            JsValue::from_str(event.class.as_str()),
        )?;
        set(
            &params,
            "event_priority",
            JsValue::from_str(event.priority.as_str()),
        )?;

        if let Some(route) = &event.context.route {
            set(&params, "route", JsValue::from_str(route))?;
        }
        if let Some(component) = &event.context.component {
            set(&params, "component", JsValue::from_str(component))?;
        }
        if let Some(session_id) = &event.context.session_id {
            set(&params, "session_id", JsValue::from_str(session_id))?;
        }
        if let Some(user_id_hash) = &event.context.user_id_hash {
            set(&params, "user_id_hash", JsValue::from_str(user_id_hash))?;
        }

        let flat_fields: BTreeMap<_, _> = event
            .fields
            .iter()
            .map(|(name, field)| (name.as_str(), &field.value))
            .collect();
        for (name, value) in flat_fields {
            set(&params, name, field_value_to_js(value))?;
        }

        Ok(params.into())
    }

    fn set(object: &Object, key: &str, value: JsValue) -> Result<(), AnalyticsError> {
        Reflect::set(object, &JsValue::from_str(key), &value)
            .map(|_| ())
            .map_err(js_error)
    }

    fn field_value_to_js(value: &FieldValue) -> JsValue {
        match value {
            FieldValue::String(value) => JsValue::from_str(value),
            FieldValue::Bool(value) => JsValue::from_bool(*value),
            // JavaScript numbers are f64 values; 64-bit integers above 2^53
            // lose precision in Firebase/GA-style browser adapters. Treat
            // long-lived IDs as strings before constructing the event.
            FieldValue::I64(value) => JsValue::from_f64(*value as f64),
            FieldValue::U64(value) => JsValue::from_f64(*value as f64),
            FieldValue::F64(value) => JsValue::from_f64(*value),
        }
    }

    fn js_error(value: JsValue) -> AnalyticsError {
        AnalyticsError::new(
            value
                .as_string()
                .unwrap_or_else(|| "JavaScript analytics sink failed".to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use pocopine_observe::{FieldPrivacy, RedactionPolicy};

    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn analytics_client_is_send_sync_on_host() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<AnalyticsClient>();
    }

    #[test]
    fn emit_continues_after_sink_error() {
        let succeeded = Arc::new(AtomicUsize::new(0));
        let succeeded_sink = Arc::clone(&succeeded);
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(|_: &ObservedEvent| Err(AnalyticsError::new("downstream failed")))
            .with_sink(move |_: &ObservedEvent| {
                succeeded_sink.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });

        let report = client.emit(event("signup"));

        assert_eq!(report.attempted, 2);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(succeeded.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn emit_continues_after_sink_panic() {
        let succeeded = Arc::new(AtomicUsize::new(0));
        let succeeded_sink = Arc::clone(&succeeded);
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(|_: &ObservedEvent| panic!("sink failed"))
            .with_sink(move |_: &ObservedEvent| {
                succeeded_sink.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });

        let report = client.emit(event("signup"));

        assert_eq!(report.attempted, 2);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(report.errors[0].message(), "analytics sink panicked");
        assert_eq!(succeeded.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn redacts_before_dispatch() {
        let seen = Arc::new(Mutex::new(None));
        let seen_sink = Arc::clone(&seen);
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_redaction(RedactionPolicy::public_only())
            .with_sink(move |event: &ObservedEvent| {
                *seen_sink.lock().expect("seen lock poisoned") = Some(event.clone());
                Ok(())
            });

        client.emit(
            event("checkout")
                .field("plan", "pro", FieldPrivacy::Public)
                .field("email", "person@example.test", FieldPrivacy::Sensitive),
        );

        let event = seen
            .lock()
            .expect("seen lock poisoned")
            .clone()
            .expect("event captured");
        assert!(event.fields.contains_key("plan"));
        assert!(!event.fields.contains_key("email"));
    }

    #[test]
    fn flush_reports_success_without_delivery_wording() {
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(|_: &ObservedEvent| Ok(()));

        let report = client.flush();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failed, 0);
        assert!(report.all_succeeded());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bounded_sink_counts_backpressure_and_flush_delivery() {
        let delivered = Arc::new(AtomicUsize::new(0));
        let delivered_sink = Arc::clone(&delivered);
        let bounded = BoundedAnalyticsSink::new(
            move |_: &ObservedEvent| {
                delivered_sink.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            1,
        );
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(bounded.clone());

        let first = client.emit(event("first"));
        let second = client.emit(event("second"));

        assert!(first.all_succeeded());
        assert_eq!(second.attempted, 1);
        assert_eq!(second.succeeded, 0);
        assert_eq!(second.failed, 1);
        assert_eq!(
            second.errors[0].message(),
            "analytics export queue full (capacity 1)"
        );
        assert_eq!(
            bounded.metrics(),
            ExporterMetrics {
                pending: 1,
                enqueued: 1,
                dropped: 1,
                delivered: 0,
                failed: 0,
            }
        );

        let flush = client.flush();

        assert!(flush.all_succeeded());
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        assert_eq!(
            bounded.metrics(),
            ExporterMetrics {
                pending: 0,
                enqueued: 1,
                dropped: 1,
                delivered: 1,
                failed: 0,
            }
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bounded_sink_flush_counts_inner_errors_and_continues() {
        let delivered = Arc::new(Mutex::new(Vec::new()));
        let delivered_sink = Arc::clone(&delivered);
        let bounded = BoundedAnalyticsSink::new(
            move |event: &ObservedEvent| {
                if event.name == "bad" {
                    return Err(AnalyticsError::new("inner exporter rejected event"));
                }
                delivered_sink
                    .lock()
                    .expect("delivered lock poisoned")
                    .push(event.name.clone());
                Ok(())
            },
            4,
        );
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(bounded.clone());

        assert!(client.emit(event("bad")).all_succeeded());
        assert!(client.emit(event("good")).all_succeeded());

        let report = client.flush();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(
            report.errors[0].message(),
            "analytics exporter flush failed for 1 operation(s)"
        );
        assert_eq!(
            delivered
                .lock()
                .expect("delivered lock poisoned")
                .as_slice(),
            ["good"]
        );
        assert_eq!(
            bounded.metrics(),
            ExporterMetrics {
                pending: 0,
                enqueued: 2,
                dropped: 0,
                delivered: 1,
                failed: 1,
            }
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bounded_sink_flush_counts_inner_emit_panic() {
        let bounded = BoundedAnalyticsSink::new(
            |_: &ObservedEvent| -> Result<(), AnalyticsError> { panic!("inner emit panic") },
            2,
        );
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(bounded.clone());

        assert!(client.emit(event("panic")).all_succeeded());

        let report = client.flush();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(
            report.errors[0].message(),
            "analytics exporter flush failed for 1 operation(s)"
        );
        assert_eq!(
            bounded.metrics(),
            ExporterMetrics {
                pending: 0,
                enqueued: 1,
                dropped: 0,
                delivered: 0,
                failed: 1,
            }
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn bounded_sink_flush_counts_inner_flush_panic_after_delivery() {
        struct FlushPanicSink {
            delivered: Arc<AtomicUsize>,
        }

        impl AnalyticsSink for FlushPanicSink {
            fn emit(&self, _: &ObservedEvent) -> Result<(), AnalyticsError> {
                self.delivered.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }

            fn flush(&self) -> Result<(), AnalyticsError> {
                panic!("inner flush panic")
            }
        }

        let delivered = Arc::new(AtomicUsize::new(0));
        let bounded = BoundedAnalyticsSink::new(
            FlushPanicSink {
                delivered: Arc::clone(&delivered),
            },
            2,
        );
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(bounded.clone());

        assert!(client.emit(event("good")).all_succeeded());

        let report = client.flush();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.succeeded, 0);
        assert_eq!(report.failed, 1);
        assert_eq!(delivered.load(Ordering::SeqCst), 1);
        assert_eq!(
            bounded.metrics(),
            ExporterMetrics {
                pending: 0,
                enqueued: 1,
                dropped: 0,
                delivered: 1,
                failed: 1,
            }
        );
    }
}
