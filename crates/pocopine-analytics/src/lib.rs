//! Analytics and telemetry dispatch for pocopine applications.
//!
//! This crate intentionally does not own a vendor SDK. It owns the
//! redaction and fan-out rules, then lets apps attach Firebase,
//! Google Analytics, Cloudflare, OpenTelemetry, or custom sinks.

use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};

use pocopine_observe::{
    emit_tracing, EventPriority, FieldPrivacy, ObserveContext, ObservedEvent, RedactionPolicy,
};

pub trait AnalyticsSink {
    fn emit(&self, event: &ObservedEvent) -> Result<(), AnalyticsError>;

    fn flush(&self) -> Result<(), AnalyticsError> {
        Ok(())
    }
}

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
    pub attempted: usize,
    pub delivered: usize,
    pub failed: usize,
    pub errors: Vec<AnalyticsError>,
}

impl AnalyticsReport {
    pub fn all_delivered(&self) -> bool {
        self.failed == 0
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
                Ok(Ok(())) => report.delivered += 1,
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
                Ok(Ok(())) => report.delivered += 1,
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use pocopine_observe::{FieldPrivacy, RedactionPolicy};

    use super::*;

    #[test]
    fn emit_continues_after_sink_error() {
        let delivered = Rc::new(RefCell::new(0));
        let delivered_sink = Rc::clone(&delivered);
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_sink(|_: &ObservedEvent| Err(AnalyticsError::new("downstream failed")))
            .with_sink(move |_: &ObservedEvent| {
                *delivered_sink.borrow_mut() += 1;
                Ok(())
            });

        let report = client.emit(event("signup"));

        assert_eq!(report.attempted, 2);
        assert_eq!(report.delivered, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(*delivered.borrow(), 1);
    }

    #[test]
    fn redacts_before_dispatch() {
        let seen = Rc::new(RefCell::new(None));
        let seen_sink = Rc::clone(&seen);
        let client = AnalyticsClient::new()
            .without_tracing_events()
            .with_redaction(RedactionPolicy::public_only())
            .with_sink(move |event: &ObservedEvent| {
                *seen_sink.borrow_mut() = Some(event.clone());
                Ok(())
            });

        client.emit(
            event("checkout")
                .field("plan", "pro", FieldPrivacy::Public)
                .field("email", "person@example.test", FieldPrivacy::Sensitive),
        );

        let event = seen.borrow().clone().expect("event captured");
        assert!(event.fields.contains_key("plan"));
        assert!(!event.fields.contains_key("email"));
    }
}
